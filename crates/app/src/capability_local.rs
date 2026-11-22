use crate::{
    config_local::capture_anchor,
    context::Context,
    domain::{
        CapabilityKind, CapabilityQueryKind, ConfigAnchor, Kind, LocalCapabilityQuery, Observation,
        OwnedPath, Package, PathKind, Provider,
    },
    error::{Error, Result},
    paths::{self, Directory, EntryKind},
    policy::capability_local::{
        self as policy, FILE_LIMIT, PAYLOAD_BYTE_BUDGET, PAYLOAD_DEPTH_LIMIT, PAYLOAD_ENTRY_LIMIT,
        StampInput,
    },
    resource::{Coverage, CoverageState, Data, Management, Resource, State},
};
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, os::unix::ffi::OsStrExt, path::Path};

fn changed() -> Error {
    Error::attention(
        "capability_namespace_changed",
        "能力包目录身份在读取期间改变",
    )
}

fn invalid_pkg() -> Error {
    Error::new("invalid_capability_package", "能力包配方结构不完整")
}

pub fn canonical_identity(package: &Package) -> Result<String> {
    policy::validate_package(package)?;
    let Provider::CapabilityObserved {
        capability,
        native_id,
        payload,
        record,
        profile_id,
        ..
    } = &package.provider
    else {
        return Err(invalid_pkg());
    };
    match capability {
        CapabilityKind::SkillsCli => {
            crate::policy::native_skills::installation_id(&paths::read_path(payload)?)
        }
        CapabilityKind::ClaudePlugin => {
            let prof = profile_id.as_deref().ok_or_else(invalid_pkg)?;
            Ok(policy::plugin_capability_id(prof, native_id))
        }
        CapabilityKind::CodexPlugin => {
            let root = record.parent().ok_or_else(invalid_pkg)?;
            codex_owner_id(&paths::read_path(root)?, native_id)
        }
    }
}

pub fn query_identity(package: &Package, local: &LocalCapabilityQuery) -> Result<String> {
    match local.capability {
        CapabilityKind::SkillsCli => crate::policy::native_skills::installation_id(
            &local.payload.canonical_parent.join(&local.payload.name),
        ),
        CapabilityKind::ClaudePlugin => canonical_identity(package),
        CapabilityKind::CodexPlugin => {
            codex_owner_id(&local.record.canonical_parent, &local.native_id)
        }
    }
}

fn codex_owner_id(root: &Path, native_id: &str) -> Result<String> {
    let profile = crate::resource::id("agent", &("codex", root))?;
    policy::codex_capability_id(&profile, native_id)
}

fn verify_codex_query(query: &LocalCapabilityQuery) -> Result<()> {
    if query.capability != CapabilityKind::CodexPlugin {
        return Ok(());
    }
    let (plugin, market) = query
        .native_id
        .rsplit_once('@')
        .filter(|_| policy::valid_plugin_id(&query.native_id))
        .ok_or_else(invalid_pkg)?;
    let native_cache = query
        .record
        .canonical_parent
        .join("plugins/cache")
        .join(market)
        .join(plugin);
    if query.payload.canonical_parent != native_cache {
        return Err(Error::attention(
            "codex_plugin_cache_rebound",
            "插件缓存物理目录与目标档案或插件不一致",
        ));
    }
    Ok(())
}

pub fn capture(package: &Package) -> Result<LocalCapabilityQuery> {
    policy::validate_package(package)?;
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
        return Err(invalid_pkg());
    };
    let query = LocalCapabilityQuery {
        kind: CapabilityQueryKind::LocalV1,
        capability: *capability,
        native_id: native_id.clone(),
        manager: manager.clone(),
        source: source.clone(),
        record: capture_anchor(record)?,
        payload: capture_anchor(payload)?,
        settings: settings.as_deref().map(capture_anchor).transpose()?,
    };
    verify_codex_query(&query)?;
    Ok(query)
}

pub fn observe(id: &str, query: &LocalCapabilityQuery, budget: &mut u64) -> Result<Observation> {
    verify_codex_query(query)?;
    crate::policy::config_local::validate_anchor(&query.record)?;
    crate::policy::config_local::validate_anchor(&query.payload)?;
    if let Some(settings) = &query.settings {
        crate::policy::config_local::validate_anchor(settings)?;
    }
    let Some(record_bytes) = read_anchored_file(&query.record, budget)? else {
        return Ok(Observation::CapabilityPack {
            capability: query.capability,
            pack: None,
        });
    };
    let parsed_record = match query.capability {
        CapabilityKind::SkillsCli => {
            let source_type = query
                .manager
                .strip_prefix("skills-cli:")
                .ok_or_else(invalid_pkg)?;
            let expected_source = query.source.as_deref().ok_or_else(invalid_pkg)?;
            policy::parse_skill_record(
                id,
                &record_bytes,
                &query.native_id,
                source_type,
                expected_source,
            )?
            .map(|(digest, revision)| policy::ParsedRecord {
                digest,
                revision,
                configured_enabled: None,
            })
        }
        CapabilityKind::ClaudePlugin => {
            let settings_bytes = match &query.settings {
                Some(anchor) => read_anchored_file(anchor, budget)?,
                None => None,
            };
            policy::parse_plugin_record(
                id,
                &record_bytes,
                &query.native_id,
                &query.payload.requested,
                settings_bytes.as_deref(),
            )?
        }
        CapabilityKind::CodexPlugin => policy::parse_codex_record(
            id,
            &query.record.requested,
            &query.payload.requested,
            &query.native_id,
            &record_bytes,
        )?,
    };
    let Some(parsed) = parsed_record else {
        return Ok(Observation::CapabilityPack {
            capability: query.capability,
            pack: None,
        });
    };
    let Some((payload_digest, payload_id, file_count, lineage)) =
        observe_payload(id, query.capability, &query.payload, budget)?
    else {
        return Ok(Observation::CapabilityPack {
            capability: query.capability,
            pack: None,
        });
    };
    let stamp = policy::build_stamp(StampInput {
        id,
        record_digest: parsed.digest,
        payload_digest,
        payload_identity: payload_id,
        file_count,
        revision: parsed.revision,
        configured_enabled: parsed.configured_enabled,
        lineage: &lineage,
    })?;
    Ok(Observation::CapabilityPack {
        capability: query.capability,
        pack: Some(stamp),
    })
}

fn open_verified_parent(anchor: &ConfigAnchor) -> Result<(Directory, Vec<crate::domain::FileId>)> {
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
    Ok((dir, lineage))
}

fn budget_err(msg: &'static str) -> Error {
    Error::attention("capability_budget_limit", msg)
}

fn read_anchored_file(anchor: &ConfigAnchor, budget: &mut u64) -> Result<Option<Vec<u8>>> {
    let (dir, lineage) = open_verified_parent(anchor)?;
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
            "unsupported_capability_record",
            "能力包账本或设置必须是普通文件",
        ));
    }
    if *budget == 0 {
        return Err(budget_err("能力包读取预算已用尽"));
    }
    let limit = (*budget).min(FILE_LIMIT);
    let (bytes, _) = match dir.regular_config_snapshot(leaf, limit) {
        Ok(pair) => pair,
        Err(e) => {
            *budget -= limit;
            return Err(if e.code == "config_file_limit" {
                budget_err("能力包记录超出读取上限")
            } else {
                e
            });
        }
    };
    *budget -= bytes.len() as u64;
    dir.verify()?;
    if dir.lineage()? != lineage {
        return Err(changed());
    }
    Ok(Some(bytes))
}

type PayloadObserved = (
    String,
    crate::domain::FileId,
    u64,
    Vec<crate::domain::FileId>,
);

fn observe_payload(
    id: &str,
    capability: CapabilityKind,
    anchor: &ConfigAnchor,
    budget: &mut u64,
) -> Result<Option<PayloadObserved>> {
    let Some(snapshot) = snapshot_payload(capability, anchor, budget)? else {
        return Ok(None);
    };
    let digest = policy::payload_tree_digest(id, &snapshot.entries)?;
    Ok(Some((
        digest,
        snapshot.identity,
        snapshot.entries.len() as u64,
        snapshot.lineage,
    )))
}

pub struct PayloadSnapshot {
    pub entries: Vec<(String, bool, String)>,
    pub identity: crate::domain::FileId,
    pub lineage: Vec<crate::domain::FileId>,
}

pub(crate) fn codex_payload_files(root: &Directory) -> Result<Vec<(String, bool, String)>> {
    let mut entries = Vec::new();
    let mut entry_limit = PAYLOAD_ENTRY_LIMIT;
    let mut byte_limit = PAYLOAD_BYTE_BUDGET;
    walk_payload(
        root,
        "",
        0,
        &mut entry_limit,
        &mut byte_limit,
        &mut entries,
        true,
    )?;
    root.verify()?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

pub fn snapshot_payload(
    capability: CapabilityKind,
    anchor: &ConfigAnchor,
    budget: &mut u64,
) -> Result<Option<PayloadSnapshot>> {
    let (parent, parent_lineage) = open_verified_parent(anchor)?;
    let leaf = OsStr::new(&anchor.name);
    let Some(meta) = parent.metadata(leaf)? else {
        parent.verify()?;
        if parent.lineage()? != parent_lineage || parent.metadata(leaf)?.is_some() {
            return Err(changed());
        }
        return Ok(None);
    };
    if meta.kind != EntryKind::Directory {
        return Err(Error::attention(
            "unsupported_capability_payload",
            "能力包物理落点必须是真实目录",
        ));
    }
    let root = parent.child(leaf)?;
    let codex = capability == CapabilityKind::CodexPlugin;
    if codex {
        verify_codex_cache(&parent, leaf)?;
    }
    parent.verify_child(leaf, &root)?;
    let lineage = root.lineage()?;
    let mut entries = Vec::new();
    let mut remaining_entries = PAYLOAD_ENTRY_LIMIT;
    walk_payload(
        &root,
        "",
        0,
        &mut remaining_entries,
        budget,
        &mut entries,
        codex,
    )?;
    if codex {
        verify_codex_manifest(&root, &entries, budget)?;
    }
    parent.verify_child(leaf, &root)?;
    parent.verify()?;
    if root.lineage()? != lineage {
        return Err(changed());
    }
    if capability == CapabilityKind::SkillsCli
        && !entries.iter().any(|(rel, _, _)| rel == "SKILL.md")
    {
        return Err(Error::attention(
            "skill_entrypoint_missing",
            "技能目录缺少普通 SKILL.md",
        ));
    }
    if codex {
        verify_codex_cache(&parent, leaf)?;
    }
    if entries.is_empty() {
        return Err(Error::attention(
            "capability_payload_empty",
            "能力包目录没有可验证的普通文件载荷",
        ));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Some(PayloadSnapshot {
        entries,
        identity: root.identity().clone(),
        lineage,
    }))
}

fn verify_codex_manifest(
    root: &Directory,
    entries: &[(String, bool, String)],
    budget: &mut u64,
) -> Result<()> {
    const AGENT_SCHEMA: &str = "https://agent-plugins.org/schemas/";
    let invalid = || {
        Error::attention(
            "codex_plugin_manifest_mismatch",
            "Codex 清单与插件名称或缓存版本不匹配",
        )
    };
    let mut read = |rel: &str| -> Result<Option<serde_json::Value>> {
        let Some((_, _, expected)) = entries.iter().find(|(path, _, _)| path == rel) else {
            return Ok(None);
        };
        let leaf = OsStr::new("plugin.json");
        let limit = (*budget).min(64 * 1024);
        let (bytes, _) = if rel == "plugin.json" {
            root.regular_config_snapshot(leaf, limit)?
        } else {
            let directory = root.child(OsStr::new(".codex-plugin"))?;
            let observed = directory.regular_config_snapshot(leaf, limit)?;
            root.verify_child(OsStr::new(".codex-plugin"), &directory)?;
            observed
        };
        *budget -= bytes.len() as u64;
        if &format!("{:x}", Sha256::digest(&bytes)) != expected {
            return Err(changed());
        }
        crate::policy::json::parse(&bytes)
            .map(Some)
            .map_err(|_| invalid())
    };
    let portable = read("plugin.json")?;
    let schema = portable
        .as_ref()
        .and_then(|value| value["$schema"].as_str());
    let manifest = match schema {
        Some("https://agent-plugins.org/schemas/1.0.0/plugin.schema.json") => portable,
        Some(schema) if schema.starts_with(AGENT_SCHEMA) => return Err(invalid()),
        _ => read(".codex-plugin/plugin.json")?,
    };
    let manifest = manifest.ok_or_else(|| {
        Error::attention(
            "codex_plugin_manifest_missing",
            "插件缓存缺少普通 Codex 清单",
        )
    })?;
    let name = root
        .path
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .ok_or_else(invalid)?;
    let version = root
        .path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(invalid)?;
    if manifest["name"].as_str() != Some(name) || manifest["version"].as_str() != Some(version) {
        return Err(invalid());
    }
    Ok(())
}

fn verify_codex_cache(parent: &Directory, leaf: &OsStr) -> Result<()> {
    for (index, item) in parent.names()?.enumerate() {
        if index >= PAYLOAD_ENTRY_LIMIT {
            return Err(budget_err("Codex 版本缓存条目超出读取上限"));
        }
        let item = item.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let bytes = item.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") || bytes == leaf.as_bytes() {
            continue;
        }
        let name = OsStr::from_bytes(bytes);
        let meta = parent.metadata(name)?.ok_or_else(changed)?;
        if matches!(meta.kind, EntryKind::Directory | EntryKind::Symlink) {
            return Err(Error::attention(
                "codex_plugin_cache_unproven",
                "无法确定唯一 Codex 插件版本缓存",
            ));
        }
    }
    parent.verify()
}

fn walk_payload(
    dir: &Directory,
    prefix: &str,
    depth: usize,
    remaining_entries: &mut usize,
    budget: &mut u64,
    out: &mut Vec<(String, bool, String)>,
    exclude_git: bool,
) -> Result<()> {
    if depth >= PAYLOAD_DEPTH_LIMIT {
        return Err(budget_err("能力包目录层级超出读取上限"));
    }
    for item in dir.names()? {
        let item = item.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let raw = item.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        if *remaining_entries == 0 {
            return Err(budget_err("能力包目录条目超出读取上限"));
        }
        *remaining_entries -= 1;
        let Ok(name) = std::str::from_utf8(raw) else {
            return Err(Error::attention(
                "unsupported_capability_payload",
                "能力包条目包含非 UTF-8 名称",
            ));
        };
        let meta = dir.metadata(OsStr::new(name))?.ok_or_else(changed)?;
        if exclude_git && depth == 0 && name == ".git" {
            if meta.kind != EntryKind::Directory {
                return Err(Error::attention(
                    "unsupported_capability_payload",
                    "Codex Git 元数据必须在实体目录中",
                ));
            }
            continue;
        }
        match policy::classify_payload_entry(name, depth, meta.kind == EntryKind::File) {
            policy::PayloadMetadataDisposition::IgnoreRegularMarker => continue,
            policy::PayloadMetadataDisposition::RejectReservedEntry => {
                return Err(Error::attention(
                    "unsupported_capability_payload",
                    "能力包租约或系统标记必须是允许位置的普通文件",
                ));
            }
            policy::PayloadMetadataDisposition::Include => (),
        }
        let rel = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        match meta.kind {
            EntryKind::Directory => {
                let child = dir.child(OsStr::new(name))?;
                walk_payload(
                    &child,
                    &rel,
                    depth + 1,
                    remaining_entries,
                    budget,
                    out,
                    exclude_git,
                )?;
                dir.verify_child(OsStr::new(name), &child)?;
            }
            EntryKind::File => {
                let limit = (*budget).min(FILE_LIMIT);
                let (bytes, verified) = match dir.regular_config_snapshot(OsStr::new(name), limit) {
                    Ok(pair) => pair,
                    Err(e) => {
                        *budget -= limit;
                        return Err(if e.code == "config_file_limit" {
                            budget_err("能力包单文件超出上限")
                        } else {
                            e
                        });
                    }
                };
                *budget -= bytes.len() as u64;
                out.push((
                    rel,
                    (verified.mode & 0o111) != 0,
                    format!("{:x}", Sha256::digest(&bytes)),
                ));
            }
            EntryKind::Symlink | EntryKind::Other => {
                return Err(Error::attention(
                    "unsupported_capability_payload",
                    "能力包载荷包含未授权链接或特殊文件",
                ));
            }
        }
    }
    Ok(())
}

pub fn resolve_package(
    ctx: &Context,
    key: &str,
    has_legacy_skill_decl: bool,
    exact: bool,
) -> Result<Option<Package>> {
    if !key.starts_with("capability:") && !key.starts_with("skill:") && !key.starts_with("plugin:")
    {
        return Ok(None);
    }
    let profiles = crate::resource_agents::profile_roots(ctx)?;
    let (raw_map, coverages) = crate::resource_capabilities::collect_raw(ctx, &profiles, None)?;
    let list: Vec<Resource> = raw_map.into_values().collect();
    let Some(resource) = select_capability(&list, key, exact)? else {
        let receipts = crate::target::receipts(ctx)?;
        if let Some(receipt) = receipts.installations.get(key)
            && matches!(
                receipt.package.provider,
                Provider::CapabilityObserved { .. }
            )
        {
            return Ok(Some(receipt.package.clone()));
        }
        return Ok(None);
    };
    if key.starts_with("skill:") && has_legacy_skill_decl {
        return Err(Error::attention(
            "ambiguous",
            format!(
                "该技能同时存在旧受管声明与原生记录，请使用明确 ID：{}",
                resource.id
            ),
        ));
    }
    package_from_resource(resource, &profiles, &coverages).map(Some)
}

pub(crate) fn registered_packages(ctx: &Context) -> Result<(Vec<Package>, Vec<String>)> {
    let profiles = crate::resource_agents::profile_roots(ctx)?;
    let (resources, coverage) = crate::resource_capabilities::collect_raw(ctx, &profiles, None)?;
    let mut verified = Vec::new();
    let mut skipped = Vec::new();
    for resource in resources.into_values() {
        if !matches!(
            resource.management,
            Management::Native | Management::Declared
        ) {
            continue;
        }
        match package_from_resource(&resource, &profiles, &coverage) {
            Ok(package) => verified.push(package),
            Err(_) => skipped.push(resource.id),
        }
    }
    Ok((verified, skipped))
}

pub fn refresh_package(ctx: &Context, planned: &Package) -> Result<Package> {
    let profiles = crate::resource_agents::profile_roots(ctx)?;
    let (raw_map, coverages) = crate::resource_capabilities::collect_raw(ctx, &profiles, None)?;
    let Some(resource) = raw_map.get(&planned.id) else {
        return Err(Error::attention(
            "plan_stale",
            "能力包声明在计划后消失，未执行",
        ));
    };
    let Ok(refreshed) = package_from_resource(resource, &profiles, &coverages) else {
        return Err(Error::attention(
            "plan_stale",
            "能力包来源状态在计划后发生变化，未执行",
        ));
    };
    if crate::domain::fingerprint(&refreshed)? != crate::domain::fingerprint(planned)? {
        return Err(Error::attention(
            "plan_stale",
            "能力包来源状态在计划后发生变化，未执行",
        ));
    }
    Ok(refreshed)
}

fn select_capability<'a>(
    items: &'a [Resource],
    key: &str,
    exact: bool,
) -> Result<Option<&'a Resource>> {
    if let Some(exact) = items.iter().find(|r| r.id == key) {
        return Ok(Some(exact));
    }
    if exact {
        return Ok(None);
    }
    let matches: Vec<_> = items
        .iter()
        .filter(|r| r.name == key || r.aliases.iter().any(|a| a == key))
        .collect();
    match matches.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        _ => Err(Error::attention(
            "ambiguous",
            format!(
                "请选择明确能力包实例：{}",
                matches
                    .iter()
                    .map(|r| r.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}

fn package_from_resource(
    resource: &Resource,
    profiles: &[Resource],
    coverages: &[Coverage],
) -> Result<Package> {
    let Data::Capability {
        origin,
        contents,
        deployments,
        ..
    } = &resource.data
    else {
        return Err(invalid_pkg());
    };
    if resource.management == Management::Untracked || origin.manager == "filesystem" {
        return Err(Error::attention(
            "unsupported_capability_source",
            "未受原生 lock 登记的技能缺少来源账本，保留 untracked 待审查",
        ));
    }
    if let Some(source_type) = origin.manager.strip_prefix("skills-cli:") {
        return skill_package(resource, source_type, origin, contents, coverages);
    }
    if origin.manager == "claude-plugin" {
        return plugin_package(resource, origin, deployments, profiles, coverages);
    }
    if origin.manager == "codex-config" {
        return codex_package(resource, origin, deployments, contents, coverages);
    }
    Err(Error::new(
        "unsupported_capability_source",
        "该能力包渠道尚未开放基线采纳，保持只读观察",
    ))
}

fn codex_package(
    resource: &Resource,
    origin: &crate::resource::Origin,
    deployments: &[crate::resource::Deployment],
    contents: &[crate::resource::Content],
    coverages: &[Coverage],
) -> Result<Package> {
    let [deployment] = deployments else {
        return Err(invalid_pkg());
    };
    require_clean_coverage(coverages, &format!("codex-config:{}", deployment.harness))?;
    let [content] = contents else {
        return Err(Error::attention(
            "codex_plugin_cache_unproven",
            "只有一个可核验的版本缓存时才能采纳",
        ));
    };
    let pkg = Package {
        id: resource.id.clone(),
        name: resource.name.clone(),
        kind: Kind::Plugin,
        official_url: "https://developers.openai.com/codex/plugins".into(),
        paths: vec![OwnedPath {
            path: content.path.clone(),
            kind: PathKind::Payload,
        }],
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::CodexPlugin,
            native_id: origin.native_key.clone(),
            manager: origin.manager.clone(),
            record: origin.record.clone(),
            payload: content.path.clone(),
            source: None,
            profile_id: Some(deployment.harness.clone()),
            settings: None,
        },
        exact_version_required: false,
    };
    policy::validate_package(&pkg)?;
    Ok(pkg)
}

fn skill_package(
    resource: &Resource,
    source_type: &str,
    origin: &crate::resource::Origin,
    contents: &[crate::resource::Content],
    coverages: &[Coverage],
) -> Result<Package> {
    require_clean_coverage(
        coverages,
        &format!("skills-lock:{}", origin.record.display()),
    )?;
    let Some(first_content) = contents.first() else {
        return Err(Error::attention("not_installed", &resource.id));
    };
    let payload = first_content.path.clone();
    let raw_source = read_skill_raw_source(&origin.record, &origin.native_key)?;
    let pkg = Package {
        id: resource.id.clone(),
        name: resource.name.clone(),
        kind: Kind::Skill,
        official_url: "https://github.com/vercel-labs/skills".into(),
        paths: vec![OwnedPath {
            path: payload.clone(),
            kind: PathKind::Payload,
        }],
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: origin.native_key.clone(),
            manager: format!("skills-cli:{source_type}"),
            record: origin.record.clone(),
            payload,
            source: Some(raw_source),
            profile_id: None,
            settings: None,
        },
        exact_version_required: false,
    };
    policy::validate_package(&pkg)?;
    Ok(pkg)
}

fn plugin_package(
    resource: &Resource,
    origin: &crate::resource::Origin,
    deployments: &[crate::resource::Deployment],
    profiles: &[Resource],
    coverages: &[Coverage],
) -> Result<Package> {
    let Some(deployment) = deployments.first().filter(|d| d.scope == "user") else {
        return Err(Error::new(
            "unsupported_capability_source",
            "仅支持 user 作用域 Claude 插件基线采纳",
        ));
    };
    require_clean_coverage(coverages, &format!("claude-plugins:{}", deployment.harness))?;
    if resource.status == State::Unknown {
        return Err(Error::attention(
            "observation_required",
            "插件登记存在重复或状态未知，需先审查",
        ));
    }
    let mut config_dir = None;
    for p in profiles {
        if p.id == deployment.harness
            && let Data::Agent {
                config_dir: dir, ..
            } = &p.data
        {
            config_dir = Some(dir.clone());
            break;
        }
    }
    let config_dir = config_dir.ok_or_else(invalid_pkg)?;
    let pkg = Package {
        id: resource.id.clone(),
        name: resource.name.clone(),
        kind: Kind::Plugin,
        official_url: "https://code.claude.com/docs/en/plugins/cli-reference".into(),
        paths: vec![OwnedPath {
            path: deployment.path.clone(),
            kind: PathKind::Payload,
        }],
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::ClaudePlugin,
            native_id: origin.native_key.clone(),
            manager: "claude-plugin".into(),
            record: origin.record.clone(),
            payload: deployment.path.clone(),
            source: None,
            profile_id: Some(deployment.harness.clone()),
            settings: Some(config_dir.join("settings.json")),
        },
        exact_version_required: false,
    };
    policy::validate_package(&pkg)?;
    Ok(pkg)
}

fn require_clean_coverage(coverages: &[Coverage], source: &str) -> Result<()> {
    for c in coverages {
        if c.source == source
            && matches!(
                c.state,
                CoverageState::Invalid | CoverageState::Unavailable | CoverageState::Truncated
            )
        {
            return Err(Error::attention(
                "capability_authority_uncertain",
                "能力包来源账本或启用设置存在损坏，无法确认基线",
            ));
        }
    }
    Ok(())
}

fn read_skill_raw_source(lock_path: &Path, name: &str) -> Result<String> {
    let anchor = capture_anchor(lock_path)?;
    let mut budget = FILE_LIMIT;
    let Some(bytes) = read_anchored_file(&anchor, &mut budget)? else {
        return Err(Error::attention("not_installed", name));
    };
    let lock = crate::policy::resources::skill_lock(&bytes, false)?;
    let Some(rec) = lock.skills.get(name) else {
        return Err(Error::attention("not_installed", name));
    };
    Ok(rec.source.clone())
}

pub fn observe_default(id: &str, query: &LocalCapabilityQuery) -> Result<Observation> {
    let mut budget = PAYLOAD_BYTE_BUDGET;
    observe(id, query, &mut budget)
}

#[cfg(test)]
#[path = "../tests/unit/capability_local.rs"]
mod tests;

pub fn project_plugin_settings(
    id: &str,
    query: &LocalCapabilityQuery,
    after: &[u8],
) -> Result<(
    crate::policy::capability_local::ParsedRecord,
    crate::policy::capability_local::ParsedRecord,
)> {
    if query.capability != CapabilityKind::ClaudePlugin {
        return Err(invalid_pkg());
    }
    let mut budget = 32 * 1024 * 1024;
    let record = read_anchored_file(&query.record, &mut budget)?.ok_or_else(invalid_pkg)?;
    let settings = read_anchored_file(
        query.settings.as_ref().ok_or_else(invalid_pkg)?,
        &mut budget,
    )?;
    let before = policy::parse_plugin_record(
        id,
        &record,
        &query.native_id,
        &query.payload.requested,
        settings.as_deref(),
    )?
    .ok_or_else(invalid_pkg)?;
    let after = policy::parse_plugin_record(
        id,
        &record,
        &query.native_id,
        &query.payload.requested,
        Some(after),
    )?
    .ok_or_else(invalid_pkg)?;
    Ok((before, after))
}
