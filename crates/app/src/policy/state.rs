use crate::{
    domain::{Action, Journal, Observation, Phase, fingerprint},
    error::{Error, Result},
};

pub fn validate_journal(journal: &Journal) -> Result<()> {
    if fingerprint(&journal.scope)? != journal.scope_sha256
        || journal.scope.home != journal.home
        || !(1..=3600).contains(&journal.plan.timeout_secs)
        || journal.schema_version != 1
        || !valid_id(&journal.id)
        || fingerprint(&journal.plan.package)? != journal.plan.recipe_sha256
        || fingerprint(&journal.plan)? != journal.plan_sha256
    {
        return Err(Error::new(
            "invalid_journal",
            "操作格式或 recipe 摘要不匹配",
        ));
    }
    crate::policy::brew::checked_plan(&journal.plan)?;
    crate::policy::capability_tx::validate_journal(journal)?;
    crate::policy::uv_local::validate_journal(journal)?;
    crate::policy::npm_local::validate_plan(&journal.plan)?;
    crate::policy::npm_local::validate_journal_home(journal)?;
    crate::policy::npm_local::validate_binding_journal(journal)?;
    crate::policy::config_local::validate_plan(&journal.plan)?;
    crate::policy::capability_local::validate_plan(&journal.plan)?;
    crate::policy::service::validate_binding(journal)?;
    crate::policy::targets::validate_journal(journal)
}

pub fn valid_id(id: &str) -> bool {
    id.starts_with("OP-") && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

pub fn expected(journal: &Journal, observed: &Observation) -> bool {
    match crate::policy::brew::checked_plan(&journal.plan) {
        Ok(Some(view)) => return crate::policy::brew::expected(&view, observed),
        Err(_) => return false,
        Ok(None) => (),
    }
    if crate::policy::service::is_control(journal.plan.action) {
        return crate::policy::service::expected(journal.plan.action, observed);
    }
    if journal.plan.action == Action::Uninstall {
        return matches!(observed, Observation::Missing { .. });
    }
    match observed {
        Observation::Present { version, .. } => {
            journal.plan.target.as_ref().is_none_or(|v| v == version)
        }
        Observation::UvTool { .. } => {
            crate::policy::uv_local::is_uv(&journal.plan.package.provider)
                && journal.plan.action == Action::Adopt
                && observed == &journal.plan.before
        }
        Observation::NativePackage { .. } => {
            journal.plan.action == Action::Adopt && observed == &journal.plan.before
        }
        Observation::ConfigFile { .. } => {
            journal.plan.action == Action::Adopt
                && matches!(
                    journal.plan.package.provider,
                    crate::domain::Provider::ConfigObserved { .. }
                )
                && observed == &journal.plan.before
        }
        Observation::CapabilityPack { .. } => {
            journal.plan.action == Action::Adopt
                && matches!(
                    journal.plan.package.provider,
                    crate::domain::Provider::CapabilityObserved { .. }
                )
                && observed == &journal.plan.before
        }
        _ => false,
    }
}

pub fn recovery_journal(
    id: &str,
    active: Option<Journal>,
    history: Option<Journal>,
) -> Result<Journal> {
    if let Some(ref old) = history {
        validate_journal(old)?;
    }
    if let Some(active) = active {
        validate_journal(&active)?;
        if active.id == id {
            if history.is_some_and(|old| {
                old.plan_sha256 != active.plan_sha256
                    || old.scope != active.scope
                    || old.state != active.state
                    || old.cache != active.cache
                    || old.settings != active.settings
            }) {
                return Err(Error::attention(
                    "operation_conflict",
                    "原操作的冻结输入不一致",
                ));
            }
            return Ok(active);
        }
    }
    if let Some(old) = history
        && matches!(old.phase, Phase::Succeeded | Phase::Failed)
    {
        return Ok(old);
    }
    Err(Error::attention(
        "operation_conflict",
        "没有该操作的权威 active 记录；保留其他操作",
    ))
}

fn same_provider_identity(
    left: &crate::domain::Provider,
    right: &crate::domain::Provider,
) -> Result<bool> {
    if let (
        crate::domain::Provider::BrewFormula { query: left, .. },
        crate::domain::Provider::BrewFormula { query: right, .. },
    ) = (left, right)
    {
        return Ok(left.key == right.key);
    }
    if let (
        crate::domain::Provider::Npm {
            package: lp,
            prefix: lpre,
            ..
        },
        crate::domain::Provider::Npm {
            package: rp,
            prefix: rpre,
            ..
        },
    ) = (left, right)
    {
        return Ok(lp == rp && lpre == rpre);
    }
    Ok(fingerprint(left)? == fingerprint(right)?)
}

pub fn existing_owner(
    scope: &crate::domain::Scope,
    package: &crate::domain::Package,
    old: Option<&crate::domain::Receipt>,
) -> Result<()> {
    if let Some(old) = old
        && let (
            crate::domain::Provider::BrewFormula { query: left, .. },
            crate::domain::Provider::BrewFormula { query: right, .. },
        ) = (&old.package.provider, &package.provider)
        && old.scope.as_ref().is_some_and(|s| s.home == scope.home)
        && left.key == right.key
    {
        return Ok(());
    }
    if let Some(old) = old
        && (old.scope.as_ref() != Some(scope)
            || !same_provider_identity(&old.package.provider, &package.provider)?)
    {
        return Err(Error::attention(
            "manager_scope_changed",
            "同一 ID 已登记其他来源或作用域；保留原记录，使用不同 ID 或先审查 owner 迁移",
        ));
    }
    Ok(())
}
pub fn receipt_matches(
    scope: &crate::domain::Scope,
    package: &crate::domain::Package,
    before: &Observation,
    query: &crate::domain::QuerySpec,
    receipt: &crate::domain::Receipt,
) -> Result<()> {
    if fingerprint(&receipt.package)? != fingerprint(package)? || &receipt.observation != before {
        if let (
            crate::domain::Provider::Npm { authority: old, .. },
            crate::domain::Provider::Npm { authority: cur, .. },
        ) = (&receipt.package.provider, &package.provider)
            && old != cur
        {
            return Err(Error::attention(
                "installation_drift",
                "npm 配方写权限或来源契约已变更，请先核对并重新 adopt 确认基线",
            ));
        }
        return Err(Error::attention(
            "installation_drift",
            "来源或安装状态已改变，重新检查后 adopt",
        ));
    }
    if receipt.query.as_ref() != Some(query) {
        return Err(Error::attention(
            "manager_identity_changed",
            "管理器路径或作用域改变，未执行；先检查原生 owner",
        ));
    }
    if receipt.scope.as_ref() != Some(scope) {
        return Err(Error::attention(
            "manager_scope_changed",
            "安装作用域改变，未执行；请先核对原生 owner",
        ));
    }
    Ok(())
}
pub fn cache_version(
    package: &crate::domain::Package,
    scope: &crate::domain::Scope,
    cached: Option<&crate::domain::Available>,
) -> Result<Option<String>> {
    let source = fingerprint(package)?;
    let scope = fingerprint(scope)?;
    Ok(cached
        .filter(|v| v.source_sha256 == source && v.scope_sha256 == scope)
        .map(|v| v.version.clone()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recovery {
    Terminal,
    ResolveFailed,
    CompleteAdopt,
    NoGate,
    Verify,
}

pub fn recovery(
    journal: &Journal,
    state: &std::path::Path,
    in_flight: bool,
    resolve_failed: bool,
    adopt_committed: bool,
) -> Result<Recovery> {
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(Recovery::Terminal);
    }
    if in_flight {
        return Err(Error::attention(
            "operation_in_flight",
            "原安装器或后代仍在运行；未收口，未重跑",
        ));
    }
    if resolve_failed {
        return Ok(Recovery::ResolveFailed);
    }
    if journal.phase == Phase::Prepared && journal.process_group.is_none() {
        return Ok(if journal.plan.action == Action::Adopt && adopt_committed {
            Recovery::CompleteAdopt
        } else {
            Recovery::NoGate
        });
    }
    if journal.process_group.is_none() || !journal.plan.synchronous || journal.plan.delegated {
        return Err(Error::attention(
            "manual_recovery_required",
            "不能证明原后台动作已结束；保留阻断，不自动重跑或改写记录",
        ));
    }
    if journal.state != state {
        return Err(Error::new("state_identity_mismatch", "恢复状态目录不匹配"));
    }
    Ok(Recovery::Verify)
}

pub fn pending(phase: Phase) -> bool {
    matches!(phase, Phase::Prepared | Phase::Running | Phase::Unknown)
}

pub fn completion(code: Option<i32>, uncertain: bool, in_flight: bool) -> Option<String> {
    if uncertain || in_flight {
        Some("安装器超时、输出越界或后代进程仍存在；保留阻断，请先 recover".into())
    } else if code != Some(0) {
        Some(format!(
            "安装器 exit {code:?}，可能有部分副作用；请 recover 查询，不会自动重跑"
        ))
    } else {
        None
    }
}

pub fn plan_inputs(
    package: &crate::domain::Package,
    action: Action,
    before: &Observation,
    version: Option<&str>,
) -> Result<()> {
    if matches!(package.provider, crate::domain::Provider::Service { .. })
        || crate::policy::service::is_control(action)
    {
        return Err(Error::new(
            "service_command_required",
            "使用 service start/stop 控制服务，不使用软件安装命令",
        ));
    }
    crate::policy::uv_local::action(&package.provider, action)?;
    if crate::policy::uv_local::is_uv(&package.provider) {
        if version.is_some() {
            return Err(Error::new(
                "version_unsupported",
                "uv 只读采纳不接受版本参数",
            ));
        }
        return crate::policy::uv_local::adoptable(before);
    }
    if matches!(before, Observation::UvTool { .. }) {
        return Err(Error::new("invalid_uv_plan", "uv 观察不能用于其他配方"));
    }
    version_request(package, action, version)?;
    if matches!(before, Observation::NativePackage { .. })
        && (action != Action::Adopt || !crate::policy::targets::npm_observer(package)?)
    {
        return Err(Error::new(
            "operation_unsupported",
            "原生观察协议仅供只读采纳，不能赋予通用写recipe",
        ));
    }
    if version.is_some() && matches!(package.provider, crate::domain::Provider::Brew { .. }) {
        return Err(Error::new(
            "brew_version_unsupported",
            "brew 不提供通用任意版本安装，请使用受审版本化配方",
        ));
    }
    if let Observation::Unknown { reason } = before {
        return Err(Error::attention(
            "observation_required",
            format!(
                "{{\"status\":\"unknown\",\"reason\":{}}}",
                serde_json::Value::String(reason.clone())
            ),
        ));
    }
    if matches!(
        package.provider,
        crate::domain::Provider::ConfigObserved { .. }
    ) {
        if version.is_some() {
            return Err(Error::new(
                "version_unsupported",
                "配置文件采纳不接受软件版本参数",
            ));
        }
        if action != Action::Adopt {
            return Err(Error::new(
                "operation_unsupported",
                "静态配置观察基线仅支持 adopt，不授予写覆盖或删除权限",
            ));
        }
        return crate::policy::config_local::adoptable_observation(package, before);
    }
    if matches!(before, Observation::ConfigFile { .. }) {
        return Err(Error::new(
            "operation_unsupported",
            "配置观察结果不能用于非配置配方",
        ));
    }
    if matches!(
        package.provider,
        crate::domain::Provider::CapabilityObserved { .. }
    ) {
        if version.is_some() {
            return Err(Error::new(
                "version_unsupported",
                "能力包采纳不接受软件版本参数",
            ));
        }
        if action != Action::Adopt {
            return Err(Error::new(
                "operation_unsupported",
                "能力包观察基线仅支持 adopt，不授予自动覆写或级联卸载权限",
            ));
        }
        return crate::policy::capability_local::adoptable_observation(package, before);
    }
    if matches!(before, Observation::CapabilityPack { .. }) {
        return Err(Error::new(
            "operation_unsupported",
            "能力包观察结果不能用于非能力包配方",
        ));
    }
    Ok(())
}

pub fn version_request(
    package: &crate::domain::Package,
    action: Action,
    version: Option<&str>,
) -> Result<()> {
    if package.exact_version_required
        && matches!(action, Action::Install | Action::Upgrade)
        && version.is_none()
    {
        return Err(Error::new("exact_version_required", &package.id));
    }
    Ok(())
}

pub fn require_install_slot(action: Action, before: &Observation) -> Result<()> {
    if action == Action::Install && !matches!(before, Observation::Missing { .. }) {
        return Err(Error::attention(
            "already_installed",
            "已安装；先 adopt 或使用 upgrade",
        ));
    }
    Ok(())
}

pub fn require_fresh_plan(
    plan: &crate::domain::Plan,
    current: &crate::domain::Package,
    observed: &Observation,
) -> Result<()> {
    if fingerprint(current)? != plan.recipe_sha256 || observed != &plan.before {
        return Err(Error::attention(
            "plan_stale",
            "声明或安装状态在计划后发生变化，未执行",
        ));
    }
    Ok(())
}
pub fn adopt_was_committed(journal: &Journal, receipt: Option<&crate::domain::Receipt>) -> bool {
    receipt.is_some_and(|r| r.operation_id == journal.id && r.observation == journal.plan.before)
}

pub fn worker_gate(bytes: [u8; 4]) -> Result<()> {
    if bytes != *b"RUN\n" {
        return Err(Error::new("invalid_gate", "worker gate 不匹配"));
    }
    Ok(())
}
pub fn worker_identity(
    journal: &Journal,
    path: &std::path::Path,
    pid: u32,
    group: i32,
) -> Result<()> {
    let pid_i32 = i32::try_from(pid).map_err(|_| Error::new("invalid_pid", "PID 超出范围"))?;
    if journal.phase != Phase::Running || journal.process_group != Some(pid) || group != pid_i32 {
        return Err(Error::new(
            "worker_identity_mismatch",
            "worker 身份与记录不匹配",
        ));
    }
    if path
        != journal
            .state
            .join("operations")
            .join(format!("{}.json", journal.id))
    {
        return Err(Error::new("journal_path_mismatch", "操作记录位置错误"));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Absent,
    Explicit(String),
    Cached(String),
    Unresolved,
    Latest,
}
pub fn version_resolution(
    action: Action,
    dry_run: bool,
    version: Option<&str>,
    cached: Option<&str>,
) -> Result<Resolution> {
    if action == Action::Uninstall {
        return Ok(Resolution::Absent);
    }
    if let Some(version) = version {
        return Ok(Resolution::Explicit(crate::policy::parsers::exact_version(
            version,
        )?));
    }
    if dry_run {
        Ok(cached.map_or(Resolution::Unresolved, |v| Resolution::Cached(v.into())))
    } else {
        Ok(Resolution::Latest)
    }
}

pub fn receipt_schema(receipts: &crate::domain::Receipts) -> Result<()> {
    if receipts.schema_version != 1 {
        return Err(Error::new("state_version", "receipt schema_version 错误"));
    }
    Ok(())
}
pub fn commit_receipt(
    receipts: &mut crate::domain::Receipts,
    journal: &Journal,
    observation: Observation,
    now: u64,
) -> Result<bool> {
    crate::policy::uv_local::validate_commit(journal, receipts, &observation)?;
    if matches!(
        journal.plan.package.provider,
        crate::domain::Provider::Governance { .. }
    ) || journal.plan.action == Action::Maintain
    {
        return Err(Error::new(
            "invalid_receipt_kind",
            "治理事务不能作为软件安装基线提交",
        ));
    }
    if matches!(journal.plan.action, Action::Uninstall | Action::Restore) {
        Ok(receipts
            .installations
            .remove(&journal.plan.package.id)
            .is_some())
    } else {
        let query = crate::policy::npm_local::committed_query(&journal.plan, &observation)?;
        if let Some(existing) = receipts.installations.get(&journal.plan.package.id)
            && existing.operation_id == journal.id
        {
            if existing.query != query
                || existing.observation != observation
                || existing.scope.as_ref() != Some(&journal.scope)
                || fingerprint(&existing.package)? != journal.plan.recipe_sha256
            {
                return Err(Error::attention(
                    "commit_conflict",
                    "同一操作不能覆盖已提交的安装基线",
                ));
            }
            return Ok(false);
        }
        receipts.installations.insert(
            journal.plan.package.id.clone(),
            crate::domain::Receipt {
                query,
                scope: Some(journal.scope.clone()),
                package: journal.plan.package.clone(),
                observation,
                operation_id: journal.id.clone(),
                updated_at: now,
            },
        );
        Ok(true)
    }
}

#[cfg(test)]
#[path = "../../tests/unit/state.rs"]
mod tests;
