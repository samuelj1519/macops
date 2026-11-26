use super::{
    confirm, finish, locked_journal, providers,
    store::{self, Journal, Phase, Store},
};
use crate::{
    context::Context,
    domain::{Action, Provider, fingerprint},
    error::{Error, Result},
    platform::{fs as paths, process},
    policy::operation::state::Recovery,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn recover_owned(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Option<Value>> {
    let result = if crate::agent::plugin_target::is_publication(journal) {
        crate::agent::plugin_target::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::config::rules::chezmoi_native::is_native(&journal.plan) {
        crate::config::chezmoi_native::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::skills::native_skills::is_native(journal) {
        crate::skills::native_skills::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::agent::ai_tx::is_native(journal) {
        crate::agent::ai_tx::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::agent::codex_install::is_install(journal) {
        crate::agent::codex_install::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::agent::codex_install::is_publication(journal) {
        crate::agent::codex_install::recover_publication(ctx, store, journal, resolve_failed)?
    } else if crate::agent::claude_install::is_install(journal) {
        crate::agent::claude_install::recover(ctx, store, journal, resolve_failed, yes)?
    } else if crate::agent::claude_install::is_publication(journal) {
        crate::agent::claude_install::recover_publication(ctx, store, journal, resolve_failed)?
    } else if crate::agent::native_plugin_tx::is_plugin(journal) {
        crate::agent::native_plugin_tx::recover(ctx, store, journal, resolve_failed, yes)?
    } else if matches!(
        journal.plan.action,
        Action::Apply | Action::Restore | Action::Reconcile
    ) {
        crate::config::config_tx::recover_journal(store, journal, resolve_failed, yes)?
    } else if matches!(
        journal.plan.package.provider,
        Provider::Governance {
            category: crate::domain::GovernanceCategory::Source,
            ..
        }
    ) {
        crate::source::source_tx::recover_journal(ctx, store, journal, resolve_failed, yes)?
    } else if matches!(journal.plan.package.provider, Provider::Governance { .. }) {
        crate::capability::capability_tx::recover_journal(ctx, store, journal, resolve_failed, yes)?
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
    let Some(original) = store::active(ctx)?
        .filter(|journal| crate::policy::operation::state::pending(journal.phase))
    else {
        return Ok(None);
    };
    let id = &original.id;
    let blocked =
        |reason: &str| Error::attention("recovery_required", format!("还没开始新操作。\n{reason}"));
    let scoped;
    let ctx = if crate::software::rules::service::is_control(original.plan.action) {
        scoped = crate::operations::identity::service_context(ctx)?;
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
    crate::operations::identity::validate(&original.scope)?;
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
    let local_ai = if crate::agent::ai_tx::is_native(&current) {
        let saved = crate::agent::ai_state::read(&store, &current)?;
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
        crate::software::brew::recover_locked(ctx, &store, &current, false, false, Some(&expected))
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
    .map_err(|error| automatic_recovery_error(error, blocked))?;
    store.verify_namespace()?;
    let active = Store::read::<Journal>(&store.root, "active.json")?
        .ok_or_else(|| blocked("没能确认上一次操作是否结束。请用 mo history 查看记录。"))?;
    store::validate_journal(&active)?;
    if crate::policy::operation::state::pending(active.phase) {
        return Err(blocked(&automatic_recovery_hint(&original, &result)));
    }
    let settled = locked_journal(&store, id)?;
    Ok(Some(Recovered {
        original,
        result,
        succeeded: settled.phase == Phase::Succeeded,
        local_ai,
    }))
}

fn automatic_recovery_error(error: Error, blocked: impl FnOnce(&str) -> Error) -> Error {
    if error.code == "plugin_target_changed" {
        error.with_guidance(
            "还没开始新操作。",
            "运行 mo recover 查看上次安装的记录，再检查原安装位置。",
        )
    } else {
        blocked(&crate::output::sanitize(&error.message, true))
    }
}

fn automatic_recovery_hint(journal: &Journal, result: &Value) -> String {
    let reason = if result["recoveryBlocker"] == "claude_profile_changed" {
        "Claude 设置后来有改动。请先确认要保留哪份设置。"
    } else if crate::agent::claude_install::is_install(journal)
        && let Some(message) = result["journal"]["message"].as_str()
    {
        message
    } else {
        "现在还不能确认上一次操作是否完成。"
    };
    let destination = crate::agent::plugin_target::recovery_target(journal)
        .ok()
        .flatten()
        .map_or_else(String::new, |target| {
            format!(
                "上次选的是 {}。",
                crate::output::sanitize(&target.name, false)
            )
        });
    format!("{destination}{reason}运行 mo recover 查看待检查的操作。")
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
            codex: crate::agent::codex_install::awaiting_followup(ctx, records).into(),
            claude: crate::agent::claude_install::awaiting_followup(ctx, records).into(),
        }
    }

    fn state(&self, journal: &Journal) -> RecoveryState {
        if crate::policy::operation::state::pending(journal.phase) {
            return RecoveryState::Pending;
        }
        if journal.phase != Phase::Succeeded {
            return RecoveryState::Settled;
        }
        let (client, selected) = if crate::agent::claude_install::is_install(journal) {
            (&self.claude, RecoveryState::ClaudeFollowup)
        } else if crate::agent::codex_install::is_install(journal) {
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
        && (crate::agent::claude_install::is_install(journal)
            || crate::agent::codex_install::is_install(journal))
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
    crate::operations::identity::validate(&journal.scope)?;
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
        return crate::software::brew::recover(
            ctx,
            &journal,
            dry_run,
            resolve_failed,
            yes,
            expected,
        );
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
    crate::operations::identity::validate(&journal.scope)?;
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
        return crate::software::npm_adoption::recover(ctx, store, &mut journal, resolve_failed);
    }
    if let Some(value) = recover_owned(ctx, store, &mut journal, resolve_failed, yes)? {
        return Ok(value);
    }
    let adopt_committed = if !resolve_failed
        && journal.phase == Phase::Prepared
        && journal.process_group.is_none()
        && journal.plan.action == Action::Adopt
    {
        crate::policy::operation::state::adopt_was_committed(
            &journal,
            store
                .receipts()?
                .installations
                .get(&journal.plan.package.id),
        )
    } else {
        false
    };
    match crate::policy::operation::state::recovery(
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
                crate::software::uv_ownership::check(ctx, &journal.plan, Some(&journal.id))?;
                let query = journal
                    .plan
                    .query
                    .as_ref()
                    .ok_or_else(|| Error::new("invalid_uv_plan", "缺少 uv 查询"))?;
                let observed =
                    providers::observe_query(&journal.plan.package, query, &journal.scope, false)?;
                crate::policy::operation::state::require_fresh_plan(
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

fn requested_journal(ctx: &Context, id: &str) -> Result<Journal> {
    if let Some(journal) = store::active(ctx)?
        && journal.id == id
    {
        return Ok(journal);
    }
    paths::regular_json(&ctx.state.join("operations").join(format!("{id}.json")))
}

#[cfg(test)]
#[path = "../../tests/unit/recovery.rs"]
mod tests;
