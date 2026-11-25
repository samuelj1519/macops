use crate::{
    domain::{
        Action, LocalUvToolQuery, Observation, Package, Plan, Provider, QuerySpec, UvToolBinding,
    },
    error::{Error, Result},
};
use std::path::Path;

pub fn normalize_name(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 214
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || !value.as_bytes()[value.len() - 1].is_ascii_alphanumeric()
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(Error::new("invalid_uv_name", "uv 包名不在支持的规范子集内"));
    }
    let mut normalized = String::new();
    for b in value.bytes() {
        if b"-_.".contains(&b) {
            if !normalized.ends_with('-') {
                normalized.push('-');
            }
        } else {
            normalized.push(char::from(b.to_ascii_lowercase()));
        }
    }
    Ok(normalized)
}

pub fn tool_id(root: &Path, name: &str) -> Result<String> {
    crate::policy::paths::lexical(root)?;
    crate::domain::resource::id("application", &("uv", root, normalize_name(name)?))
}

pub fn descriptor(provider: &Provider) -> Option<(&str, &UvToolBinding)> {
    match provider {
        Provider::Uv {
            package,
            binding: Some(binding),
        }
        | Provider::UvObserved { package, binding } => Some((package, binding)),
        _ => None,
    }
}
pub fn is_uv(provider: &Provider) -> bool {
    matches!(provider, Provider::Uv { .. } | Provider::UvObserved { .. })
}
pub fn blocked() -> Error {
    Error::attention(
        "uv_write_blocked_review",
        "UV-RO 仅支持本地观察和零写权 adopt；原生写入、版本获取、共享入口及清理域仍待独立评审",
    )
}
pub fn action(provider: &Provider, action: Action) -> Result<()> {
    if is_uv(provider) && action != Action::Adopt {
        return Err(blocked());
    }
    Ok(())
}
pub fn validate_package(package: &Package) -> Result<()> {
    if let Some((name, binding)) = descriptor(&package.provider) {
        normalize_name(name)?;
        crate::policy::paths::lexical(&binding.tool_dir)?;
        crate::policy::paths::lexical(&binding.bin_dir)?;
    }
    Ok(())
}
pub fn binding(package: &Package, query: &QuerySpec) -> Result<()> {
    if matches!(package.provider, Provider::Uv { binding: None, .. }) {
        return Err(Error::attention(
            "legacy_uv_query_unbound",
            "旧 uv 记录未绑定 namespace",
        ));
    }
    match (descriptor(&package.provider), query) {
        (Some((name, binding)), QuerySpec::LocalUvTool(local))
            if normalize_name(name)? == local.name
                && binding.tool_dir == local.namespace.tool_dir.path
                && binding.bin_dir == local.namespace.bin_dir.path =>
        {
            validate_package(package)?;
            Ok(())
        }
        (None, QuerySpec::LocalUvTool(_)) | (Some(_), _) => {
            Err(Error::new("invalid_uv_query", "uv 查询未绑定原 namespace"))
        }
        _ => Ok(()),
    }
}
pub fn adoptable(before: &Observation) -> Result<()> {
    if matches!(before, Observation::UvTool { .. }) {
        Ok(())
    } else {
        Err(Error::attention(
            "uv_observation_required",
            "uv 采纳需要稳定的完整 Present 观察",
        ))
    }
}
pub fn validate_plan(plan: &Plan) -> Result<()> {
    action(&plan.package.provider, plan.action)?;
    if is_uv(&plan.package.provider) {
        let query = plan
            .query
            .as_ref()
            .ok_or_else(|| Error::new("invalid_uv_plan", "uv 计划缺少冻结查询"))?;
        binding(&plan.package, query)?;
        if plan.command.is_some() || plan.delegated || !plan.synchronous || plan.target.is_some() {
            return Err(Error::new(
                "invalid_uv_plan",
                "uv 采纳不携带写命令或执行授权",
            ));
        }
        let owner = plan
            .uv_owner
            .as_ref()
            .ok_or_else(|| Error::new("invalid_uv_plan", "缺少冻结 uv owner"))?;
        if owner.selected_owner != plan.package.id
            || owner.entries.len() > 1
            || owner.entries.keys().any(|id| id != &plan.package.id)
        {
            return Err(Error::new("invalid_uv_plan", "uv owner 身份不匹配"));
        }
        adoptable(&plan.before)?;
    } else if plan.uv_owner.is_some()
        || matches!(plan.query, Some(QuerySpec::LocalUvTool(_)))
        || matches!(plan.before, Observation::UvTool { .. })
    {
        return Err(Error::new(
            "invalid_uv_plan",
            "uv 观察不能赋予其他 provider 权限",
        ));
    }
    Ok(())
}
pub fn query_id(query: &LocalUvToolQuery) -> Result<String> {
    tool_id(&query.namespace.tool_dir.path, &query.name)
}

#[cfg(test)]
#[path = "../../tests/unit/uv_local_policy.rs"]
mod tests;

pub fn optional_binding<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<UvToolBinding>, D::Error> {
    crate::policy::json::object(input).map(Some)
}

pub fn validate_journal(journal: &crate::domain::Journal) -> Result<()> {
    if matches!(
        journal.plan.package.provider,
        Provider::Uv { binding: None, .. }
    ) && journal.plan.uv_owner.is_none()
        && matches!(journal.plan.query, Some(QuerySpec::Command(_)))
    {
        return Ok(());
    }
    validate_plan(&journal.plan)?;
    if is_uv(&journal.plan.package.provider)
        && (journal.process_group.is_some() || journal.phase == crate::domain::Phase::Running)
    {
        return Err(Error::new(
            "invalid_uv_journal",
            "uv 采纳不能携带原生执行状态",
        ));
    }
    Ok(())
}
pub fn validate_commit(
    journal: &crate::domain::Journal,
    receipts: &crate::domain::Receipts,
    observation: &Observation,
) -> Result<()> {
    if !is_uv(&journal.plan.package.provider) {
        validate_plan(&journal.plan)?;
        return Ok(());
    }
    if observation != &journal.plan.before {
        return Err(Error::attention(
            "uv_observation_changed",
            "uv 采纳观察已改变",
        ));
    }
    let expected = journal
        .plan
        .uv_owner
        .as_ref()
        .ok_or_else(|| Error::new("invalid_uv_plan", "缺少 uv owner CAS"))?;
    let Some(QuerySpec::LocalUvTool(query)) = &journal.plan.query else {
        return Err(Error::new("invalid_uv_plan", "缺少 uv 查询"));
    };
    let identity = query_id(query)?;
    validate_plan(&journal.plan)?;
    let mut actual =
        std::collections::BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for (key, receipt) in &receipts.installations {
        if key == &journal.plan.package.id && receipt.operation_id == journal.id {
            continue;
        }
        if let Some(QuerySpec::LocalUvTool(q)) = &receipt.query
            && query_id(q)? == identity
        {
            actual
                .entry(key.clone())
                .or_default()
                .insert(format!("receipt:{}", crate::domain::fingerprint(receipt)?));
        } else if key == &journal.plan.package.id {
            return Err(Error::attention(
                "owner_migration_required",
                "原 owner 不能被 uv 采纳覆盖",
            ));
        }
    }
    let mut frozen = expected.entries.clone();
    frozen.retain(|_, tokens| {
        tokens.retain(|s| s.starts_with("receipt:"));
        !tokens.is_empty()
    });
    if receipts
        .installations
        .get(&journal.plan.package.id)
        .is_some_and(|r| r.operation_id == journal.id)
    {
        frozen.remove(&journal.plan.package.id);
    }
    if frozen != actual {
        return Err(Error::attention(
            "uv_owner_changed",
            "uv 收据 owner CAS 不匹配",
        ));
    }
    Ok(())
}
