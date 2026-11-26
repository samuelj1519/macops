use crate::{
    domain::resource::CoverageState,
    domain::{
        Action, ConfigAnchor, ConfigAuthority, ConfigFileStamp, ConfigModePolicy, FileId, Kind,
        Observation, Package, Plan, Provider, QuerySpec,
    },
    error::{Error, Result},
};
use std::path::Path;

pub fn config_id(target: &Path) -> Result<String> {
    let clean = crate::policy::paths::lexical(target)?;
    crate::domain::resource::id("config", &clean)
}

pub fn classify_source_name(name: &str) -> std::result::Result<ConfigModePolicy, String> {
    match crate::config::rules::chezmoi::source_name(name, false) {
        Ok(Some(mapped)) if mapped.template => Err("source_template_not_evaluated".into()),
        Ok(Some(mapped)) if mapped.symlink => Err("source_link_expression_not_read".into()),
        Ok(Some(_)) => {
            let mut rest = name;
            let private = if let Some(v) = rest.strip_prefix("private_") {
                rest = v;
                true
            } else {
                false
            };
            let readonly = if let Some(v) = rest.strip_prefix("readonly_") {
                rest = v;
                true
            } else {
                false
            };
            let executable = rest.starts_with("executable_");
            Ok(ConfigModePolicy {
                private,
                readonly,
                executable,
            })
        }
        Ok(None) => Err("source_control_not_evaluated".into()),
        Err(error) => Err(error.code),
    }
}

pub fn source_file_policy(name: &str) -> Result<ConfigModePolicy> {
    classify_source_name(name).map_err(|code| Error::new("unsupported_config_source", code))
}

pub fn mode_satisfies(mode: u32, policy: ConfigModePolicy) -> bool {
    (!policy.private || mode.trailing_zeros() >= 6)
        && (!policy.readonly || mode & 0o222 == 0)
        && (policy.readonly || mode & 0o200 != 0)
        && (!policy.executable || mode & 0o100 != 0)
        && (policy.executable || mode & 0o111 == 0)
}

pub fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn content_digest(id: &str, raw_sha256: &str) -> Result<String> {
    crate::domain::fingerprint(&("macops-config-content-v1", id, raw_sha256))
}

pub fn stamp_fingerprint(
    parent: &Path,
    name: &str,
    raw_sha256: &str,
    identity: &FileId,
    mode: u32,
    lineage: &[FileId],
) -> Result<String> {
    crate::domain::fingerprint(&(
        "macops-config-local-v1",
        parent,
        name,
        raw_sha256,
        identity,
        mode,
        lineage,
    ))
}

pub fn validate_anchor(anchor: &ConfigAnchor) -> Result<()> {
    crate::policy::paths::lexical(&anchor.requested)?;
    crate::policy::paths::lexical(&anchor.canonical_parent)?;
    if !crate::policy::catalog::resources::component(&anchor.name)
        || anchor.requested.file_name().and_then(|n| n.to_str()) != Some(anchor.name.as_str())
    {
        return Err(Error::new(
            "invalid_config_anchor",
            "配置锚点文件名或目标路径不一致",
        ));
    }
    Ok(())
}

pub fn validate_package(package: &Package) -> Result<()> {
    if matches!(package.kind, Kind::Config)
        != matches!(package.provider, Provider::ConfigObserved { .. })
    {
        return Err(Error::new("invalid_config_kind", &package.id));
    }
    let Provider::ConfigObserved {
        target,
        source,
        authority,
    } = &package.provider
    else {
        return Ok(());
    };
    let expected_id = config_id(target)?;
    if package.id != expected_id
        || package.exact_version_required
        || target
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| !crate::policy::catalog::resources::component(n))
    {
        return Err(Error::new("invalid_config_package", &package.id));
    }
    match (authority, source) {
        (ConfigAuthority::ChezmoiCandidate, Some(src)) => {
            crate::policy::paths::lexical(src)?;
            let name = src
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::new("invalid_config_package", &package.id))?;
            source_file_policy(name)?;
            Ok(())
        }
        (ConfigAuthority::ChezmoiNative, Some(src)) => {
            crate::policy::paths::lexical(src).map(|_| ())
        }
        (ConfigAuthority::Untracked, None) => Ok(()),
        _ => Err(Error::new("invalid_config_package", &package.id)),
    }
}

fn invalid_plan() -> Error {
    Error::new("invalid_config_plan", "配置查询、目录身份或观察结果不匹配")
}

pub fn binding(package: &Package, query: &QuerySpec) -> Result<()> {
    if let QuerySpec::NativeConfig(native) = query {
        return if matches!(&package.provider, Provider::ConfigObserved { target, authority: ConfigAuthority::ChezmoiNative, .. } if native.targets == [target.clone()])
        {
            validate_package(package)
        } else {
            Err(invalid_plan())
        };
    }
    let QuerySpec::LocalConfig(query) = query else {
        return if matches!(package.provider, Provider::ConfigObserved { .. }) {
            Err(invalid_plan())
        } else {
            Ok(())
        };
    };
    let Provider::ConfigObserved {
        target,
        source,
        authority,
    } = &package.provider
    else {
        return Err(Error::new(
            "invalid_config_query_binding",
            "本地配置查询不匹配provider",
        ));
    };
    validate_package(package)?;
    validate_anchor(&query.target)?;
    if &query.target.requested != target || query.authority != *authority {
        return Err(Error::new(
            "invalid_config_query_binding",
            "本地配置查询与冻结对象不匹配",
        ));
    }
    match (*authority, source, &query.source, query.expected_mode) {
        (ConfigAuthority::ChezmoiCandidate, Some(pkg_src), Some(qry_src), Some(mode)) => {
            validate_anchor(qry_src)?;
            let expected = source_file_policy(&qry_src.name)?;
            if &qry_src.requested != pkg_src || mode != expected {
                return Err(Error::new(
                    "invalid_config_query_binding",
                    "配置源锚点或权限契约与配方不一致",
                ));
            }
            Ok(())
        }
        (ConfigAuthority::Untracked, None, None, None) => Ok(()),
        _ => Err(Error::new(
            "invalid_config_query_binding",
            "配置查询归属与源字段组合无效",
        )),
    }
}

fn valid_stamp(stamp: &ConfigFileStamp) -> bool {
    stamp.mode <= 0o7777 && valid_digest(&stamp.content_digest) && valid_digest(&stamp.fingerprint)
}

pub fn adoptable_observation(package: &Package, before: &Observation) -> Result<()> {
    let Provider::ConfigObserved {
        authority: pkg_auth,
        ..
    } = &package.provider
    else {
        return Err(invalid_plan());
    };
    let Observation::ConfigFile {
        authority,
        target,
        source,
        mode_matches_source,
    } = before
    else {
        return Err(invalid_plan());
    };
    if authority != pkg_auth {
        return Err(invalid_plan());
    }
    let Some(tgt) = target else {
        return Err(Error::attention("not_installed", &package.id));
    };
    if !valid_stamp(tgt) {
        return Err(invalid_plan());
    }
    match authority {
        ConfigAuthority::ChezmoiNative => Err(invalid_plan()),
        ConfigAuthority::ChezmoiCandidate => {
            let Some(src) = source else {
                return Err(Error::attention(
                    "config_source_missing",
                    "配置源候选不存在，无法建立采纳基线",
                ));
            };
            if !valid_stamp(src) || mode_matches_source.is_none() {
                return Err(invalid_plan());
            }
            if src.content_digest != tgt.content_digest || mode_matches_source != &Some(true) {
                return Err(Error::attention(
                    "configuration_drift",
                    "配置源候选与实际目标内容或权限不一致；先用 config diff 审查",
                ));
            }
            Ok(())
        }
        ConfigAuthority::Untracked => {
            if source.is_some() || mode_matches_source.is_some() {
                return Err(invalid_plan());
            }
            Ok(())
        }
    }
}

pub fn validate_plan(plan: &Plan) -> Result<()> {
    if let Some(query @ QuerySpec::NativeConfig(_)) = &plan.query {
        binding(&plan.package, query)?;
        return crate::config::rules::chezmoi_native::validate_plan(plan);
    }

    if let Some(query) = &plan.query {
        binding(&plan.package, query)?;
    }
    let config_pkg = matches!(plan.package.provider, Provider::ConfigObserved { .. });
    let config_obs = matches!(plan.before, Observation::ConfigFile { .. });
    let Some(QuerySpec::LocalConfig(_)) = &plan.query else {
        return if config_pkg || config_obs {
            Err(invalid_plan())
        } else {
            Ok(())
        };
    };
    if matches!(
        plan.action,
        Action::Apply | Action::Restore | Action::Reconcile
    ) {
        return crate::config::rules::config_tx::validate_config_tx_plan(plan);
    }
    if plan.action != Action::Adopt
        || plan.command.is_some()
        || plan.target.is_some()
        || plan.delegated
        || !plan.synchronous
    {
        return Err(invalid_plan());
    }
    adoptable_observation(&plan.package, &plan.before).map_err(|_| invalid_plan())
}

pub fn non_generating_control_issue(issue: &str) -> bool {
    let Some(raw_path) = issue.strip_prefix("source_control_not_evaluated:") else {
        return false;
    };
    let leaf = raw_path.rsplit_once('/').map_or(raw_path, |(_, file)| file);
    leaf == ".chezmoiignore"
        || leaf.starts_with(".chezmoiignore.")
        || leaf == ".chezmoiremove"
        || leaf.starts_with(".chezmoiremove.")
        || leaf == ".chezmoiroot"
        || leaf == ".chezmoiversion"
        || leaf == ".chezmoitemplates"
        || leaf.starts_with(".chezmoidata.")
}

pub fn authority_allows_untracked(state: &CoverageState, issues: &[String]) -> bool {
    match state {
        CoverageState::Complete | CoverageState::Absent => true,
        CoverageState::Unsupported => issues.iter().all(|issue| {
            issue == "physical_candidates_only"
                || issue.starts_with("source_template_not_evaluated:")
                || issue.starts_with("source_link_expression_not_read:")
                || non_generating_control_issue(issue)
        }),
        CoverageState::Unavailable | CoverageState::Invalid | CoverageState::Truncated => false,
    }
}

#[cfg(test)]
#[path = "../../tests/unit/config_policy.rs"]
mod tests;
