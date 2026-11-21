use crate::{
    config_inventory,
    context::Context,
    domain::{
        ConfigAnchor, ConfigAuthority, ConfigFileStamp, ConfigQueryKind, Kind, LocalConfigQuery,
        Observation, OwnedPath, Package, PathKind, Provider, Receipts,
    },
    error::{Error, Result},
    paths::{self, Directory, EntryKind},
    policy::config_local::{self as policy, authority_allows_untracked},
    resource::CoverageState,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

const FILE_LIMIT: u64 = 1024 * 1024;

fn changed() -> Error {
    Error::attention("config_namespace_changed", "配置目录身份在读取期间改变")
}

pub fn canonical_identity(target: &Path) -> Result<String> {
    let clean = crate::policy::paths::lexical(target)?;
    let parent = clean
        .parent()
        .ok_or_else(|| Error::new("invalid_config_target", "配置路径缺少父目录"))?;
    let name = clean
        .file_name()
        .ok_or_else(|| Error::new("invalid_config_target", "配置文件名缺失"))?;
    policy::config_id(&paths::read_path(parent)?.join(name))
}

pub fn capture(package: &Package) -> Result<LocalConfigQuery> {
    policy::validate_package(package)?;
    let Provider::ConfigObserved {
        target,
        source,
        authority,
    } = &package.provider
    else {
        return Err(Error::new(
            "invalid_config_package",
            "本地配置捕获仅适用于静态配置对象",
        ));
    };
    let target_anchor = capture_anchor(target)?;
    let (source_anchor, expected_mode) = if let Some(src) = source {
        let anchor = capture_anchor(src)?;
        let mode = policy::source_file_policy(&anchor.name)?;
        (Some(anchor), Some(mode))
    } else {
        (None, None)
    };
    Ok(LocalConfigQuery {
        kind: ConfigQueryKind::LocalV1,
        authority: *authority,
        target: target_anchor,
        source: source_anchor,
        expected_mode,
    })
}

pub(crate) fn capture_anchor(path: &Path) -> Result<ConfigAnchor> {
    let clean = crate::policy::paths::lexical(path)?;
    let parent = clean
        .parent()
        .ok_or_else(|| Error::new("invalid_config_target", "配置路径缺少父目录"))?;
    let name = clean
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| crate::policy::resources::component(n))
        .ok_or_else(|| Error::new("invalid_config_target", "配置文件名不安全"))?
        .to_owned();
    let dir = Directory::open(parent)?;
    dir.verify()?;
    let canonical_parent = paths::read_path(parent)?;
    if dir.path != canonical_parent {
        return Err(changed());
    }
    Ok(ConfigAnchor {
        requested: clean,
        canonical_parent,
        parent_identity: dir.identity().clone(),
        name,
    })
}

pub fn observe(query: &LocalConfigQuery, budget: &mut u64) -> Result<Observation> {
    policy::validate_anchor(&query.target)?;
    if let Some(source) = &query.source {
        policy::validate_anchor(source)?;
    }
    let id = policy::config_id(&query.target.requested)?;
    let target = observe_anchor(&id, &query.target, budget)?;
    let source = query
        .source
        .as_ref()
        .map(|anchor| observe_anchor(&id, anchor, budget))
        .transpose()?
        .flatten();
    let mode_matches_source = match (query.authority, &target, query.expected_mode) {
        (ConfigAuthority::ChezmoiCandidate, Some(tgt), Some(mode)) => {
            Some(policy::mode_satisfies(tgt.mode, mode))
        }
        _ => None,
    };
    Ok(Observation::ConfigFile {
        authority: query.authority,
        target,
        source,
        mode_matches_source,
    })
}

fn observe_anchor(
    config_id: &str,
    anchor: &ConfigAnchor,
    budget: &mut u64,
) -> Result<Option<ConfigFileStamp>> {
    let parent_req = anchor.requested.parent().ok_or_else(changed)?;
    if paths::read_path(parent_req)? != anchor.canonical_parent {
        return Err(changed());
    }
    let dir = Directory::open(&anchor.canonical_parent).map_err(|e| {
        if e.code == "path_missing" {
            changed()
        } else {
            e
        }
    })?;
    if dir.path != anchor.canonical_parent || dir.identity() != &anchor.parent_identity {
        return Err(changed());
    }
    let lineage = dir.lineage()?;
    let leaf = OsStr::new(&anchor.name);
    let Some(meta) = dir.metadata(leaf)? else {
        dir.verify()?;
        if dir.lineage()? != lineage || dir.metadata(leaf)?.is_some() {
            return Err(changed());
        }
        return Ok(None);
    };
    if meta.kind != EntryKind::File {
        return Err(Error::attention(
            "unsupported_config_file",
            "配置对象必须是普通文件",
        ));
    }
    if *budget == 0 {
        return Err(Error::attention("config_file_limit", "配置读取预算已用尽"));
    }
    let limit = (*budget).min(FILE_LIMIT);
    let (bytes, verified) = dir.regular_config_snapshot(leaf, limit).inspect_err(|_| {
        *budget -= limit;
    })?;
    *budget -= bytes.len() as u64;
    dir.verify()?;
    if dir.lineage()? != lineage || paths::read_path(parent_req)? != anchor.canonical_parent {
        return Err(changed());
    }
    stamp_from_bytes(
        config_id,
        &anchor.canonical_parent,
        &anchor.name,
        &bytes,
        verified,
        &lineage,
    )
    .map(Some)
}

fn stamp_from_bytes(
    config_id: &str,
    parent: &Path,
    name: &str,
    bytes: &[u8],
    meta: paths::Metadata,
    lineage: &[crate::domain::FileId],
) -> Result<ConfigFileStamp> {
    let raw = format!("{:x}", Sha256::digest(bytes));
    Ok(ConfigFileStamp {
        content_digest: policy::content_digest(config_id, &raw)?,
        fingerprint: policy::stamp_fingerprint(
            parent,
            name,
            &raw,
            &meta.identity,
            meta.mode,
            lineage,
        )?,
        identity: meta.identity,
        mode: meta.mode,
    })
}

pub fn resolve_package(ctx: &Context, key: &str) -> Result<Option<Package>> {
    if !key.starts_with("config:") && !Path::new(key).is_absolute() {
        return Ok(None);
    }
    let mapping = config_inventory::discover(ctx);
    let groups = candidate_groups(&mapping);
    let receipts = crate::target::receipts(ctx)?;
    let target = if key.starts_with("config:") {
        find_target_by_id(ctx, key, &groups, &receipts)?
            .ok_or_else(|| Error::attention("not_found", "没有发现对应配置对象"))?
    } else {
        let clean = crate::policy::paths::lexical(Path::new(key))?;
        paths::read_path(&clean)?;
        clean
    };
    package_for_target(&target, &mapping, &groups, &receipts).map(Some)
}

pub fn refresh_package(ctx: &Context, planned: &Package) -> Result<Package> {
    let Provider::ConfigObserved { target, .. } = &planned.provider else {
        return Err(Error::new("invalid_config_package", &planned.id));
    };
    let mapping = config_inventory::discover(ctx);
    let groups = candidate_groups(&mapping);
    let receipts = crate::target::receipts(ctx)?;
    let refreshed = package_for_target(target, &mapping, &groups, &receipts)
        .map_err(|_| Error::attention("plan_stale", "配置来源归属在计划后发生变化，未执行"))?;
    if crate::domain::fingerprint(&refreshed)? != crate::domain::fingerprint(planned)? {
        return Err(Error::attention(
            "plan_stale",
            "配置来源归属在计划后发生变化，未执行",
        ));
    }
    Ok(refreshed)
}

fn candidate_groups(mapping: &config_inventory::Snapshot) -> BTreeMap<PathBuf, Vec<PathBuf>> {
    let mut groups = BTreeMap::<PathBuf, Vec<PathBuf>>::new();
    for item in &mapping.candidates {
        groups
            .entry(item.target.clone())
            .or_default()
            .push(item.source.path.clone());
    }
    for sources in groups.values_mut() {
        sources.sort();
        sources.dedup();
    }
    groups
}

fn package_for_target(
    target: &Path,
    mapping: &config_inventory::Snapshot,
    groups: &BTreeMap<PathBuf, Vec<PathBuf>>,
    receipts: &Receipts,
) -> Result<Package> {
    let id = policy::config_id(target)?;
    if let Some(sources) = groups.get(target) {
        if sources.len() > 1 {
            return Err(Error::attention(
                "owner_conflict",
                "多个配置源映射到同一目标，需要先审查冲突",
            ));
        }
        if matches!(
            mapping.coverage.state,
            CoverageState::Unavailable | CoverageState::Invalid | CoverageState::Truncated
        ) {
            return Err(Error::attention(
                "config_authority_uncertain",
                "配置来源扫描不完整，无法确认唯一归属",
            ));
        }
        let source = &sources[0];
        let name = source
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::new("invalid_config_package", &id))?;
        policy::source_file_policy(name)?;
        return Ok(config_package(
            id,
            target.to_path_buf(),
            Some(source.clone()),
            ConfigAuthority::ChezmoiCandidate,
        ));
    }
    if !authority_allows_untracked(&mapping.coverage.state, &mapping.coverage.issues) {
        return Err(Error::attention(
            "config_authority_uncertain",
            "配置来源映射存在未求值属性或错误，不能假定目标未纳管",
        ));
    }
    let parent = target
        .parent()
        .ok_or_else(|| Error::new("invalid_config_target", "配置路径缺少父目录"))?;
    let leaf = target
        .file_name()
        .ok_or_else(|| Error::new("invalid_config_target", "配置文件名缺失"))?;
    let exists = match Directory::open(parent) {
        Ok(dir) => {
            let found = dir.metadata(leaf)?.is_some();
            dir.verify()?;
            found
        }
        Err(error) if error.code == "path_missing" => false,
        Err(error) => return Err(error),
    };
    if !exists && !receipts.installations.contains_key(&id) {
        return Err(Error::attention("not_found", "没有发现对应配置对象"));
    }
    Ok(config_package(
        id,
        target.to_path_buf(),
        None,
        ConfigAuthority::Untracked,
    ))
}

fn config_package(
    id: String,
    target: PathBuf,
    source: Option<PathBuf>,
    authority: ConfigAuthority,
) -> Package {
    let official_url = match authority {
        ConfigAuthority::ChezmoiCandidate | ConfigAuthority::ChezmoiNative => {
            "https://www.chezmoi.io/reference/target-types/".into()
        }
        ConfigAuthority::Untracked => String::new(),
    };
    Package {
        id,
        name: target.display().to_string(),
        kind: Kind::Config,
        official_url,
        paths: vec![OwnedPath {
            path: target.clone(),
            kind: PathKind::Config,
        }],
        provider: Provider::ConfigObserved {
            target,
            source,
            authority,
        },
        exact_version_required: false,
    }
}

fn find_target_by_id(
    ctx: &Context,
    key: &str,
    groups: &BTreeMap<PathBuf, Vec<PathBuf>>,
    receipts: &Receipts,
) -> Result<Option<PathBuf>> {
    for target in groups.keys() {
        if policy::config_id(target)? == key {
            return Ok(Some(target.clone()));
        }
    }
    for receipt in receipts.installations.values() {
        if receipt.package.id == key
            && let Provider::ConfigObserved { target, .. } = &receipt.package.provider
        {
            return Ok(Some(target.clone()));
        }
    }
    for (resource, _) in [crate::resource_configs::collect(ctx, &[], None)?] {
        for item in resource {
            if item.id == key
                && let crate::resource::Data::Config { target, .. } = item.data
            {
                return Ok(Some(target));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "../tests/unit/config_local.rs"]
mod tests;
