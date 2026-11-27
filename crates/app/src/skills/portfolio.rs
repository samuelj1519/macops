use super::{native_skills, public_skill};
use crate::{
    agent::{plugin_install, resource_agents},
    cli::{Cli, Command, SkillAgents, SkillCmd},
    context::Context,
    domain::resource::{CapabilityAction as Operation, Data, Resource, State},
    error::{Error, Result},
    terminal::picker,
};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

impl Operation {
    fn label(self) -> &'static str {
        match self {
            Self::Install => "安装",
            Self::Upgrade => "升级",
            Self::Uninstall => "卸载",
            Self::Enable => "启用",
            Self::Disable => "停用",
        }
    }
}

pub(crate) fn recommend(ctx: &Context, source: &str, kind: Operation) -> Result<Option<Value>> {
    if resource_agents::profile_roots_matching(ctx, |client, _| {
        matches!(client, "claude" | "codex" | "cursor")
    })?
    .is_empty()
    {
        return Ok(None);
    }
    let agents = SkillAgents::default();
    let report = execute(
        ctx,
        Request {
            name: source,
            kind,
            agents: &agents,
            revision: None,
            switch_source: false,
        },
        true,
        false,
        false,
    )?;
    if let Some(rows) = report["results"].as_array() {
        crate::terminal::presentation::panel(
            "操作结果",
            &batch_text(rows, crate::terminal::presentation::table_width()),
        );
    }
    Ok(Some(report))
}

#[derive(Clone, Copy)]
struct Request<'a> {
    name: &'a str,
    kind: Operation,
    agents: &'a SkillAgents,
    revision: Option<&'a str>,
    switch_source: bool,
}

fn scoped(agents: &SkillAgents) -> bool {
    agents.shared || !agents.names.is_empty()
}

pub(crate) fn routed(cli: &Cli) -> bool {
    let Command::Skill {
        command: Some(command),
        ..
    } = &cli.command
    else {
        return false;
    };
    let interactive =
        !cli.json && !cli.yes && !cli.dry_run && crate::browser::interactive_terminal();
    match command {
        SkillCmd::Install {
            source: Some(_),
            agents,
            ..
        }
        | SkillCmd::Remove { agents, .. } => scoped(agents) || interactive,
        SkillCmd::Upgrade(selection) => {
            selection.name.is_some()
                && (scoped(&selection.agents) || interactive || selection.switch_source)
                && !selection.all
                && selection.only.is_empty()
                && selection.change_id.is_none()
        }
        SkillCmd::Enable { .. } | SkillCmd::Disable { .. } => true,
        _ => false,
    }
}

pub(crate) fn run(cli: &Cli, ctx: &Context) -> Result<Option<Value>> {
    let Command::Skill {
        project,
        command: Some(command),
        ..
    } = &cli.command
    else {
        return Ok(None);
    };
    let interactive =
        !cli.json && !cli.yes && !cli.dry_run && crate::browser::interactive_terminal();
    let request = match command {
        SkillCmd::Install {
            source: Some(name),
            agents,
            ..
        } => Request {
            name,
            kind: Operation::Install,
            agents,
            revision: None,
            switch_source: false,
        },
        SkillCmd::Upgrade(selection) if selection.name.is_some() => Request {
            name: selection.name.as_deref().unwrap_or_default(),
            kind: Operation::Upgrade,
            agents: &selection.agents,
            revision: selection.revision.as_deref(),
            switch_source: selection.switch_source,
        },
        SkillCmd::Remove { id, agents } => Request {
            name: id,
            kind: Operation::Uninstall,
            agents,
            revision: None,
            switch_source: false,
        },
        SkillCmd::Enable { id, agents } | SkillCmd::Disable { id, agents } => Request {
            name: id,
            kind: if matches!(command, SkillCmd::Enable { .. }) {
                Operation::Enable
            } else {
                Operation::Disable
            },
            agents,
            revision: None,
            switch_source: false,
        },
        SkillCmd::Install { agents, .. } if scoped(agents) => return Err(selector_error()),
        SkillCmd::Upgrade(selection) | SkillCmd::Refresh(selection) | SkillCmd::Sync(selection)
            if scoped(&selection.agents) || selection.switch_source =>
        {
            return Err(selector_error());
        }
        _ => return Ok(None),
    };
    let agent_operation = scoped(request.agents)
        || request.switch_source
        || interactive
        || matches!(request.kind, Operation::Enable | Operation::Disable);
    if !agent_operation {
        if cli.yes && crate::browser::interactive_terminal() {
            return Err(Error::attention(
                "skill_agent_required",
                "使用 --yes 时请同时加 --agent 指定本次操作的位置。",
            ));
        }
        return Ok(None);
    }
    if interactive
        && !scoped(request.agents)
        && !matches!(request.kind, Operation::Enable | Operation::Disable)
        && !resource_agents::profile_roots_matching(ctx, |client, _| {
            matches!(client, "claude" | "codex" | "cursor")
        })
        .is_ok_and(|profiles| !profiles.is_empty())
    {
        return Ok(None);
    }
    if let SkillCmd::Upgrade(selection) = command
        && (selection.all || !selection.only.is_empty() || selection.change_id.is_some())
    {
        if !scoped(request.agents) && !request.switch_source {
            return Ok(None);
        }
        return Err(selector_error());
    }
    if project.is_some()
        || ctx.chezmoi.source.is_some()
        || ctx.chezmoi.destination.is_some()
        || ctx.chezmoi.config.is_some()
    {
        return Err(Error::attention(
            "agent_skill_scope_unsupported",
            "按 Agent 操作时，请使用对应的用户配置目录。",
        ));
    }
    execute(ctx, request, interactive, cli.dry_run, cli.yes).map(Some)
}

fn selector_error() -> Error {
    Error::attention(
        "skill_agent_selection",
        "按 Agent 操作时，请指定一个能力名或 GitHub 仓库。",
    )
    .with_guidance(
        "尚未开始操作。",
        "例如 mo skill upgrade owner/repo --agent claude-mi --agent claude-glm",
    )
}

struct Agent<'a> {
    profile: &'a Resource,
    name: String,
    client: &'a str,
    path: &'a Path,
}

fn agents(profiles: &[Resource]) -> Vec<Agent<'_>> {
    profiles
        .iter()
        .filter_map(|profile| {
            let Data::Agent {
                client,
                config_dir,
                profile: label,
                runtime,
                ..
            } = &profile.data
            else {
                return None;
            };
            let name = runtime
                .binary
                .as_ref()
                .and_then(|path| path.file_name()?.to_str())
                .map_or_else(
                    || resource_agents::command_name(client, label),
                    str::to_owned,
                );
            Some(Agent {
                profile,
                name,
                client,
                path: config_dir,
            })
        })
        .collect()
}

fn selected(
    agents: &[Agent<'_>],
    request: &Request<'_>,
    interactive: bool,
    dry: bool,
) -> Result<Vec<usize>> {
    let all = request.agents.names.as_slice() == ["all"];
    if all || (dry && request.agents.names.is_empty()) {
        return Ok((0..agents.len()).collect());
    }
    if request.agents.names.is_empty() {
        if !interactive {
            return Err(Error::attention(
                "skill_agent_required",
                "请加 --agent NAME 指定 Agent，或用 --agent all 选择全部。",
            ));
        }
        let has_all = agents.len() > 1;
        let rows = has_all
            .then(|| vec!["全部 Agent".into(), "—".into()])
            .into_iter()
            .chain(
                agents
                    .iter()
                    .map(|agent| vec![agent.name.clone(), agent.path.display().to_string()]),
            )
            .collect::<Vec<_>>();
        let offset = usize::from(has_all);
        let selected = picker::select_table(
            &["Agent", "配置目录"],
            &rows,
            offset,
            true,
            "选择 Agent",
            |_| None,
        )?;
        return if has_all && selected.contains(&0) {
            Ok((0..agents.len()).collect())
        } else {
            Ok(selected.into_iter().map(|index| index - offset).collect())
        };
    }
    let mut indices = BTreeSet::new();
    for name in &request.agents.names {
        let found = agents
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.name == *name || candidate.profile.name == *name)
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if let [i] = found.as_slice() {
            indices.insert(*i);
        } else {
            return Err(Error::attention(
                "skill_agent_not_unique",
                format!("无法确定 Agent {name}。"),
            )
            .with_guidance(
                "尚未开始操作。",
                "运行 mo agent 核对命令名和配置目录，再指定目标。",
            ));
        }
    }
    Ok(indices.into_iter().collect())
}

struct Site<'a> {
    agent: &'a Agent<'a>,
    work: Option<Work>,
    status: &'static str,
    reason: String,
    version: Option<String>,
}

enum Work {
    Shared,
    Install(usize, Box<resource_agents::InstallTarget>),
    Native(Box<crate::agent::plugin_lifecycle::Intent>),
    Switch(Box<crate::agent::plugin_lifecycle::SourceSwitch>),
}

struct PluginSite<'a> {
    id: &'a str,
    revision: &'a crate::domain::resource::Revision,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shared {
    Missing,
    Present,
    Unverified,
}
struct Evidence<'a> {
    shared: Shared,
    source_conflict: Option<&'a crate::domain::resource::Revision>,
    unsupported: bool,
    plugins: Vec<PluginSite<'a>>,
}

fn evidence<'a>(
    ctx: &Context,
    request: &Request<'_>,
    agent: &Agent<'_>,
    installs: &[plugin_install::Offer],
    local: &'a [Resource],
    repository: Option<&str>,
) -> Evidence<'a> {
    let mut shared = Shared::Missing;
    let mut source_conflict = None;
    let mut unsupported = false;
    let mut plugins = vec![];
    for resource in local {
        let Data::Capability {
            origin,
            deployments,
            revision,
            native_id,
            contents,
            ..
        } = &resource.data
        else {
            continue;
        };
        let is_shared = origin.manager.starts_with("skills-cli:")
            && contents
                .iter()
                .any(|content| content.path.starts_with(ctx.home.join(".agents/skills")));
        let belongs = deployments
            .iter()
            .any(|item| item.harness == agent.profile.id && item.scope == "user");
        if !is_shared && !belongs {
            continue;
        }
        let matches = if let Some(repository) = repository {
            let actual = if origin.repository.is_some() {
                origin.repository.clone()
            } else if belongs && matches!(origin.manager.as_str(), "claude-plugin" | "codex-config")
            {
                crate::agent::plugin_marketplaces::registered_github_repository(
                    ctx,
                    &origin.manager,
                    &origin.record,
                    native_id,
                )
                .unwrap_or_default()
            } else {
                None
            };
            actual.is_some_and(|source| source.key().eq_ignore_ascii_case(repository))
        } else {
            resource.id == request.name
                || resource.name.eq_ignore_ascii_case(request.name)
                || resource
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(request.name))
                || (belongs
                    && matches!(origin.manager.as_str(), "claude-plugin" | "codex-config")
                    && native_id
                        .rsplit_once('@')
                        .is_some_and(|(name, _)| name.eq_ignore_ascii_case(request.name)))
        };
        if !matches {
            if belongs
                && installs.iter().any(|offer| {
                    offer.client() == agent.client
                        && offer.plugin_info().is_ok_and(|(id, ..)| id == *native_id)
                })
            {
                source_conflict = Some(revision);
            }
            continue;
        }
        if is_shared {
            if belongs || matches!(agent.client, "codex" | "cursor") {
                if resource.status != State::Present {
                    shared = Shared::Unverified;
                } else if shared == Shared::Missing {
                    shared = Shared::Present;
                }
            }
        } else if matches!(origin.manager.as_str(), "claude-plugin" | "codex-config") {
            plugins.push(PluginSite {
                id: native_id,
                revision,
            });
        } else {
            unsupported = true;
        }
    }
    Evidence {
        shared,
        source_conflict,
        unsupported,
        plugins,
    }
}

fn plan_plugin(
    ctx: &Context,
    operation: Operation,
    site: &mut Site<'_>,
    plugin: &PluginSite<'_>,
    installs: &[plugin_install::Offer],
    require_offer: bool,
) {
    let PluginSite { id, revision } = *plugin;
    let agent = site.agent;
    site.version.clone_from(&revision.recorded);
    if operation == Operation::Install {
        match installs.iter().enumerate().find(|(_, offer)| {
            offer.client() == agent.client && offer.plugin_info().is_ok_and(|(name, ..)| name == id)
        }) {
            Some((index, offer)) => {
                match resource_agents::InstallTarget::capture(ctx, agent.profile) {
                    Ok(target) => {
                        let done = offer.plugin_info().is_ok_and(|(_, version, _, commit)| {
                            crate::agent::plugin_lifecycle::matches_install(
                                ctx, &target, id, version, commit,
                            )
                            .unwrap_or_default()
                        });
                        if done {
                            site.status = "current";
                        } else {
                            site.status = "planned";
                            site.work = Some(Work::Install(index, Box::new(target)));
                        }
                    }
                    Err(error) => site.reason = error.message,
                }
            }
            None => site.reason = "已有同来源插件。请用 upgrade 检查目标版本。".into(),
        }
    } else {
        let version = installs
            .iter()
            .filter(|offer| offer.client() == agent.client)
            .find_map(|offer| {
                offer
                    .plugin_info()
                    .ok()
                    .filter(|(candidate, ..)| candidate == id)
            })
            .map(|(_, version, _, commit)| (version, commit));
        match resource_agents::InstallTarget::capture(ctx, agent.profile).and_then(|target| {
            crate::agent::plugin_lifecycle::Intent::plan(
                ctx,
                target,
                id,
                operation,
                version,
                require_offer,
            )
        }) {
            Ok(Some(intent)) => {
                site.work = Some(Work::Native(Box::new(intent)));
                site.status = "planned";
            }
            Ok(None) => site.status = "current",
            Err(error) => site.reason = error.message,
        }
    }
}

fn classify<'a>(
    ctx: &Context,
    request: &Request<'_>,
    agent: &'a Agent<'a>,
    installs: &[plugin_install::Offer],
    local: &[Resource],
    repository: Option<&str>,
) -> Site<'a> {
    let mut site = Site {
        agent,
        work: None,
        status: "blocked",
        reason: String::new(),
        version: None,
    };
    let Evidence {
        shared,
        source_conflict,
        unsupported,
        plugins,
    } = evidence(ctx, request, agent, installs, local, repository);
    let share = shared == Shared::Present;
    if let Some(revision) = source_conflict {
        site.version.clone_from(&revision.recorded);
        site.reason =
            "发现同名插件，但当前来源无法核实。请在这个 Agent 的插件菜单检查市场。".into();
    } else if plugins.len() > 1 || (share && !plugins.is_empty()) {
        site.reason = "找到多份安装。请用插件 ID 选定一份。".into();
    } else if let Some(plugin) = plugins.first() {
        plan_plugin(
            ctx,
            request.kind,
            &mut site,
            plugin,
            installs,
            repository.is_some(),
        );
    } else if shared == Shared::Unverified {
        site.reason = "共享安装记录存在，但无法确认实际技能文件。".into();
    } else if request.kind == Operation::Install {
        if share {
            site.status = "planned";
            site.work = Some(Work::Shared);
            return site;
        }
        if let Some((index, _)) = installs
            .iter()
            .enumerate()
            .find(|(_, offer)| offer.client() == agent.client)
        {
            match resource_agents::InstallTarget::capture(ctx, agent.profile) {
                Ok(target) => {
                    site.status = "planned";
                    site.work = Some(Work::Install(index, Box::new(target)));
                }
                Err(error) => site.reason = error.message,
            }
        } else if matches!(agent.client, "codex" | "cursor") {
            site.status = "planned";
            site.work = Some(Work::Shared);
        } else {
            site.reason = "这个仓库没有可核验的 Claude Code 插件安装方式。".into();
        }
    } else if share && matches!(request.kind, Operation::Upgrade | Operation::Uninstall) {
        site.status = "planned";
        site.work = Some(Work::Shared);
    } else if share {
        site.reason = "共享技能没有独立的启停开关。".into();
    } else if unsupported {
        site.reason = "在这个 Agent 中找到了该能力。请用这个 Agent 的原生入口维护。".into();
    } else {
        site.status = "not-installed";
    }
    site
}

fn verified_offers(
    ctx: &Context,
    request: &Request<'_>,
    repo: Option<&public_skill::Request>,
    agents: &[Agent<'_>],
    picked: &[usize],
    local: &[Resource],
) -> Result<Vec<plugin_install::Offer>> {
    let repository = repo.map(|parsed| parsed.repository.key());
    let query = request.name;
    let needs_offer = request.kind == Operation::Install
        || local.iter().any(|resource| {
            let Data::Capability {
                origin,
                deployments,
                ..
            } = &resource.data
            else {
                return false;
            };
            matches!(origin.manager.as_str(), "claude-plugin" | "codex-config")
                && picked.iter().any(|&index| {
                    deployments
                        .iter()
                        .any(|deployment| deployment.harness == agents[index].profile.id)
                })
        });
    if let Some(repository) = repository
        && matches!(request.kind, Operation::Install | Operation::Upgrade)
        && needs_offer
        && picked
            .iter()
            .any(|&i| agents[i].client == "claude" || agents[i].client == "codex")
    {
        let data = crate::recommend::public_repository(ctx, repository)?;
        let mut offers = plugin_install::offers(ctx, &data);
        if query.contains('@') || request.revision.is_some() {
            let commit = if let Some(revision) = request.revision {
                if query.contains('@') {
                    return Err(selector_error());
                }
                crate::policy::source_git::ObjectId::parse(revision)?
            } else {
                repo.ok_or_else(selector_error)?.resolve()?
            };
            offers.retain(|offer| {
                offer
                    .plugin_info()
                    .is_ok_and(|(_, _, _, actual)| actual == commit.as_str())
            });
            if picked.iter().any(|&i| agents[i].client == "claude")
                && !offers.iter().any(|offer| offer.client() == "claude")
                && let Some(parsed) = repo
                && let Ok(binary) = crate::agent::claude_install::binary(ctx)
                && let Some(offer) = parsed.claude_offer_at(&commit)?
            {
                offers.push(plugin_install::Offer::Claude(Box::new(offer), binary));
            }
        }
        Ok(offers)
    } else {
        Ok(vec![])
    }
}

fn plan_source_switches(
    ctx: &Context,
    request: &Request<'_>,
    sites: &mut [Site<'_>],
    offers: &[plugin_install::Offer],
    local: &[Resource],
    repo: &str,
    allow_change: bool,
) {
    let Some((offer, binary)) = offers.iter().find_map(|offer| match offer {
        plugin_install::Offer::Claude(offer, binary) => Some((offer.as_ref(), binary)),
        plugin_install::Offer::Codex(..) => None,
    }) else {
        return;
    };
    let id = offer.native_id();
    let mut basis = None;
    for site in sites {
        if site.agent.client != "claude" || (site.status != "blocked" && !request.switch_source) {
            continue;
        }
        let installed = local.iter().any(|resource| {
            matches!(&resource.data, Data::Capability { native_id, origin, deployments, .. }
                if *native_id == id && origin.manager == "claude-plugin"
                    && deployments.iter().any(|site_at| site_at.scope == "user" && site_at.harness == site.agent.profile.id))
        });
        if !installed
            || evidence(ctx, request, site.agent, offers, local, Some(repo)).shared
                != Shared::Missing
        {
            continue;
        }
        let target = match resource_agents::InstallTarget::capture(ctx, site.agent.profile) {
            Ok(target) => target,
            Err(error) => {
                if allow_change {
                    site.work = None;
                    site.status = "blocked";
                    site.reason = error.message;
                }
                continue;
            }
        };
        if !allow_change
            && !crate::agent::plugin_lifecycle::SourceSwitch::is_bound(&target, offer)
                .unwrap_or_default()
        {
            continue;
        }
        let prepared = basis.get_or_insert_with(|| {
            crate::terminal::progress::during("核验新市场与插件", || {
                crate::agent::plugin_lifecycle::SwitchOffer::prepare(offer, binary)
            })
        });
        let result = match prepared {
            Ok(prepared) => {
                crate::agent::plugin_lifecycle::SourceSwitch::plan(target, prepared, allow_change)
            }
            Err(error) => {
                site.work = None;
                site.status = "blocked";
                site.reason.clone_from(&error.message);
                continue;
            }
        };
        match result {
            Ok(Some(plan)) => {
                site.status = "planned";
                site.reason = plan.detail();
                site.work = Some(Work::Switch(Box::new(plan)));
            }
            Ok(None) => {
                site.work = None;
                site.status = "current";
                site.reason.clear();
            }
            Err(error) => {
                site.work = None;
                site.status = "blocked";
                site.reason = error.message;
            }
        }
    }
}

fn execute(
    ctx: &Context,
    request: Request<'_>,
    interactive: bool,
    dry: bool,
    yes: bool,
) -> Result<Value> {
    let profiles = resource_agents::profile_roots_matching(ctx, |client, _| {
        matches!(client, "claude" | "codex" | "cursor")
    })?;
    let agents = agents(&profiles);
    let picked = selected(&agents, &request, interactive, dry)?;
    if picked.is_empty() {
        return if interactive {
            Ok(json!({"mode":"cancelled","persistentWrites":false}))
        } else {
            Err(Error::attention(
                "skill_agents_missing",
                "本机找不到适用的 Agent 配置。运行 mo agent 查看配置。",
            ))
        };
    }
    let query = request.name.strip_prefix("source:").unwrap_or(request.name);
    let subject = query.split_once('@').map_or(query, |(head, _)| head);
    let github = subject.strip_prefix("github.com/").unwrap_or(subject);
    let repo = crate::policy::public_source::repository_subject(github)
        .map(|_| public_skill::Request::parse(query))
        .transpose()?;
    let repository = repo.as_ref().map(|value| value.repository.key());
    if (request.kind == Operation::Install || request.revision.is_some() || request.switch_source)
        && repository.is_none()
    {
        return Err(selector_error());
    }
    let (local, _) =
        crate::capability::resource_capabilities::collect_observed(ctx, &profiles, None, false)?;
    let offers = if dry {
        verified_offers(ctx, &request, repo.as_ref(), &agents, &picked, &local)?
    } else {
        crate::terminal::progress::during("核对来源和安装版本", || {
            verified_offers(ctx, &request, repo.as_ref(), &agents, &picked, &local)
        })?
    };
    let mut sites = picked
        .iter()
        .map(|&index| classify(ctx, &request, &agents[index], &offers, &local, repository))
        .collect::<Vec<_>>();
    if request.kind == Operation::Upgrade
        && let Some(repo) = repository
    {
        plan_source_switches(
            ctx,
            &request,
            &mut sites,
            &offers,
            &local,
            repo,
            interactive || request.switch_source,
        );
    }
    check_target_scope(&mut sites, &agents, request.agents.shared || interactive);
    let shared = if sites
        .iter()
        .any(|site| matches!(site.work, Some(Work::Shared)))
    {
        match crate::terminal::progress::during("核对共享安装和目标版本", || {
            native_skills::plan_shared(ctx, request.name, request.kind, request.revision)
        }) {
            Ok(plan) => Some(plan),
            Err(error) => {
                for site in &mut sites {
                    if matches!(site.work, Some(Work::Shared)) {
                        site.work = None;
                        site.status = "blocked";
                        site.reason.clone_from(&error.message);
                    }
                }
                None
            }
        }
    } else {
        None
    };
    let planned = preview(ctx, &request, &sites, &offers, &agents);
    if dry {
        return Ok(
            json!({"agentBatch":true,"mode":"dry-run","verb":request.kind.label(),"results":planned,"persistentWrites":false}),
        );
    }
    if sites.iter().any(|site| site.work.is_some()) {
        let text = batch_text(&planned, crate::terminal::presentation::table_width());
        let action = if sites
            .iter()
            .any(|site| matches!(site.work, Some(Work::Switch(_))))
        {
            "换源并升级"
        } else {
            request.kind.label()
        };
        crate::operations::confirm(&format!("{action} {}\n{text}", request.name), yes)?;
        crate::operations::recover_before_write(ctx, None)?;
    }
    perform(ctx, &request, sites, &offers, planned, shared)
}

fn check_target_scope(sites: &mut [Site<'_>], agents: &[Agent<'_>], shared_approved: bool) {
    if !shared_approved {
        for site in &mut *sites {
            if matches!(site.work, Some(Work::Shared)) {
                site.work = None;
                site.status = "blocked";
                site.reason = "需要修改共享技能目录。确认影响范围后，加 --shared 重试。".into();
            }
        }
    }
    for site in &mut *sites {
        if matches!(
            site.work,
            Some(Work::Install(..) | Work::Native(_) | Work::Switch(_))
        ) && agents.iter().any(|other| {
            other.path != site.agent.path
                && (other.path.starts_with(site.agent.path)
                    || site.agent.path.starts_with(other.path))
        }) {
            site.work = None;
            site.status = "blocked";
            site.reason = "配置目录和另一套 Agent 的目录重叠。请先核对配置位置。".into();
        }
    }
}

fn preview(
    ctx: &Context,
    request: &Request<'_>,
    sites: &[Site<'_>],
    offers: &[plugin_install::Offer],
    agents: &[Agent<'_>],
) -> Vec<Value> {
    let sharing = agents
        .iter()
        .filter(|agent| matches!(agent.client, "codex" | "cursor"))
        .map(|agent| agent.name.as_str())
        .collect::<Vec<_>>()
        .join("、");
    let shared_reason = if sharing.is_empty() {
        "共享目录会影响所有读取它的 Agent".into()
    } else {
        format!("共享目录会影响 {sharing} 及其他读取它的 Agent")
    };
    sites.iter().map(|site| {
        let shared = matches!(site.work, Some(Work::Shared));
        let version = match &site.work {
            Some(Work::Install(index, _)) => offers[*index].plugin_info().ok().map(|(_,v,_,_)| v),
            Some(Work::Native(intent)) => intent.target_version(),
            Some(Work::Switch(plan)) => Some(plan.version()),
            Some(Work::Shared) => request.revision.or_else(|| request.name.rsplit_once('@').map(|(_,reference)| reference)),
            None if site.status == "blocked" && site.version.is_some() => offers.iter()
                .filter(|offer| offer.client() == site.agent.client)
                .find_map(|offer| offer.plugin_info().ok().map(|(_, version, _, _)| version)),
            None => None,
        };
        json!({"agent":site.agent.name,"directory":if shared {ctx.home.join(".agents/skills")} else {site.agent.path.to_path_buf()},
            "status":site.status,"currentVersion":site.version,"targetVersion":version,"reason":if shared {
                &shared_reason
            } else {&site.reason}})
    }).collect()
}

fn perform(
    ctx: &Context,
    request: &Request<'_>,
    sites: Vec<Site<'_>>,
    offers: &[plugin_install::Offer],
    rows: Vec<Value>,
    mut shared_plan: Option<native_skills::SharedPlan>,
) -> Result<Value> {
    perform_with(ctx, request, sites, offers, rows, |work| match work {
        Work::Shared => shared_plan.take().ok_or_else(selector_error)?.run(ctx),
        Work::Install(index, target) => target.open().and_then(|_| {
            crate::terminal::progress::with_install_progress(|| {
                plugin_install::install_at_profile_in_batch(ctx, &offers[index], *target)
            })
        }),
        Work::Native(intent) => intent.run(ctx),
        Work::Switch(plan) => plan.run(ctx),
    })
}

fn perform_with(
    ctx: &Context,
    request: &Request<'_>,
    sites: Vec<Site<'_>>,
    offers: &[plugin_install::Offer],
    mut rows: Vec<Value>,
    mut execute: impl FnMut(Work) -> Result<Value>,
) -> Result<Value> {
    let mut shared: Option<(&str, String)> = None;
    let mut physical = std::collections::BTreeMap::new();
    let mut stopped = false;
    for (site, row) in sites.into_iter().zip(&mut rows) {
        let Some(work) = site.work else { continue };
        if matches!(work, Work::Shared)
            && let Some((status, reason)) = &shared
        {
            row["status"] = json!(status);
            row["reason"] = json!(reason);
            continue;
        }
        let physical_key = match &work {
            Work::Shared => None,
            Work::Install(index, target) => {
                let (id, _, _, commit) = offers[*index].plugin_info()?;
                let identity = &target.directory.identity;
                Some((
                    identity.device,
                    identity.inode,
                    format!("install:{}:{id}:{commit}", target.client),
                ))
            }
            Work::Native(intent) => Some(intent.key()),
            Work::Switch(plan) => Some(plan.key()),
        };
        if let Some((status, reason)) = physical_key.as_ref().and_then(|key| physical.get(key)) {
            row["status"] = json!(status);
            row["reason"] = json!(reason);
            continue;
        }
        if stopped {
            row["status"] = json!("unattempted");
            continue;
        }
        let is_shared = matches!(work, Work::Shared);
        let (status, reason) = result_status(ctx, execute(work))?;
        row["status"] = json!(status);
        row["reason"] = json!(reason);
        if is_shared {
            shared = Some((status, reason));
        } else if let Some(key) = physical_key {
            physical.insert(key, (status, reason));
        }
        stopped = status == "unknown";
    }
    let incomplete = rows.iter().any(|row| {
        matches!(
            row["status"].as_str(),
            Some("unknown" | "blocked" | "failed" | "unattempted")
        )
    });
    Ok(
        json!({"agentBatch":true,"verb":request.kind.label(),"results":rows,"incomplete":incomplete}),
    )
}

#[cfg(test)]
#[path = "../../tests/unit/portfolio.rs"]
mod tests;

fn result_status(ctx: &Context, result: Result<Value>) -> Result<(&'static str, String)> {
    Ok(match result {
        Ok(value) if value["skipped"] == true => (
            "blocked",
            value["message"]
                .as_str()
                .unwrap_or("已有安装，请核对来源。")
                .into(),
        ),
        Ok(value)
            if value["journal"]["phase"] == "unknown" || value["continuationRequired"] == true =>
        {
            (
                "unknown",
                "尚未确认结果。再次执行时会先检查这次操作。".into(),
            )
        }
        Ok(value) if value["incomplete"] == true => {
            let pending = crate::operations::store::active(ctx)?
                .is_some_and(|journal| crate::policy::operation::state::pending(journal.phase));
            if pending {
                (
                    "unknown",
                    "尚未确认结果。再次执行时会先检查这次操作。".into(),
                )
            } else {
                (
                    "failed",
                    "这一份共享安装有未完成的技能。请核对 mo skill --details。".into(),
                )
            }
        }
        Ok(value) if value["journal"]["phase"] == "failed" => (
            "failed",
            value["journal"]["message"]
                .as_str()
                .unwrap_or("未达到目标状态。")
                .into(),
        ),
        Ok(value) if value["status"] == "current" => ("current", String::new()),
        Ok(_) => ("changed", String::new()),
        Err(error)
            if crate::operations::store::active(ctx)?
                .is_some_and(|journal| crate::policy::operation::state::pending(journal.phase)) =>
        {
            ("unknown", error.message)
        }
        Err(error) => ("failed", error.message),
    })
}

pub(crate) fn batch_text(rows: &[Value], columns: usize) -> String {
    use crate::terminal::presentation::table;
    use std::fmt::Write as _;

    let fields = ["agent", "currentVersion", "targetVersion", "directory"];
    let data = rows
        .iter()
        .map(|row| {
            let status = match row["status"].as_str() {
                Some("planned") => "将执行",
                Some("changed") => "已完成",
                Some("current") => "已达到目标",
                Some("not-installed") => "未安装",
                Some("unattempted") => "尚未开始",
                Some("unknown") => "结果待核对",
                Some("blocked") => "暂不能操作",
                _ => "未完成",
            };
            let mut cells = fields
                .map(|key| row[key].as_str().unwrap_or("—").to_owned())
                .to_vec();
            cells.insert(1, status.into());
            cells
        })
        .collect::<Vec<_>>();
    let mut text = table(
        &["Agent", "状态", "当前版本", "目标版本", "安装位置"],
        &data,
        columns,
    );
    for row in rows {
        if let Some(reason) = row["reason"].as_str().filter(|reason| !reason.is_empty()) {
            let agent = row["agent"].as_str().unwrap_or("—");
            let _ = writeln!(
                text,
                "说明 · {}　{}",
                crate::output::sanitize(agent, false),
                crate::output::sanitize(reason, false)
            );
        }
    }
    text.trim_end().into()
}
