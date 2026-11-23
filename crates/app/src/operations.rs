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
    collections::BTreeSet,
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
                "目标尚未解析；预演不联网。先运行 mo app refresh 或提供 --version，才能显示完整执行命令".into(),
            ],
            synchronous: providers::synchronous(package),
            delegated: false,
        });
    }
    let retirement = frozen
        .as_ref()
        .and_then(crate::mise_owner::retirement::query_witness);
    let target = match resolution {
        crate::policy::state::Resolution::Explicit(v)
        | crate::policy::state::Resolution::Cached(v) => Some(v),
        crate::policy::state::Resolution::Latest => Some(crate::updates::latest(ctx, package)?),
        crate::policy::state::Resolution::Absent | crate::policy::state::Resolution::Unresolved => {
            retirement.map(|release| release.version.clone())
        }
    };
    let command = native_write_spec(ctx, package, action, target.as_deref(), frozen.as_ref())?;
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

fn native_write_spec(
    ctx: &Context,
    package: &Package,
    action: Action,
    target: Option<&str>,
    frozen: Option<&QuerySpec>,
) -> Result<crate::domain::CommandSpec> {
    let native_version = if matches!(package.provider, Provider::Brew { .. }) {
        None
    } else {
        target
    };
    let mut command =
        if let Some(release) = frozen.and_then(crate::mise_owner::retirement::query_witness) {
            crate::mise_owner::retirement::write_spec(package, action, native_version, release)?
        } else {
            providers::mutation_spec(ctx, package, action, native_version)?
        };
    if let Some(QuerySpec::LocalNpm(query)) = frozen {
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
    Ok(command)
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
        confirm(&confirmation(ctx, &plan), yes)?;
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    store.receipts()?;
    let frozen = plan.query.as_ref().filter(|query| checked_query(query));
    let observed = match frozen {
        Some(query) => providers::observe_query(&plan.package, query, &ctx.scope, false)?,
        None => inventory::observe(ctx, &plan.package),
    };
    let current = crate::target::refresh(ctx, &plan.package)?;
    crate::policy::state::require_fresh_plan(&plan, &current, &observed)?;
    check_before(ctx, &plan.package, plan.action, &observed, frozen)?;
    if let Some(QuerySpec::LocalNpm(query)) = &plan.query {
        crate::npm_local::verify_plain_namespace_for_write(query, plan.action)?;
    }
    if plan.uv_owner.is_some() {
        crate::uv_ownership::check(ctx, &plan, None)?;
    }
    check()?;
    if crate::mise_owner::applies(&plan)
        && let Some(spec) = &plan.command
    {
        let mut command = process::command(spec, &ctx.scope)?;
        mise_admission(ctx, &plan, &mut command)?;
    }
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
    if let Err(error) = crate::mise_owner::record_completion(&store, &journal) {
        journal.phase = Phase::Unknown;
        journal.message = error.to_string();
        store.save(&journal)?;
        return Ok(json!(journal));
    }
    finish(&store, &mut journal)?;
    Ok(json!(journal))
}

fn confirmation(ctx: &Context, plan: &Plan) -> String {
    if crate::mise_owner::retirement::bound_query(plan).is_some() {
        format!(
            "卸载 {}\n{}",
            crate::browser::sanitize(&plan.package.name, false),
            crate::recommend::plan_scope(ctx, plan)
        )
    } else if plan.action == Action::Adopt {
        format!(
            "纳管 {}\n{}",
            crate::browser::sanitize(&plan.package.name, false),
            crate::recommend::plan_scope(ctx, plan)
        )
    } else {
        format!("{:?} {}", plan.action, plan.package.id)
    }
}

fn mise_admission(ctx: &Context, plan: &Plan, command: &mut std::process::Command) -> Result<()> {
    crate::mise_owner::bind_mutation(&ctx.scope, plan, command)?;
    if plan.action == Action::Uninstall {
        crate::mise_owner::retirement::check_receipt(ctx, plan)
    } else {
        crate::mise_owner::check_command(ctx, command)
    }
}

fn observe_frozen(
    store: &Store,
    journal: &Journal,
) -> Result<(Observation, Option<crate::domain::MiseQuery>)> {
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
    if let QuerySpec::Mise(query) = spec {
        let final_query = crate::mise_owner::completion_query(store, journal, query)?;
        let finalized = QuerySpec::Mise(final_query.clone());
        if receipts
            .installations
            .get(&journal.plan.package.id)
            .is_some_and(|known| {
                known.operation_id == journal.id && known.query.as_ref() != Some(&finalized)
            })
        {
            return Err(Error::attention(
                "commit_conflict",
                "已登记的 mise 安装目录身份发生变化",
            ));
        }
        let observed =
            providers::observe_query(&journal.plan.package, &finalized, &journal.scope, true)?;
        return Ok((observed, Some(final_query)));
    }
    let observed = providers::observe_query(
        &journal.plan.package,
        committed.as_ref().unwrap_or(spec),
        &journal.scope,
        true,
    )?;
    Ok((observed, None))
}

fn finish(store: &Store, journal: &mut Journal) -> Result<()> {
    let (observed, mise) = observe_frozen(store, journal).unwrap_or_else(|error| {
        (
            Observation::Unknown {
                reason: error.to_string(),
            },
            None,
        )
    });
    if expected(journal, &observed) {
        let committed = if let Some(query) = mise {
            store.commit_mise(journal, observed, query)
        } else {
            store.commit(journal, observed)
        };
        match committed {
            Ok(()) => {
                journal.phase = Phase::Succeeded;
                journal.message = "安装状态通过冻结契约验证".into();
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "commit_conflict" | "mise_scope_changed" | "mise_release_changed"
                ) =>
            {
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
    } else if crate::codex_install::is_install(journal) {
        crate::codex_install::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::codex_install::is_publication(journal) {
        crate::codex_install::recover_publication(ctx, store, journal, resolve_failed)?
    } else if crate::claude_install::is_install(journal) {
        crate::claude_install::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::claude_install::is_publication(journal) {
        crate::claude_install::recover_publication(ctx, store, journal, resolve_failed)?
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
    recover_requested(ctx, id, dry_run, resolve_failed, yes, None)
}

pub(crate) struct RecoveryPreview {
    pub journal: Journal,
    sha256: String,
}

impl RecoveryPreview {
    pub(crate) fn read(ctx: &Context, id: &str) -> Result<Self> {
        let journal = validated_recovery_journal(ctx, id)?;
        if journal.id != id {
            return Err(Error::attention("operation_conflict", "原操作标识不一致"));
        }
        require_recoverable_from_history(ctx, &journal)?;
        if journal.home != ctx.home
            || journal.state != ctx.state
            || journal.cache != ctx.cache
            || journal.settings != ctx.settings
            || journal.scope != ctx.scope
        {
            return Err(Error::attention(
                "recovery_scope_changed",
                "请用原环境及作用域核对",
            ));
        }
        let sha256 = fingerprint(&journal)?;
        Ok(Self { journal, sha256 })
    }

    pub(crate) fn confirm(self, ctx: &Context) -> Result<Value> {
        recover_requested(
            ctx,
            &self.journal.id,
            false,
            false,
            true,
            Some(&self.sha256),
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryState {
    Pending,
    CodexPublication,
    ClaudeFollowup,
    Settled,
    Unverified,
}

impl RecoveryState {
    pub(crate) fn can_confirm(self) -> bool {
        matches!(
            self,
            Self::Pending | Self::CodexPublication | Self::ClaudeFollowup
        )
    }
}

#[derive(Default)]
struct ClaudeFollowups {
    candidates: BTreeSet<String>,
    incomplete: bool,
}

impl ClaudeFollowups {
    fn read(ctx: &Context, records: &[Journal]) -> Self {
        let (candidates, incomplete) = crate::claude_install::awaiting_followup(ctx, records)
            .unwrap_or_else(|_| (BTreeSet::new(), true));
        Self {
            candidates,
            incomplete,
        }
    }
}

fn recovery_records(ctx: &Context) -> Result<Vec<Journal>> {
    let mut records = store::history(ctx)?;
    merge_active(&mut records, store::active(ctx)?);
    Ok(records)
}

fn merge_active(records: &mut Vec<Journal>, active: Option<Journal>) {
    if let Some(active) = active {
        records.retain(|record| record.id != active.id);
        records.push(active);
        records.sort_by(|left, right| left.id.cmp(&right.id));
    }
}

fn succeeded_claude_stage(journal: &Journal) -> bool {
    journal.phase == Phase::Succeeded && crate::claude_install::is_install(journal)
}

fn recovery_state(ctx: &Context, journal: &Journal, claude: &ClaudeFollowups) -> RecoveryState {
    if crate::policy::state::pending(journal.phase) {
        return RecoveryState::Pending;
    }
    if succeeded_claude_stage(journal) {
        return if claude.incomplete {
            RecoveryState::Unverified
        } else if claude.candidates.contains(&journal.id) {
            RecoveryState::ClaudeFollowup
        } else {
            RecoveryState::Settled
        };
    }
    match crate::codex_install::awaiting_publication(ctx, journal) {
        Ok(true) => RecoveryState::CodexPublication,
        Ok(false) => RecoveryState::Settled,
        Err(_) => RecoveryState::Unverified,
    }
}

pub(crate) fn recovery_history(ctx: &Context) -> Result<(Vec<(Journal, RecoveryState)>, bool)> {
    let mut records = store::history(ctx)?;
    let active = store::active(ctx);
    let active_unreadable = active.is_err();
    merge_active(&mut records, active.unwrap_or_default());
    let claude = ClaudeFollowups::read(ctx, &records);
    let rows = records
        .into_iter()
        .map(|journal| {
            let state = recovery_state(ctx, &journal, &claude);
            let state = if active_unreadable && state.can_confirm() {
                RecoveryState::Unverified
            } else {
                state
            };
            (journal, state)
        })
        .collect();
    Ok((rows, active_unreadable))
}

pub(crate) fn pending_recovery(ctx: &Context) -> Result<(Vec<Journal>, bool)> {
    let mut records = recovery_records(ctx)?;
    let claude = ClaudeFollowups::read(ctx, &records);
    let mut incomplete = claude.incomplete;
    records.retain(|journal| {
        if claude.candidates.contains(&journal.id) {
            return true;
        }
        let state = recovery_state(ctx, journal, &claude);
        incomplete |= state == RecoveryState::Unverified;
        state.can_confirm()
    });
    Ok((records, incomplete))
}

fn require_recoverable_from_history(ctx: &Context, journal: &Journal) -> Result<()> {
    let claude = if succeeded_claude_stage(journal) {
        ClaudeFollowups::read(ctx, &recovery_records(ctx)?)
    } else {
        ClaudeFollowups::default()
    };
    match recovery_state(ctx, journal, &claude) {
        RecoveryState::Unverified => Err(Error::attention(
            "recovery_evidence_incomplete",
            "缺少原操作或后续阶段的证据，请刷新清单并核对记录",
        )),
        RecoveryState::Settled => Err(Error::attention(
            "operation_finished",
            "此操作已结束，请刷新清单",
        )),
        RecoveryState::Pending
        | RecoveryState::CodexPublication
        | RecoveryState::ClaudeFollowup => Ok(()),
    }
}

pub(crate) fn verify_recovery_preview(expected: Option<&str>, current: &Journal) -> Result<()> {
    if let Some(expected) = expected
        && fingerprint(current)? != expected
    {
        return Err(Error::attention(
            "plan_stale",
            "原操作发生变化，请刷新后重新核对",
        ));
    }
    Ok(())
}

fn validated_recovery_journal(ctx: &Context, id: &str) -> Result<Journal> {
    if !store::valid_id(id) {
        return Err(Error::new("invalid_operation_id", id));
    }
    let journal = requested_journal(ctx, id)?;
    store::validate_journal(&journal)?;
    crate::identity::validate(&journal.scope)?;
    Ok(journal)
}

fn recover_requested(
    ctx: &Context,
    id: &str,
    dry_run: bool,
    resolve_failed: bool,
    yes: bool,
    expected: Option<&str>,
) -> Result<Value> {
    let mut journal = validated_recovery_journal(ctx, id)?;
    verify_recovery_preview(expected, &journal)?;
    if journal.plan.brew.is_some() {
        if journal.id != id {
            return Err(Error::attention(
                "operation_conflict",
                "原 Brew 操作标识不一致",
            ));
        }
        return crate::brew::recover(ctx, &journal, dry_run, resolve_failed, yes, expected);
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
    verify_recovery_preview(expected, &journal)?;
    if expected.is_some() {
        require_recoverable_from_history(ctx, &journal)?;
    }
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
    if crate::native_plugin_tx::is_plugin(journal)
        || crate::codex_install::is_install(journal)
        || crate::claude_install::is_install(journal)
    {
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
    if let Some(query) = journal
        .plan
        .query
        .as_ref()
        .filter(|query| checked_query(query))
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
    let mut command = process::command(spec, &journal.scope)?;
    if crate::mise_owner::applies(&journal.plan) {
        let ctx = crate::mise_owner::worker_context(journal)?;
        mise_admission(&ctx, &journal.plan, &mut command)?;
    }
    Ok(command)
}

fn checked_query(query: &QuerySpec) -> bool {
    matches!(
        query,
        QuerySpec::Mise(_)
            | QuerySpec::LocalUvTool(_)
            | QuerySpec::LocalNpm(_)
            | QuerySpec::LocalConfig(_)
            | QuerySpec::LocalCapability(_)
    )
}

#[cfg(test)]
#[path = "../tests/unit/operation_worker.rs"]
mod worker_tests;

fn requested_journal(ctx: &Context, id: &str) -> Result<Journal> {
    if let Some(journal) = store::active(ctx)?
        && journal.id == id
    {
        return Ok(journal);
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
        let mut query = match frozen {
            Some(query) => query.clone(),
            None => providers::query_spec(ctx, package)?,
        };
        if let QuerySpec::Mise(query) = &mut query {
            query.retirement = None;
        }
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
    crate::codex_install::require_ordinary_adoption(package)?;
    crate::claude_install::require_ordinary_adoption(package)?;
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
            Provider::Mise { .. }
                | Provider::Uv {
                    binding: Some(_),
                    ..
                }
                | Provider::UvObserved { .. }
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
