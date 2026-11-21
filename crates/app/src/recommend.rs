use crate::{
    context::Context,
    domain::{Package, Provider},
    error::{Error, Result},
    inventory,
    paths::Directory,
    policy::recommend::{self as policy, Candidate, Scope},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

const EVIDENCE_LIMIT: u64 = 128 * 1024;

pub fn show(
    ctx: &Context,
    product: &str,
    evidence: Option<&Path>,
    github: Option<&str>,
    model: bool,
    account: Option<&str>,
    dry_run: bool,
) -> Result<Value> {
    let query = policy::query_name(product)?;
    let account = crate::policy::recommend_model::account(account.unwrap_or("default"))?;
    if let Some(path) = evidence {
        let mut result = from_evidence(query, path)?;
        result["model"] = json!({"status":if model {"local-evidence-only"} else {"not-requested"}});
        return Ok(result);
    }
    if let Some(repository) = github {
        return crate::recommend_upstream::show(ctx, query, repository, dry_run);
    }
    let public = policy::public_analysis(query)?;
    let mut known = Vec::new();
    for item in inventory::declared_catalog(ctx)?.packages {
        let candidate = from_declaration(&item);
        let shares_public_channel = public.as_ref().is_some_and(|snapshot| {
            snapshot
                .facts()
                .findings
                .iter()
                .any(|source| matches_public_channel(&item, &source.candidate))
        });
        if !shares_public_channel
            && !item.id.eq_ignore_ascii_case(query)
            && !item.name.eq_ignore_ascii_case(query)
        {
            continue;
        }
        known.push(json!({
            "id":item.id,
            "name":item.name,
            "source": policy::domain_link(&item.official_url).then_some(&item.official_url),
            "candidate": candidate,
            "matchesPublicChannel":shares_public_channel,
            "preview":preview_resource(&item),
        }));
    }
    let annotation = match (model, dry_run, public.as_ref()) {
        (false, _, _) => json!({"status":"not-requested"}),
        (true, true, _) => json!({"status":"dry-run"}),
        (true, false, Some(analysis)) => crate::recommend_model::annotate(ctx, analysis, account),
        (true, false, None) => json!({"status":"no-public-evidence"}),
    };
    Ok(json!({
        "query":query,
        "model":annotation,
        "evidenceStatus":if public.is_some() {"embedded-public-snapshot"} else {"declared-catalog"},
        "snapshotDate":public.as_ref().map(|_| policy::PUBLIC_SNAPSHOT_DATE),
        "analysis":public,
        "canExecute":false,
        "sourceChecks":"not-requested",
        "known":known,
        "persistentWrites":false,
    }))
}

fn from_evidence(query: &str, path: &Path) -> Result<Value> {
    let lexical = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let without_aliases = Directory::open(lexical.parent().ok_or_else(invalid_path)?)?;
    let leaf = lexical.file_name().ok_or_else(invalid_path)?;
    let (bytes, _) = without_aliases.regular_config_snapshot(leaf, EVIDENCE_LIMIT)?;
    let document_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let crate::policy::json::Object(input) = serde_json::from_value::<
        crate::policy::json::Object<policy::Input>,
    >(crate::policy::json::parse(&bytes)?)?;
    let result = policy::analyze(query, &input)?;
    Ok(json!({
        "query":query,
        "evidenceStatus":"provided-by-caller",
        "documentSha256":document_sha256,
        "canExecute":false,
        "analysis":result,
        "persistentWrites":false,
    }))
}

fn invalid_path() -> Error {
    Error::new(
        "recommend_evidence_invalid",
        "需要一个可读取的普通安装证据文件",
    )
}

fn matches_public_channel(declared: &Package, advised: &Candidate) -> bool {
    match (&declared.provider, advised) {
        (Provider::BrewFormula { query, .. }, Candidate::Brew { name, cask: false }) => {
            if name.contains('/') {
                query.key.full_name == *name
            } else {
                query.selection.tap == "homebrew/core" && query.selection.name == *name
            }
        }
        (
            Provider::Npm {
                package,
                authority:
                    Some(crate::domain::NpmWriteAuthority {
                        registry: Some(registry),
                        ..
                    }),
                ..
            },
            Candidate::Npm {
                name,
                scope: Scope::Global,
                ..
            },
        ) => {
            package == name
                && registry.url.trim_end_matches('/') == "https://registry.npmjs.org"
                && (!name.starts_with('@') || registry.scope_url.is_none())
        }
        _ => false,
    }
}

fn preview_resource(item: &Package) -> Option<Value> {
    if crate::policy::manifest::supports_replay(&item.provider)
        || matches!(item.provider, Provider::BrewFormula { .. })
    {
        return Some(json!({"entry":"install","id":item.id,"dryRun":true}));
    }
    None
}

fn from_declaration(item: &Package) -> Option<Candidate> {
    match &item.provider {
        Provider::Brew { package, cask } => Some(Candidate::Brew {
            name: package.clone(),
            cask: *cask,
        }),
        Provider::BrewFormula { query, .. } => Some(Candidate::Brew {
            name: query.key.full_name.clone(),
            cask: false,
        }),
        Provider::Npm { package, .. } => Some(Candidate::Npm {
            name: package.clone(),
            version: None,
            scope: Scope::Global,
            development: false,
        }),
        Provider::Mise { package } => Some(Candidate::Mise {
            name: package.clone(),
            request: None,
            selection: None,
        }),
        _ => None,
    }
}

pub(crate) fn interactive(cli: &crate::cli::Cli) -> bool {
    !cli.json
        && !cli.dry_run
        && !cli.yes
        && crate::browser::interactive_terminal()
        && matches!(
            cli.command,
            crate::cli::Command::Recommend {
                evidence: None,
                github: None,
                model: false,
                ..
            }
        )
}

pub(crate) fn choose(ctx: &Context, data: Value) -> Result<Value> {
    use crate::{browser::sanitize, domain::Action, operations};
    use std::io;
    let safe = |v: &Value| sanitize(v.as_str().unwrap_or(""), false);
    eprintln!("安装建议 · {}", safe(&data["query"]));
    if let Some(documents) = data["documents"].as_array() {
        for doc in documents {
            eprintln!("  {}  {}", safe(&doc["path"]), safe(&doc["url"]));
        }
        if let Some(candidates) = data["model"]["candidates"].as_array() {
            for candidate in candidates {
                eprintln!(
                    "  {} · {} · {} · {} · 版本 {}",
                    safe(&candidate["name"]),
                    safe(&candidate["kind"]),
                    safe(&candidate["adapter"]),
                    safe(&candidate["scope"]),
                    sanitize(candidate["version"].as_str().unwrap_or("待核对"), false)
                );
                eprintln!("    {}", safe(&candidate["summary"]));
                if let Some(document) = candidate["document"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .and_then(|index| documents.get(index))
                {
                    eprintln!("    证据 {}", safe(&document["url"]));
                }
            }
        }
    }
    let known = data["known"].as_array().map_or(&[][..], Vec::as_slice);
    if let Some(reason) = data["reason"].as_str() {
        eprintln!("来源核对未完成 · {}", sanitize(reason, false));
    }
    let declared = crate::inventory::declared_catalog(ctx)?;
    let mut installs = vec![];
    for item in known {
        let Some(id) = item["id"].as_str() else {
            continue;
        };
        if let Some(package) = declared.packages.iter().find(|p| p.id == id)
            && crate::policy::manifest::supports_replay(&package.provider)
        {
            eprintln!(
                "  {} {} · {}",
                installs.len() + 1,
                sanitize(&package.name, false),
                source_label(&package.official_url)
            );
            installs.push(package);
        }
    }
    if installs.is_empty() {
        eprintln!("当前没有可执行的已核验安装计划");
        return Ok(data);
    }
    eprintln!("选择安装候选（Enter 返回）");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let picked = input
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|index| index.checked_sub(1));
    let Some(package) = picked.and_then(|index| installs.get(index)) else {
        return Ok(json!({"mode":"cancelled"}));
    };
    let version = if package.exact_version_required {
        eprintln!("输入精确版本（Enter 返回）");
        let mut value = String::new();
        io::stdin().read_line(&mut value)?;
        let value = value.trim();
        if value.is_empty() {
            return Ok(json!({"mode":"cancelled"}));
        }
        Some(value.to_owned())
    } else {
        None
    };
    let plan = operations::plan(ctx, package, Action::Install, version.as_deref(), false)?;
    eprintln!(
        "安装并纳管 {} · 版本 {} · npm",
        sanitize(&package.name, false),
        sanitize(plan.target.as_deref().unwrap_or(""), false),
    );
    eprintln!("{}", plan_scope(&plan));
    operations::confirm("执行此安装计划", false)?;
    let result = operations::execute(ctx, plan, false, true)?;
    let journal = result.get("journal").unwrap_or(&result);
    eprintln!(
        "结果 {} · 操作 {}",
        safe(&journal["phase"]),
        safe(&journal["id"])
    );
    if let Some(message) = journal["message"].as_str() {
        eprintln!("{}", sanitize(message, false));
    }
    Ok(result)
}

fn source_label(value: &str) -> &str {
    let public = value.split(['?', '#']).next().unwrap_or_default();
    if policy::domain_link(public) {
        public
    } else {
        "待核对"
    }
}

pub(crate) fn plan_scope(plan: &crate::domain::Plan) -> String {
    use crate::{browser::sanitize, domain::QuerySpec};
    let safe = |s: &str| sanitize(s, false);
    let mut lines = vec![
        format!("实例 {}", safe(&plan.package.id)),
        format!("来源 {}", source_label(&plan.package.official_url)),
    ];
    if let Some(QuerySpec::LocalNpm(query)) = &plan.query {
        lines.push(format!(
            "安装范围 {}",
            safe(&query.namespace.canonical_prefix.display().to_string())
        ));
    }
    if let Provider::Npm {
        package, authority, ..
    } = &plan.package.provider
    {
        lines.push(format!("npm 包 {}", safe(package)));
        if let Some(registry) = authority.as_ref().and_then(|auth| auth.registry.as_ref()) {
            lines.push(format!("registry {}", source_label(&registry.url)));
            if let Some(scope_url) = &registry.scope_url {
                lines.push(format!("作用域 registry {}", source_label(scope_url)));
            }
            lines.push(format!(
                "安装脚本 {}",
                if registry.allow_scripts {
                    "允许"
                } else {
                    "禁止"
                }
            ));
        }
    }
    lines.extend(plan.warnings.iter().map(|warning| safe(warning)));
    lines.join("\n")
}

#[cfg(test)]
#[path = "../tests/unit/recommend_catalog.rs"]
mod tests;
