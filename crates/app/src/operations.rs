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
            "这里不能输入确认。请在终端里重新运行这条命令，先看清它会改什么。",
        ));
    }
    let mut error = std::io::stderr().lock();
    writeln!(
        error,
        "{message}\n同意就输入 yes，再按 Enter。直接按 Enter 可以取消。"
    )?;
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
        if let Some(previous) = recover_before_write(ctx, None)?
            && previous.original.plan.recipe_sha256 == plan.recipe_sha256
            && previous.matches(plan.action, &plan.package.id, plan.target.as_deref())
        {
            return Ok(previous.result);
        }
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
            "纳入管理 {}\n{}",
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
    let result = if crate::plugin_target::is_publication(journal) {
        crate::plugin_target::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::policy::chezmoi_native::is_native(&journal.plan) {
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

pub(crate) struct Recovered {
    pub original: Journal,
    pub result: Value,
    pub succeeded: bool,
    pub local_ai: Option<bool>,
}

impl Recovered {
    pub(crate) fn matches(&self, action: Action, target: &str, version: Option<&str>) -> bool {
        self.succeeded && matches_intent(&self.original, action, target, version)
    }
}

pub(crate) fn matches_intent(
    journal: &Journal,
    action: Action,
    target: &str,
    version: Option<&str>,
) -> bool {
    let plan = &journal.plan;
    plan.action == action
        && plan.package.id == target
        && version.is_none_or(|version| plan.target.as_deref() == Some(version))
}

fn same_recovery_roots(ctx: &Context, journal: &Journal) -> bool {
    journal.home == ctx.home
        && journal.state == ctx.state
        && journal.cache == ctx.cache
        && journal.settings == ctx.settings
}

pub(crate) fn require_recovery_roots(ctx: &Context, journal: &Journal) -> Result<()> {
    if same_recovery_roots(ctx, journal) {
        return Ok(());
    }
    Err(Error::attention(
        "recovery_scope_changed",
        "请用上一次的目录选项核对原操作。",
    ))
}

pub(crate) fn lock_after_recovery(ctx: &Context) -> Result<Store> {
    recover_before_write(ctx, None)?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    Ok(store)
}

pub(crate) fn recover_before_write(
    ctx: &Context,
    local_tool: Option<&str>,
) -> Result<Option<Recovered>> {
    recover_before_write_checked(ctx, local_tool, |_| Ok(true))
}

pub(crate) fn recover_before_write_checked(
    ctx: &Context,
    local_tool: Option<&str>,
    accepts: impl FnOnce(&Journal) -> Result<bool>,
) -> Result<Option<Recovered>> {
    recover_before_write_with(ctx, local_tool, |_| Ok(()), accepts)
}

pub(crate) fn recover_before_new_plan(
    ctx: &Context,
    yes: bool,
    related: impl FnOnce(&Journal) -> Result<bool>,
) -> Result<Option<Recovered>> {
    recover_before_write_with(
        ctx,
        None,
        |journal| {
            if yes || related(journal)? {
                return Ok(());
            }
            confirm(
                &format!(
                    "上一次操作「{}」还待核对。确认后只核对那次操作，再准备本次计划。",
                    crate::output::sanitize(&journal.plan.package.name, false)
                ),
                false,
            )
        },
        |_| Ok(true),
    )
}

fn recover_before_write_with(
    ctx: &Context,
    local_tool: Option<&str>,
    approve: impl FnOnce(&Journal) -> Result<()>,
    accepts: impl FnOnce(&Journal) -> Result<bool>,
) -> Result<Option<Recovered>> {
    let Some(original) =
        store::active(ctx)?.filter(|journal| crate::policy::state::pending(journal.phase))
    else {
        return Ok(None);
    };
    let id = &original.id;
    let blocked =
        |reason: &str| Error::attention("recovery_required", format!("还没开始新操作。\n{reason}"));
    let scoped;
    let ctx = if crate::policy::service::is_control(original.plan.action) {
        scoped = crate::identity::service_context(ctx)?;
        &scoped
    } else {
        ctx
    };
    if !same_recovery_roots(ctx, &original) || original.scope != ctx.scope {
        return Err(blocked("请用上一次的目录选项运行原命令。"));
    }
    if original.process_group.is_some_and(process::group_alive) {
        return Err(blocked(
            "上一次操作还在运行。等它结束后，再运行同一条命令。",
        ));
    }
    crate::identity::validate(&original.scope)?;
    approve(&original)?;
    let store = Store::lock(ctx)?;
    store.check_migration_pending()?;
    store.verify_namespace()?;
    let expected = fingerprint(&original)?;
    let current = Store::read::<Journal>(&store.root, "active.json")?
        .ok_or_else(|| blocked("操作记录刚刚变了。请再运行一次命令。"))?;
    verify_recovery_preview(Some(&expected), &current)?;
    store::validate_journal(&current)?;
    if !accepts(&current)? {
        return Err(blocked(
            "上一次操作不属于这次选择。请运行 mo recover 查看记录。",
        ));
    }
    if current.process_group.is_some_and(process::group_alive) {
        return Err(blocked(
            "上一次操作还在运行。等它结束后，再运行同一条命令。",
        ));
    }
    let local_ai = if crate::ai_tx::is_native(&current) {
        let saved = crate::ai_state::read(&store, &current)?;
        if let Some(mise) = &saved.mise {
            if mise.roots != ctx.chezmoi {
                return Err(blocked("请用上一次的配置文件和目录选项运行原命令。"));
            }
            if local_tool == Some(saved.tool.as_str()) && !saved.force_live {
                return Err(blocked(
                    "上一次需要保存设置来源。这次只改本机。请用原来的升级选项检查上一次。",
                ));
            }
        }
        Some(saved.force_live)
    } else {
        None
    };
    let result = if current.plan.brew.is_some() {
        crate::brew::recover_locked(ctx, &store, &current, false, false, Some(&expected))
    } else if matches!(
        current.plan.package.provider,
        Provider::Uv { binding: None, .. }
    ) {
        return Err(blocked(
            "缺少原来安装位置的信息。请用 mo history 查看记录。",
        ));
    } else {
        recover_locked(ctx, &store, current, false, false)
    }
    .map_err(|error| blocked(&crate::output::sanitize(&error.message, true)))?;
    store.verify_namespace()?;
    let active = Store::read::<Journal>(&store.root, "active.json")?
        .ok_or_else(|| blocked("没能确认上一次操作是否结束。请用 mo history 查看记录。"))?;
    store::validate_journal(&active)?;
    if crate::policy::state::pending(active.phase) {
        let reason = if result["recoveryBlocker"] == "claude_profile_changed" {
            "Claude 设置后来有改动。请先确认要保留哪份设置。"
        } else {
            "现在还不能确认上一次操作是否完成。"
        };
        return Err(blocked(&format!(
            "{reason}运行 mo recover 查看待检查的操作。"
        )));
    }
    let settled = locked_journal(&store, id)?;
    Ok(Some(Recovered {
        original,
        result,
        succeeded: settled.phase == Phase::Succeeded,
        local_ai,
    }))
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
        require_recovery_roots(ctx, &journal)?;
        if journal.scope != ctx.scope {
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
struct ClientFollowups {
    candidates: BTreeSet<String>,
    incomplete: bool,
}

impl From<Result<(BTreeSet<String>, bool)>> for ClientFollowups {
    fn from(report: Result<(BTreeSet<String>, bool)>) -> Self {
        let (candidates, incomplete) = report.unwrap_or_else(|_| (BTreeSet::new(), true));
        Self {
            candidates,
            incomplete,
        }
    }
}

#[derive(Default)]
struct Followups {
    codex: ClientFollowups,
    claude: ClientFollowups,
}

impl Followups {
    fn read(ctx: &Context, records: &[Journal]) -> Self {
        Self {
            codex: crate::codex_install::awaiting_followup(ctx, records).into(),
            claude: crate::claude_install::awaiting_followup(ctx, records).into(),
        }
    }

    fn state(&self, journal: &Journal) -> RecoveryState {
        if crate::policy::state::pending(journal.phase) {
            return RecoveryState::Pending;
        }
        if journal.phase != Phase::Succeeded {
            return RecoveryState::Settled;
        }
        let (client, selected) = if crate::claude_install::is_install(journal) {
            (&self.claude, RecoveryState::ClaudeFollowup)
        } else if crate::codex_install::is_install(journal) {
            (&self.codex, RecoveryState::CodexPublication)
        } else {
            return RecoveryState::Settled;
        };
        if client.incomplete {
            RecoveryState::Unverified
        } else if client.candidates.contains(&journal.id) {
            selected
        } else {
            RecoveryState::Settled
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

pub(crate) fn recovery_history(ctx: &Context) -> Result<(Vec<(Journal, RecoveryState)>, bool)> {
    let mut records = store::history(ctx)?;
    let active = store::active(ctx);
    let active_unreadable = active.is_err();
    merge_active(&mut records, active.unwrap_or_default());
    let followups = Followups::read(ctx, &records);
    let rows = records
        .into_iter()
        .map(|journal| {
            let state = followups.state(&journal);
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
    let followups = Followups::read(ctx, &records);
    let mut incomplete = followups.claude.incomplete || followups.codex.incomplete;
    records.retain(|journal| {
        if followups.claude.candidates.contains(&journal.id)
            || followups.codex.candidates.contains(&journal.id)
        {
            return true;
        }
        let state = followups.state(journal);
        incomplete |= state == RecoveryState::Unverified;
        state.can_confirm()
    });
    Ok((records, incomplete))
}

fn pending_list_text(pending: &[Value], incomplete: bool) -> String {
    use std::fmt::Write as _;
    if pending.is_empty() && !incomplete {
        return "目前没有要检查的操作。\n".into();
    }
    let mut text = String::from("还有这些操作要检查\n");
    for journal in pending {
        let id = journal["id"].as_str().unwrap_or_default();
        let name = journal["plan"]["package"]["name"]
            .as_str()
            .unwrap_or_default();
        let status = match journal["phase"].as_str() {
            Some("prepared") => "已准备",
            Some("running") => "最后记录为运行中",
            Some("succeeded") => "原阶段已完成，待核对后续步骤",
            _ => "结果还没确认",
        };
        let _ = writeln!(
            text,
            "  {id} · {} · {status}",
            crate::browser::sanitize(name, false)
        );
    }
    if incomplete {
        text.push_str("有些记录没能读到，清单可能少了几项。\n");
    }
    if let [journal] = pending
        && let Some(id) = journal["id"].as_str().filter(|id| store::valid_id(id))
    {
        let _ = writeln!(
            text,
            "运行 mo recover {id} 检查那次操作的结果。\n只想看计划时，运行 mo recover {id} --dry-run。"
        );
    } else if !pending.is_empty() {
        text.push_str("挑一个操作号，放在 mo recover 后面。加 --dry-run 只看计划。\n");
    }
    if !pending.is_empty() {
        text.push_str("请带上原来的目录选项。\n");
    }
    text
}

pub(crate) fn recovery_text(data: &Value, dry_run: bool) -> String {
    use crate::policy::cli::{Continuation, continuation};
    use std::fmt::Write as _;
    let safe = |value: &Value| crate::browser::sanitize(value.as_str().unwrap_or_default(), false);

    if let Some(pending) = data["pending"].as_array() {
        return pending_list_text(pending, data["incomplete"] == true);
    }
    if dry_run {
        let mut text = crate::output::human(data);
        if let Some(journal) = data.get("journal") {
            let plan = &journal["plan"];
            let package = &plan["package"];
            let _ = writeln!(
                text,
                "核对预演（只读）\n原目标 {} · {}\n原 HOME {}\n原状态目录 {}",
                safe(&package["name"]),
                safe(&package["id"]),
                safe(&journal["home"]),
                safe(&journal["state"]),
            );
            if let Some(version) = plan["target"].as_str() {
                let _ = writeln!(
                    text,
                    "计划版本 {}",
                    crate::browser::sanitize(version, false)
                );
            }
        }
        return text;
    }
    match continuation(data) {
        Some(Continuation::Ready) => {
            return data["continuationCommand"].as_str().map_or_else(
                || "原阶段已核对。运行 mo recover 查看下一步。\n".into(),
                |command| {
                    format!(
                        "原阶段已核对。沿用原来的目录选项，运行 {} 继续。\n",
                        crate::browser::sanitize(command, false)
                    )
                },
            );
        }
        Some(Continuation::Unverified) => {
            return "这条记录只证明原阶段已完成。运行 mo agent 查看当前安装，或运行 mo recover 查看是否仍有待核对步骤。\n".into();
        }
        None => {}
    }
    let mut text = crate::output::human(data);
    if data["reason"] == "operation_busy"
        && let Some(id) = crate::output::original_operation(data)
    {
        let _ = writeln!(
            text,
            "另一个操作正在使用这份记录。等它结束，再运行 mo recover {id}。"
        );
    } else if let Some(code) = data["recoveryBlocker"].as_str()
        && let Some(id) = crate::output::original_operation(data)
    {
        let next = match code {
            "claude_profile_changed" => "先确认是否要保留上面列出的档案改动。",
            "ai_live_changed"
            | "ai_source_changed"
            | "ai_roots_changed"
            | "ai_gate_inputs_changed"
            | "ai_rollback_cas_failed" => {
                "沿用原作用域参数，运行 mo config reconcile 查看本机和管理源的差异。"
            }
            _ => "运行 mo agent tools 查看当前版本，并核对上方提示的证据。",
        };
        let _ = writeln!(
            text,
            "{next}\n如果确认要保留现在的安装和设置，并结束这次操作，请运行 mo recover {id} --mark-failed。它会再问一次，只把这次操作记为失败。",
        );
    } else if let Some(id) = crate::output::original_operation(data) {
        let _ = writeln!(
            text,
            "现在还不能确认结果。先用 mo history {id} 查看记录，再检查当前安装和设置。"
        );
    }
    text
}

fn require_recoverable_from_history(ctx: &Context, journal: &Journal) -> Result<()> {
    let followups = if journal.phase == Phase::Succeeded
        && (crate::claude_install::is_install(journal) || crate::codex_install::is_install(journal))
    {
        Followups::read(ctx, &recovery_records(ctx)?)
    } else {
        Followups::default()
    };
    match followups.state(journal) {
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

pub(crate) fn validated_recovery_journal(ctx: &Context, id: &str) -> Result<Journal> {
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
    require_recovery_roots(ctx, &journal)?;
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
    require_recovery_roots(ctx, &journal)?;
    crate::identity::validate(&journal.scope)?;
    verify_recovery_preview(expected, &journal)?;
    if expected.is_some() {
        require_recoverable_from_history(ctx, &journal)?;
    }
    recover_locked(ctx, &store, journal, resolve_failed, yes)
}

fn recover_locked(
    ctx: &Context,
    store: &Store,
    mut journal: Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    if journal.plan.npm_binding.is_some() {
        return crate::npm_adoption::recover(ctx, store, &mut journal, resolve_failed);
    }
    if let Some(value) = recover_owned(ctx, store, &mut journal, resolve_failed, yes)? {
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
            journal.message = "已记为失败。这条记录不再挡住新操作。安装和设置照旧。".into();
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
    finish(store, &mut journal)?;
    Ok(json!(journal))
}

pub fn launch(store: &Store, journal: &mut Journal) -> Result<process::Output> {
    if crate::policy::uv_local::is_uv(&journal.plan.package.provider) {
        return Err(crate::policy::uv_local::blocked());
    }
    let mut output_file = tempfile::tempfile()?;
    let mut error_file = tempfile::tempfile()?;
    let mut progress = if journal.plan.package.kind == crate::domain::Kind::Config {
        None
    } else {
        process::LiveProgress::open()
    };
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
        if let Some(progress) = &mut progress {
            progress.relay([&output_file, &error_file], false);
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    if let Some(progress) = &mut progress {
        progress.relay([&output_file, &error_file], true);
    }
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
