use crate::policy::skills::matches_resource;
use crate::{
    cli::{Command, Policy, Skills},
    context::Context,
    error::{Error, Result},
    governance::Governance,
    policy::governance::Dataset,
    resource::{CoverageState, Data, Resource, State},
};
use serde_json::{Value, json};

pub fn query(ctx: &Context, command: &Command) -> Result<Option<Value>> {
    let Command::Skills {
        command: Some(command),
    } = command
    else {
        return Ok(None);
    };
    let value = match command {
        Skills::List {
            name,
            only,
            details,
        } => inventory(ctx, "list", name.as_deref(), only, *details)?,
        Skills::Check { name, only } => inventory(ctx, "check", name.as_deref(), only, true)?,
        Skills::Status { name, only } => inventory(ctx, "status", name.as_deref(), only, true)?,

        Skills::Policy {
            command: Policy::Check,
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
    crate::governance::read(ctx, |authority| {
        let (mut resources, coverage, _) = observed_inventory(ctx, authority)?;
        let maintenance = match crate::native_skills::catalog_summary(ctx) {
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
            let drift=baseline.is_some_and(|b|b.comparison!=crate::resource::Comparison::Same);
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
) -> Result<(Vec<Resource>, Vec<crate::resource::Coverage>, Vec<Resource>)> {
    let profiles = crate::resource_agents::profile_roots(ctx)?;
    let (mut resources, coverage) = crate::resource_capabilities::collect(ctx, &profiles, None)?;
    add_source_aliases(ctx, authority, &mut resources)?;
    Ok((resources, coverage, profiles))
}

pub(crate) fn browse(ctx: &Context) -> Result<crate::browser::View> {
    use crate::browser::View;
    use std::collections::BTreeMap;
    crate::governance::read(ctx, |authority| {
        let (resources, coverage, profiles) = observed_inventory(ctx, authority)?;
        let profiles: BTreeMap<_, _> = profiles
            .into_iter()
            .map(|profile| (profile.id, profile.name))
            .collect();
        let recipes = crate::native_skills::catalog_summary(ctx).ok();
        let mut incomplete = coverage.iter().any(|row| {
            (row.source.starts_with("skills-lock") || row.source == "skill-deployments")
                && !matches!(row.state, CoverageState::Complete | CoverageState::Absent)
        });
        let mut items = Vec::new();
        for resource in resources {
            if let Some((item, attention)) = skill_item(resource, &profiles, recipes.as_ref()) {
                items.push(item);
                incomplete |= attention;
            }
        }
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
        Ok(View {
            scope: "global".into(),
            items,
            coverage: if incomplete { "partial" } else { "observed" },
            difference_coverage: "not_applicable",
            observed_at: Some(crate::store::now()),
            note: if incomplete {
                "技能盘点不完整，显示已取得的记录"
            } else {
                "技能清单"
            }
            .into(),
            incomplete,
        })
    })
}

fn skill_item(
    resource: Resource,
    profiles: &std::collections::BTreeMap<String, String>,
    recipes: Option<&Value>,
) -> Option<(crate::browser::Item, bool)> {
    use crate::{browser::Item, resource::Management};
    use std::collections::BTreeSet;
    let agent_name = |id: &str| profiles.get(id).map_or("未知档案", String::as_str);
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
    if delivery_kind != "skill" {
        return None;
    }
    let incomplete = matches!(resource.status, State::Unknown | State::Missing)
        || baseline.is_some_and(|b| b.comparison != crate::resource::Comparison::Same);
    let agents: Vec<_> = deployments
        .iter()
        .map(|d| agent_name(&d.harness).to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let version = revision.recorded.or(revision.declared).or(revision.source);
    let manual = recipes
        .and_then(|summary| summary["sources"].as_array())
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["id"].as_str() == Some(&resource.id))
        })
        .and_then(|row| row["recipe"]["manualOnly"].as_bool());
    let mut details = vec![
        format!("管理器 {}", origin.manager),
        format!("来源 {}", origin.label.as_deref().unwrap_or("待核对")),
        format!("版本 {}", version.as_deref().unwrap_or("未知")),
        format!("安装状态 {:?}", resource.status),
        format!("配置启用 {}", label_bool(configured_enabled)),
        format!("会话载入 {}", label_bool(loaded)),
        format!("仅手动触发 {}", label_bool(manual)),
    ];
    details.extend(
        contents
            .iter()
            .map(|item| format!("载荷 {}", item.path.display())),
    );
    details.extend(deployments.iter().map(|d| {
        format!(
            "投放 {} · {} · {}",
            agent_name(&d.harness),
            d.scope,
            d.path.display()
        )
    }));
    let display_version: String = version
        .as_deref()
        .unwrap_or("未知")
        .chars()
        .take(12)
        .collect();
    let summary = format!("{display_version} · {}", agents.join(", "));
    Some((
        Item {
            id: resource.id,
            name: resource.name,
            summary,
            managed: resource.management != Management::Untracked,
            directory: false,
            path: contents.into_iter().next().map(|item| item.path),
            source: None,
            agents,
            loaded,
            details,
        },
        incomplete,
    ))
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
        rows.push(json!({"id":format!("asset:{name}"),"name":crate::policy::resources::source_label(name),"kind":if known {kind}else{"unsupported"},"status":"declared"}));
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
    let bound = crate::paths::Bindings::new(64).target(&lock)?;
    let crate::paths::ObservedRegular::Regular { bytes, .. } =
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
        if crate::policy::native_skills::installation_id(&crate::paths::read_path(
            &ctx.home.join(".agents/skills").join(native_id),
        )?)? != resource.id
            || !resource
                .aliases
                .contains(&crate::policy::capability_local::skill_capability_id(
                    &lock, native_id, kind, source,
                )?)
        {
            continue;
        }
        if let Some(alias) = ids
            .get(source)
            .and_then(Value::as_str)
            .filter(|alias| crate::policy::resources::component(alias))
        {
            resource.aliases.push(alias.into());
            resource.aliases.push(format!("source:{alias}"));
        }
    }
    Ok(())
}

fn policy_check(ctx: &Context) -> Result<Value> {
    crate::governance::read(ctx, |authority| {
        let plan = crate::invocation_inventory::assess(ctx, authority)?;
        let summary = plan.summary();
        plan.verify()?;
        Ok(summary)
    })
}
