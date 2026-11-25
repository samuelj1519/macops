use crate::{
    config_inventory, config_local,
    context::Context,
    domain::{
        Action, ConfigAnchor, ConfigAuthority, ConfigFileStamp, Kind, LocalConfigQuery,
        Observation, OwnedPath, Package, PathKind, Plan, Provider, QuerySpec, fingerprint,
    },
    error::{Error, Result},
    operations,
    paths::{self, Directory, EntryKind},
    policy::{
        config_local as local_policy,
        config_tx::{
            self as policy, ConfigBackupRecord, MiseReconcilePlan, ReconcileGuard,
            RecoveryDecision, RestoreDecision, RootGuard,
        },
    },
    store::{CommitFailure, Journal, Phase, Store},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

const FILE_LIMIT: u64 = 1024 * 1024;

fn observe_anchor_stamp(id: &str, dir: &Directory, leaf: &str) -> Result<Option<ConfigFileStamp>> {
    let lineage = dir.lineage()?;
    let os_leaf = OsStr::new(leaf);
    let Some(meta) = dir.metadata(os_leaf)? else {
        dir.verify()?;
        return Ok(None);
    };
    if meta.kind != EntryKind::File {
        return Err(Error::attention(
            "unsupported_config_file",
            "配置对象必须是普通文件",
        ));
    }
    let (bytes, verified) = dir.regular_config_snapshot(os_leaf, FILE_LIMIT)?;
    dir.verify()?;
    let raw = format!("{:x}", Sha256::digest(&bytes));
    Ok(Some(ConfigFileStamp {
        content_digest: local_policy::content_digest(id, &raw)?,
        fingerprint: local_policy::stamp_fingerprint(
            &dir.path,
            leaf,
            &raw,
            &verified.identity,
            verified.mode,
            &lineage,
        )?,
        identity: verified.identity,
        mode: verified.mode,
    }))
}

fn backup_dir(state_root: &Path, op_id: &str) -> PathBuf {
    state_root.join("backups").join(op_id)
}

fn write_backup(
    store: &Store,
    record: &ConfigBackupRecord,
    before_bytes: Option<&[u8]>,
) -> Result<()> {
    use rustix::fs::{Mode, mkdirat};
    policy::validate_backup_record(record)?;
    let directory = store.path.join("backups");
    match mkdirat(&store.root, "backups", Mode::RWXU) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => (),
        Err(error) => return Err(std::io::Error::from_raw_os_error(error.raw_os_error()).into()),
    }
    let parent = Directory::open(&directory)?;
    mkdirat(parent.file(), record.operation_id.as_str(), Mode::RWXU)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    parent.file().sync_all()?;
    store.root.sync_all()?;
    let dir_fd = parent.child(OsStr::new(&record.operation_id))?;
    if let Some(raw) = before_bytes {
        dir_fd.atomic_replace_regular(OsStr::new("before.bin"), raw, 0o600, |_| Ok(()))?;
    }
    let mut meta_bytes = serde_json::to_vec_pretty(record)?;
    meta_bytes.push(b'\n');
    dir_fd.atomic_replace_regular(OsStr::new("config-backup.json"), &meta_bytes, 0o600, |_| {
        Ok(())
    })
}

fn load_backup(state_root: &Path, op_id: &str) -> Result<(ConfigBackupRecord, Option<Vec<u8>>)> {
    if !crate::store::valid_id(op_id) {
        return Err(Error::new("invalid_operation_id", op_id));
    }
    let dir = Directory::open(&backup_dir(state_root, op_id)).map_err(|e| {
        if e.code == "path_missing" {
            Error::attention("backup_not_found", format!("未找到操作 {op_id} 的配置备份"))
        } else {
            e
        }
    })?;
    let (meta_bytes, _) =
        dir.regular_config_snapshot(OsStr::new("config-backup.json"), FILE_LIMIT)?;
    let record: ConfigBackupRecord = serde_json::from_slice(&meta_bytes)?;
    policy::validate_backup_record(&record)?;
    if record.operation_id != op_id {
        return Err(Error::new("invalid_config_backup", "备份操作 ID 不一致"));
    }
    let before_bytes = if let Some(expected_before) = &record.before_stamp {
        let (bytes, _) = dir.regular_config_snapshot(OsStr::new("before.bin"), FILE_LIMIT)?;
        let raw = format!("{:x}", Sha256::digest(&bytes));
        if local_policy::content_digest(&record.config_id, &raw)? != expected_before.content_digest
        {
            return Err(Error::new("invalid_config_backup", "备份内容摘要校验失败"));
        }
        Some(bytes)
    } else {
        None
    };
    Ok((record, before_bytes))
}

fn check_stamp_cas(
    config_id: &str,
    dir: &Directory,
    leaf: &str,
    expected: Option<&ConfigFileStamp>,
) -> Result<()> {
    let current = observe_anchor_stamp(config_id, dir, leaf)?;
    if current.as_ref() != expected {
        return Err(Error::attention(
            "concurrent_target_modified",
            "目标配置文件在写入或回退前被外部并发修改，保留原状与备份",
        ));
    }
    Ok(())
}

fn rollback_to_backup(
    dir: &Directory,
    leaf: &str,
    backup: &ConfigBackupRecord,
    before_bytes: Option<&[u8]>,
    expected_current: &ConfigFileStamp,
) -> Result<()> {
    let check = |d: &Directory| check_stamp_cas(&backup.config_id, d, leaf, Some(expected_current));
    if let (Some(prev), Some(raw)) = (&backup.before_stamp, before_bytes) {
        dir.atomic_replace_regular(OsStr::new(leaf), raw, prev.mode, check)
    } else {
        dir.unlink_regular(OsStr::new(leaf), check)
    }
}

struct StagedConfigTx {
    parent_dir: Directory,
    before_bytes: Option<Vec<u8>>,
    cur_target: Option<ConfigFileStamp>,
    journal: Journal,
    backup: ConfigBackupRecord,
}

fn stage_config_tx(
    store: &Store,
    ctx: &Context,
    plan: Plan,
    query: &LocalConfigQuery,
    desired_mode: u32,
    reconcile: Option<ReconcileGuard>,
) -> Result<StagedConfigTx> {
    let locked_obs = config_local::observe(query, &mut (2 * FILE_LIMIT))?;
    if locked_obs != plan.before {
        return Err(Error::attention(
            "plan_stale",
            "配置文件状态在获取写锁前发生变化，未执行",
        ));
    }
    let Observation::ConfigFile {
        target: cur_target, ..
    } = locked_obs
    else {
        return Err(Error::new("invalid_config_plan", "配置观察结构异常"));
    };
    let parent_dir = Directory::open(&query.target.canonical_parent)?;
    if parent_dir.identity() != &query.target.parent_identity {
        return Err(Error::attention(
            "config_namespace_changed",
            "目标父目录身份已改变",
        ));
    }
    let before_bytes = if cur_target.is_some() {
        Some(
            parent_dir
                .regular_config_snapshot(OsStr::new(&query.target.name), FILE_LIMIT)?
                .0,
        )
    } else {
        None
    };
    check_stamp_cas(
        &plan.package.id,
        &parent_dir,
        &query.target.name,
        cur_target.as_ref(),
    )?;
    if let Some(raw) = &before_bytes {
        let digest =
            local_policy::content_digest(&plan.package.id, &format!("{:x}", Sha256::digest(raw)))?;
        if cur_target
            .as_ref()
            .is_none_or(|stamp| stamp.content_digest != digest)
        {
            return Err(Error::attention("plan_stale", "备份内容与冻结前态不一致"));
        }
    }
    let mut journal = operations::new_journal(ctx, plan)?;
    let expected_digest = journal.plan.target.clone().unwrap_or(fingerprint(&(
        "config-absent-v1",
        &journal.plan.package.id,
    ))?);
    let backup = ConfigBackupRecord {
        schema_version: 1,
        operation_id: journal.id.clone(),
        config_id: journal.plan.package.id.clone(),
        target: query.target.requested.clone(),
        parent_canonical: query.target.canonical_parent.clone(),
        parent_identity: query.target.parent_identity.clone(),
        before_stamp: cur_target.clone(),
        expected_after_digest: expected_digest.clone(),
        expected_after_mode: desired_mode,
        expected_absent: journal.plan.target.is_none(),
        reconcile,
    };
    write_backup(store, &backup, before_bytes.as_deref())?;
    store.save(&journal)?;
    journal.phase = Phase::Running;
    store.save(&journal)?;
    Ok(StagedConfigTx {
        parent_dir,
        before_bytes,
        cur_target,
        journal,
        backup,
    })
}

struct ConfigWrite<'a> {
    guard: Option<&'a dyn Fn() -> Result<()>>,
    query: &'a LocalConfigQuery,
    bytes: Option<&'a [u8]>,
    mode: u32,
    reconcile: Option<ReconcileGuard>,
}

fn verify_apply_authority(ctx: &Context, plan: &Plan, query: &LocalConfigQuery) -> Result<()> {
    if plan.action != Action::Apply {
        return Ok(());
    }
    let current = config_local::resolve_package(ctx, &plan.package.id)?
        .ok_or_else(|| Error::attention("config_authority_uncertain", "配置源候选已失效"))?;
    if fingerprint(&current)? != plan.recipe_sha256 || config_local::capture(&current)? != *query {
        return Err(Error::attention("plan_stale", "配置候选归属或目录身份改变"));
    }
    let mapping = config_inventory::discover(ctx);
    let receipts = crate::target::receipts(ctx)?;
    let before = match &plan.before {
        Observation::ConfigFile { target, .. } => target.as_ref(),
        _ => None,
    };
    policy::validate_static_apply_eligibility(
        &mapping.coverage.state,
        &mapping.coverage.issues,
        receipts.installations.get(&plan.package.id),
        before,
    )?;
    Ok(())
}

pub fn root_digest(ctx: &Context) -> Result<String> {
    let mapping = config_inventory::discover(ctx);
    let roots = mapping
        .roots
        .as_ref()
        .ok_or_else(|| Error::attention("config_authority_uncertain", "根解析不可用"))?;
    let (live, source, editable) = resolve_mise_paths(ctx)?;
    if !editable {
        return Err(Error::attention(
            "unsupported_config_source",
            "源不是静态文件",
        ));
    }
    let configured = Directory::open(&roots.configured_source)?;
    let effective = Directory::open(&roots.effective_source)?;
    let destination = Directory::open(&roots.destination)?;
    let directive = observe_anchor_stamp("config-root-directive", &configured, ".chezmoiroot")?;
    let config = roots
        .config
        .as_ref()
        .map(|path| {
            let anchor = config_local::capture_anchor(path)?;
            let directory = Directory::open(&anchor.canonical_parent)?;
            observe_anchor_stamp("config-root-settings", &directory, &anchor.name)
        })
        .transpose()?;
    fingerprint(&(
        roots,
        live,
        source,
        configured.lineage()?,
        effective.lineage()?,
        destination.lineage()?,
        directive,
        config,
    ))
}

fn verify_root_guard(home: &Path, guard: Option<&RootGuard>) -> Result<()> {
    let guard =
        guard.ok_or_else(|| Error::attention("reconcile_guard_missing", "缺少冻结的根解析前提"))?;
    let home = home.to_path_buf();
    let ctx = Context {
        harness: crate::context::HarnessRoots::default(),
        uv: crate::uv_local::DiscoveryInputs::default(),
        scope: crate::domain::Scope {
            home: home.clone(),
            paths: std::collections::BTreeMap::new(),
            service_identity: None,
        },
        timeout_secs: 120,
        home: home.clone(),
        config: home.clone(),
        data: home.clone(),
        settings: home.clone(),
        state: home.clone(),
        cache: home.clone(),
        catalog: home,
        catalog_explicit: false,
        isolated: guard.isolated,
        chezmoi: guard.inputs.clone(),
    };
    if root_digest(&ctx)? != guard.digest {
        return Err(Error::attention(
            "config_roots_changed",
            "配置根、控制文件或候选归属改变",
        ));
    }
    Ok(())
}

fn verify_reconcile_guard(home: &Path, guard: Option<&ReconcileGuard>) -> Result<()> {
    if let Some(guard) = guard {
        check_mise_layers(&guard.live.canonical_parent)?;
        verify_root_guard(home, guard.roots.as_ref())?;
        let live_dir = Directory::open(&guard.live.canonical_parent)?;
        if live_dir.identity() != &guard.live.parent_identity {
            return Err(Error::attention("plan_stale", "实际 mise 目录身份改变"));
        }
        let id = local_policy::config_id(&guard.live.requested)?;
        check_stamp_cas(&id, &live_dir, &guard.live.name, Some(&guard.stamp))?;
    }
    Ok(())
}

fn write_candidate(
    store: &Store,
    ctx: &Context,
    staged: &StagedConfigTx,
    write: &ConfigWrite<'_>,
) -> Result<Option<ConfigFileStamp>> {
    let leaf = OsStr::new(&write.query.target.name);
    let id = &staged.journal.plan.package.id;
    let check = |dir: &Directory| {
        if let Some(guard) = write.guard {
            guard()?;
        }
        verify_apply_authority(ctx, &staged.journal.plan, write.query)?;
        let now = config_local::observe(write.query, &mut (2 * FILE_LIMIT))?;
        if now != staged.journal.plan.before {
            return Err(Error::attention("plan_stale", "配置输入在写入准备期间改变"));
        }
        verify_reconcile_guard(&staged.journal.scope.home, staged.backup.reconcile.as_ref())?;
        check_stamp_cas(
            id,
            dir,
            &write.query.target.name,
            staged.cur_target.as_ref(),
        )?;
        store.verify_namespace()
    };
    if let Some(bytes) = write.bytes {
        let mut candidate = None;
        staged.parent_dir.atomic_replace_regular_verified(
            leaf,
            bytes,
            write.mode,
            |dir, meta| {
                let raw = format!("{:x}", Sha256::digest(bytes));
                let stamp = ConfigFileStamp {
                    identity: meta.identity.clone(),
                    mode: meta.mode,
                    content_digest: local_policy::content_digest(id, &raw)?,
                    fingerprint: local_policy::stamp_fingerprint(
                        &dir.path,
                        &write.query.target.name,
                        &raw,
                        &meta.identity,
                        meta.mode,
                        &dir.lineage()?,
                    )?,
                };
                let backup = Directory::open(&backup_dir(&store.path, &staged.journal.id))?;
                Store::atomic_new(backup.file(), "candidate.json", &stamp)?;
                candidate = Some(stamp);
                check(dir)
            },
        )?;
        Ok(candidate)
    } else {
        staged.parent_dir.unlink_regular(leaf, check)?;
        Ok(None)
    }
}

fn execute_locked_write(
    store: &Store,
    ctx: &Context,
    plan: Plan,
    write: &ConfigWrite<'_>,
) -> Result<Value> {
    let mut staged = stage_config_tx(
        store,
        ctx,
        plan,
        write.query,
        write.mode,
        write.reconcile.clone(),
    )?;
    let candidate = match write_candidate(store, ctx, &staged, write) {
        Ok(candidate) => candidate,
        Err(err) => {
            staged.journal.phase = Phase::Unknown;
            staged.journal.message = err.to_string();
            store.save(&staged.journal)?;
            return Ok(json!(staged.journal));
        }
    };
    let result = verify_apply_authority(ctx, &staged.journal.plan, write.query)
        .and_then(|()| verify_written(&staged, write.query, candidate.as_ref()));
    match result {
        Ok(observation) => match commit_config(
            store,
            Some(ctx),
            &staged,
            write.query,
            candidate.as_ref(),
            observation,
        ) {
            Ok(()) => {
                staged.journal.phase = Phase::Succeeded;
                staged.journal.message = "配置写事务通过锚定印记与权限复核".into();
            }
            Err(CommitFailure::Check(error)) => {
                fail_config_verification(&mut staged, write.query, candidate.as_ref(), &error);
            }
            Err(CommitFailure::Outcome(error)) => {
                staged.journal.phase = Phase::Unknown;
                staged.journal.message = format!("收据提交结果待恢复核验：{}", error.code);
            }
        },
        Err(error) => {
            fail_config_verification(&mut staged, write.query, candidate.as_ref(), &error);
        }
    }
    store.save(&staged.journal)?;
    Ok(json!(staged.journal))
}

fn fail_config_verification(
    staged: &mut StagedConfigTx,
    query: &LocalConfigQuery,
    candidate: Option<&ConfigFileStamp>,
    error: &Error,
) {
    let rollback = candidate
        .ok_or_else(|| Error::attention("restore_result_unknown", "删除后验证失败，保留阻断"))
        .and_then(|written| {
            rollback_to_backup(
                &staged.parent_dir,
                &query.target.name,
                &staged.backup,
                staged.before_bytes.as_deref(),
                written,
            )
        });
    staged.journal.phase = if rollback.is_ok() {
        Phase::Failed
    } else {
        Phase::Unknown
    };
    staged.journal.message = format!(
        "配置后置核验失败：{}；{}",
        error.code,
        if rollback.is_ok() {
            "已精确回退"
        } else {
            "保留备份与阻断"
        }
    );
}

fn commit_config(
    store: &Store,
    ctx: Option<&Context>,
    staged: &StagedConfigTx,
    query: &LocalConfigQuery,
    candidate: Option<&ConfigFileStamp>,
    observation: Observation,
) -> std::result::Result<(), CommitFailure> {
    store.commit_checked(&staged.journal, observation, || {
        if let Some(ctx) = ctx {
            verify_apply_authority(ctx, &staged.journal.plan, query)?;
        }
        verify_written(staged, query, candidate).map(|_| ())
    })
}

fn verify_written(
    staged: &StagedConfigTx,
    query: &LocalConfigQuery,
    candidate: Option<&ConfigFileStamp>,
) -> Result<Observation> {
    let post = config_local::observe(query, &mut (2 * FILE_LIMIT))?;
    let Observation::ConfigFile { target, source, .. } = &post else {
        return Err(Error::new("invalid_config_plan", "配置观察结构异常"));
    };
    if target.as_ref() != candidate {
        return Err(Error::attention(
            "concurrent_target_modified",
            "写后目标与本次临时文件印记不一致",
        ));
    }
    if let Observation::ConfigFile {
        source: before_source,
        ..
    } = &staged.journal.plan.before
        && source != before_source
    {
        return Err(Error::attention(
            "config_source_changed",
            "配置源在应用期间改变",
        ));
    }
    verify_reconcile_guard(&staged.journal.scope.home, staged.backup.reconcile.as_ref())?;
    check_stamp_cas(
        &staged.journal.plan.package.id,
        &staged.parent_dir,
        &query.target.name,
        candidate,
    )?;
    Ok(post)
}

pub fn restore(ctx: &Context, operation_id: &str, dry_run: bool, yes: bool) -> Result<Value> {
    let (backup, before_bytes) = load_backup(&ctx.state, operation_id)?;
    let parent_dir = Directory::open(&backup.parent_canonical)?;
    if parent_dir.identity() != &backup.parent_identity {
        return Err(Error::attention(
            "config_namespace_changed",
            "配置父目录身份已变化，拒绝自动恢复",
        ));
    }
    let leaf = backup
        .target
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("invalid_config_backup", "备份目标缺少文件名"))?;
    let current = observe_anchor_stamp(&backup.config_id, &parent_dir, leaf)?;
    match policy::decide_restore(&backup, current.as_ref()) {
        RestoreDecision::AlreadyAtBefore => {
            return Ok(json!({
                "backend": "rust",
                "mode": if dry_run { "dry-run" } else { "no-op" },
                "operation_id": operation_id,
                "id": backup.config_id,
                "changed": false,
                "persistentWrites": false
            }));
        }
        RestoreDecision::ExternalConflict => {
            return Err(Error::attention(
                "external_modification_detected",
                "目标文件在操作后已被外部修改，拒绝用历史备份覆盖",
            ));
        }
        RestoreDecision::ReadyToRestore => (),
    }
    if dry_run {
        return Ok(json!({
            "mode": "dry-run",
            "backend": "rust",
            "action": "restore",
            "from_operation": operation_id,
            "target": backup.target,
            "persistentWrites": false
        }));
    }
    operations::confirm(
        &format!("Restore {} from {}", backup.config_id, operation_id),
        yes,
    )?;
    let store = operations::lock_after_recovery(ctx)?;
    let (plan, query) = restore_plan(ctx, &backup, leaf)?;
    let Observation::ConfigFile { target: locked, .. } = &plan.before else {
        return Err(Error::new("invalid_config_plan", "配置观察结构异常"));
    };
    if locked.as_ref() != current.as_ref() {
        return Err(Error::attention("plan_stale", "配置在恢复确认期间改变"));
    }
    execute_locked_write(
        &store,
        ctx,
        plan,
        &ConfigWrite {
            guard: None,
            query: &query,
            bytes: before_bytes.as_deref(),
            mode: backup.before_stamp.as_ref().map_or(0o600, |s| s.mode),
            reconcile: None,
        },
    )
}

fn restore_plan(
    ctx: &Context,
    backup: &ConfigBackupRecord,
    leaf: &str,
) -> Result<(Plan, LocalConfigQuery)> {
    let query = LocalConfigQuery {
        kind: crate::domain::ConfigQueryKind::LocalV1,
        authority: ConfigAuthority::Untracked,
        target: ConfigAnchor {
            requested: backup.target.clone(),
            canonical_parent: backup.parent_canonical.clone(),
            parent_identity: backup.parent_identity.clone(),
            name: leaf.into(),
        },
        source: None,
        expected_mode: None,
    };
    let package = Package {
        id: backup.config_id.clone(),
        name: backup.target.display().to_string(),
        kind: Kind::Config,
        official_url: String::new(),
        provider: Provider::ConfigObserved {
            target: backup.target.clone(),
            source: None,
            authority: ConfigAuthority::Untracked,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Restore,
        before: config_local::observe(&query, &mut (2 * FILE_LIMIT))?,
        target: backup
            .before_stamp
            .as_ref()
            .map(|s| s.content_digest.clone()),
        command: None,
        query: Some(QuerySpec::LocalConfig(query.clone())),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    Ok((plan, query))
}

pub fn recover_journal(
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(json!(journal));
    }
    if resolve_failed {
        operations::confirm(
            "确认已人工核对配置事务与备份，记录失败并解除阻断；此动作不自动覆盖文件。",
            yes,
        )?;
        journal.phase = Phase::Failed;
        journal.message = "人工确认记录配置事务失败并保留备份".into();
        store.save(journal)?;
        return Ok(json!(journal));
    }
    let Some(QuerySpec::LocalConfig(query)) = journal.plan.query.clone() else {
        return Err(Error::new("invalid_config_plan", "配置恢复缺少冻结查询"));
    };
    let (backup, before_bytes) = load_backup(&store.path, &journal.id)?;
    validate_backup_binding(&backup, journal, &query)?;
    let current_obs = config_local::observe(&query, &mut (2 * FILE_LIMIT))?;
    let current = match &current_obs {
        Observation::ConfigFile { target, .. } => target.as_ref(),
        _ => None,
    };
    let committed = store
        .receipts()?
        .installations
        .get(&journal.plan.package.id)
        .is_some_and(|r| r.operation_id == journal.id);
    let candidate = load_candidate(store, &journal.id)?;
    let decision = policy::decide_tx_recovery(
        journal.phase == Phase::Prepared,
        committed,
        &backup,
        current,
        None,
    );
    let parent_dir = Directory::open(&backup.parent_canonical)?;
    let staged = StagedConfigTx {
        parent_dir,
        before_bytes,
        cur_target: backup.before_stamp.clone(),
        journal: journal.clone(),
        backup,
    };
    let proof = (staged.backup.expected_absent && current.is_none())
        || (candidate.is_some() && candidate.as_ref() == current);
    match decision {
        RecoveryDecision::AlreadyCommitted | RecoveryDecision::CommitVerifiedWrite if proof => {
            finish_verified_recovery(
                store,
                journal,
                &staged,
                &query,
                current,
                candidate.as_ref(),
                committed,
            );
        }
        RecoveryDecision::PreparedUntouched if !committed => {
            journal.phase = Phase::Failed;
            journal.message = "目标仍为冻结前态，记录未执行并保留备份".into();
        }
        _ => {
            journal.phase = Phase::Unknown;
            journal.message = "目标与本次候选印记不一致，保留外部修改与阻断".into();
        }
    }
    store.save(journal)?;
    Ok(json!(journal))
}

fn finish_verified_recovery(
    store: &Store,
    journal: &mut Journal,
    staged: &StagedConfigTx,
    query: &LocalConfigQuery,
    current: Option<&ConfigFileStamp>,
    candidate: Option<&ConfigFileStamp>,
    committed: bool,
) {
    let verified = if journal.plan.action == Action::Reconcile && staged.backup.reconcile.is_none()
    {
        Err(Error::attention(
            "reconcile_guard_missing",
            "缺少冻结的全文件恢复前提",
        ))
    } else {
        verify_written(staged, query, current)
    };
    if let Ok(observation) = verified {
        match commit_config(store, None, staged, query, current, observation) {
            Ok(()) => {
                journal.phase = Phase::Succeeded;
                journal.message = "恢复已验证完整配置写入与冻结前提，完成收口".into();
            }
            Err(CommitFailure::Check(error)) => {
                if !committed
                    && let Some(written) = candidate
                    && rollback_to_backup(
                        &staged.parent_dir,
                        &query.target.name,
                        &staged.backup,
                        staged.before_bytes.as_deref(),
                        written,
                    )
                    .is_ok()
                {
                    journal.phase = Phase::Failed;
                    journal.message = format!("提交前验证拒绝，已精确回退：{}", error.code);
                } else {
                    journal.phase = Phase::Unknown;
                    journal.message = format!("提交前验证拒绝，保留阻断：{}", error.code);
                }
            }
            Err(CommitFailure::Outcome(error)) => {
                journal.phase = Phase::Unknown;
                journal.message = format!("收据提交结果待恢复核验：{}", error.code);
            }
        }
    } else if !committed
        && let Some(written) = candidate
        && rollback_to_backup(
            &staged.parent_dir,
            &query.target.name,
            &staged.backup,
            staged.before_bytes.as_deref(),
            written,
        )
        .is_ok()
    {
        journal.phase = Phase::Failed;
        journal.message = "恢复前提失效，仅对本次候选印记精确回退".into();
    } else {
        journal.phase = Phase::Unknown;
        journal.message = "恢复完整前提未通过，保留备份与阻断".into();
    }
}

fn load_candidate(store: &Store, id: &str) -> Result<Option<ConfigFileStamp>> {
    let dir = Directory::open(&backup_dir(&store.path, id))?;
    let stamp: Option<ConfigFileStamp> = Store::read(dir.file(), "candidate.json")?;
    if stamp.as_ref().is_some_and(|s| {
        !local_policy::valid_digest(&s.content_digest)
            || !local_policy::valid_digest(&s.fingerprint)
            || s.mode > 0o7777
    }) {
        return Err(Error::new("invalid_config_backup", "候选印记无效"));
    }
    Ok(stamp)
}

fn validate_backup_binding(
    backup: &ConfigBackupRecord,
    journal: &Journal,
    query: &LocalConfigQuery,
) -> Result<()> {
    if backup.config_id != journal.plan.package.id
        || backup.target != query.target.requested
        || backup.parent_canonical != query.target.canonical_parent
        || backup.parent_identity != query.target.parent_identity
        || backup.expected_absent != journal.plan.target.is_none()
        || journal
            .plan
            .target
            .as_ref()
            .is_some_and(|d| d != &backup.expected_after_digest)
        || !matches!(&journal.plan.before, Observation::ConfigFile { target, .. } if target == &backup.before_stamp)
    {
        return Err(Error::new("invalid_config_backup", "备份与冻结事务不一致"));
    }
    Ok(())
}

pub fn check_mise_layers(mise_dir: &Path) -> Result<()> {
    for (raw_k, _) in std::env::vars_os() {
        if raw_k.to_str().is_some_and(policy::has_mise_env_override) {
            return Err(Error::attention(
                "mise_env_override_active",
                "检测到 MISE_* 环境变量覆盖，拒绝单文件配置收敛",
            ));
        }
    }
    let mise_dir = paths::read_path(mise_dir)?;
    let dir = match Directory::open(&mise_dir) {
        Ok(dir) => dir,
        Err(error) if error.code == "path_missing" => return Ok(()),
        Err(error) => return Err(error),
    };
    for item in dir.names()? {
        let item = item.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let Ok(name) = std::str::from_utf8(item.file_name().to_bytes()) else {
            continue;
        };
        if name == "config.local.toml"
            || (name.starts_with("config.")
                && name != "config.toml"
                && Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml")))
        {
            return Err(Error::attention(
                "mise_layered_config_present",
                format!("存在分层配置 {name}，拒绝单文件自动收敛"),
            ));
        }
    }
    if dir.metadata(OsStr::new("conf.d"))?.is_some() {
        let conf_d = dir.child(OsStr::new("conf.d"))?;
        for item in conf_d.names()? {
            let item = item.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
            if std::str::from_utf8(item.file_name().to_bytes()).is_ok_and(|n| {
                Path::new(n)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
            }) {
                return Err(Error::attention(
                    "mise_layered_config_present",
                    "存在 conf.d/*.toml 分层配置，拒绝单文件自动收敛",
                ));
            }
        }
    }
    Ok(())
}

pub fn resolve_mise_paths(ctx: &Context) -> Result<(PathBuf, PathBuf, bool)> {
    let mapping = config_inventory::discover(ctx);
    let dest_root = mapping
        .roots
        .as_ref()
        .map_or_else(|| ctx.home.clone(), |r| r.destination.clone());
    let live = dest_root.join(".config/mise/config.toml");
    let matched: Vec<_> = mapping
        .candidates
        .iter()
        .filter(|c| c.target == live)
        .map(|c| c.source.path.clone())
        .collect();
    if matched.len() > 1 {
        return Err(Error::attention(
            "owner_conflict",
            "mise 配置文件存在多个 chezmoi 候选源",
        ));
    }
    if matches!(
        mapping.coverage.state,
        crate::resource::CoverageState::Unavailable
            | crate::resource::CoverageState::Invalid
            | crate::resource::CoverageState::Truncated
    ) {
        return Err(Error::attention(
            "config_authority_uncertain",
            "mise 配置来源扫描不完整",
        ));
    }
    let src = matched.into_iter().next().ok_or_else(|| {
        Error::attention("mise_source_config_missing", "没有唯一静态 mise 配置源")
    })?;
    let editable = src
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| local_policy::classify_source_name(name).is_ok());
    Ok((live, src, editable))
}

fn read_utf8_regular(path: &Path, missing_code: &'static str) -> Result<String> {
    let clean = crate::policy::paths::lexical(path)?;
    let parent = clean
        .parent()
        .ok_or_else(|| Error::new("invalid_config_target", "缺少父目录"))?;
    let leaf = clean
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::new("invalid_config_target", "缺少文件名"))?;
    let dir = Directory::open(parent).map_err(|e| {
        if e.code == "path_missing" {
            Error::attention(
                missing_code,
                format!("配置文件目录不存在：{}", parent.display()),
            )
        } else {
            e
        }
    })?;
    if dir.metadata(OsStr::new(leaf))?.is_none() {
        return Err(Error::attention(
            missing_code,
            format!("配置文件不存在：{}", clean.display()),
        ));
    }
    let (bytes, _) = dir.regular_config_snapshot(OsStr::new(leaf), FILE_LIMIT)?;
    String::from_utf8(bytes)
        .map_err(|_| Error::attention("invalid_mise_toml", "mise 配置文件不是有效 UTF-8"))
}

pub fn reconcile(ctx: &Context, tool: Option<&str>, dry_run: bool, yes: bool) -> Result<Value> {
    if let Some(t) = tool {
        policy::ai_mise_spec(t)?;
    }
    let (live_path, src_path, editable) = resolve_mise_paths(ctx)?;
    check_mise_layers(
        live_path
            .parent()
            .ok_or_else(|| Error::new("invalid_config_target", "缺少配置目录"))?,
    )?;
    if !editable {
        if tool.is_none() {
            return Ok(json!({
                "backend": "rust",
                "reconcile_state": "drift",
                "reason": "source_not_editable",
                "live": live_path,
                "source": src_path,
                "rows": []
            }));
        }
        return Err(Error::attention(
            "unsupported_config_source",
            "mise 声明源包含模板或非静态属性，拒绝自动写回",
        ));
    }
    let fence = capture_reconcile_fence(ctx, &live_path, &src_path)?;
    let live_text = read_utf8_regular(&live_path, "mise_live_config_missing")?;
    let src_text = read_utf8_regular(&src_path, "mise_source_config_missing")?;
    let evaluated = policy::evaluate_mise_reconcile(&live_text, &src_text, tool)?;
    match evaluated {
        MiseReconcilePlan::InSync { spec, pin } => Ok(json!({
            "backend": "rust",
            "reconcile_state": "in_sync",
            "spec": spec,
            "pin": pin,
            "live": live_path,
            "source": src_path
        })),
        MiseReconcilePlan::Drift { reason, rows } => Ok(json!({
            "backend": "rust",
            "reconcile_state": "drift",
            "reason": reason,
            "rows": rows,
            "live": live_path,
            "source": src_path
        })),
        MiseReconcilePlan::Rewrite {
            spec,
            from,
            to,
            rewritten,
        } => {
            if dry_run {
                return Ok(json!({
                    "mode": "dry-run",
                    "backend": "rust",
                    "reconcile_state": "would_reconcile",
                    "spec": spec,
                    "from": from,
                    "to": to,
                    "source": src_path,
                    "persistentWrites": false
                }));
            }
            operations::confirm(&format!("Reconcile {spec} ({from} -> {to})"), yes)?;
            let store = Store::lock(ctx)?;
            store.check_pending()?;
            reconcile_source_locked(
                &store,
                ctx,
                &ReconcileWrite {
                    guard: None,
                    operation: None,
                    live_path: &live_path,
                    src_path: &src_path,
                    spec: &spec,
                    from: &from,
                    to: &to,
                    rewritten: &rewritten,
                    fence: &fence,
                },
            )
        }
    }
}

pub struct ReconcileWrite<'a> {
    pub guard: Option<&'a dyn Fn() -> Result<()>>,
    pub operation: Option<&'a Journal>,
    pub live_path: &'a Path,
    pub src_path: &'a Path,
    pub spec: &'a str,
    pub from: &'a str,
    pub to: &'a str,
    pub rewritten: &'a str,
    pub fence: &'a ReconcileFence,
}

pub struct ReconcileFence {
    source: ConfigAnchor,
    source_stamp: ConfigFileStamp,
    live: ConfigAnchor,
    live_stamp: ConfigFileStamp,
    roots: RootGuard,
}

pub fn capture_reconcile_fence(
    ctx: &Context,
    live: &Path,
    source: &Path,
) -> Result<ReconcileFence> {
    let source = config_local::capture_anchor(source)?;
    let live = config_local::capture_anchor(live)?;
    let stamp = |anchor: &ConfigAnchor| {
        let directory = Directory::open(&anchor.canonical_parent)?;
        let id = local_policy::config_id(&anchor.requested)?;
        observe_anchor_stamp(&id, &directory, &anchor.name)?
            .ok_or_else(|| Error::attention("mise_config_missing", "配置输入缺失"))
    };
    let source_stamp = stamp(&source)?;
    let live_stamp = stamp(&live)?;
    let roots = RootGuard {
        inputs: ctx.chezmoi.clone(),
        isolated: ctx.isolated,
        digest: root_digest(ctx)?,
    };
    Ok(ReconcileFence {
        source,
        source_stamp,
        live,
        live_stamp,
        roots,
    })
}

fn capture_reconcile_roots(ctx: &Context, req: &ReconcileWrite<'_>) -> Result<RootGuard> {
    let (active_live, active_source, editable) = resolve_mise_paths(ctx)?;
    if active_live != req.live_path || active_source != req.src_path || !editable {
        return Err(Error::attention(
            "plan_stale",
            "mise 配置的根解析或唯一源候选改变",
        ));
    }
    let roots = req.fence.roots.clone();
    if roots.inputs != ctx.chezmoi
        || roots.isolated != ctx.isolated
        || roots.digest != root_digest(ctx)?
    {
        return Err(Error::attention(
            "config_roots_changed",
            "配置根在确认期间改变",
        ));
    }
    check_mise_layers(
        active_live
            .parent()
            .ok_or_else(|| Error::new("invalid_config_target", "缺少配置目录"))?,
    )?;
    Ok(roots)
}

fn capture_reconcile_before(
    req: &ReconcileWrite<'_>,
    query: &LocalConfigQuery,
    roots: RootGuard,
) -> Result<(Observation, ReconcileGuard, u32)> {
    let source_text = read_utf8_regular(req.src_path, "mise_source_config_missing")?;
    let live_text = read_utf8_regular(req.live_path, "mise_live_config_missing")?;
    let tool = ["codex", "claude", "cursor", "kimi", "dsh", "codegraph"]
        .into_iter()
        .find(|tool| policy::ai_mise_spec(tool).is_ok_and(|spec| spec == req.spec))
        .ok_or_else(|| Error::new("unsupported_reconcile_tool", req.spec))?;
    let expected = policy::evaluate_mise_reconcile(&live_text, &source_text, Some(tool))?;
    if !matches!(expected, MiseReconcilePlan::Rewrite { ref rewritten, ref from, ref to, .. }
        if rewritten == req.rewritten && from == req.from && to == req.to)
        || live_text != req.rewritten
    {
        return Err(Error::attention(
            "plan_stale",
            "mise 全文件或目标 pin 在确认期间改变",
        ));
    }
    let live_anchor = config_local::capture_anchor(req.live_path)?;
    let live_dir = Directory::open(&live_anchor.canonical_parent)?;
    let live_id = local_policy::config_id(req.live_path)?;
    let live_stamp = observe_anchor_stamp(&live_id, &live_dir, &live_anchor.name)?
        .ok_or_else(|| Error::attention("mise_live_config_missing", "实际配置缺失"))?;
    let raw_live = format!("{:x}", Sha256::digest(live_text.as_bytes()));
    if local_policy::content_digest(&live_id, &raw_live)? != live_stamp.content_digest
        || live_anchor != req.fence.live
        || live_stamp != req.fence.live_stamp
    {
        return Err(Error::attention("plan_stale", "实际配置在读取期间改变"));
    }
    let before = config_local::observe(query, &mut (2 * FILE_LIMIT))?;
    let Observation::ConfigFile {
        target: Some(ref src_stamp),
        ..
    } = before
    else {
        return Err(Error::attention(
            "mise_source_config_missing",
            "mise 源文件缺失",
        ));
    };
    let source_digest = local_policy::content_digest(
        &local_policy::config_id(req.src_path)?,
        &format!("{:x}", Sha256::digest(source_text.as_bytes())),
    )?;
    if src_stamp.content_digest != source_digest
        || query.target != req.fence.source
        || *src_stamp != req.fence.source_stamp
    {
        return Err(Error::attention("plan_stale", "配置源在读取期间改变"));
    }
    let mode = src_stamp.mode;
    Ok((
        before,
        ReconcileGuard {
            live: live_anchor,
            stamp: live_stamp,
            roots: Some(roots),
        },
        mode,
    ))
}

pub fn reconcile_source_locked(
    store: &Store,
    ctx: &Context,
    req: &ReconcileWrite<'_>,
) -> Result<Value> {
    let clean_src = crate::policy::paths::lexical(req.src_path)?;
    let src_id = local_policy::config_id(&clean_src)?;
    let package = Package {
        id: src_id.clone(),
        name: clean_src.display().to_string(),
        kind: Kind::Config,
        official_url: String::new(),
        paths: vec![OwnedPath {
            path: clean_src.clone(),
            kind: PathKind::Config,
        }],
        provider: Provider::ConfigObserved {
            target: clean_src,
            source: None,
            authority: ConfigAuthority::Untracked,
        },
        exact_version_required: false,
    };
    let query = config_local::capture(&package)?;
    let roots = capture_reconcile_roots(ctx, req)?;
    let (before, guard, mode) = capture_reconcile_before(req, &query, roots)?;
    let expected_digest = local_policy::content_digest(
        &src_id,
        &format!("{:x}", Sha256::digest(req.rewritten.as_bytes())),
    )?;
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Reconcile,
        before,
        target: Some(expected_digest),
        command: None,
        query: Some(QuerySpec::LocalConfig(query.clone())),
        warnings: vec![format!("{}: {} -> {}", req.spec, req.from, req.to)],
        synchronous: true,
        delegated: false,
    };
    let journal = if let Some(parent) = req.operation {
        reconcile_in_operation(
            store,
            ctx,
            parent,
            plan,
            &ConfigWrite {
                guard: req.guard,
                query: &query,
                bytes: Some(req.rewritten.as_bytes()),
                mode,
                reconcile: Some(guard),
            },
        )?
    } else {
        execute_locked_write(
            store,
            ctx,
            plan,
            &ConfigWrite {
                guard: req.guard,
                query: &query,
                bytes: Some(req.rewritten.as_bytes()),
                mode,
                reconcile: Some(guard),
            },
        )?
    };
    Ok(
        json!({"backend":"rust", "reconcile_state": if journal["phase"] == "succeeded" { "reconciled" } else { "unknown" },
        "spec":req.spec, "from":req.from, "to":req.to, "journal":journal}),
    )
}

pub fn ai_reconcile_completed(store: &Store, journal: &Journal) -> Result<()> {
    let (record, _) = load_backup(&store.path, &journal.id)?;
    let directory = Directory::open(&record.parent_canonical)?;
    let backup = Directory::open(&backup_dir(&store.path, &journal.id))?;
    let candidate: ConfigFileStamp = Store::read(backup.file(), "candidate.json")?
        .ok_or_else(|| Error::attention("ai_source_write_unknown", "源写回候选印记缺失"))?;
    if directory.identity() != &record.parent_identity
        || candidate.content_digest != record.expected_after_digest
    {
        return Err(Error::attention(
            "ai_source_write_unknown",
            "源写回身份或摘要改变",
        ));
    }
    verify_reconcile_guard(&journal.home, record.reconcile.as_ref())?;
    check_stamp_cas(
        &record.config_id,
        &directory,
        record
            .target
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::new("invalid_config_backup", "文件名无效"))?,
        Some(&candidate),
    )
}

fn reconcile_in_operation(
    store: &Store,
    ctx: &Context,
    parent: &Journal,
    plan: Plan,
    write: &ConfigWrite<'_>,
) -> Result<Value> {
    let before = config_local::observe(write.query, &mut (2 * FILE_LIMIT))?;
    if before != plan.before {
        return Err(Error::attention("plan_stale", "源写回前态改变"));
    }
    let Observation::ConfigFile {
        target: Some(stamp),
        ..
    } = &before
    else {
        return Err(Error::attention("mise_source_config_missing", "源文件缺失"));
    };
    let directory = Directory::open(&write.query.target.canonical_parent)?;
    let bytes = directory
        .regular_config_snapshot(OsStr::new(&write.query.target.name), FILE_LIMIT)?
        .0;
    let mut internal = parent.clone();
    internal.plan = plan;
    let backup = ConfigBackupRecord {
        schema_version: 1,
        operation_id: parent.id.clone(),
        config_id: internal.plan.package.id.clone(),
        target: write.query.target.requested.clone(),
        parent_canonical: write.query.target.canonical_parent.clone(),
        parent_identity: write.query.target.parent_identity.clone(),
        before_stamp: Some(stamp.clone()),
        expected_after_digest: internal
            .plan
            .target
            .clone()
            .ok_or_else(|| Error::new("invalid_config_plan", "缺少写回摘要"))?,
        expected_after_mode: write.mode,
        expected_absent: false,
        reconcile: write.reconcile.clone(),
    };
    write_backup(store, &backup, Some(&bytes))?;
    let mut staged = StagedConfigTx {
        parent_dir: directory,
        before_bytes: Some(bytes),
        cur_target: Some(stamp.clone()),
        journal: internal,
        backup,
    };
    store.verify_namespace()?;
    let candidate = write_candidate(store, ctx, &staged, write)?;
    store.verify_namespace()?;
    if let Err(error) = verify_written(&staged, write.query, candidate.as_ref()) {
        fail_config_verification(&mut staged, write.query, candidate.as_ref(), &error);
        return Err(Error::attention(
            "ai_source_write_unknown",
            staged.journal.message,
        ));
    }
    Ok(json!({"phase":"succeeded", "id":parent.id}))
}

#[cfg(test)]
#[path = "../tests/unit/config_tx.rs"]
mod tests;
