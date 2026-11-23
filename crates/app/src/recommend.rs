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
    let account = account
        .map(crate::policy::recommend_model::account)
        .transpose()?;
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
    if install_channel(&item.provider).is_some()
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

fn install_channel(provider: &Provider) -> Option<&'static str> {
    if matches!(
        provider,
        Provider::BrewFormula {
            recipe_ref: Some(_),
            ..
        }
    ) {
        Some("Homebrew")
    } else if matches!(provider, Provider::Mise { .. }) {
        Some("mise")
    } else if crate::policy::manifest::supports_replay(provider) {
        Some("npm")
    } else {
        None
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
                setup: false,
                ..
            }
        )
}

pub(crate) fn choose(ctx: &Context, data: Value) -> Result<Value> {
    use crate::browser::sanitize;
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
    if data["model"]["status"] == "keychain-unavailable" {
        eprintln!("推荐模型凭据暂不可读取，已继续使用本地分析。");
    }
    show_codex_market(&data["codexMarketplace"]);
    let known = data["known"].as_array().map_or(&[][..], Vec::as_slice);
    if let Some(reason) = data["reason"].as_str() {
        eprintln!("来源核对未完成 · {}", sanitize(reason, false));
    }
    let installs = install_choices(ctx, &data, known)?;
    if !installs.is_empty() {
        check_install_ready(ctx)?;
    }
    match installs.as_slice() {
        [] => {
            if let Some(repo) = search_repository(ctx, &data)? {
                return choose(
                    ctx,
                    crate::recommend_upstream::show_repository(ctx, &repo, false, true)?,
                );
            }
            eprintln!("当前没有可执行的已核验安装计划");
            return Ok(data);
        }
        [InstallChoice::Codex(offer, binary)] => {
            return install_isolated_codex(ctx, (**offer).clone(), binary.clone());
        }
        _ => {}
    }
    for (index, choice) in installs.iter().enumerate() {
        eprintln!("  {} {}", index + 1, choice.label());
    }
    eprintln!("选择安装候选（Enter 返回）");
    let Some(package) = choice_index()?.and_then(|index| installs.get(index)) else {
        return Ok(json!({"mode":"cancelled"}));
    };
    check_install_ready(ctx)?;
    match package {
        InstallChoice::Application(package) => install_selected(ctx, package),
        InstallChoice::Skill(recipe) => Ok(report_install(
            crate::native_skills::install_recommended(ctx, (**recipe).clone())?,
        )),
        InstallChoice::Codex(offer, binary) => {
            install_isolated_codex(ctx, (**offer).clone(), binary.clone())
        }
        InstallChoice::Claude(offer, binary) => {
            install_isolated_claude(ctx, (**offer).clone(), binary.clone())
        }
    }
}

fn check_install_ready(ctx: &Context) -> Result<()> {
    crate::store::check_active_pending(ctx).map_err(|mut error| {
        error.message = format!("暂不能安装。{}", error.message);
        error
    })
}

fn choice_index() -> Result<Option<usize>> {
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(input
        .trim()
        .parse::<usize>()
        .ok()
        .and_then(|index| index.checked_sub(1)))
}

fn search_repository(ctx: &Context, data: &Value) -> Result<Option<String>> {
    search_repository_using(
        data,
        ctx.isolated,
        |name| crate::recommend_upstream::search(ctx, name),
        choice_index,
    )
}

fn search_repository_using(
    data: &Value,
    isolated: bool,
    search: impl FnOnce(&str) -> Option<Result<Vec<crate::recommend_upstream::RepositoryMatch>>>,
    choice: impl FnOnce() -> Result<Option<usize>>,
) -> Result<Option<String>> {
    if isolated
        || !matches!(
            data["evidenceStatus"].as_str(),
            Some("declared-catalog" | "embedded-public-snapshot")
        )
    {
        return Ok(None);
    }
    let Some(matches) = data["query"].as_str().and_then(search) else {
        return Ok(None);
    };
    let repositories = match matches {
        Ok(matches) if matches.is_empty() => {
            eprintln!("公开 GitHub 搜索未找到同名仓库");
            return Ok(None);
        }
        Ok(matches) => matches,
        Err(_) => {
            eprintln!("公开 GitHub 搜索暂不可用");
            return Ok(None);
        }
    };
    for (index, repository) in repositories.iter().enumerate() {
        let suffix = if repository.fork {
            " · 派生仓库"
        } else {
            ""
        };
        eprintln!("  {} {}{suffix}", index + 1, repository.name);
    }
    eprintln!("选择一个公开仓库核对安装方式（Enter 返回）");
    Ok(choice()?
        .and_then(|index| repositories.get(index))
        .map(|entry| entry.name.clone()))
}

enum InstallChoice {
    Application(Box<Package>),
    Skill(Box<crate::policy::native_skills::Recipe>),
    Codex(Box<crate::codex_install_source::Offer>, std::path::PathBuf),
    Claude(Box<crate::claude_install_source::Offer>, std::path::PathBuf),
}

impl InstallChoice {
    fn label(&self) -> String {
        use crate::browser::sanitize;
        match self {
            Self::Application(package) => format!(
                "{} · {} · {}",
                sanitize(&package.name, false),
                install_channel(&package.provider).unwrap_or_default(),
                source_label(&package.official_url)
            ),
            Self::Skill(recipe) => format!(
                "{} · Skills CLI · Codex 用户级 · 共享技能目录 · {}@{}",
                recipe.install_name, recipe.source.repository, recipe.source.commit
            ),
            Self::Codex(offer, _) => format!(
                "{} · 独立 Codex 档案 · {}",
                offer.native_id(),
                offer.declared_version
            ),
            Self::Claude(offer, _) => format!(
                "{} · 独立 Claude Code 档案 · {}",
                offer.native_id(),
                offer.version,
            ),
        }
    }
}

fn install_choices(ctx: &Context, data: &Value, known: &[Value]) -> Result<Vec<InstallChoice>> {
    let declared = crate::inventory::declared_catalog(ctx)?;
    let mut choices = Vec::new();
    for row in known {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        if let Some(package) = declared.packages.iter().find(|p| p.id == id)
            && install_channel(&package.provider).is_some()
            && (!matches!(package.provider, Provider::Mise { .. })
                || crate::resource_inventory::native_program(ctx, "mise").is_ok())
        {
            choices.push(InstallChoice::Application(Box::new(package.clone())));
        }
    }
    if let Some(subject) = data["query"].as_str() {
        match crate::native_skills::recommended_recipes(ctx, subject) {
            Ok(recipes) => choices.extend(
                recipes
                    .into_iter()
                    .map(|recipe| InstallChoice::Skill(Box::new(recipe))),
            ),
            Err(_) => eprintln!("已登记的 Skills CLI 配方暂不可核对"),
        }
    }
    if let Ok(offer) = crate::codex_install_source::Offer::parse(&data["codexMarketplace"])
        && let Ok(binary) = crate::codex_install::binary(ctx)
    {
        choices.push(InstallChoice::Codex(Box::new(offer), binary));
    }
    if let Ok(offer) = crate::claude_install_source::Offer::parse(&data["claudeMarketplace"])
        && let Ok(binary) = crate::claude_install::binary(ctx)
    {
        choices.push(InstallChoice::Claude(Box::new(offer), binary));
    }
    Ok(choices)
}

fn install_selected(ctx: &Context, package: &Package) -> Result<Value> {
    use crate::{browser::sanitize, domain::Action, operations};
    use std::io;
    if matches!(package.provider, Provider::BrewFormula { .. }) {
        return Ok(report_install(crate::brew::execute_selected(
            ctx,
            package,
            Action::Install,
            false,
            false,
        )?));
    }
    let version =
        if package.exact_version_required || matches!(package.provider, Provider::Mise { .. }) {
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
        "安装并纳管 {} · 版本 {} · {}",
        sanitize(&package.name, false),
        sanitize(plan.target.as_deref().unwrap_or(""), false),
        install_channel(&package.provider).unwrap_or_default(),
    );
    eprintln!("{}", plan_scope(ctx, &plan));
    operations::confirm("执行此安装计划", false)?;
    Ok(report_install(operations::execute(ctx, plan, false, true)?))
}

fn report_install(result: Value) -> Value {
    use crate::browser::sanitize;
    let safe = |v: &Value| sanitize(v.as_str().unwrap_or(""), false);
    let journal = result.get("journal").unwrap_or(&result);
    eprintln!(
        "结果 {} · 操作 {}",
        safe(&journal["phase"]),
        safe(&journal["id"])
    );
    if let Some(message) = journal["message"].as_str() {
        eprintln!("{}", sanitize(message, false));
    }
    result
}

fn install_isolated_codex(
    ctx: &Context,
    offer: crate::codex_install_source::Offer,
    binary: std::path::PathBuf,
) -> Result<Value> {
    install_codex_using(
        ctx,
        offer,
        binary,
        confirm_initial_activation,
        crate::codex_install::install,
    )
}

fn install_isolated_claude(
    ctx: &Context,
    offer: crate::claude_install_source::Offer,
    binary: std::path::PathBuf,
) -> Result<Value> {
    install_claude_using(
        ctx,
        offer,
        binary,
        confirm_initial_activation,
        crate::claude_install::install,
    )
}

fn install_claude_using(
    ctx: &Context,
    offer: crate::claude_install_source::Offer,
    binary: std::path::PathBuf,
    confirm: impl FnOnce() -> Result<crate::codex_install::InitialActivation>,
    install: impl FnOnce(
        &Context,
        crate::claude_install_source::Offer,
        std::path::PathBuf,
        bool,
    ) -> Result<Value>,
) -> Result<Value> {
    eprintln!(
        "安装并纳管 {} · {} · 独立 Claude Code 档案",
        offer.native_id(),
        offer.version
    );
    eprintln!("来源 {} · {}", offer.url()?, offer.commit);
    let enabled = confirm()? == crate::codex_install::InitialActivation::Enabled;
    let result = report_install(install(ctx, offer, binary, enabled)?);
    if let Some(path) = result["claudeConfigDir"].as_str() {
        eprintln!("Claude Code 档案 {}", crate::browser::sanitize(path, false));
    }
    Ok(result)
}

fn install_codex_using(
    ctx: &Context,
    offer: crate::codex_install_source::Offer,
    binary: std::path::PathBuf,
    confirm: impl FnOnce() -> Result<crate::codex_install::InitialActivation>,
    install: impl FnOnce(
        &Context,
        crate::codex_install_source::Offer,
        std::path::PathBuf,
        crate::codex_install::InitialActivation,
    ) -> Result<Value>,
) -> Result<Value> {
    eprintln!(
        "安装并纳管 {} · {} · 独立 Codex 档案",
        offer.native_id(),
        offer.declared_version
    );
    eprintln!("来源 {} · {}", offer.url()?, offer.payload_commit);
    let activation = confirm()?;
    let result = install(ctx, offer, binary, activation)?;
    let safe = |key: &str| {
        crate::browser::sanitize(result["journal"][key].as_str().unwrap_or_default(), false)
    };
    eprintln!("结果 {} · 操作 {}", safe("phase"), safe("id"));
    if let Some(home) = result["codexHome"].as_str() {
        eprintln!("Codex 档案 {}", crate::browser::sanitize(home, false));
    }
    Ok(result)
}

fn confirm_initial_activation() -> Result<crate::codex_install::InitialActivation> {
    use crate::codex_install::InitialActivation;
    eprintln!("输入 yes 安装并启用，输入 disabled 安装后保持停用，回车取消。");
    let mut choice = String::new();
    std::io::stdin().read_line(&mut choice)?;
    match choice.trim() {
        "yes" => Ok(InitialActivation::Enabled),
        "disabled" => Ok(InitialActivation::Disabled),
        _ => Err(Error::attention("cancelled", "已取消安装")),
    }
}

fn show_codex_market(market: &Value) {
    if !market.is_object() {
        return;
    }
    let field = |name: &str| crate::browser::sanitize(market[name].as_str().unwrap_or(""), false);
    eprintln!(
        "  Codex 插件 {}@{} · 兼容清单声明版本 {}",
        field("plugin"),
        field("marketplace"),
        field("declaredVersion")
    );
    eprintln!(
        "    市场提交 {} · 载荷 {} → {}",
        field("marketplaceCommit"),
        field("payloadRef"),
        field("payloadCommit")
    );
    if market["status"] == "divergent-commits" {
        eprintln!("    载荷与市场来自不同提交，执行计划仍待核验");
    }
}

fn source_label(value: &str) -> &str {
    let public = value.split(['?', '#']).next().unwrap_or_default();
    if policy::domain_link(public) {
        public
    } else {
        "待核对"
    }
}

pub(crate) fn plan_scope(ctx: &Context, plan: &crate::domain::Plan) -> String {
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
    if let Provider::Mise { package } = &plan.package.provider {
        let scope = match &plan.query {
            Some(QuerySpec::Mise(query)) => query.namespace.data.clone(),
            _ => crate::resource_software::mise_scope(ctx),
        };
        lines.push(format!(
            "mise 工具 {}\n安装范围 {}",
            safe(package),
            safe(&scope.display().to_string())
        ));
        if matches!(
            plan.action,
            crate::domain::Action::Install | crate::domain::Action::Upgrade
        ) {
            lines.push("安装并更新用户级配置中的版本".into());
        }
    }
    if let Some(release) = crate::mise_owner::retirement::bound_query(plan) {
        lines.push(format!(
            "移除版本 {} · {}",
            safe(&release.version),
            safe(&release.payload.display().to_string())
        ));
        lines.push(format!(
            "查询目录 {} · 此处未启用，其他项目仍可能使用此版本",
            safe(&ctx.home.display().to_string())
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
