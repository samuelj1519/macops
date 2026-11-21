use crate::policy::state::Recovery;
use crate::{
    context::Context,
    domain::{Action, Observation, Package, Plan, Provider, QuerySpec, Receipts, fingerprint},
    error::{Error, Result},
    inventory, paths, process, providers,
    store::{self, Journal, Phase, Store},
};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Read, Seek, SeekFrom, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

pub fn confirm(message: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(Error::attention(
            "confirmation_required",
            "非交互环境需要 --yes",
        ));
    }
    let mut error = std::io::stderr().lock();
    writeln!(error, "{message}\n继续？输入 yes：")?;
    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;
    if value.trim() == "yes" {
        Ok(())
    } else {
        Err(Error::attention("cancelled", "操作已取消"))
    }
}

pub fn plan(
    ctx: &Context,
    package: &Package,
    action: Action,
    version: Option<&str>,
    dry_run: bool,
) -> Result<Plan> {
    crate::policy::uv_local::action(&package.provider, action)?;
    if matches!(package.provider, Provider::Uv { binding: None, .. }) {
        return Err(Error::attention(
            "legacy_uv_query_unbound",
            "旧 uv 声明未绑定 namespace，不能采纳或启动原生查询",
        ));
    }
    crate::policy::state::version_request(package, action, version)?;
    let frozen = local_query(ctx, package, action)?;
    let before = match &frozen {
        Some(query) => providers::observe_query(package, query, &ctx.scope, false)?,
        None => inventory::observe(ctx, package),
    };
    crate::policy::state::plan_inputs(package, action, &before, version)?;
    if action == Action::Adopt {
        return adoption_plan(ctx, package, before, frozen);
    }
    check_before(ctx, package, action, &before, frozen.as_ref())?;
    let cached = if dry_run && version.is_none() && action != Action::Uninstall {
        crate::policy::cache::indexed_version(&crate::updates::read(ctx)?, package, &ctx.scope)?
    } else {
        None
    };
    let resolution =
        crate::policy::state::version_resolution(action, dry_run, version, cached.as_deref())?;
    if resolution == crate::policy::state::Resolution::Unresolved {
        return Ok(Plan {
            brew: None,
            npm_binding: None,
            uv_owner: None,
            timeout_secs: ctx.timeout_secs,
            package: package.clone(),
            action,
            before,
            target: None,
            command: None,
            query: None,
            recipe_sha256: fingerprint(package)?,
            warnings: vec![
                "目标尚未解析；预演不联网。先 update 或提供 --version，才能显示完整执行命令".into(),
            ],
            synchronous: providers::synchronous(package),
            delegated: false,
        });
    }
    let target = match resolution {
        crate::policy::state::Resolution::Explicit(v)
        | crate::policy::state::Resolution::Cached(v) => Some(v),
        crate::policy::state::Resolution::Latest => Some(crate::updates::latest(ctx, package)?),
        crate::policy::state::Resolution::Absent | crate::policy::state::Resolution::Unresolved => {
            None
        }
    };
    let native_version = if matches!(package.provider, Provider::Brew { .. }) {
        None
    } else {
        target.as_deref()
    };
    let mut command = providers::mutation_spec(ctx, package, action, native_version)?;
    if let Some(QuerySpec::LocalNpm(query)) = &frozen {
        crate::npm_local::verify_plain_namespace_for_write(query, action)?;
        crate::policy::npm_local::bind_canonical_command_paths(
            &mut command,
            &query.namespace.canonical_prefix,
        )?;
    }
    if ctx.isolated && !paths::read_path(&command.program)?.starts_with(&ctx.home) {
        return Err(Error::new(
            "global_write_in_fixture",
            "fixture 只能运行 fixture 内声明的写程序",
        ));
    }
    let query = match frozen {
        Some(query) => query,
        None => providers::query_spec(ctx, package)?,
    };
    Ok(Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        package: package.clone(),
        action,
        before,
        target,
        command: Some(command),
        query: Some(query),
        recipe_sha256: fingerprint(package)?,
        warnings: if matches!(package.provider, Provider::Brew { .. }) {
            vec!["brew 无通用固定版本参数；若解析后版本变化，后置验证将停止并报告 unknown".into()]
        } else if let Provider::Npm { authority, .. } = &package.provider {
            crate::policy::npm_local::write_warnings(authority.as_ref(), action)
        } else {
            vec![]
        },
        synchronous: providers::synchronous(package),
        delegated: false,
    })
}

pub fn new_journal(ctx: &Context, plan: Plan) -> Result<Journal> {
    new_journal_with_id(ctx, plan, store::operation_id())
}

pub fn new_journal_with_id(ctx: &Context, plan: Plan, id: String) -> Result<Journal> {
    let journal = Journal {
        scope_sha256: fingerprint(&ctx.scope)?,
        scope: ctx.scope.clone(),
        schema_version: 1,
        id,
        phase: Phase::Prepared,
        plan_sha256: fingerprint(&plan)?,
        plan,
        home: ctx.home.clone(),
        state: ctx.state.clone(),
        cache: ctx.cache.clone(),
        settings: ctx.settings.clone(),
        process_group: None,
        started_at: store::now(),
        message: String::new(),
    };
    store::validate_journal(&journal)?;
    Ok(journal)
}

pub fn execute(ctx: &Context, plan: Plan, dry_run: bool, yes: bool) -> Result<Value> {
    execute_checked(ctx, plan, dry_run, yes, || Ok(()))
}

pub(crate) fn execute_checked(
    ctx: &Context,
    plan: Plan,
    dry_run: bool,
    yes: bool,
    check: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    crate::policy::uv_local::validate_plan(&plan)?;
    if dry_run {
        return Ok(json!({"mode":"dry-run","backend":"rust","plan":plan,"persistentWrites":false}));
    }
    if matches!(
        plan.action,
        Action::Uninstall | Action::Adopt | Action::Start | Action::Stop
    ) {
        confirm(&format!("{:?} {}", plan.action, plan.package.id), yes)?;
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    store.receipts()?;
    let observed = match &plan.query {
        Some(
            query @ (QuerySpec::LocalUvTool(_)
            | QuerySpec::LocalNpm(_)
            | QuerySpec::LocalConfig(_)
            | QuerySpec::LocalCapability(_)),
        ) => providers::observe_query(&plan.package, query, &ctx.scope, false)?,
        _ => inventory::observe(ctx, &plan.package),
    };
    let current = crate::target::refresh(ctx, &plan.package)?;
    crate::policy::state::require_fresh_plan(&plan, &current, &observed)?;
    check_before(
        ctx,
        &plan.package,
        plan.action,
        &observed,
        plan.query.as_ref().filter(|query| {
            matches!(
                query,
                QuerySpec::LocalUvTool(_)
                    | QuerySpec::LocalNpm(_)
                    | QuerySpec::LocalConfig(_)
                    | QuerySpec::LocalCapability(_)
            )
        }),
    )?;
    if let Some(QuerySpec::LocalNpm(query)) = &plan.query {
        crate::npm_local::verify_plain_namespace_for_write(query, plan.action)?;
    }
    if plan.uv_owner.is_some() {
        crate::uv_ownership::check(ctx, &plan, None)?;
    }
    check()?;
    let mut journal = new_journal(ctx, plan)?;
    store.save(&journal)?;
    if journal.plan.action == Action::Adopt {
        if journal.plan.uv_owner.is_some() {
            store
                .commit_checked(&journal, journal.plan.before.clone(), || {
                    crate::uv_ownership::check(ctx, &journal.plan, None)?;
                    let query = journal
                        .plan
                        .query
                        .as_ref()
                        .ok_or_else(|| Error::new("invalid_uv_plan", "缺少冻结 uv 查询"))?;
                    let current = providers::observe_query(
                        &journal.plan.package,
                        query,
                        &journal.scope,
                        false,
                    )?;
                    crate::policy::state::require_fresh_plan(
                        &journal.plan,
                        &journal.plan.package,
                        &current,
                    )
                })
                .map_err(|e| match e {
                    store::CommitFailure::Check(e) | store::CommitFailure::Outcome(e) => e,
                })?;
        } else {
            store.commit(&journal, journal.plan.before.clone())?;
        }
        journal.phase = Phase::Succeeded;
        store.save(&journal)?;
        return Ok(json!(journal));
    }
    let result = launch(&store, &mut journal)?;
    if let Some(message) = crate::policy::state::completion(
        result.code,
        result.uncertain,
        journal.process_group.is_some_and(process::group_alive),
    ) {
        journal.phase = Phase::Unknown;
        journal.message = message;
        store.save(&journal)?;
        return Ok(json!(journal));
    }
    finish(&store, &mut journal)?;
    Ok(json!(journal))
}

fn observe_frozen(store: &Store, journal: &Journal) -> Result<Observation> {
    let receipts = store.receipts()?;
    let committed = receipts
        .installations
        .get(&journal.plan.package.id)
        .map(|receipt| crate::policy::npm_local::committed_install_query(journal, receipt))
        .transpose()?
        .flatten();
    let spec = journal
        .plan
        .query
        .as_ref()
        .ok_or_else(|| Error::new("verification_missing", "原操作没有冻结验证查询"))?;
    providers::observe_query(
        &journal.plan.package,
        committed.as_ref().unwrap_or(spec),
        &journal.scope,
        true,
    )
}

fn finish(store: &Store, journal: &mut Journal) -> Result<()> {
    let observed = match observe_frozen(store, journal) {
        Ok(v) => v,
        Err(e) => Observation::Unknown {
            reason: e.to_string(),
        },
    };
    if expected(journal, &observed) {
        match store.commit(journal, observed) {
            Ok(()) => {
                journal.phase = Phase::Succeeded;
                journal.message = "安装状态通过冻结契约验证".into();
            }
            Err(error) if error.code == "commit_conflict" => {
                journal.phase = Phase::Unknown;
                journal.message = error.to_string();
            }
            Err(error) => return Err(error),
        }
    } else {
        journal.phase = Phase::Unknown;
        journal.message = format!(
            "未验证预期结果：{}；保留阻断",
            serde_json::to_string(&observed)?
        );
    }
    store.save(journal)
}

fn recover_owned(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Option<Value>> {
    let result = if crate::policy::chezmoi_native::is_native(&journal.plan) {
        crate::chezmoi_native::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::native_skills::is_native(journal) {
        crate::native_skills::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::ai_tx::is_native(journal) {
        crate::ai_tx::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::native_plugin_tx::is_plugin(journal) {
        crate::native_plugin_tx::recover(ctx, store, journal, resolve_failed, yes)?
    } else if matches!(
        journal.plan.action,
        Action::Apply | Action::Restore | Action::Reconcile
    ) {
        crate::config_tx::recover_journal(store, journal, resolve_failed, yes)?
    } else if matches!(
        journal.plan.package.provider,
        Provider::Governance {
            category: crate::domain::GovernanceCategory::Source,
            ..
        }
    ) {
        crate::source_tx::recover_journal(ctx, store, journal, resolve_failed, yes)?
    } else if matches!(journal.plan.package.provider, Provider::Governance { .. }) {
        crate::capability_tx::recover_journal(ctx, store, journal, resolve_failed, yes)?
    } else {
        return Ok(None);
    };
    Ok(Some(result))
}

pub fn recover(
    ctx: &Context,
    id: &str,
    dry_run: bool,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    if !store::valid_id(id) {
        return Err(Error::new("invalid_operation_id", id));
    }
    let mut journal = requested_journal(ctx, id)?;
    store::validate_journal(&journal)?;
    crate::identity::validate(&journal.scope)?;
    if journal.plan.brew.is_some() {
        if journal.id != id {
            return Err(Error::attention(
                "operation_conflict",
                "原 Brew 操作标识不一致",
            ));
        }
        return crate::brew::recover(ctx, &journal, dry_run, resolve_failed, yes);
    }
    if matches!(
        journal.plan.package.provider,
        Provider::Uv { binding: None, .. }
    ) && !resolve_failed
    {
        return Ok(
            json!({"journal":journal,"status":"unknown","reason":"legacy_uv_query_unbound","persistentWrites":false}),
        );
    }
    if dry_run {
        return Ok(json!({"mode":"dry-run","journal":journal,"persistentWrites":false}));
    }
    let store = Store::lock(ctx)?;
    store.check_migration_pending()?;
    journal = locked_journal(&store, id)?;
    if journal.plan.npm_binding.is_some() {
        return crate::npm_adoption::recover(ctx, &store, &mut journal);
    }
    if let Some(value) = recover_owned(ctx, &store, &mut journal, resolve_failed, yes)? {
        return Ok(value);
    }
    let adopt_committed = if !resolve_failed
        && journal.phase == Phase::Prepared
        && journal.process_group.is_none()
        && journal.plan.action == Action::Adopt
    {
        crate::policy::state::adopt_was_committed(
            &journal,
            store
                .receipts()?
                .installations
                .get(&journal.plan.package.id),
        )
    } else {
        false
    };
    match crate::policy::state::recovery(
        &journal,
        &ctx.state,
        journal.process_group.is_some_and(process::group_alive),
        resolve_failed,
        adopt_committed,
    )? {
        Recovery::Terminal => return Ok(json!(journal)),
        Recovery::ResolveFailed => {
            confirm(
                "确认已人工核对原操作、外部进程和当前安装，记录失败并解除阻断；此动作不会修复、重跑或回滚软件。",
                yes,
            )?;
            journal.phase = Phase::Failed;
            journal.message = "用户确认人工核对后记录失败；没有执行安装器、回滚或成功验收".into();
            store.save(&journal)?;
            return Ok(json!(journal));
        }
        Recovery::CompleteAdopt => {
            if journal.plan.uv_owner.is_some() {
                crate::uv_ownership::check(ctx, &journal.plan, Some(&journal.id))?;
                let query = journal
                    .plan
                    .query
                    .as_ref()
                    .ok_or_else(|| Error::new("invalid_uv_plan", "缺少 uv 查询"))?;
                let observed =
                    providers::observe_query(&journal.plan.package, query, &journal.scope, false)?;
                crate::policy::state::require_fresh_plan(
                    &journal.plan,
                    &journal.plan.package,
                    &observed,
                )?;
            }
            journal.phase = Phase::Succeeded;
            journal.message = "已恢复完成的采纳记录；未执行安装器".into();
            store.save(&journal)?;
            return Ok(json!(journal));
        }
        Recovery::NoGate => {
            journal.phase = Phase::Failed;
            journal.message = "未发送执行 gate，外部动作未开始".into();
            store.save(&journal)?;
            return Ok(json!(journal));
        }
        Recovery::Verify => (),
    }
    finish(&store, &mut journal)?;
    Ok(json!(journal))
}

pub fn launch(store: &Store, journal: &mut Journal) -> Result<process::Output> {
    if crate::policy::uv_local::is_uv(&journal.plan.package.provider) {
        return Err(crate::policy::uv_local::blocked());
    }
    let mut output_file = tempfile::tempfile()?;
    let mut error_file = tempfile::tempfile()?;
    let path = store
        .path
        .join("operations")
        .join(format!("{}.json", journal.id));
    let binary = std::env::current_exe()?;
    let mut child = std::process::Command::new(binary)
        .arg("__worker")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(output_file.try_clone()?)
        .stderr(error_file.try_clone()?)
        .process_group(0)
        .spawn()?;
    journal.phase = Phase::Running;
    journal.process_group = Some(child.id());
    if let Err(e) = store.save(journal) {
        process::kill_group(child.id());
        let _ = child.wait();
        return Err(e);
    }
    let mut gate = child
        .stdin
        .take()
        .ok_or_else(|| Error::new("worker_gate_missing", "worker stdin 未建立"))?;
    if let Err(e) = gate.write_all(b"RUN\n") {
        process::kill_group(child.id());
        let _ = child.wait();
        journal.phase = Phase::Unknown;
        journal.message = e.to_string();
        store.save(journal)?;
        return Err(e.into());
    }
    drop(gate);
    let start = Instant::now();
    let mut uncertain = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > Duration::from_secs(journal.plan.timeout_secs)
            || output_file.metadata()?.len() + error_file.metadata()?.len() > 4 * 1024 * 1024
        {
            uncertain = true;
            process::kill_group(child.id());
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    uncertain |= output_file.metadata()?.len() > 2 * 1024 * 1024
        || error_file.metadata()?.len() > 2 * 1024 * 1024;
    output_file.seek(SeekFrom::Start(0))?;
    error_file.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    let mut diagnostic = Vec::new();
    output_file.take(2 * 1024 * 1024).read_to_end(&mut output)?;
    error_file
        .take(2 * 1024 * 1024)
        .read_to_end(&mut diagnostic)?;
    Ok(process::Output {
        code: status.code(),
        stdout: String::from_utf8_lossy(&output).into_owned(),
        stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
        uncertain,
    })
}

pub fn worker(path: &Path) -> Result<()> {
    let mut gate = [0_u8; 4];
    std::io::stdin().read_exact(&mut gate)?;
    crate::policy::state::worker_gate(gate)?;
    let journal: Journal = paths::regular_json(path)?;
    let error = prepare_worker(
        &journal,
        path,
        std::process::id(),
        nix::unistd::getpgrp().as_raw(),
    )?
    .exec();
    Err(error.into())
}

fn prepare_worker(
    journal: &Journal,
    path: &Path,
    pid: u32,
    group: i32,
) -> Result<std::process::Command> {
    store::validate_journal(journal)?;
    if crate::native_skills::is_native(journal) {
        return Err(Error::attention(
            "native_worker_required",
            "Native Skills 只能使用冻结的原生 supervisor 协议",
        ));
    }
    if crate::policy::uv_local::is_uv(&journal.plan.package.provider) {
        return Err(crate::policy::uv_local::blocked());
    }
    if crate::native_plugin_tx::is_plugin(journal) {
        return Err(crate::native_plugins::refusal(
            "native_plugin_worker_required",
        ));
    }
    if crate::policy::chezmoi_native::is_native(&journal.plan) || journal.plan.brew.is_some() {
        return Err(Error::attention(
            "native_worker_required",
            "此计划必须使用已绑定原生执行器",
        ));
    }
    crate::policy::state::worker_identity(journal, path, pid, group)?;
    if let Some(
        query @ (QuerySpec::LocalUvTool(_)
        | QuerySpec::LocalNpm(_)
        | QuerySpec::LocalConfig(_)
        | QuerySpec::LocalCapability(_)),
    ) = &journal.plan.query
    {
        let current =
            providers::observe_query(&journal.plan.package, query, &journal.scope, false)?;
        crate::policy::state::require_fresh_plan(&journal.plan, &journal.plan.package, &current)?;
        if let QuerySpec::LocalNpm(npm_query) = query {
            crate::npm_local::verify_plain_namespace_for_write(npm_query, journal.plan.action)?;
        }
    }
    let spec = journal
        .plan
        .command
        .as_ref()
        .ok_or_else(|| Error::new("worker_command_missing", "没有写命令"))?;
    let native = crate::ai_tx::worker_command(journal)?;
    let spec = native.as_ref().unwrap_or(spec);
    process::command(spec, &journal.scope)
}

#[cfg(test)]
#[path = "../tests/unit/operation_worker.rs"]
mod worker_tests;

fn requested_journal(ctx: &Context, id: &str) -> Result<Journal> {
    let active = ctx.state.join("active.json");
    if paths::read_path(&active)?.exists() {
        let journal: Journal = paths::regular_json(&active)?;
        store::validate_journal(&journal)?;
        if journal.id == id {
            return Ok(journal);
        }
    }
    paths::regular_json(&ctx.state.join("operations").join(format!("{id}.json")))
}

pub(crate) fn locked_journal(store: &Store, id: &str) -> Result<Journal> {
    let history = Store::read::<Journal>(&store.operations, &format!("{id}.json"))?;
    let active = Store::read::<Journal>(&store.root, "active.json")?;
    crate::policy::state::recovery_journal(id, active, history)
}

fn check_before(
    ctx: &Context,
    package: &Package,
    action: Action,
    before: &Observation,
    frozen: Option<&QuerySpec>,
) -> Result<()> {
    check_existing_owner(ctx, package)?;
    crate::policy::state::require_install_slot(action, before)?;
    if matches!(action, Action::Upgrade | Action::Uninstall) {
        let receipts_file = ctx.state.join("receipts.json");
        let receipts: Receipts = if paths::read_path(&receipts_file)?.exists() {
            paths::regular_json(&receipts_file)?
        } else {
            Receipts {
                schema_version: 1,
                installations: std::collections::BTreeMap::default(),
            }
        };
        let receipt = receipts.installations.get(&package.id).ok_or_else(|| {
            Error::attention("adoption_required", "先 adopt 采纳已安装基线，再升级或卸载")
        })?;
        let query = match frozen {
            Some(query) => query.clone(),
            None => providers::query_spec(ctx, package)?,
        };
        crate::policy::state::receipt_matches(&ctx.scope, package, before, &query, receipt)?;
    }
    Ok(())
}

fn check_existing_owner(ctx: &Context, package: &Package) -> Result<()> {
    if crate::policy::uv_local::is_uv(&package.provider) {
        return crate::uv_ownership::snapshot(ctx, package).map(|_| ());
    }
    crate::target::check_owner(ctx, package)?;
    let path = ctx.state.join("receipts.json");
    if !paths::read_path(&path)?.exists() {
        return Ok(());
    }
    let receipts: Receipts = paths::regular_json(&path)?;
    crate::policy::state::existing_owner(
        &ctx.scope,
        package,
        receipts.installations.get(&package.id),
    )
}

fn adoption_plan(
    ctx: &Context,
    package: &Package,
    before: Observation,
    frozen: Option<QuerySpec>,
) -> Result<Plan> {
    if matches!(before, Observation::Missing { .. }) {
        return Err(Error::attention("not_installed", &package.id));
    }
    check_existing_owner(ctx, package)?;
    Ok(Plan {
        brew: None,
        npm_binding: None,
        uv_owner: if crate::policy::uv_local::is_uv(&package.provider) {
            Some(crate::uv_ownership::snapshot(ctx, package)?)
        } else {
            None
        },
        timeout_secs: ctx.timeout_secs,
        package: package.clone(),
        action: Action::Adopt,
        target: if crate::policy::uv_local::is_uv(&package.provider) {
            None
        } else {
            before.version().map(str::to_owned)
        },
        before,
        command: None,
        query: frozen.or_else(|| providers::query_spec(ctx, package).ok()),
        recipe_sha256: fingerprint(package)?,
        warnings: vec![],
        synchronous: true,
        delegated: false,
    })
}

use crate::policy::state::expected;

#[cfg(test)]
#[path = "../tests/unit/npm_operations.rs"]
mod npm_tests;

#[cfg(test)]
#[path = "../tests/unit/uv_operations.rs"]
mod uv_tests;

fn local_query(ctx: &Context, package: &Package, action: Action) -> Result<Option<QuerySpec>> {
    if matches!(
        package.provider,
        Provider::ConfigObserved {
            authority: crate::domain::ConfigAuthority::ChezmoiNative,
            ..
        }
    ) {
        return Err(Error::attention(
            "config_native_explicit_entry_required",
            "原生配置仅通过明确 config 命令执行",
        ));
    }
    Ok(
        if matches!(
            package.provider,
            Provider::Uv {
                binding: Some(_),
                ..
            } | Provider::UvObserved { .. }
                | Provider::Npm { .. }
                | Provider::NpmObserved { .. }
                | Provider::ConfigObserved { .. }
                | Provider::CapabilityObserved { .. }
        ) {
            Some(providers::capture_query(ctx, package, Some(action))?)
        } else {
            None
        },
    )
}
