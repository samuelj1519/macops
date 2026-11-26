use crate::skills::rules::skills::matches_resource;
use crate::{
    cli::{Command, Policy, SkillCmd},
    context::Context,
    domain::resource::{Content, CoverageState, Data, Deployment, Resource, State},
    error::{Error, Result},
    operations::governance::Governance,
    policy::governance::Dataset,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub fn query(ctx: &Context, command: &Command) -> Result<Option<Value>> {
    let value = match command {
        Command::Skill {
            project: _,
            command: None,
            details,
            only,
        } => inventory(ctx, "list", None, only, *details)?,
        Command::Skill {
            project: _,
            details: _,
            only: _,
            command: Some(SkillCmd::Doctor { name, only }),
        } => inventory(ctx, "check", name.as_deref(), only, true)?,
        Command::Skill {
            project: _,
            details: _,
            only: _,
            command: Some(SkillCmd::Policy {
                command: Policy::Check,
            }),
        } => policy_check(ctx)?,
        _ => return Ok(None),
    };
    Ok(Some(value))
}

fn inventory(
    ctx: &Context,
    verb: &str,
    name: Option<&str>,
    only: &[String],
    details: bool,
) -> Result<Value> {
    crate::operations::governance::read(ctx, |authority| {
        let (mut resources, coverage, _) = observed_inventory(ctx, authority, false)?;
        let maintenance = match crate::skills::native_skills::catalog_summary(ctx) {
            Ok(summary) => summary,
            Err(error) => {
                json!({"maintainable_scope":"public-github-existing-roots","status":"unknown","reason":error.code})
            }
        };
        let declared = declared(authority)?;
        let unsupported_declarations = declared.iter().any(|row| row["kind"] == "unsupported");
        let selectors: Vec<_> = name
            .into_iter()
            .chain(only.iter().map(String::as_str))
            .collect();
        let matched_declared: Vec<_> = declared
            .into_iter()
            .filter(|row| {
                selectors.is_empty()
                    || selectors
                        .iter()
                        .any(|key| row["name"].as_str() == Some(key))
            })
            .collect();
        if !selectors.is_empty() {
            resources.retain(|r| selectors.iter().any(|key| matches_resource(r, key)));
            for key in &selectors {
                if !resources.iter().any(|r| matches_resource(r, key))
                    && !matched_declared
                        .iter()
                        .any(|r| r["name"].as_str() == Some(key))
                {
                    return Err(Error::attention("not_found", "没有发现对应能力包或来源"));
                }
            }
        }
        let mut incomplete = coverage
            .iter()
            .any(|c| !matches!(c.state, CoverageState::Complete | CoverageState::Absent));
        incomplete |= unsupported_declarations;
        incomplete |= resources
            .iter()
            .any(|r| matches!(r.status, State::Unknown | State::Missing));
        let results:Vec<_>=resources.iter().map(|resource| {
            let baseline=match &resource.data {Data::Capability {baseline,..}=>baseline.as_ref(),_=>None};
            let drift=baseline.is_some_and(|b|b.comparison!=crate::domain::resource::Comparison::Same);
            json!({"id":resource.id,"name":resource.name,"status":if drift {json!("drift")}else{json!(resource.status)},
                "verification":if baseline.is_some(){"baseline"}else{"presence_only"},"baseline":baseline,"incomplete":drift})
        }).collect();
        incomplete |= results.iter().any(|row| row["incomplete"] == true);
        Ok(
            json!({"backend":"rust","command":verb,"scope":"global","resources":resources,"declared":matched_declared,"source_maintenance":maintenance,
            "results":results,"coverage":coverage,"incomplete":incomplete,"details":details,"persistentWrites":false}),
        )
    })
}

fn observed_inventory(
    ctx: &Context,
    authority: &Governance,
    for_browser: bool,
) -> Result<(
    Vec<Resource>,
    Vec<crate::domain::resource::Coverage>,
    Vec<Resource>,
)> {
    let profiles = crate::agent::resource_agents::profile_roots(ctx)?;
    let (mut resources, coverage) = crate::capability::resource_capabilities::collect_observed(
        ctx,
        &profiles,
        None,
        for_browser,
    )?;
    add_source_aliases(ctx, authority, &mut resources)?;
    Ok((resources, coverage, profiles))
}

pub(crate) fn browse(
    ctx: &Context,
    include_other_capabilities: bool,
) -> Result<crate::browser::View> {
    use crate::browser::View;
    crate::operations::governance::read(ctx, |authority| {
        let (resources, coverage, profiles) = observed_inventory(ctx, authority, true)?;
        let profiles: BTreeMap<_, _> = profiles
            .into_iter()
            .map(|profile| (profile.id, profile.name))
            .collect();
        let recipes = crate::skills::native_skills::catalog_summary(ctx).ok();
        let mut incomplete = coverage.iter().any(|row| {
            (row.source.starts_with("skills-lock")
                || matches!(
                    row.source.as_str(),
                    "skill-deployments" | "cursor-standalone-skills"
                )
                || (include_other_capabilities
                    && ["claude-plugins:", "codex-config:", "cursor-plugins:"]
                        .iter()
                        .any(|prefix| row.source.starts_with(prefix))))
                && !matches!(row.state, CoverageState::Complete | CoverageState::Absent)
        });
        let mut items = Vec::new();
        for resource in resources {
            if let Some((item, attention)) = skill_item(
                resource,
                &ctx.home,
                &profiles,
                recipes.as_ref(),
                include_other_capabilities,
            ) {
                items.push(item);
                incomplete |= attention;
            }
        }
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        let note = if incomplete {
            "能力盘点不完整，显示已取得的记录"
        } else if include_other_capabilities {
            "本机能力清单"
        } else {
            "技能清单"
        };
        Ok(View {
            scope: "global".into(),
            items,
            coverage: if incomplete { "partial" } else { "observed" },
            difference_coverage: "not_applicable",
            observed_at: Some(crate::operations::store::now()),
            note: if include_other_capabilities
                && profiles.values().any(|name| name == "cursor:default")
            {
                format!("{note} · Cursor 插件启用状态未核验")
            } else {
                note.into()
            },
            incomplete,
        })
    })
}

fn skill_item(
    resource: Resource,
    home: &Path,
    profiles: &BTreeMap<String, String>,
    recipes: Option<&Value>,
    include_other_capabilities: bool,
) -> Option<(crate::browser::Item, bool)> {
    use crate::{browser::Item, domain::resource::Management};
    let Data::Capability {
        delivery_kind,
        origin,
        revision,
        contents,
        deployments,
        configured_enabled,
        loaded,
        baseline,
        ..
    } = resource.data
    else {
        return None;
    };
    if delivery_kind != "skill" && !include_other_capabilities {
        return None;
    }
    let incomplete = matches!(resource.status, State::Unknown | State::Missing)
        || baseline.is_some_and(|b| b.comparison != crate::domain::resource::Comparison::Same);
    let version = revision.recorded.or(revision.declared).or(revision.source);
    let mut details = vec![
        format!("管理器 {}", origin.manager),
        format!("来源 {}", origin.label.as_deref().unwrap_or("待核对")),
        format!("版本 {}", version.as_deref().unwrap_or("未知")),
        format!("安装状态 {}", state_label(&resource.status)),
        format!("会话载入 {}", label_bool(loaded)),
    ];
    if include_other_capabilities {
        details.insert(0, format!("类型 {delivery_kind}"));
    }
    let plugin_enabled = match configured_enabled {
        Some(true) => "已启用",
        Some(false) => "已停用",
        None => "未知",
    };
    let agents = if delivery_kind == "skill" {
        let manual = label_bool(manual_only(recipes, &resource.id));
        details.push(format!("仅手动触发 {manual}"));
        add_skill_details(home, profiles, &contents, &deployments, &mut details)
    } else {
        let plugin = delivery_kind == "plugin";
        if plugin {
            details.push(format!("安装记录 {}", origin.record.display()));
            if deployments.is_empty() {
                details.push(format!("插件配置 {plugin_enabled}"));
            }
        } else {
            details.push(format!("配置启用 {}", label_bool(configured_enabled)));
        }
        add_native_details(
            profiles,
            &contents,
            &deployments,
            plugin.then_some(plugin_enabled),
            &mut details,
        )
    };
    let version_label = version.as_deref().unwrap_or("未知");
    let display_version: String = version_label.chars().take(12).collect();
    let agents: Vec<_> = agents.into_iter().collect();
    let groups = if delivery_kind == "skill" {
        skill_groups(home, profiles, &contents, &deployments)
    } else {
        agents.clone()
    };
    let profiles = agents.join(", ");
    let summary = match delivery_kind.as_str() {
        "skill" if agents.is_empty() => format!("{display_version} · 未观察到可发现档案"),
        "skill" => format!("{display_version} · 可发现 {profiles}"),
        "plugin" if agents.is_empty() => format!("{display_version} · 插件配置 {plugin_enabled}"),
        "plugin" => format!("{display_version} · {profiles} · {plugin_enabled}"),
        _ => format!("{display_version} · {profiles}"),
    };
    Some((
        Item {
            id: resource.id,
            name: resource.name,
            summary,
            managed: resource.management != Management::Untracked,
            management: resource.management,
            directory: false,
            path: contents.into_iter().next().map(|item| item.path),
            source: None,
            agents,
            loaded,
            details,
            inventory: Some(crate::browser::InventoryInfo {
                manager: origin.manager,
                groups,
                status: resource.status,
                version,
                aliases: resource.aliases,
                repository: origin.repository,
                native_record: Some(origin.record),
            }),
        },
        incomplete,
    ))
}

fn manual_only(recipes: Option<&Value>, id: &str) -> Option<bool> {
    let sources = recipes?["sources"].as_array()?;
    sources.iter().find(|row| row["id"].as_str() == Some(id))?["recipe"]["manualOnly"].as_bool()
}

fn profile_name<'a>(profiles: &'a BTreeMap<String, String>, id: &str) -> &'a str {
    profiles.get(id).map_or("未知档案", String::as_str)
}

fn add_native_details(
    profiles: &BTreeMap<String, String>,
    contents: &[Content],
    deployments: &[Deployment],
    plugin_enabled: Option<&str>,
    details: &mut Vec<String>,
) -> BTreeSet<String> {
    details.extend(contents.iter().map(|item| {
        format!(
            "载荷 {} · {}",
            state_label(&item.status),
            item.path.display()
        )
    }));
    let mut agents = BTreeSet::new();
    for d in deployments {
        let name = profile_name(profiles, &d.harness);
        agents.insert(name.to_owned());
        if let Some(enabled) = plugin_enabled {
            details.push(format!("插件配置 {name} · {enabled} · {}", d.scope));
        } else {
            details.push(format!(
                "配置位置 {name} · {} · {}",
                d.scope,
                d.path.display()
            ));
        }
    }
    agents
}

fn add_skill_details(
    home: &Path,
    profiles: &BTreeMap<String, String>,
    contents: &[Content],
    deployments: &[Deployment],
    details: &mut Vec<String>,
) -> BTreeSet<String> {
    let locations = skill_locations(contents, deployments);
    details.extend(
        locations
            .iter()
            .map(|(path, status)| format!("载荷 {} · {}", state_label(status), path.display())),
    );
    let visibility = skill_visibility(home, profiles, deployments, &locations);
    if visibility.is_empty() {
        details.push("可发现档案 未观察到".into());
    }
    let mut agents = BTreeSet::new();
    for (profile, path, route) in visibility {
        agents.insert(profile.to_owned());
        details.push(format!("可发现 {profile} · {route} · {}", path.display()));
    }
    agents
}

fn skill_locations<'a>(
    contents: &'a [Content],
    deployments: &'a [Deployment],
) -> BTreeMap<&'a Path, &'a State> {
    let mut locations: BTreeMap<_, _> = contents
        .iter()
        .map(|item| (item.path.as_path(), &item.status))
        .collect();
    for deployment in deployments {
        locations.insert(deployment.path.as_path(), &State::Present);
    }
    locations
}

fn skill_groups(
    home: &Path,
    profiles: &BTreeMap<String, String>,
    contents: &[Content],
    deployments: &[Deployment],
) -> Vec<String> {
    let shared = home.join(".agents/skills");
    let codex = home.join(".codex/skills");
    let locations = skill_locations(contents, deployments);
    let mut groups: BTreeSet<_> = deployments
        .iter()
        .filter(|deployment| deployment.path.parent() != Some(shared.as_path()))
        .filter_map(|deployment| profiles.get(&deployment.harness).cloned())
        .collect();
    if locations.keys().any(|path| path.parent() == Some(&shared)) {
        groups.insert("shared".into());
    }
    if locations.keys().any(|path| path.parent() == Some(&codex)) {
        groups.insert("codex:default".into());
    }
    groups.into_iter().collect()
}

fn skill_visibility<'a>(
    home: &Path,
    profiles: &'a BTreeMap<String, String>,
    deployments: &'a [Deployment],
    locations: &BTreeMap<&'a Path, &'a State>,
) -> BTreeSet<(&'a str, &'a Path, &'static str)> {
    let shared = home.join(".agents/skills");
    let mut visibility = BTreeSet::new();
    for d in deployments {
        let route = if d.path.parent() == Some(shared.as_path()) {
            "共享目录"
        } else if d.scope == "system" {
            "系统目录"
        } else {
            "档案目录"
        };
        visibility.insert((profile_name(profiles, &d.harness), d.path.as_path(), route));
    }
    if let Some(cursor) = profiles.values().find(|name| *name == "cursor:default") {
        // These are directory imports, independent of Cursor's plugin registry.
        let claude = home.join(".claude/skills");
        let codex = home.join(".codex/skills");
        for (&path, &state) in locations {
            if *state != State::Present {
                continue;
            }
            if path.parent() == Some(shared.as_path()) {
                visibility.insert((cursor.as_str(), path, "共享目录"));
            } else if path.parent() == Some(claude.as_path())
                || path.parent() == Some(codex.as_path())
            {
                visibility.insert((cursor.as_str(), path, "兼容目录"));
            }
        }
    }
    visibility
}

fn state_label(state: &State) -> &'static str {
    match state {
        State::Present => "在磁盘上",
        State::Declared => "已登记",
        State::Missing => "缺失",
        State::Unknown => "未知",
    }
}

fn label_bool(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "是",
        Some(false) => "否",
        None => "未知",
    }
}

fn declared(authority: &Governance) -> Result<Vec<Value>> {
    let Some(bytes) = authority.read(Dataset::AssetDefinitions, 1024 * 1024)? else {
        return Ok(vec![]);
    };
    let value = crate::policy::json::parse(&bytes)?;
    let assets = value
        .get("assets")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::new("invalid_assets", "能力包声明必须包含 assets 对象"))?;
    let mut rows = vec![];
    for (name, spec) in assets {
        let kind = spec
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new("invalid_assets", "能力包声明缺少 kind"))?;
        let known = matches!(
            kind,
            "local-plugin"
                | "local-release-plugin"
                | "github-skill"
                | "npx-github"
                | "npx-url"
                | "zip"
                | "marketplace-plugin"
                | "local-skills"
                | "derived-skills"
                | "app-managed"
        );
        rows.push(json!({"id":format!("asset:{name}"),"name":crate::policy::catalog::resources::source_label(name),"kind":if known {kind}else{"unsupported"},"status":"declared"}));
    }
    Ok(rows)
}

fn add_source_aliases(
    ctx: &Context,
    authority: &Governance,
    resources: &mut [Resource],
) -> Result<()> {
    let Some(bytes) = authority.read(Dataset::SourceChannels, 1024 * 1024)? else {
        return Ok(());
    };
    let channels = crate::policy::json::parse(&bytes)?;
    let Some(ids) = channels.get("source_ids").and_then(Value::as_object) else {
        return Ok(());
    };
    let lock = ctx
        .scope
        .paths
        .get("XDG_STATE_HOME")
        .and_then(Option::as_ref)
        .map_or_else(
            || ctx.home.join(".agents/.skill-lock.json"),
            |p| p.join("skills/.skill-lock.json"),
        );
    let bound = crate::platform::fs::Bindings::new(64).target(&lock)?;
    let crate::platform::fs::ObservedRegular::Regular { bytes, .. } =
        bound.observe_regular(1024 * 1024)?
    else {
        return Ok(());
    };
    let value = crate::policy::json::parse(&bytes)?;
    let Some(skills) = value.get("skills").and_then(Value::as_object) else {
        return Ok(());
    };
    for resource in resources {
        let Data::Capability {
            native_id, origin, ..
        } = &resource.data
        else {
            continue;
        };
        if origin.record != lock {
            continue;
        }
        let Some(row) = skills.get(native_id) else {
            continue;
        };
        let (Some(source), Some(kind)) = (
            row.get("source").and_then(Value::as_str),
            row.get("sourceType").and_then(Value::as_str),
        ) else {
            continue;
        };
        if crate::skills::rules::native_skills::installation_id(&crate::platform::fs::read_path(
            &ctx.home.join(".agents/skills").join(native_id),
        )?)? != resource.id
            || !resource.aliases.contains(
                &crate::capability::rules::capability_local::skill_capability_id(
                    &lock, native_id, kind, source,
                )?,
            )
        {
            continue;
        }
        if let Some(alias) = ids
            .get(source)
            .and_then(Value::as_str)
            .filter(|alias| crate::policy::catalog::resources::component(alias))
        {
            resource.aliases.push(alias.into());
            resource.aliases.push(format!("source:{alias}"));
        }
    }
    Ok(())
}

fn policy_check(ctx: &Context) -> Result<Value> {
    crate::operations::governance::read(ctx, |authority| {
        let plan = crate::capability::invocation_inventory::assess(ctx, authority)?;
        let summary = plan.summary();
        plan.verify()?;
        Ok(summary)
    })
}
