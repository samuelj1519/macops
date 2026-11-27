use super::{
    profile_install::{self, Event, InitialActivation, Selection},
    resource_agents::InstallTarget,
};
use crate::{
    context::Context,
    error::{Error, Result},
    output::TerminalLine,
    terminal::{
        feedback as terminal,
        install::{report_install, show_install_result},
    },
};
use serde_json::{Value, json};

pub(crate) enum Offer {
    Codex(
        Box<crate::agent::codex_install_source::Offer>,
        std::path::PathBuf,
    ),
    Claude(
        Box<crate::agent::claude_install_source::Offer>,
        std::path::PathBuf,
    ),
}

impl Offer {
    pub(crate) fn plugin_info(&self) -> Result<(String, &str, String, &str)> {
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
        }
    }

    pub(crate) fn client(&self) -> &'static str {
        match self {
            Self::Codex(..) => "codex",
            Self::Claude(..) => "claude",
        }
    }

    pub(crate) fn label(&self) -> String {
        match self {
            Self::Codex(offer, _) => {
                format!("{} · Codex · {}", offer.native_id(), offer.declared_version)
            }
            Self::Claude(offer, _) => {
                format!("{} · Claude Code · {}", offer.native_id(), offer.version)
            }
        }
    }
}

pub(crate) fn offers(ctx: &Context, data: &Value) -> Vec<Offer> {
    offers_using(
        ctx,
        data,
        super::codex_install::binary,
        super::claude_install::binary,
    )
}

fn offers_using(
    ctx: &Context,
    data: &Value,
    codex_binary: impl FnOnce(&Context) -> Result<std::path::PathBuf>,
    claude_binary: impl FnOnce(&Context) -> Result<std::path::PathBuf>,
) -> Vec<Offer> {
    let mut choices = Vec::new();
    if let Ok(offer) = crate::agent::codex_install_source::Offer::parse(&data["codexMarketplace"])
        && let Ok(binary) = codex_binary(ctx)
    {
        choices.push(Offer::Codex(Box::new(offer), binary));
    }
    if let Ok(offer) = crate::agent::claude_install_source::Offer::parse(&data["claudeMarketplace"])
        && let Ok(binary) = claude_binary(ctx)
    {
        choices.push(Offer::Claude(Box::new(offer), binary));
    }
    choices
}

pub(crate) fn run(ctx: &Context, choice: &Offer) -> Result<Value> {
    run_using(
        ctx,
        choice,
        |client| choose_plugin_targets_using(ctx, client, Some(choice), target_numbers),
        confirm_initial_activation,
        |activation, target| {
            crate::terminal::progress::with_install_progress(|| {
                install_at_profile(ctx, choice, activation, target)
            })
        },
    )
}

fn run_using(
    ctx: &Context,
    choice: &Offer,
    select: impl FnOnce(&str) -> Result<Vec<Destination>>,
    confirm: impl FnOnce() -> Result<InitialActivation>,
    install: impl FnMut(InitialActivation, InstallTarget) -> Result<Value>,
) -> Result<Value> {
    let client = match choice {
        Offer::Codex(..) => "codex",
        Offer::Claude(..) => "claude",
    };
    let destinations = select(client)?;
    match destinations.as_slice() {
        [] => Ok(json!({"mode":"cancelled"})),
        [Destination::Independent] => install_isolated(ctx, choice, confirm),
        _ => {
            let selected = destinations
                .into_iter()
                .map(|item| match item {
                    Destination::Existing(selection) => Ok(selection),
                    Destination::Independent => Err(incompatible_profile_offer()),
                })
                .collect::<Result<Vec<_>>>()?;
            install_profiles_using(ctx, choice, &selected, confirm, install)
        }
    }
}

enum Destination {
    Independent,
    Existing(Selection),
}

fn choose_plugin_targets_using(
    ctx: &Context,
    client: &str,
    choice: Option<&Offer>,
    select: impl FnOnce(&[String]) -> Result<Vec<usize>>,
) -> Result<Vec<Destination>> {
    use crate::{
        agent::resource_agents,
        browser::sanitize,
        domain::resource::{Data, Management},
        terminal::presentation::{table, table_width},
    };
    let mut profiles = resource_agents::profile_roots_matching(ctx, |kind, _| kind == client)?;
    profiles.sort_by(|a, b| {
        (b.management == Management::Adopted)
            .cmp(&(a.management == Management::Adopted))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    let mut choices = vec![];
    let mut labels = vec![];
    let mut rows = vec![];
    let mut notes = vec![];
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
            rows.push(vec!["—".into(), name, "目录待核验".into(), path]);
            continue;
        };
        let registered = profile.management == Management::Adopted;
        let status = match (registered, resource_agents::managed_root(config_dir)) {
            (true, true) => "单独配置 · 已纳入管理",
            (true, false) => "已纳入管理",
            (false, _) => "可登记",
        };
        let existing = match choice {
            Some(Offer::Claude(offer, _)) => {
                match super::plugin_target::claude_first_install_conflict(
                    &target,
                    &offer.native_id(),
                    &offer.marketplace,
                    &offer.version,
                ) {
                    Ok(Some(note)) => format!("\n    {}", sanitize(&note, false)),
                    Ok(None) => String::new(),
                    Err(_) => "\n    插件记录待读取".into(),
                }
            }
            _ => String::new(),
        };
        choices.push(Some(Destination::Existing(Selection {
            target: Box::new(target),
            registered,
        })));
        labels.push(name.clone());
        rows.push(vec![
            choices.len().to_string(),
            name.clone(),
            status.into(),
            path,
        ]);
        if !existing.is_empty() {
            notes.push(format!("{name}　{}", existing.trim()));
        }
    }
    choices.push(Some(Destination::Independent));
    rows.push(vec![
        choices.len().to_string(),
        "新建单独配置".into(),
        "—".into(),
        "—".into(),
    ]);
    eprint!(
        "{}",
        table(
            &["编号", "Agent", "管理状态", "配置目录"],
            &rows,
            table_width()
        )
    );
    for note in notes {
        eprintln!("说明　{note}");
    }
    labels.push("新建单独配置".into());
    labels.push("返回".into());
    Ok(select(&labels)?
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

fn target_numbers(labels: &[String]) -> Result<Vec<usize>> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() {
        loop {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            if input.trim().is_empty() {
                return Ok(vec![]);
            }
            if let Some(selected) = parse_target_numbers(&input, labels.len() - 1) {
                return Ok(selected);
            }
            eprintln!("请输入列表里的编号。「新建单独配置」需要单独选择。");
        }
    }
    let selected = crate::terminal::picker::select(labels, labels.len() - 1, true, "", |typed| {
        parse_target_numbers(typed, labels.len() - 1)
    })?;
    if selected.contains(&(labels.len() - 1)) {
        return Ok(vec![]);
    }
    if selected.len() > 1 && selected.contains(&(labels.len() - 2)) {
        return Err(incompatible_profile_offer()
            .with_guidance("请选择一种安装位置。", "新建单独配置需要单独选择。"));
    }
    Ok(selected)
}

fn target_label(target: &crate::agent::resource_agents::InstallTarget) -> String {
    crate::browser::sanitize(
        &crate::agent::resource_agents::profile_label(&target.name, &target.directory.canonical),
        false,
    )
}

fn install_profiles_using(
    ctx: &Context,
    choice: &Offer,
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
            if matches!(choice, Offer::Claude(..)) {
                eprintln!("已有同名插件或插件市场的配置会跳过。");
            } else {
                eprintln!("已经装好的插件保留当前开关。");
            }
            confirm_installation()
        },
        install,
        show_profile_install_event,
    )
}

fn show_profile_install_event(event: Event<'_>) {
    render_profile_install_event(event, |tone, message| {
        if let Some(tone) = tone {
            terminal(tone, message);
        } else {
            eprintln!("{message}");
        }
    });
}

fn render_profile_install_event(
    event: Event<'_>,
    mut line: impl FnMut(Option<TerminalLine>, &str),
) {
    match event {
        Event::Reconciled(result) => {
            let previous = result.get("journal").unwrap_or(result);
            if previous["phase"] == "failed"
                && let Some(message) = previous["message"].as_str().filter(|text| !text.is_empty())
            {
                line(
                    Some(TerminalLine::Note),
                    &format!("上次操作已核对。{message}"),
                );
            } else {
                line(Some(TerminalLine::Step), "已核对上次操作，继续本次安装。");
            }
        }
        Event::Complete(raw) => show_install_result(raw),
        Event::Skipped(target, raw) => {
            line(
                Some(TerminalLine::Note),
                &format!("{} · 本次跳过", target_label(target)),
            );
            if let Some(message) = raw["message"].as_str() {
                line(
                    None,
                    &format!("    {}", crate::output::sanitize(message, false)),
                );
            }
        }
        Event::Stopped {
            target,
            completed,
            remaining,
            pending,
        } => {
            let label = target_label(target);
            if let Some(pending) = pending {
                line(Some(TerminalLine::Note), &format!("{label} · 安装尚未完成"));
                if pending.continuation {
                    line(None, "上一步已核对。请用同样的安装选项重试。");
                } else {
                    let journal = &pending.raw["journal"];
                    if let Some(message) =
                        journal["message"].as_str().filter(|text| !text.is_empty())
                    {
                        line(
                            None,
                            &format!("  错误原因　{}", crate::browser::sanitize(message, false)),
                        );
                    }
                    if let Some(hint) = crate::output::recovery_hint(journal) {
                        line(None, &format!("  正确用法　{hint}"));
                    }
                }
            } else {
                line(Some(TerminalLine::Note), &format!("{label} 安装停止。"));
            }
            if completed > 0 {
                line(None, &format!("已完成 {completed} 个配置的安装。"));
            }
            if !remaining.is_empty() {
                let labels: Vec<_> = remaining
                    .iter()
                    .map(|item| target_label(&item.target))
                    .collect();
                line(None, &format!("尚未开始本次安装 {}", labels.join("、")));
            }
        }
        Event::BatchComplete { installed, skipped } => {
            let summary = if skipped == 0 {
                format!("已完成 {installed} 个配置的安装。")
            } else {
                format!("安装完成 {installed} 个，跳过 {skipped} 个。")
            };
            line(
                Some(if skipped == 0 {
                    TerminalLine::Done
                } else {
                    TerminalLine::Note
                }),
                &summary,
            );
        }
    }
}

pub(crate) fn install_at_profile(
    ctx: &Context,
    choice: &Offer,
    activation: InitialActivation,
    target: crate::agent::resource_agents::InstallTarget,
) -> Result<Value> {
    match choice {
        Offer::Codex(offer, binary) => crate::agent::codex_install::install_at(
            ctx,
            (**offer).clone(),
            binary.clone(),
            activation,
            target,
            crate::terminal::report_install_stage,
        ),
        Offer::Claude(offer, binary) => crate::agent::claude_install::install_at(
            ctx,
            (**offer).clone(),
            binary.clone(),
            activation == InitialActivation::Enabled,
            target,
            crate::terminal::report_install_stage,
        ),
    }
}

pub(crate) fn install_at_profile_in_batch(
    ctx: &Context,
    choice: &Offer,
    target: InstallTarget,
) -> Result<Value> {
    let offer = match choice {
        Offer::Codex(offer, _) => crate::domain::fingerprint(offer)?,
        Offer::Claude(offer, _) => crate::domain::fingerprint(offer)?,
    };
    {
        let store = crate::operations::store::Store::lock(ctx)?;
        store.check_pending()?;
        if let Some(mut result) = super::plugin_target::previous(ctx, &store, &target, &offer)? {
            result["status"] = json!("current");
            return Ok(result);
        }
    }
    install_at_profile(ctx, choice, InitialActivation::Enabled, target)
}

fn incompatible_profile_offer() -> Error {
    Error::new(
        "plugin_target_unsupported",
        "请选择可以安装到 Agent 档案的插件。",
    )
}

fn install_isolated(
    ctx: &Context,
    choice: &Offer,
    confirm: impl FnOnce() -> Result<InitialActivation>,
) -> Result<Value> {
    install_isolated_using(ctx, choice, confirm, |activation| {
        let progress = crate::terminal::report_install_stage;
        match choice {
            Offer::Codex(offer, binary) => crate::agent::codex_install::install(
                ctx,
                (**offer).clone(),
                binary.clone(),
                activation,
                progress,
            ),
            Offer::Claude(offer, binary) => crate::agent::claude_install::install(
                ctx,
                (**offer).clone(),
                binary.clone(),
                activation == InitialActivation::Enabled,
                progress,
            ),
        }
    })
}

fn install_isolated_using(
    ctx: &Context,
    choice: &Offer,
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
            let previous = match choice {
                Offer::Codex(offer, _) => {
                    crate::agent::codex_install::matching_activation(journal, offer, None)?
                }
                Offer::Claude(offer, _) => {
                    crate::agent::claude_install::matching_activation(journal, offer, None)?
                }
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
            if matches!(choice, Offer::Codex(..))
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
                return Ok(report_install(crate::agent::codex_install::resume(
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
    let result = crate::terminal::progress::with_install_progress(|| install(activation))?;
    Ok(report_install(result))
}

fn confirm_initial_activation() -> Result<InitialActivation> {
    use std::io::IsTerminal as _;
    if !std::io::stdin().is_terminal() {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        return activation(match input.trim() {
            "yes" => &[1],
            "disabled" => &[2],
            _ => &[],
        });
    }
    let labels = ["取消", "安装并启用", "安装后保持停用"].map(str::to_owned);
    let selected = crate::terminal::picker::select(&labels, 0, false, "", |value| match value {
        "yes" => Some(vec![1]),
        "disabled" => Some(vec![2]),
        _ => None,
    })?;
    activation(&selected)
}

fn activation(selected: &[usize]) -> Result<InitialActivation> {
    match selected {
        [1] => Ok(InitialActivation::Enabled),
        [2] => Ok(InitialActivation::Disabled),
        _ => Err(Error::attention("cancelled", "已取消安装")),
    }
}

pub(crate) fn show_codex_market(market: &Value) {
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

#[cfg(test)]
#[path = "../../tests/unit/agent_plugin_install.rs"]
mod tests;
