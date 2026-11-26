mod candidate;
mod inventory_view;
mod keychain;
mod model;
mod model_policy;
mod setup;
mod upstream;

use crate::output::TerminalLine;
use crate::terminal::feedback as terminal;
use crate::{
    agent::plugin_install,
    catalog::inventory,
    context::Context,
    domain::{Package, Provider},
    error::{Error, Result},
    platform::fs::Directory,
    policy::public_source::{domain_link, repository_subject},
    skills::native_skills::{self, RepositoryAction},
    terminal::{
        install::report_install,
        plan::{plan_scope, source_label},
        presentation::panel,
    },
};
use candidate::{self as policy, Candidate, Scope};
use serde_json::{Value, json};
pub(crate) use setup::run as run_setup;
use sha2::{Digest, Sha256};
use std::path::Path;

pub(crate) fn public_repository(ctx: &Context, repository: &str) -> Result<Value> {
    upstream::show_repository(ctx, repository, false, false)
}

pub(crate) fn run(
    cli: &crate::cli::Cli,
    ctx: &Context,
    product: Option<&str>,
    evidence: Option<&std::path::Path>,
    github: Option<&str>,
    model: bool,
    keychain_account: Option<&str>,
) -> Result<Value> {
    let Some(product) = product else {
        return Err(Error::new(
            "recommend_target_required",
            "recommend 需要一个目标，或使用 --setup 配置模型",
        ));
    };
    let interactive = interactive(cli);
    let repository = if evidence.is_none() && github.is_none() && !model {
        repository_subject(product.trim())
    } else {
        None
    };
    let data = if let Some(repository) = repository {
        let scan = || upstream::show_repository(ctx, repository, cli.dry_run, interactive);
        if interactive {
            crate::terminal::progress::during("核对仓库的安装方式", scan)?
        } else {
            scan()?
        }
    } else {
        show(
            ctx,
            product,
            evidence,
            github,
            model,
            keychain_account,
            cli.dry_run,
        )?
    };
    if interactive {
        choose(ctx, data, cli.verbose > 0)
    } else {
        Ok(data)
    }
}

const EVIDENCE_LIMIT: u64 = 128 * 1024;

fn show(
    ctx: &Context,
    product: &str,
    evidence: Option<&Path>,
    github: Option<&str>,
    model: bool,
    account: Option<&str>,
    dry_run: bool,
) -> Result<Value> {
    let query = policy::query_name(product)?;
    let account = account.map(model_policy::account).transpose()?;
    if let Some(path) = evidence {
        let mut result = from_evidence(query, path)?;
        result["model"] = json!({"status":if model {"local-evidence-only"} else {"not-requested"}});
        return Ok(result);
    }
    if let Some(repository) = github {
        return upstream::show(ctx, query, repository, dry_run);
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
            "source": domain_link(&item.official_url).then_some(&item.official_url),
            "candidate": candidate,
            "matchesPublicChannel":shares_public_channel,
            "preview":preview_resource(&item),
        }));
    }
    let annotation = match (model, dry_run, public.as_ref()) {
        (false, _, _) => json!({"status":"not-requested"}),
        (true, true, _) => json!({"status":"dry-run"}),
        (true, false, Some(analysis)) => model::annotate(ctx, analysis, account),
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
    } else if crate::policy::operation::manifest::supports_replay(provider) {
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

fn choose(ctx: &Context, data: Value, verbose: bool) -> Result<Value> {
    terminal(
        TerminalLine::Heading,
        &format!("安装建议 · {}", safe_text(&data["query"])),
    );
    let known = data["known"].as_array().map_or(&[][..], Vec::as_slice);
    let installs = install_choices(ctx, &data, known, || plugin_install::offers(ctx, &data))?;
    let local = inventory_view::Related::read(ctx, &data);
    if installs.is_empty()
        && local.views.is_empty()
        && let Some(repo) = search_repository(ctx, &data)?
    {
        return choose(
            ctx,
            upstream::show_repository(ctx, &repo, false, true)?,
            verbose,
        );
    }
    panel("本机状态", local.status().trim_end());
    let notes = if verbose {
        recommendation_sections(&data, &installs)
    } else {
        installation_notes(&data, &installs)
    };
    if notes.is_empty() {
        terminal(TerminalLine::Section, "安装方案");
    } else {
        panel("安装方案", notes.trim_end());
    }
    if !local.views.is_empty() {
        panel("维护入口", local.maintenance(installs.len() + 1).trim_end());
    }
    if verbose {
        show_model_details(&data);
        plugin_install::show_codex_market(&data["codexMarketplace"]);
    }
    if (verbose || (installs.is_empty() && data["manualInstall"].is_null()))
        && let Some(hint) = recommendation_model_hint(&data["model"])
    {
        terminal(TerminalLine::Note, hint);
    }
    if installs.is_empty() && local.views.is_empty() {
        return Ok(data);
    }
    if let Some(reason) = data["reason"].as_str() {
        terminal(TerminalLine::Note, &source_notice(reason));
    }
    let prompt = if installs.is_empty() {
        "选择维护入口"
    } else if local.views.is_empty() {
        "选择安装候选"
    } else {
        "选择安装候选或维护入口"
    };
    let labels = installs
        .iter()
        .map(InstallChoice::label)
        .chain(local.views.iter().map(|(kind, _)| {
            if *kind == crate::browser::Kind::Apps {
                "查看和维护软件".into()
            } else {
                "查看技能与插件".into()
            }
        }))
        .enumerate()
        .map(|(index, label)| format!("{} {label}", index + 1))
        .collect::<Vec<_>>();
    let Some(index) = choose_label(labels, prompt)? else {
        return Ok(json!({"mode":"cancelled"}));
    };
    let install_count = installs.len();
    if index < install_count
        && let Some(offer) = installs.into_iter().nth(index)
    {
        return install_choice(ctx, offer);
    }
    if let Some((kind, view)) = local.views.into_iter().nth(index - install_count) {
        crate::browser::browse_related(ctx, kind, view)?;
    }
    Ok(data)
}

fn safe_text(value: &Value) -> String {
    crate::browser::sanitize(value.as_str().unwrap_or_default(), false)
}

fn recommendation_sections(data: &Value, choices: &[InstallChoice]) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    if !choices.is_empty() {
        text.push_str("可直接安装\n");
        for (index, choice) in choices.iter().enumerate() {
            let _ = writeln!(text, "  {} {}", index + 1, choice.label());
        }
    }
    text.push_str(&installation_notes(data, choices));
    text
}

fn installation_notes(data: &Value, choices: &[InstallChoice]) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    let has_claude = choices.iter().any(|choice| {
        matches!(
            choice,
            InstallChoice::Plugin(plugin_install::Offer::Claude(..))
        )
    });
    let has_shared = choices
        .iter()
        .any(|choice| matches!(choice, InstallChoice::PublicSkills(_, Ok(_))));
    let manual = manual_commands(data, has_claude);
    if !manual.is_empty() {
        let heading = if has_shared {
            "项目或其他客户端"
        } else {
            "手动安装"
        };
        let _ = writeln!(text, "{heading}{manual}");
    }
    if let Some(documents) = data["documents"].as_array().filter(|rows| !rows.is_empty()) {
        text.push_str("来源文档\n");
        let mut seen = std::collections::BTreeSet::new();
        for doc in documents {
            if seen.insert(safe_text(&doc["url"])) {
                let _ = writeln!(
                    text,
                    "  {}  {}",
                    safe_text(&doc["path"]),
                    safe_text(&doc["url"])
                );
            }
        }
    }
    if choices.is_empty() && manual.is_empty() {
        let _ = writeln!(text, "{}", no_install_plan(data));
    }
    text
}

fn show_model_details(data: &Value) {
    let documents = data["documents"].as_array().map_or(&[][..], Vec::as_slice);
    if let Some(candidates) = data["model"]["candidates"]
        .as_array()
        .filter(|rows| !rows.is_empty())
    {
        eprintln!("模型说明");
        for candidate in candidates {
            eprintln!(
                "  {} · {} · {} · {} · 版本 {}",
                safe_text(&candidate["name"]),
                safe_text(&candidate["kind"]),
                safe_text(&candidate["adapter"]),
                safe_text(&candidate["scope"]),
                crate::browser::sanitize(candidate["version"].as_str().unwrap_or("待核对"), false)
            );
            eprintln!("    {}", safe_text(&candidate["summary"]));
            if let Some(document) = candidate["document"]
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .and_then(|index| documents.get(index))
            {
                eprintln!("    证据 {}", safe_text(&document["url"]));
            }
        }
    }
}

fn manual_commands(data: &Value, has_claude: bool) -> String {
    use std::fmt::Write as _;
    let documents = data["documents"].as_array().map_or(&[][..], Vec::as_slice);
    let mut text = String::new();
    for item in data["manualInstall"].as_array().into_iter().flatten() {
        if let (Some(manager), Some(document), Some(commands)) = (
            item["manager"].as_str(),
            item["document"]
                .as_u64()
                .and_then(|i| usize::try_from(i).ok())
                .and_then(|i| documents.get(i)),
            item["commands"].as_array(),
        ) && let Some(path) = document["path"].as_str()
            && !(has_claude && manager == "Claude Code")
        {
            let _ = write!(
                text,
                "\n  {} · {}",
                crate::browser::sanitize(manager, false),
                crate::browser::sanitize(path, false)
            );
            for command in commands.iter().filter_map(Value::as_str) {
                let _ = write!(text, "\n    {}", crate::browser::sanitize(command, false));
            }
            if manager == "Skills CLI"
                && commands
                    .iter()
                    .filter_map(Value::as_str)
                    .any(|command| command.split_ascii_whitespace().count() == 4)
            {
                text.push_str("\n    按 Skills CLI 提示多选客户端，再确认安装范围。");
            }
        }
    }
    text
}

fn no_install_plan(data: &Value) -> String {
    use crate::browser::sanitize;
    use std::fmt::Write as _;
    let mut text = String::from("错误表现　当前没有可执行的已核验安装计划");
    let documents = data["documents"].as_array().map_or(&[][..], Vec::as_slice);
    if documents.is_empty() {
        if let Some(reason) = data["reason"].as_str() {
            let _ = write!(text, "\n错误原因　{}", source_notice(reason));
            let usage = match reason {
                "recommend_github_rate_limited" => "等 GitHub 查询额度恢复后，再运行原命令。",
                "recommend_github_not_found" => {
                    "核对 GitHub 仓库名称和访问权限，再运行 mo recommend OWNER/REPO。"
                }
                _ => "核对来源地址和网络连接，再运行 mo recommend OWNER/REPO。",
            };
            let _ = write!(text, "\n正确用法　{usage}");
        } else {
            text.push_str("\n错误原因　没有可核对的安装说明或本机配方。");
            if let Some(repo) = data["repository"].as_str().and_then(repository_subject) {
                let _ = write!(
                    text,
                    "\n正确用法　打开 https://github.com/{repo} 查找项目的安装说明。"
                );
            } else {
                text.push_str(
                    "\n正确用法　已知 GitHub 仓库时，运行 mo recommend OWNER/REPO 查看来源。",
                );
            }
        }
        return text;
    }
    text.push_str("\n错误原因　找到了项目的安装说明，但缺少 mo 可直接执行的配方或插件验证条件。");
    let path = documents
        .iter()
        .find(|document| document["path"] == "INSTALL.md")
        .or_else(|| documents.first())
        .and_then(|document| document["path"].as_str())
        .unwrap_or("项目文档");
    let _ = write!(
        text,
        "\n正确用法　打开上面的 {}，按你使用的客户端选择安装方式。",
        sanitize(path, false)
    );
    text
}

fn install_choice(ctx: &Context, choice: InstallChoice) -> Result<Value> {
    let version = if let InstallChoice::Application(package) = &choice
        && !matches!(package.provider, Provider::BrewFormula { .. })
        && (package.exact_version_required || matches!(package.provider, Provider::Mise { .. }))
    {
        eprintln!("输入精确版本（Enter 返回）");
        let mut value = String::new();
        std::io::stdin().read_line(&mut value)?;
        let value = value.trim();
        if value.is_empty() {
            return Ok(json!({"mode":"cancelled"}));
        }
        Some(value.to_owned())
    } else {
        None
    };
    match choice {
        InstallChoice::Application(package) => install_selected(ctx, &package, version.as_deref()),
        InstallChoice::Skill(recipe) => Ok(report_install(native_skills::install_recommended(
            ctx, *recipe,
        )?)),
        InstallChoice::PublicSkills(source, action) => public_skill_action(ctx, &source, action?),
        InstallChoice::Plugin(offer) => plugin_install::run(ctx, &offer),
    }
}

fn public_skill_action(ctx: &Context, source: &str, action: RepositoryAction) -> Result<Value> {
    let operation = match action {
        RepositoryAction::Install => crate::domain::resource::CapabilityAction::Install,
        RepositoryAction::Adopt | RepositoryAction::Upgrade => {
            crate::domain::resource::CapabilityAction::Upgrade
        }
    };
    if let Some(report) = crate::skills::portfolio::recommend(ctx, source, operation)? {
        return Ok(report);
    }
    let show = |title: &str, report: &Value| {
        if let Some(summary) = crate::output::skill_journey(report) {
            panel(title, summary.trim_end());
        }
    };
    let upgrade = || {
        let report = native_skills::upgrade_repository(ctx, source)?;
        show("更新结果", &report);
        Ok::<_, Error>(report)
    };
    match action {
        RepositoryAction::Install => {
            let report = native_skills::install_repository(ctx, source)?;
            show("安装结果", &report);
            Ok(report)
        }
        RepositoryAction::Adopt => {
            let report = native_skills::adopt_repository(ctx, source)?;
            after_skill_adoption(report, |report| show("接管结果", report), upgrade)
        }
        RepositoryAction::Upgrade => upgrade(),
    }
}

fn after_skill_adoption(
    mut report: Value,
    show: impl FnOnce(&Value),
    upgrade: impl FnOnce() -> Result<Value>,
) -> Result<Value> {
    let complete = report["incomplete"] == false || report["status"] == "already-managed";
    if complete {
        report["target"] = Value::Null;
    }
    show(&report);
    if !complete {
        return Ok(report);
    }
    match upgrade() {
        Err(error) if error.code == "cancelled" => {
            terminal(TerminalLine::Note, "已保留当前版本。");
            Ok(report)
        }
        result => result,
    }
}

fn source_notice(reason: &str) -> String {
    match reason {
        "recommend_github_rate_limited" => "GitHub 匿名 API 查询额度已用完。".into(),
        "recommend_github_not_found" => "GitHub 找不到可公开访问的仓库或文件。".into(),
        _ => format!("来源核对未完成 · {reason}"),
    }
}

fn recommendation_model_hint(model: &Value) -> Option<&'static str> {
    let status = model["status"].as_str()?;
    recommendation_keychain_hint(Some(status)).or_else(|| match status {
        "profile-unavailable" => Some("推荐模型配置暂不可用。运行 mo recommend --setup 检查设置。"),
        "transport-unavailable" => Some("推荐模型连接失败。检查网络，或按来源文档继续。"),
        "deadline" => Some("推荐模型请求超时。可以重试，也可以按来源文档继续。"),
        "http-status" => {
            Some("推荐模型服务返回了错误。运行 mo recommend --setup 并选 t 测试连接。")
        }
        "output-limit" | "unverified-response" => Some("推荐模型回答无法核对。请按来源文档继续。"),
        "suggested" if model["candidates"].as_array().is_some_and(Vec::is_empty) => {
            Some("推荐模型没有返回可引用的安装方式。请按来源文档继续。")
        }
        _ => None,
    })
}

fn recommendation_keychain_hint(status: Option<&str>) -> Option<&'static str> {
    match status? {
        "keychain-authorization-required" => Some(
            "钥匙串需要授权或解锁，已继续使用本地分析。运行 mo recommend --setup 后选 t，在 macOS 提示中允许访问。",
        ),
        "keychain-missing" => Some(
            "钥匙串找不到已保存的推荐模型密钥，已继续使用本地分析。运行 mo recommend --setup 重新设置或绑定密钥。",
        ),
        "keychain-unavailable" => Some(
            "推荐模型凭据暂不可读取，已继续使用本地分析。运行 mo recommend --setup 后选 t 检查已保存的密钥。",
        ),
        _ => None,
    }
}

fn choose_label(mut labels: Vec<String>, prompt: &str) -> Result<Option<usize>> {
    let back = labels.len();
    labels.push("返回".into());
    let chosen = crate::terminal::picker::select(&labels, back, false, prompt, |typed| {
        typed
            .parse::<usize>()
            .ok()
            .filter(|number| (1..=back).contains(number))
            .map(|number| vec![number - 1])
    })?;
    Ok(chosen.into_iter().next().filter(|index| *index < back))
}

fn choice_index(count: usize) -> Result<Option<usize>> {
    choose_label(
        (1..=count).map(|index| format!("仓库 {index}")).collect(),
        "",
    )
}

fn search_repository(ctx: &Context, data: &Value) -> Result<Option<String>> {
    search_repository_using(
        data,
        ctx.isolated,
        |name| upstream::search(ctx, name),
        choice_index,
    )
}

fn search_repository_using(
    data: &Value,
    isolated: bool,
    search: impl FnOnce(&str) -> Option<Result<Vec<upstream::RepositoryMatch>>>,
    choice: impl FnOnce(usize) -> Result<Option<usize>>,
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
    Ok(choice(repositories.len())?
        .and_then(|index| repositories.get(index))
        .map(|entry| entry.name.clone()))
}

enum InstallChoice {
    Application(Box<Package>),
    Skill(Box<crate::skills::rules::native_skills::Recipe>),
    PublicSkills(String, Result<RepositoryAction>),
    Plugin(plugin_install::Offer),
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
            Self::Plugin(offer) => offer.label(),
            Self::PublicSkills(source, Ok(RepositoryAction::Install)) => {
                format!("安装技能 · {source} · 用户共享目录")
            }
            Self::PublicSkills(_, Ok(RepositoryAction::Adopt)) => {
                "检查并更新已有技能 · 用户共享目录".into()
            }
            Self::PublicSkills(source, Ok(RepositoryAction::Upgrade)) => {
                format!("检查并更新技能 · {source} · 用户共享目录")
            }
            Self::PublicSkills(_, Err(_)) => "查看共享技能的处理办法".into(),
        }
    }
}

fn install_choices(
    ctx: &Context,
    data: &Value,
    known: &[Value],
    plugin_offers: impl FnOnce() -> Vec<plugin_install::Offer>,
) -> Result<Vec<InstallChoice>> {
    let declared = crate::catalog::inventory::declared_catalog(ctx)?;
    let mut choices = Vec::new();
    for row in known {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        if let Some(package) = declared.packages.iter().find(|p| p.id == id)
            && install_channel(&package.provider).is_some()
            && (!matches!(package.provider, Provider::Mise { .. })
                || crate::catalog::resource_inventory::native_program(ctx, "mise").is_ok())
        {
            choices.push(InstallChoice::Application(Box::new(package.clone())));
        }
    }
    if let Some(subject) = data["query"].as_str() {
        match crate::skills::native_skills::recommended_recipes(ctx, subject) {
            Ok(recipes) => choices.extend(
                recipes
                    .into_iter()
                    .map(|recipe| InstallChoice::Skill(Box::new(recipe))),
            ),
            Err(_) => eprintln!("已登记的 Skills CLI 配方暂不可核对"),
        }
    }

    if native_skills::has_supported_runner(ctx)
        && let Some(source) = documented_skill_source(data)
    {
        let action = native_skills::repository_action(ctx, &source);
        if !matches!(action, Ok(RepositoryAction::Install))
            || !choices
                .iter()
                .any(|choice| matches!(choice, InstallChoice::Skill(_)))
        {
            choices.push(InstallChoice::PublicSkills(source, action));
        }
    }
    choices.extend(plugin_offers().into_iter().map(InstallChoice::Plugin));
    Ok(choices)
}

fn documented_skill_source(data: &Value) -> Option<String> {
    let repository = data["repository"]
        .as_str()
        .and_then(crate::policy::public_source::repository_subject)?;
    data["manualInstall"]
        .as_array()?
        .iter()
        .filter(|entry| entry["manager"] == "Skills CLI")
        .flat_map(|entry| entry["commands"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .find_map(|command| {
            let words: Vec<_> = command.split_ascii_whitespace().collect();
            let ["npx", "skills", "add", source] = words.as_slice() else {
                return None;
            };
            let repo = source.split_once('@').map_or(*source, |(repo, _)| repo);
            repo.eq_ignore_ascii_case(repository)
                .then(|| (*source).to_owned())
        })
}

fn install_selected(ctx: &Context, package: &Package, version: Option<&str>) -> Result<Value> {
    use crate::{browser::sanitize, domain::Action, operations};
    if matches!(package.provider, Provider::BrewFormula { .. }) {
        return Ok(report_install(crate::software::brew::execute_selected(
            ctx,
            package,
            Action::Install,
            false,
            false,
        )?));
    }
    if let Some(previous) = operations::recover_before_new_plan(ctx, false, |journal| {
        Ok(
            operations::matches_intent(journal, Action::Install, &package.id, version)
                && journal.plan.recipe_sha256 == crate::domain::fingerprint(package)?,
        )
    })? && previous.matches(Action::Install, &package.id, version)
        && previous.original.plan.recipe_sha256 == crate::domain::fingerprint(package)?
    {
        return Ok(report_install(previous.result));
    }
    let plan = operations::plan(ctx, package, Action::Install, version, false)?;
    eprintln!(
        "安装并纳入管理 {} · 版本 {} · {}",
        sanitize(&package.name, false),
        sanitize(plan.target.as_deref().unwrap_or(""), false),
        install_channel(&package.provider).unwrap_or_default(),
    );
    eprintln!("{}", plan_scope(ctx, &plan));
    operations::confirm("执行此安装计划", false)?;
    Ok(report_install(operations::execute(ctx, plan, false, true)?))
}

#[cfg(test)]
#[path = "../../tests/unit/recommend_catalog.rs"]
mod tests;
