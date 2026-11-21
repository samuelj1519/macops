use crate::{
    context::Context,
    error::{Error, Result},
    paths,
    policy::resources as policy,
    resource::{
        self, Content, Coverage, CoverageState, Data, Deployment, Management, Origin, Resource,
        Revision, State,
    },
};
use std::{collections::BTreeMap, path::Path};

pub fn collect(
    ctx: &Context,
    profiles: &[Resource],
    project: Option<&Path>,
) -> Result<(Vec<Resource>, Vec<Coverage>)> {
    let (mut resources, mut reports) = collect_raw(ctx, profiles, project)?;
    apply_receipts(ctx, &mut resources, &mut reports)?;
    Ok((resources.into_values().collect(), reports))
}

pub(crate) fn collect_raw(
    ctx: &Context,
    profiles: &[Resource],
    project: Option<&Path>,
) -> Result<(BTreeMap<String, Resource>, Vec<Coverage>)> {
    let mut resources = BTreeMap::new();
    let mut reports = vec![];
    let global = ctx
        .scope
        .paths
        .get("XDG_STATE_HOME")
        .and_then(Option::as_ref)
        .map_or_else(
            || ctx.home.join(".agents/.skill-lock.json"),
            |p| p.join("skills/.skill-lock.json"),
        );
    reports.push(lock(ctx, &global, None, &mut resources)?);
    if let Some(project) = project {
        reports.push(lock(
            ctx,
            &project.join("skills-lock.json"),
            Some(project),
            &mut resources,
        )?);
    }
    let mut fs = Coverage {
        source: "skill-deployments".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    };
    skill_dir(
        &ctx.home.join(".agents/skills"),
        None,
        "canonical",
        None,
        &mut resources,
        &mut fs,
    )?;
    for profile in profiles {
        profile_capabilities(ctx, profile, project, &mut resources, &mut fs, &mut reports)?;
    }
    reports.push(fs);
    Ok((resources, reports))
}

fn apply_receipts(
    ctx: &Context,
    resources: &mut BTreeMap<String, Resource>,
    reports: &mut [Coverage],
) -> Result<()> {
    let receipts = crate::target::receipts(ctx)?;
    for (id, receipt) in &receipts.installations {
        let crate::domain::Provider::CapabilityObserved {
            capability,
            native_id,
            manager,
            record,
            payload,
            ..
        } = &receipt.package.provider
        else {
            continue;
        };
        let logical = if *capability == crate::domain::CapabilityKind::SkillsCli {
            crate::capability_local::canonical_identity(&receipt.package)?
        } else {
            id.clone()
        };
        let current = crate::inventory::observe(ctx, &receipt.package);
        let base = crate::policy::targets::baseline(&receipt.observation, current);
        if base.comparison == crate::resource::Comparison::Unknown
            && let Some(fs) = reports.iter_mut().find(|r| r.source == "skill-deployments")
        {
            crate::policy::chezmoi::issue(
                fs,
                CoverageState::Invalid,
                "adopted_capability_unobserved".into(),
            );
        }
        if let Some(item) = resources.get_mut(&logical) {
            if logical != *id {
                item.aliases.push(id.clone());
            }
            item.management = Management::Adopted;
            if base.comparison == crate::resource::Comparison::Unknown {
                item.status = State::Unknown;
            }
            if let Data::Capability { baseline, .. } = &mut item.data {
                *baseline = Some(base);
            }
            continue;
        }
        let status = if base.comparison == crate::resource::Comparison::Unknown {
            State::Unknown
        } else {
            State::Missing
        };
        let delivery_kind = match capability {
            crate::domain::CapabilityKind::SkillsCli => "skill",
            crate::domain::CapabilityKind::ClaudePlugin => "plugin",
        };
        let alias_prefix = delivery_kind;
        resources.insert(
            logical.clone(),
            Resource {
                id: logical,
                name: native_id.clone(),
                aliases: vec![format!("{alias_prefix}:{native_id}")],
                management: Management::Adopted,
                status,
                data: Data::Capability {
                    delivery_kind: delivery_kind.into(),
                    native_id: native_id.clone(),
                    origin: Origin {
                        manager: manager.clone(),
                        label: None,
                        record: record.clone(),
                        native_key: native_id.clone(),
                    },
                    revision: Revision::default(),
                    contents: vec![Content {
                        id: resource::id("content", payload)?,
                        path: payload.clone(),
                        realpath: None,
                        status: State::Missing,
                    }],
                    deployments: vec![],
                    configured_enabled: None,
                    loaded: None,
                    baseline: Some(base),
                },
            },
        );
    }
    Ok(())
}

fn lock(
    ctx: &Context,
    path: &Path,
    project: Option<&Path>,
    resources: &mut BTreeMap<String, Resource>,
) -> Result<Coverage> {
    let mut report = Coverage {
        source: format!("skills-lock:{}", path.display()),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![path.to_path_buf()],
        issues: vec![],
    };
    if !paths::read_path(path)?.exists() {
        report.state = CoverageState::Absent;
        return Ok(report);
    }
    let record = match paths::regular_bytes(path)
        .and_then(|bytes| policy::skill_lock(&bytes, project.is_some()))
    {
        Ok(record) => record,
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
            return Ok(report);
        }
    };
    let canonical = project.unwrap_or(&ctx.home).join(".agents/skills");
    for (name, record) in record.skills {
        let target = canonical.join(&name);
        let content = skill_content(&target)?;
        let status =
            if record.install_mode.as_deref() == Some("copy") && content.status == State::Missing {
                State::Unknown
            } else {
                content.status.clone()
            };
        let legacy_key = resource::id(
            "capability",
            &(
                "skills-cli",
                path,
                &name,
                &record.source_type,
                &record.source,
            ),
        )?;
        let key = if project.is_none() {
            crate::policy::native_skills::installation_id(&paths::read_path(&target)?)?
        } else {
            legacy_key.clone()
        };
        resources.insert(
            key.clone(),
            Resource {
                id: key,
                name: name.clone(),
                aliases: vec![format!("skill:{name}"), legacy_key],
                management: Management::Native,
                status,
                data: Data::Capability {
                    delivery_kind: "skill".into(),
                    native_id: name.clone(),
                    origin: Origin {
                        manager: format!("skills-cli:{}", record.source_type),
                        label: Some(policy::source_label(
                            record.source_url.as_deref().unwrap_or(&record.source),
                        )),
                        record: path.to_path_buf(),
                        native_key: name,
                    },
                    revision: Revision {
                        source: record.revision,
                        ..Revision::default()
                    },
                    contents: vec![content],
                    deployments: vec![],
                    configured_enabled: None,
                    loaded: None,
                    baseline: None,
                },
            },
        );
        report.count += 1;
    }
    Ok(report)
}

fn content(path: &Path) -> Result<Content> {
    let real = paths::read_path(path)?;
    let status = if real.exists() {
        State::Present
    } else {
        State::Missing
    };
    Ok(Content {
        id: resource::id("content", &real)?,
        path: path.to_path_buf(),
        realpath: Some(real),
        status,
    })
}

fn skill_content(path: &Path) -> Result<Content> {
    let mut content = content(path)?;
    if content.status == State::Present {
        let marker = paths::read_path(&path.join("SKILL.md"))?;
        content.status = match std::fs::metadata(marker) {
            Ok(metadata) if metadata.is_file() => State::Present,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => State::Missing,
            _ => State::Unknown,
        };
    }
    Ok(content)
}

fn skill_dir(
    root: &Path,
    harness: Option<&str>,
    scope: &str,
    project: Option<&Path>,
    resources: &mut BTreeMap<String, Resource>,
    report: &mut Coverage,
) -> Result<()> {
    let logical_root = root;
    let root = match paths::read_path(root) {
        Ok(p) => p,
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
            return Ok(());
        }
    };
    if !root.exists() {
        return Ok(());
    }
    report.roots.push(root.clone());
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            report.state = CoverageState::Invalid;
            report.issues.push("non_utf8_skill_name".into());
            continue;
        };
        if name.starts_with('.') || !policy::component(&name) {
            continue;
        }
        let logical = logical_root.join(&name);
        let marker = logical.join("SKILL.md");
        let safe = match paths::read_path(&marker) {
            Ok(p) => p,
            Err(error) => {
                report.state = CoverageState::Invalid;
                report.issues.push(error.code);
                continue;
            }
        };
        if !safe.is_file() {
            continue;
        }
        let contents = content(&logical)?;
        let existing = resources
            .values()
            .find(|r| {
                policy::skill_target_matches(
                    r,
                    &name,
                    &logical,
                    contents.realpath.as_deref(),
                    scope,
                    project,
                )
            })
            .map(|r| r.id.clone());
        let key = existing.unwrap_or(resource::id("capability", &("untracked-skill", &logical))?);
        let record = resources.entry(key.clone()).or_insert_with(|| Resource {
            id: key,
            name: name.clone(),
            aliases: vec![format!("skill:{name}")],
            management: Management::Untracked,
            status: State::Present,
            data: Data::Capability {
                delivery_kind: "skill".into(),
                native_id: name.clone(),
                origin: Origin {
                    manager: "filesystem".into(),
                    label: None,
                    record: logical_root.to_path_buf(),
                    native_key: name,
                },
                revision: Revision::default(),
                contents: vec![contents],
                deployments: vec![],
                configured_enabled: None,
                loaded: None,
                baseline: None,
            },
        });
        if let (Some(harness), Data::Capability { deployments, .. }) = (harness, &mut record.data) {
            let deployment = Deployment {
                harness: harness.into(),
                scope: scope.into(),
                project: project.map(Path::to_path_buf),
                path: logical,
            };
            if !deployments
                .iter()
                .any(|d| d.harness == harness && d.path == deployment.path && d.scope == scope)
            {
                deployments.push(deployment);
            }
        }
        report.count += 1;
    }
    Ok(())
}

fn plugins(
    profile: &Resource,
    root: &Path,
    project: Option<&Path>,
    resources: &mut BTreeMap<String, Resource>,
) -> Result<Coverage> {
    let path = root.join("plugins/installed_plugins.json");
    let mut report = Coverage {
        source: format!("claude-plugins:{}", profile.id),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![path.clone()],
        issues: vec![],
    };
    if !paths::read_path(&path)?.exists() {
        report.state = CoverageState::Absent;
        return Ok(report);
    }
    let book = match paths::regular_bytes(&path).and_then(|b| policy::plugin_book(&b)) {
        Ok(book) => book,
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
            return Ok(report);
        }
    };
    for (name, records) in book.plugins {
        for record in records {
            if record.scope != "user" && record.project.as_deref() != project {
                continue;
            }
            let key = resource::id(
                "capability",
                &(
                    "claude-plugin",
                    &profile.id,
                    &name,
                    &record.scope,
                    &record.project,
                ),
            )?;
            let enabled = policy::plugin_settings_path(root, &record)
                .map(|path| plugin_flags(&path, &mut report))
                .transpose()?
                .unwrap_or_default();
            let contents = content(&record.path)?;
            let duplicate = policy::insert_capability(
                resources,
                Resource {
                    id: key,
                    name: name.clone(),
                    aliases: vec![format!("plugin:{name}")],
                    management: Management::Native,
                    status: contents.status.clone(),
                    data: Data::Capability {
                        delivery_kind: "plugin".into(),
                        native_id: name.clone(),
                        origin: Origin {
                            manager: "claude-plugin".into(),
                            label: None,
                            record: path.clone(),
                            native_key: name.clone(),
                        },
                        revision: Revision {
                            recorded: record.version,
                            source: record.revision,
                            declared: None,
                        },
                        contents: vec![contents],
                        deployments: vec![Deployment {
                            harness: profile.id.clone(),
                            scope: record.scope,
                            project: record.project,
                            path: record.path,
                        }],
                        configured_enabled: enabled.get(&name).copied(),
                        loaded: None,
                        baseline: None,
                    },
                },
            );
            if duplicate {
                report.state = CoverageState::Invalid;
                report.issues.push("duplicate_plugin_scope".into());
            }
            report.count += 1;
        }
    }
    Ok(report)
}

fn mcp_entries(
    profile: &Resource,
    path: &Path,
    scope: &str,
    project: Option<&Path>,
    records: Vec<policy::McpRecord>,
    resources: &mut BTreeMap<String, Resource>,
) -> Result<usize> {
    let count = records.len();
    for record in records {
        let key = resource::id(
            "capability",
            &("mcp-registration", path, scope, project, &record.name),
        )?;
        let entry = resources.entry(key.clone()).or_insert_with(|| Resource {
            id: key,
            name: record.name.clone(),
            aliases: vec![format!("mcp:{}", record.name)],
            management: Management::Native,
            status: State::Declared,
            data: Data::Capability {
                delivery_kind: "mcp".into(),
                native_id: record.name.clone(),
                origin: Origin {
                    manager: "client-config".into(),
                    label: record.runner,
                    record: path.to_path_buf(),
                    native_key: record.name,
                },
                revision: Revision::default(),
                contents: vec![],
                deployments: vec![],
                configured_enabled: record.enabled,
                loaded: None,
                baseline: None,
            },
        });
        if let Data::Capability { deployments, .. } = &mut entry.data
            && !deployments
                .iter()
                .any(|d| d.harness == profile.id && d.path == path)
        {
            deployments.push(Deployment {
                harness: profile.id.clone(),
                scope: scope.into(),
                project: project.map(Path::to_path_buf),
                path: path.to_path_buf(),
            });
        }
    }
    Ok(count)
}

fn mcp_json(
    profile: &Resource,
    path: &Path,
    scope: &str,
    project: Option<&Path>,
    resources: &mut BTreeMap<String, Resource>,
) -> Result<Coverage> {
    let mut report = Coverage {
        source: format!("mcp-json:{}:{}", profile.id, path.display()),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![path.to_path_buf()],
        issues: vec![],
    };
    if !paths::read_path(path)?.exists() {
        report.state = CoverageState::Absent;
        return Ok(report);
    }
    match paths::regular_bytes(path).and_then(|b| policy::mcp_json(&b)) {
        Ok(records) => {
            report.count = mcp_entries(profile, path, scope, project, records, resources)?;
        }
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
        }
    }
    Ok(report)
}

fn codex(
    profile: &Resource,
    root: &Path,
    resources: &mut BTreeMap<String, Resource>,
) -> Result<Coverage> {
    let path = root.join("config.toml");
    let mut report = Coverage {
        source: format!("codex-config:{}", profile.id),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![path.clone()],
        issues: vec![],
    };
    if !paths::read_path(&path)?.exists() {
        report.state = CoverageState::Absent;
        return Ok(report);
    }
    let bytes = paths::regular_bytes(&path)?;
    match policy::codex_plugins(&bytes) {
        Ok(plugins) => {
            for (name, enabled) in plugins {
                let (plugin, market) = name
                    .rsplit_once('@')
                    .ok_or_else(|| Error::new("invalid_plugin_id", "插件身份不完整"))?;
                let cache = root.join("plugins/cache").join(market).join(plugin);
                paths::read_path(&cache)?;
                let mut contents = vec![];
                if cache.is_dir() {
                    for p in std::fs::read_dir(&cache)? {
                        let p = p?;
                        if p.file_type()?.is_dir() {
                            contents.push(content(&p.path())?);
                        }
                    }
                }
                let key = resource::id("capability", &("codex-plugin", &profile.id, &name))?;
                resources.insert(
                    key.clone(),
                    Resource {
                        id: key,
                        name: name.clone(),
                        aliases: vec![format!("plugin:{name}")],
                        management: Management::Native,
                        status: State::Declared,
                        data: Data::Capability {
                            delivery_kind: "plugin".into(),
                            native_id: name.clone(),
                            origin: Origin {
                                manager: "codex-config".into(),
                                label: None,
                                record: path.clone(),
                                native_key: name,
                            },
                            revision: Revision::default(),
                            contents,
                            deployments: vec![Deployment {
                                harness: profile.id.clone(),
                                scope: "user".into(),
                                project: None,
                                path: path.clone(),
                            }],
                            configured_enabled: enabled,
                            loaded: None,
                            baseline: None,
                        },
                    },
                );
                report.count += 1;
            }
        }
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
        }
    }
    match policy::codex_mcp(&bytes) {
        Ok(records) => {
            report.count += mcp_entries(profile, &path, "user", None, records, resources)?;
        }
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
        }
    }
    Ok(report)
}

fn profile_capabilities(
    ctx: &Context,
    profile: &Resource,
    project: Option<&Path>,
    resources: &mut BTreeMap<String, Resource>,
    fs: &mut Coverage,
    reports: &mut Vec<Coverage>,
) -> Result<()> {
    if let Data::Agent {
        client, config_dir, ..
    } = &profile.data
    {
        let root = if client == "codex" {
            ctx.home.join(".agents/skills")
        } else {
            config_dir.join("skills")
        };
        skill_dir(&root, Some(&profile.id), "user", None, resources, fs)?;
        if client == "codex" {
            skill_dir(
                &config_dir.join("skills/.system"),
                Some(&profile.id),
                "system",
                None,
                resources,
                fs,
            )?;
        }
        if let Some(project) = project {
            let root = if client == "codex" {
                project.join(".agents/skills")
            } else if client == "claude" {
                project.join(".claude/skills")
            } else {
                project.join(format!(".{client}/skills"))
            };
            skill_dir(
                &root,
                Some(&profile.id),
                "project",
                Some(project),
                resources,
                fs,
            )?;
        }
        if client == "claude" {
            reports.push(plugins(profile, config_dir, project, resources)?);
            let sidecar = if config_dir == &ctx.home.join(".claude") {
                ctx.home.join(".claude.json")
            } else {
                config_dir.join(".claude.json")
            };
            reports.push(mcp_json(profile, &sidecar, "user", None, resources)?);
            if let Some(project) = project {
                reports.push(claude_local_mcp(profile, &sidecar, project, resources)?);
            }
            if let Some(project) = project {
                reports.push(mcp_json(
                    profile,
                    &project.join(".mcp.json"),
                    "project",
                    Some(project),
                    resources,
                )?);
            }
        }
        if client == "codex" {
            reports.push(codex(profile, config_dir, resources)?);
        }
        if client == "cursor" && config_dir.join("plugins/cache").exists() {
            reports.push(Coverage {
                source: format!("cursor-plugins:{}", profile.id),
                state: CoverageState::Unsupported,
                count: 0,
                roots: vec![config_dir.join("plugins/cache")],
                issues: vec!["cache_not_installation_proof".into()],
            });
        }
    }
    Ok(())
}

fn claude_local_mcp(
    profile: &Resource,
    path: &Path,
    project: &Path,
    resources: &mut BTreeMap<String, Resource>,
) -> Result<Coverage> {
    let mut report = Coverage {
        source: format!("mcp-local:{}:{}", profile.id, project.display()),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![path.to_path_buf()],
        issues: vec![],
    };
    if !paths::read_path(path)?.exists() {
        report.state = CoverageState::Absent;
        return Ok(report);
    }
    match paths::regular_bytes(path).and_then(|bytes| policy::mcp_local(&bytes, project)) {
        Ok(records) => {
            report.count = mcp_entries(profile, path, "local", Some(project), records, resources)?;
        }
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
        }
    }
    Ok(report)
}

fn plugin_flags(path: &Path, report: &mut Coverage) -> Result<BTreeMap<String, bool>> {
    if !paths::read_path(path)?.exists() {
        return Ok(BTreeMap::new());
    }
    match paths::regular_bytes(path).and_then(|bytes| policy::plugin_enabled(&bytes)) {
        Ok(flags) => Ok(flags),
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
            Ok(BTreeMap::new())
        }
    }
}
