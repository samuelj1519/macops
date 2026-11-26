use crate::{
    domain::{
        Action, CapabilityKind, CapabilityStamp, FileId, Kind, Observation, Package, Plan,
        Provider, QuerySpec, fingerprint,
    },
    error::{Error, Result},
    policy::config_local::{valid_digest, validate_anchor},
};
use std::path::Path;

pub fn skill_capability_id(
    record: &Path,
    name: &str,
    source_type: &str,
    source: &str,
) -> Result<String> {
    let clean = crate::policy::paths::lexical(record)?;
    crate::domain::resource::id(
        "capability",
        &("skills-cli", clean, name, source_type, source),
    )
}

pub fn plugin_capability_id(profile_id: &str, native_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(&(
        "claude-plugin",
        profile_id,
        native_id,
        "user",
        Option::<String>::None,
    ))
    .unwrap_or_default();
    format!("capability:{:x}", Sha256::digest(bytes))
}

pub fn codex_capability_id(profile_id: &str, native_id: &str) -> Result<String> {
    crate::domain::resource::id("capability", &("codex-plugin", profile_id, native_id))
}

fn codex_cache_binding<'a>(
    record: &Path,
    payload: &'a Path,
    native_id: &'a str,
) -> Option<(&'a str, &'a str)> {
    let (plugin, market) = native_id.rsplit_once('@')?;
    let root = record.parent()?;
    let version = payload.file_name()?.to_str()?;
    (record.file_name().and_then(std::ffi::OsStr::to_str) == Some("config.toml")
        && valid_plugin_id(native_id)
        && crate::agent::native_plugins::valid_name(version)
        && payload.parent()
            == Some(
                root.join("plugins/cache")
                    .join(market)
                    .join(plugin)
                    .as_path(),
            ))
    .then_some((market, version))
}

pub fn valid_plugin_id(value: &str) -> bool {
    value.rsplit_once('@').is_some_and(|(name, market)| {
        crate::policy::catalog::resources::component(name)
            && crate::policy::catalog::resources::component(market)
    })
}

pub fn validate_package(package: &Package) -> Result<()> {
    let Provider::CapabilityObserved {
        capability,
        native_id,
        manager,
        record,
        payload,
        source,
        profile_id,
        settings,
    } = &package.provider
    else {
        return Ok(());
    };
    crate::policy::paths::lexical(payload)?;
    if package.exact_version_required {
        return Err(Error::new("invalid_capability_package", &package.id));
    }
    match capability {
        CapabilityKind::SkillsCli => {
            let Some(source_type) = manager
                .strip_prefix("skills-cli:")
                .filter(|s| !s.is_empty())
            else {
                return Err(Error::new("invalid_capability_package", &package.id));
            };
            let Some(src) = source.as_deref().filter(|s| !s.is_empty()) else {
                return Err(Error::new("invalid_capability_package", &package.id));
            };
            if package.kind != Kind::Skill
                || !crate::policy::catalog::resources::component(native_id)
                || profile_id.is_some()
                || settings.is_some()
                || (package.id != skill_capability_id(record, native_id, source_type, src)?
                    && package.id != crate::skills::rules::native_skills::installation_id(payload)?)
            {
                return Err(Error::new("invalid_capability_package", &package.id));
            }
            Ok(())
        }
        CapabilityKind::ClaudePlugin => {
            crate::policy::paths::lexical(record)?;
            let (Some(prof), Some(cfg)) = (profile_id.as_deref(), settings.as_deref()) else {
                return Err(Error::new("invalid_capability_package", &package.id));
            };
            crate::policy::paths::lexical(cfg)?;
            if package.kind != Kind::Plugin
                || !valid_plugin_id(native_id)
                || manager != "claude-plugin"
                || source.is_some()
                || !prof.starts_with("agent:")
                || package.id != plugin_capability_id(prof, native_id)
            {
                return Err(Error::new("invalid_capability_package", &package.id));
            }
            Ok(())
        }
        CapabilityKind::CodexPlugin => {
            crate::policy::paths::lexical(record)?;
            let root = record.parent().ok_or_else(invalid_plan)?;
            let profile = crate::domain::resource::id("agent", &("codex", root)).ok();
            if package.kind != Kind::Plugin
                || manager != "codex-config"
                || source.is_some()
                || settings.is_some()
                || !profile.as_ref().is_some_and(|profile| {
                    profile_id.as_ref() == Some(profile)
                        && codex_capability_id(profile, native_id).is_ok_and(|id| package.id == id)
                })
                || codex_cache_binding(record, payload, native_id).is_none()
            {
                return Err(Error::new("invalid_capability_package", &package.id));
            }
            Ok(())
        }
    }
}

pub fn parse_codex_record(
    id: &str,
    record: &Path,
    payload: &Path,
    native_id: &str,
    bytes: &[u8],
) -> Result<Option<ParsedRecord>> {
    let (market, version) =
        codex_cache_binding(record, payload, native_id).ok_or_else(invalid_plan)?;
    let config = crate::policy::catalog::resources::codex_config_document(bytes)?;
    let plugins = crate::policy::catalog::resources::codex_plugins_from(&config)?;
    let Some(configured_enabled) = plugins.get(native_id).copied() else {
        return Ok(None);
    };
    let market_record = config
        .get("marketplaces")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|table| table.get(market))
        .map(ToString::to_string);
    fingerprint(&(
        "macops-codex-plugin-v1",
        id,
        native_id,
        version,
        configured_enabled,
        market_record,
    ))
    .map(|digest| {
        Some(ParsedRecord {
            digest,
            revision: Some(version.into()),
            configured_enabled,
        })
    })
}

fn invalid_plan() -> Error {
    Error::new(
        "invalid_capability_plan",
        "能力包查询、目录身份或观察结果不匹配",
    )
}

pub fn binding(package: &Package, query: &QuerySpec) -> Result<()> {
    let QuerySpec::LocalCapability(query) = query else {
        return if matches!(package.provider, Provider::CapabilityObserved { .. }) {
            Err(invalid_plan())
        } else {
            Ok(())
        };
    };
    let Provider::CapabilityObserved {
        capability,
        native_id,
        manager,
        record,
        payload,
        source,
        settings,
        ..
    } = &package.provider
    else {
        return Err(Error::new(
            "invalid_capability_query_binding",
            "本地能力包查询不匹配provider",
        ));
    };
    validate_package(package)?;
    validate_anchor(&query.record)?;
    validate_anchor(&query.payload)?;
    if query.capability != *capability
        || &query.native_id != native_id
        || &query.manager != manager
        || &query.source != source
        || &query.record.requested != record
        || &query.payload.requested != payload
    {
        return Err(Error::new(
            "invalid_capability_query_binding",
            "本地能力包查询与冻结对象不匹配",
        ));
    }
    match (settings, &query.settings) {
        (Some(pkg_cfg), Some(qry_cfg)) => {
            validate_anchor(qry_cfg)?;
            if &qry_cfg.requested != pkg_cfg {
                return Err(Error::new(
                    "invalid_capability_query_binding",
                    "能力包设置锚点与配方不一致",
                ));
            }
            Ok(())
        }
        (None, None) => Ok(()),
        _ => Err(Error::new(
            "invalid_capability_query_binding",
            "能力包设置字段组合无效",
        )),
    }
}

pub fn parse_skill_record(
    id: &str,
    bytes: &[u8],
    name: &str,
    expected_type: &str,
    expected_source: &str,
) -> Result<Option<(String, Option<String>)>> {
    let lock = crate::policy::catalog::resources::skill_lock(bytes, false)?;
    let Some(rec) = lock.skills.get(name) else {
        return Ok(None);
    };
    let same_source = rec.source == expected_source
        || (expected_type == "github"
            && rec.source_type == "github"
            && crate::policy::source_git::Repository::parse(&rec.source)?.key()
                == crate::policy::source_git::Repository::parse(expected_source)?.key());
    if rec.source_type != expected_type || !same_source {
        return Err(Error::attention(
            "capability_source_changed",
            "技能 lock 中的来源已改变",
        ));
    }
    fingerprint(&(
        "macops-skill-lock-v1",
        id,
        name,
        &rec.source_type,
        &rec.source,
        &rec.source_url,
        &rec.revision,
        &rec.install_mode,
        &rec.folder_hash,
    ))
    .map(|digest| Some((digest, rec.revision.clone())))
}

#[derive(Debug, PartialEq, Eq)]
pub struct ParsedRecord {
    pub digest: String,
    pub revision: Option<String>,
    pub configured_enabled: Option<bool>,
}

pub fn parse_plugin_record(
    id: &str,
    bytes: &[u8],
    native_id: &str,
    expected_payload: &Path,
    settings_bytes: Option<&[u8]>,
) -> Result<Option<ParsedRecord>> {
    let book = crate::policy::catalog::resources::plugin_book(bytes)?;
    let user_records: Vec<_> = book.plugins.get(native_id).map_or_else(Vec::new, |rows| {
        rows.iter().filter(|r| r.scope == "user").collect()
    });
    let rec = match user_records.as_slice() {
        [] => return Ok(None),
        [one] => *one,
        _ => {
            return Err(Error::attention(
                "duplicate_plugin_scope",
                "同一作用域存在重复插件登记",
            ));
        }
    };
    if rec.path != expected_payload {
        return Err(Error::attention(
            "capability_payload_rebound",
            "插件登记的物理路径已改变",
        ));
    }
    let configured_enabled = match settings_bytes {
        Some(raw) => crate::policy::catalog::resources::plugin_enabled(raw)?
            .get(native_id)
            .copied(),
        None => None,
    };
    let revision = rec.revision.clone().or_else(|| rec.version.clone());
    fingerprint(&(
        "macops-claude-plugin-v1",
        id,
        native_id,
        &rec.path,
        &rec.version,
        &rec.revision,
        configured_enabled,
    ))
    .map(|digest| {
        Some(ParsedRecord {
            digest,
            revision,
            configured_enabled,
        })
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadMetadataDisposition {
    Include,
    IgnoreRegularMarker,
    RejectReservedEntry,
}

pub fn classify_payload_entry(
    name: &str,
    depth: usize,
    is_regular_file: bool,
) -> PayloadMetadataDisposition {
    if name == ".DS_Store" {
        return if is_regular_file {
            PayloadMetadataDisposition::IgnoreRegularMarker
        } else {
            PayloadMetadataDisposition::RejectReservedEntry
        };
    }
    if name == ".in_use" {
        return if is_regular_file && depth == 0 {
            PayloadMetadataDisposition::IgnoreRegularMarker
        } else {
            PayloadMetadataDisposition::RejectReservedEntry
        };
    }
    PayloadMetadataDisposition::Include
}

pub fn payload_tree_digest(id: &str, entries: &[(String, bool, String)]) -> Result<String> {
    fingerprint(&("macops-capability-payload-v1", id, entries))
}

#[derive(Debug)]
pub struct StampInput<'a> {
    pub id: &'a str,
    pub record_digest: String,
    pub payload_digest: String,
    pub payload_identity: FileId,
    pub file_count: u64,
    pub revision: Option<String>,
    pub configured_enabled: Option<bool>,
    pub lineage: &'a [FileId],
}

pub fn build_stamp(input: StampInput<'_>) -> Result<CapabilityStamp> {
    fingerprint(&(
        "macops-capability-stamp-v1",
        input.id,
        &input.record_digest,
        &input.payload_digest,
        &input.payload_identity,
        input.file_count,
        &input.revision,
        input.configured_enabled,
        input.lineage,
    ))
    .map(|digest| CapabilityStamp {
        record_digest: input.record_digest,
        payload_digest: input.payload_digest,
        payload_identity: input.payload_identity,
        file_count: input.file_count,
        revision: input.revision,
        configured_enabled: input.configured_enabled,
        fingerprint: digest,
    })
}

pub fn adoptable_observation(package: &Package, before: &Observation) -> Result<()> {
    let Provider::CapabilityObserved {
        capability: pkg_cap,
        ..
    } = &package.provider
    else {
        return Err(invalid_plan());
    };
    let Observation::CapabilityPack { capability, pack } = before else {
        return Err(invalid_plan());
    };
    if capability != pkg_cap {
        return Err(invalid_plan());
    }
    let Some(stamp) = pack else {
        return Err(Error::attention("not_installed", &package.id));
    };
    if stamp.file_count == 0
        || !valid_digest(&stamp.record_digest)
        || !valid_digest(&stamp.payload_digest)
        || !valid_digest(&stamp.fingerprint)
        || (*capability == CapabilityKind::SkillsCli && stamp.configured_enabled.is_some())
    {
        return Err(invalid_plan());
    }
    Ok(())
}

pub fn validate_plan(plan: &Plan) -> Result<()> {
    if plan.delegated
        && plan
            .warnings
            .first()
            .is_some_and(|s| s.starts_with("native-skills-v1:"))
    {
        validate_package(&plan.package)?;
        if !matches!(
            plan.package.provider,
            Provider::CapabilityObserved {
                capability: CapabilityKind::SkillsCli,
                ..
            }
        ) || plan.query.is_some()
            || plan.target.is_some()
            || !plan.synchronous
            || !matches!(
                plan.action,
                Action::Install | Action::Upgrade | Action::Uninstall | Action::Adopt
            )
            || (plan.command.is_none() != (plan.action == Action::Adopt))
        {
            return Err(invalid_plan());
        }
        return Ok(());
    }
    if let Some(query) = &plan.query {
        binding(&plan.package, query)?;
    }
    let cap_pkg = matches!(plan.package.provider, Provider::CapabilityObserved { .. });
    let cap_obs = matches!(plan.before, Observation::CapabilityPack { .. });
    let Some(QuerySpec::LocalCapability(_)) = &plan.query else {
        return if cap_pkg || cap_obs {
            Err(invalid_plan())
        } else {
            Ok(())
        };
    };
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

#[cfg(test)]
#[path = "../../../tests/unit/capability_policy.rs"]
mod tests;

pub const FILE_LIMIT: u64 = 1024 * 1024;
pub const PAYLOAD_BYTE_BUDGET: u64 = 16 * 1024 * 1024;
pub const PAYLOAD_ENTRY_LIMIT: usize = 2000;
pub const PAYLOAD_DEPTH_LIMIT: usize = 12;
