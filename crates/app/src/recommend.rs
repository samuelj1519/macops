use crate::output::TerminalLine;
use crate::terminal::feedback as terminal;
use crate::{
    context::Context,
    domain::{Package, Provider},
    error::{Error, Result},
    inventory,
    paths::Directory,
    policy::recommend::{self as policy, Candidate, Scope},
    profile_install::{self, Event, InitialActivation, Selection},
    resource_agents::InstallTarget,
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
    terminal(
        TerminalLine::Heading,
        &format!("安装建议 · {}", safe(&data["query"])),
    );
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
    if let Some(hint) = recommendation_keychain_hint(data["model"]["status"].as_str()) {
        terminal(TerminalLine::Note, hint);
    }
    show_codex_market(&data["codexMarketplace"]);
    let known = data["known"].as_array().map_or(&[][..], Vec::as_slice);
    if let Some(reason) = data["reason"].as_str() {
        terminal(TerminalLine::Note, &source_notice(reason));
    }
    let installs = install_choices(ctx, &data, known)?;
    if installs.is_empty() {
        if let Some(repo) = search_repository(ctx, &data)? {
            return choose(
                ctx,
                crate::recommend_upstream::show_repository(ctx, &repo, false, true)?,
            );
        }
        eprintln!("当前没有可执行的已核验安装计划");
        return Ok(data);
    }
    for (index, choice) in installs.iter().enumerate() {
        eprintln!("  {} {}", index + 1, choice.label());
    }
    eprintln!("选择安装候选（Enter 返回）");
    let Some(package) = choice_index(installs.len())?.and_then(|index| installs.get(index)) else {
        return Ok(json!({"mode":"cancelled"}));
    };
    install_choice(ctx, package)
}

fn install_choice(ctx: &Context, choice: &InstallChoice) -> Result<Value> {
    install_choice_using(
        ctx,
        choice,
        |client| choose_plugin_targets_using(ctx, client, target_numbers),
        confirm_initial_activation,
        |activation, target| install_at_profile(ctx, choice, activation, target),
    )
}

fn install_choice_using(
    ctx: &Context,
    choice: &InstallChoice,
    select: impl FnOnce(&str) -> Result<Vec<Destination>>,
    confirm: impl FnOnce() -> Result<InitialActivation>,
    install: impl FnMut(InitialActivation, InstallTarget) -> Result<Value>,
) -> Result<Value> {
    let client = match choice {
        InstallChoice::Codex(..) => Some("codex"),
        InstallChoice::Claude(..) => Some("claude"),
        _ => None,
    };
    if let Some(client) = client {
        let destinations = select(client)?;
        match destinations.as_slice() {
            [] => return Ok(json!({"mode":"cancelled"})),
            [Destination::Independent] => {
                return install_isolated(ctx, choice, confirm);
            }
            _ => {
                let selected = destinations
                    .into_iter()
                    .map(|item| match item {
                        Destination::Existing(selection) => Ok(selection),
                        Destination::Independent => Err(incompatible_profile_offer()),
                    })
                    .collect::<Result<Vec<_>>>()?;
                return install_profiles_using(ctx, choice, &selected, confirm, install);
            }
        }
    }
    let version = if let InstallChoice::Application(package) = choice
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
        InstallChoice::Application(package) => install_selected(ctx, package, version.as_deref()),
        InstallChoice::Skill(recipe) => Ok(report_install(
            crate::native_skills::install_recommended(ctx, (**recipe).clone())?,
        )),
        InstallChoice::Codex(..) | InstallChoice::Claude(..) => Err(incompatible_profile_offer()),
    }
}

enum Destination {
    Independent,
    Existing(Selection),
}

fn choose_plugin_targets_using(
    ctx: &Context,
    client: &str,
    select: impl FnOnce(usize) -> Result<Vec<usize>>,
) -> Result<Vec<Destination>> {
    use crate::{
        browser::sanitize,
        resource::{Data, Management},
        resource_agents,
    };
    let mut profiles = resource_agents::profile_roots_matching(ctx, |kind, _| kind == client)?;
    profiles.sort_by(|a, b| {
        (b.management == Management::Adopted)
            .cmp(&(a.management == Management::Adopted))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let mut choices = vec![];
    terminal(TerminalLine::Heading, "安装位置");
    for profile in profiles {
        let Data::Agent { config_dir, .. } = &profile.data else {
            continue;
        };
        let name = sanitize(
            &resource_agents::profile_label(&profile.name, config_dir),
            false,
        );
        let path = sanitize(&config_dir.display().to_string(), false);
        let Ok(target) = resource_agents::InstallTarget::capture(ctx, &profile) else {
            eprintln!("  {name} · {path} · 暂不能核验目录");
            continue;
        };
        let registered = profile.management == Management::Adopted;
        let status = match (registered, resource_agents::managed_root(config_dir)) {
            (true, true) => "单独配置 · 已纳入管理",
            (true, false) => "已纳入管理",
            (false, _) => "可登记",
        };
        choices.push(Some(Destination::Existing(Selection {
            target: Box::new(target),
            registered,
        })));
        eprintln!("  {} {name} · {path} · {status}", choices.len());
    }
    choices.push(Some(Destination::Independent));
    eprintln!("  {} 新建单独配置", choices.len());
    eprintln!("输入一个或多个编号，用空格或逗号分隔（例如 3,5）。直接按 Enter 返回。");
    Ok(select(choices.len())?
        .into_iter()
        .filter_map(|index| choices.get_mut(index).and_then(Option::take))
        .collect())
}

fn parse_target_numbers(input: &str, count: usize) -> Option<Vec<usize>> {
    let mut selected = Vec::new();
    for item in input
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | '，'))
        .filter(|item| !item.is_empty())
    {
        let number = item
            .parse::<usize>()
            .ok()
            .filter(|value| (1..=count).contains(value))?;
        if !selected.contains(&(number - 1)) {
            selected.push(number - 1);
        }
    }
    if selected.is_empty() || (selected.len() > 1 && selected.contains(&(count - 1))) {
        return None;
    }
    Some(selected)
}

fn target_numbers(count: usize) -> Result<Vec<usize>> {
    loop {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if input.trim().is_empty() {
            return Ok(Vec::new());
        }
        if let Some(selected) = parse_target_numbers(&input, count) {
            return Ok(selected);
        }
        eprintln!("请输入列表里的编号。「新建单独配置」需要单独选择。");
    }
}

fn source_notice(reason: &str) -> String {
    match reason {
        "recommend_github_rate_limited" => {
            "GitHub 匿名 API 额度已用完。请等额度恢复后重试。".into()
        }
        "recommend_github_not_found" => {
            "GitHub 找不到可公开访问的仓库或文件。请核对仓库名和访问权限。".into()
        }
        _ => format!("来源核对未完成 · {reason}"),
    }
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

fn target_label(target: &crate::resource_agents::InstallTarget) -> String {
    crate::browser::sanitize(
        &crate::resource_agents::profile_label(&target.name, &target.directory.canonical),
        false,
    )
}

fn install_profiles_using(
    ctx: &Context,
    choice: &InstallChoice,
    targets: &[Selection],
    confirm_installation: impl FnOnce() -> Result<InitialActivation>,
    install: impl FnMut(InitialActivation, InstallTarget) -> Result<Value>,
) -> Result<Value> {
    let (plugin, version, url, commit) = choice.plugin_info()?;
    profile_install::execute(
        ctx,
        targets,
        || {
            terminal(TerminalLine::Heading, &format!("安装 {plugin} · {version}"));
            terminal(TerminalLine::Step, &format!("来源 {url} · {commit}"));
            let mut enroll = Vec::new();
            for selection in targets {
                let target = &selection.target;
                let label = target_label(target);
                eprintln!(
                    "  {label} · {}",
                    crate::browser::sanitize(
                        &target.directory.canonical.display().to_string(),
                        false
                    )
                );
                if !selection.registered {
                    enroll.push(label);
                }
            }
            if !enroll.is_empty() {
                terminal(
                    TerminalLine::Note,
                    &format!("确认后会登记并安装到 {}。", enroll.join("、")),
                );
            }
            eprintln!("已经装好的插件保留当前开关。");
            confirm_installation()
        },
        install,
        show_profile_install_event,
    )
}

fn show_profile_install_event(event: Event<'_>) {
    match event {
        Event::Reconciled => terminal(TerminalLine::Note, "已检查上一次安装。"),
        Event::Complete(raw) => show_install_result(raw),
        Event::Stopped {
            target,
            completed,
            remaining,
            pending,
        } => {
            let label = target_label(target);
            if let Some(pending) = pending {
                terminal(TerminalLine::Note, &format!("{label} 尚未完成。"));
                if pending.continuation {
                    eprintln!("上一步已核对。请用同样的安装选项重试。");
                } else {
                    let journal = &pending.raw["journal"];
                    if let Some(message) =
                        journal["message"].as_str().filter(|text| !text.is_empty())
                    {
                        eprintln!("{}", crate::browser::sanitize(message, false));
                    }
                    if let Some(hint) = crate::output::recovery_hint(journal) {
                        eprintln!("{hint}");
                    }
                }
            } else {
                terminal(TerminalLine::Note, &format!("{label} 安装停止。"));
            }
            if completed > 0 {
                eprintln!("已完成 {completed} 个配置的安装。");
            }
            if !remaining.is_empty() {
                let labels: Vec<_> = remaining
                    .iter()
                    .map(|item| target_label(&item.target))
                    .collect();
                eprintln!("尚未开始本次安装 {}", labels.join("、"));
            }
        }
        Event::BatchComplete(count) => terminal(
            TerminalLine::Done,
            &format!("已完成 {count} 个配置的安装。"),
        ),
    }
}

fn install_at_profile(
    ctx: &Context,
    choice: &InstallChoice,
    activation: InitialActivation,
    target: crate::resource_agents::InstallTarget,
) -> Result<Value> {
    match choice {
        InstallChoice::Codex(offer, binary) => crate::codex_install::install_at(
            ctx,
            (**offer).clone(),
            binary.clone(),
            activation,
            target,
            crate::terminal::report_install_stage,
        ),
        InstallChoice::Claude(offer, binary) => crate::claude_install::install_at(
            ctx,
            (**offer).clone(),
            binary.clone(),
            activation == InitialActivation::Enabled,
            target,
            crate::terminal::report_install_stage,
        ),
        _ => Err(incompatible_profile_offer()),
    }
}

fn incompatible_profile_offer() -> Error {
    Error::new(
        "plugin_target_unsupported",
        "请选择可以安装到 Agent 档案的插件。",
    )
}

fn choice_index(count: usize) -> Result<Option<usize>> {
    let mut input = String::new();
    loop {
        input.clear();
        std::io::stdin().read_line(&mut input)?;
        let value = input.trim();
        if value.is_empty() {
            return Ok(None);
        }
        if let Ok(number) = value.parse::<usize>()
            && (1..=count).contains(&number)
        {
            return Ok(Some(number - 1));
        }
        eprintln!("请输入列表中的编号，或按 Enter 返回。");
    }
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
    Skill(Box<crate::policy::native_skills::Recipe>),
    Codex(Box<crate::codex_install_source::Offer>, std::path::PathBuf),
    Claude(Box<crate::claude_install_source::Offer>, std::path::PathBuf),
}

impl InstallChoice {
    fn plugin_info(&self) -> Result<(String, &str, String, &str)> {
        match self {
            Self::Codex(offer, _) => Ok((
                offer.native_id(),
                &offer.declared_version,
                offer.url()?,
                &offer.payload_commit,
            )),
            Self::Claude(offer, _) => Ok((
                offer.native_id(),
                &offer.version,
                offer.url()?,
                &offer.commit,
            )),
            _ => Err(incompatible_profile_offer()),
        }
    }

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
            Self::Codex(offer, _) => {
                format!("{} · Codex · {}", offer.native_id(), offer.declared_version)
            }
            Self::Claude(offer, _) => {
                format!("{} · Claude Code · {}", offer.native_id(), offer.version)
            }
        }
    }
}

fn install_choices(ctx: &Context, data: &Value, known: &[Value]) -> Result<Vec<InstallChoice>> {
    install_choices_using(
        ctx,
        data,
        known,
        crate::codex_install::binary,
        crate::claude_install::binary,
    )
}

fn install_choices_using(
    ctx: &Context,
    data: &Value,
    known: &[Value],
    codex_binary: impl FnOnce(&Context) -> Result<std::path::PathBuf>,
    claude_binary: impl FnOnce(&Context) -> Result<std::path::PathBuf>,
) -> Result<Vec<InstallChoice>> {
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
        && let Ok(binary) = codex_binary(ctx)
    {
        choices.push(InstallChoice::Codex(Box::new(offer), binary));
    }
    if let Ok(offer) = crate::claude_install_source::Offer::parse(&data["claudeMarketplace"])
        && let Ok(binary) = claude_binary(ctx)
    {
        choices.push(InstallChoice::Claude(Box::new(offer), binary));
    }
    Ok(choices)
}

fn install_selected(ctx: &Context, package: &Package, version: Option<&str>) -> Result<Value> {
    use crate::{browser::sanitize, domain::Action, operations};
    if matches!(package.provider, Provider::BrewFormula { .. }) {
        return Ok(report_install(crate::brew::execute_selected(
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

fn report_install(result: Value) -> Value {
    show_install_result(&result);
    result
}

fn show_install_result(result: &Value) {
    use crate::browser::sanitize;
    if crate::policy::cli::continuation(result).is_some() {
        eprintln!(
            "{}",
            crate::operations::recovery_text(result, false).trim_end()
        );
        return;
    }
    let journal = result.get("journal").unwrap_or(result);
    let phase = journal["phase"].as_str().unwrap_or_default();
    terminal(
        if phase == "succeeded" {
            TerminalLine::Done
        } else {
            TerminalLine::Note
        },
        &format!("结果 {}", crate::output::phase_text(phase)),
    );
    if let Some(message) = journal["message"].as_str() {
        eprintln!("{}", sanitize(message, false));
    }
    if let Some(hint) = crate::output::recovery_hint(journal) {
        eprintln!("{hint}");
    }
    if let (Some(name), Some(path)) = (result["targetName"].as_str(), result["profile"].as_str()) {
        eprintln!(
            "安装位置 {} · {}",
            sanitize(
                &crate::resource_agents::profile_label(name, Path::new(path)),
                false
            ),
            sanitize(path, false)
        );
    }
    if let Some(path) = result["codexHome"].as_str() {
        eprintln!("Codex 档案 {}", sanitize(path, false));
        eprintln!("运行 mo agent 查看已纳入管理的单独配置。");
    } else if let Some(path) = result["claudeConfigDir"].as_str() {
        eprintln!("Claude Code 档案 {}", sanitize(path, false));
        eprintln!("运行 mo agent 查看已纳入管理的单独配置。");
    }
}

fn install_isolated(
    ctx: &Context,
    choice: &InstallChoice,
    confirm: impl FnOnce() -> Result<InitialActivation>,
) -> Result<Value> {
    install_isolated_using(ctx, choice, confirm, |activation| {
        let progress = crate::terminal::report_install_stage;
        match choice {
            InstallChoice::Codex(offer, binary) => crate::codex_install::install(
                ctx,
                (**offer).clone(),
                binary.clone(),
                activation,
                progress,
            ),
            InstallChoice::Claude(offer, binary) => crate::claude_install::install(
                ctx,
                (**offer).clone(),
                binary.clone(),
                activation == InitialActivation::Enabled,
                progress,
            ),
            _ => Err(incompatible_profile_offer()),
        }
    })
}

fn install_isolated_using(
    ctx: &Context,
    choice: &InstallChoice,
    confirm: impl FnOnce() -> Result<InitialActivation>,
    install: impl FnOnce(InitialActivation) -> Result<Value>,
) -> Result<Value> {
    let (plugin, version, url, commit) = choice.plugin_info()?;
    terminal(
        TerminalLine::Heading,
        &format!("在新建的单独配置中安装 {plugin} · {version}"),
    );
    terminal(TerminalLine::Step, &format!("来源 {url} · {commit}"));
    let activation = confirm()?;
    let mut same_install = false;
    if let Some(recovered) =
        crate::operations::recover_before_write_checked(ctx, None, |journal| {
            if !crate::plugin_target::recovery_matches(journal, None)? {
                return Ok(false);
            }
            let previous = match choice {
                InstallChoice::Codex(offer, _) => {
                    crate::codex_install::matching_activation(journal, offer, None)?
                }
                InstallChoice::Claude(offer, _) => {
                    crate::claude_install::matching_activation(journal, offer, None)?
                }
                _ => return Err(incompatible_profile_offer()),
            };
            if let Some(previous) = previous {
                if previous != activation {
                    let option = if previous == InitialActivation::Enabled {
                        "yes"
                    } else {
                        "disabled"
                    };
                    return Err(Error::attention(
                        "plugin_install_activation_changed",
                        format!("上次安装使用了不同的启停设置。请重新运行安装并输入 {option}。"),
                    ));
                }
                same_install = true;
            }
            Ok(true)
        })?
    {
        if recovered.succeeded && same_install {
            if matches!(choice, InstallChoice::Codex(..))
                && recovered.result["continuationRequired"] == true
            {
                let original = &recovered.original;
                let current = crate::operations::validated_recovery_journal(ctx, &original.id)?;
                if current.plan_sha256 != original.plan_sha256 {
                    return Err(Error::attention(
                        "plan_stale",
                        "原安装记录已经改变。请重新核对。",
                    ));
                }
                return Ok(report_install(crate::codex_install::resume(
                    ctx,
                    &current,
                    false,
                    true,
                    crate::terminal::report_install_stage,
                )?));
            }
            return Ok(report_install(recovered.result));
        }
        terminal(TerminalLine::Note, "已检查上一次操作。现在准备本次安装。");
    }
    Ok(report_install(install(activation)?))
}

fn confirm_initial_activation() -> Result<InitialActivation> {
    terminal(
        TerminalLine::Note,
        "输入 yes 安装并启用，输入 disabled 安装后保持停用，回车取消。",
    );
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
