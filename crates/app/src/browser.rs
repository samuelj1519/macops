use crate::{
    cli::{Cli, Command, SkillCmd},
    config::chezmoi_native::ConfigBrowser,
    context::Context,
    error::{Error, Result},
    operations::RecoveryState,
};
use serde::Serialize;
use std::{collections::BTreeSet, io::IsTerminal, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Config,
    Skills,
    Capabilities,
    Apps,
    Agents,
    History,
    Marketplaces,
}

impl Kind {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Config => "配置",
            Self::Skills => "技能",
            Self::Capabilities => "能力",
            Self::Apps => "软件",
            Self::Agents => "Agent 档案",
            Self::History => "最近操作",
            Self::Marketplaces => "插件市场",
        }
    }

    pub(crate) const TABS: [Self; 5] = [
        Self::Apps,
        Self::Capabilities,
        Self::Agents,
        Self::Config,
        Self::History,
    ];
}

#[derive(Debug, Serialize)]
pub(crate) struct Item {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub managed: bool,
    #[serde(skip)]
    pub management: crate::domain::resource::Management,
    pub directory: bool,
    pub path: Option<PathBuf>,
    pub source: Option<PathBuf>,
    pub agents: Vec<String>,
    pub loaded: Option<bool>,
    pub details: Vec<String>,
    #[serde(skip)]
    pub list: ListDetail,
    #[serde(skip)]
    pub inventory: Option<InventoryInfo>,
}

#[derive(Debug)]
pub(crate) enum ListDetail {
    Plain,
    Application {
        scope: String,
        version_count: usize,
        recipe: Option<String>,
    },
    Agent {
        command: String,
        version: Option<String>,
        model: Option<String>,
        configuration: bool,
    },
    History {
        action: &'static str,
        result: &'static str,
    },
    Market {
        profile: String,
        status: String,
        registered: bool,
    },
}

impl ListDetail {
    fn profile(command: String, model: Option<String>, version: Option<String>) -> Self {
        Self::Agent {
            command,
            model,
            version,
            configuration: true,
        }
    }
}

pub(crate) fn list_headers(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::Apps => &["软件", "管理状态", "版本", "渠道", "安装范围", "版本数"],
        Kind::Skills | Kind::Capabilities => &["能力", "管理状态", "版本", "安装位置", "渠道"],
        Kind::Agents => &["Agent", "管理状态", "版本", "命令", "模型", "配置目录"],
        Kind::Config => &["配置", "管理状态", "本机状态", "本机位置"],
        Kind::History => &["目标", "操作", "结果"],
        Kind::Marketplaces => &["市场", "Agent", "目录状态"],
    }
}

fn management_label(item: &Item) -> &'static str {
    use crate::domain::resource::Management;
    if item.management == Management::Adopted
        && item
            .inventory
            .as_ref()
            .is_some_and(|info| info.manager == "uv")
    {
        return "已记录基线";
    }
    match item.management {
        Management::Declared => "有声明",
        Management::Native => "原生登记",
        Management::Adopted => "受管",
        Management::Untracked => "已发现",
    }
}

pub(crate) fn list_row(kind: Kind, item: &Item) -> Vec<String> {
    let dash = "—";
    let path = || {
        item.path
            .as_ref()
            .map_or_else(|| dash.into(), |path| path.display().to_string())
    };
    let status = || management_label(item).into();
    let metadata = item.inventory.as_ref();
    let version = || {
        metadata
            .and_then(|info| info.version.as_deref())
            .unwrap_or(dash)
            .to_owned()
    };
    let channel = || {
        metadata
            .map_or(dash, |info| info.manager.as_str())
            .to_owned()
    };
    let mut row = vec![item.name.clone()];
    let columns = match (kind, &item.list) {
        (
            Kind::Agents,
            ListDetail::Agent {
                command,
                version,
                model,
                configuration,
            },
        ) => vec![
            status(),
            version.as_deref().unwrap_or("待核验").into(),
            command.clone(),
            model.as_deref().unwrap_or(dash).into(),
            if *configuration { path() } else { dash.into() },
        ],
        (Kind::History, ListDetail::History { action, result }) => {
            vec![(*action).into(), (*result).into()]
        }
        (
            Kind::Marketplaces,
            ListDetail::Market {
                profile,
                status,
                registered,
            },
        ) => {
            if !registered {
                row[0] = dash.into();
            }
            vec![profile.clone(), status.clone()]
        }
        (Kind::Config, _) => vec![status(), item.summary.clone(), path()],
        (Kind::Skills | Kind::Capabilities, _) => {
            let groups = metadata.map_or(&[][..], |info| info.groups.as_slice());
            let place = if groups.is_empty() {
                dash.into()
            } else {
                groups
                    .iter()
                    .map(|group| crate::terminal::group_label(group))
                    .collect::<Vec<_>>()
                    .join("、")
            };
            vec![status(), version(), place, channel()]
        }
        (
            Kind::Apps,
            ListDetail::Application {
                scope,
                version_count,
                ..
            },
        ) => {
            use crate::domain::resource::State;
            let version = metadata.map_or(dash, |info| {
                info.version.as_deref().unwrap_or(
                    if matches!(info.status, State::Present | State::Unknown) {
                        "待核验"
                    } else {
                        dash
                    },
                )
            });
            vec![
                status(),
                version.into(),
                channel(),
                scope.clone(),
                version_count.to_string(),
            ]
        }
        _ => vec![dash.into(); list_headers(kind).len() - 1],
    };
    row.extend(columns);
    row
}

#[derive(Debug)]
pub(crate) struct InventoryInfo {
    pub manager: String,
    pub groups: Vec<String>,
    pub status: crate::domain::resource::State,
    pub version: Option<String>,
    pub aliases: Vec<String>,
    pub repository: Option<crate::policy::source_git::Repository>,
    pub native_record: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub(crate) struct View {
    pub scope: String,
    pub items: Vec<Item>,
    pub coverage: &'static str,
    pub difference_coverage: &'static str,
    pub observed_at: Option<u64>,
    pub note: String,
    pub incomplete: bool,
}

pub(crate) enum Session {
    Config(Box<ConfigBrowser>),
    Skills,
    Capabilities,
    Apps,
    Agents,
    History,
    Marketplaces,
    Related { kind: Kind, ids: BTreeSet<String> },
}

impl Session {
    pub(crate) fn load(ctx: &Context, kind: Kind) -> Result<(Self, View)> {
        match kind {
            Kind::Config => {
                let browser = ConfigBrowser::load(ctx)?;
                let view = browser.view();
                Ok((Self::Config(Box::new(browser)), view))
            }
            Kind::Skills => Ok((
                Self::Skills,
                crate::capability::capabilities::browse(ctx, false)?,
            )),
            Kind::Capabilities => Ok((
                Self::Capabilities,
                crate::capability::capabilities::browse(ctx, true)?,
            )),
            Kind::Apps => Ok((Self::Apps, resource_view(ctx, "applications")?)),
            Kind::Agents => Ok((Self::Agents, resource_view(ctx, "agents")?)),
            Kind::History => Ok((Self::History, history_view(ctx)?)),
            Kind::Marketplaces => {
                let data = crate::agent::plugin_marketplaces::list(ctx, None, false, false)?;
                Ok((
                    Self::Marketplaces,
                    crate::agent::plugin_marketplaces::view(&data),
                ))
            }
        }
    }

    pub(crate) fn refresh(&mut self, ctx: &Context) -> Result<View> {
        match self {
            Self::Config(browser) => {
                browser.refresh(ctx)?;
                Ok(browser.view())
            }
            Self::Skills => crate::capability::capabilities::browse(ctx, false),
            Self::Capabilities => crate::capability::capabilities::browse(ctx, true),
            Self::Apps => resource_view(ctx, "applications"),
            Self::Agents => agent_view(ctx),
            Self::History => history_view(ctx),
            Self::Marketplaces => Err(Error::attention(
                "marketplace_snapshot",
                "请重新运行 mo skill market 查看最新市场信息",
            )),
            Self::Related { kind, ids } => {
                let mut view = if *kind == Kind::Apps {
                    local_applications(ctx)?
                } else {
                    Self::load(ctx, *kind)?.1
                };
                view.items.retain(|item| ids.contains(&item.id));
                Ok(view)
            }
        }
    }

    pub(crate) fn observed(
        &self,
        id: &str,
        source: bool,
    ) -> Result<crate::config::chezmoi_native::ViewerFile> {
        match self {
            Self::Config(browser) => browser.select_file(id, source),
            _ => Err(Error::attention("file_unavailable", "此入口不提供文件查看")),
        }
    }
}

pub(crate) fn browse_related(ctx: &Context, kind: Kind, view: View) -> Result<()> {
    let ids = view.items.iter().map(|item| item.id.clone()).collect();
    crate::terminal::browse(ctx, kind, &mut Session::Related { kind, ids }, view, false)?;
    Ok(())
}

pub(crate) fn interactive_terminal() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| !term.is_empty() && term != "dumb")
}

pub(crate) fn interactive_marketplaces(cli: &Cli) -> bool {
    matches!(
        cli.command,
        Command::Skill {
            project: _,
            details: _,
            only: _,
            command: Some(SkillCmd::Market { .. })
        }
    ) && !cli.json
        && !cli.dry_run
        && !cli.yes
        && interactive_terminal()
}

pub(crate) fn browse_marketplaces(ctx: &Context, data: &serde_json::Value) -> Result<()> {
    crate::terminal::browse(
        ctx,
        Kind::Marketplaces,
        &mut Session::Marketplaces,
        crate::agent::plugin_marketplaces::view(data),
        false,
    )?;
    Ok(())
}

pub(crate) struct Output {
    pub text: Option<String>,
    pub incomplete: bool,
}

pub(crate) fn run(cli: &Cli, ctx: &Context) -> Result<Option<Output>> {
    if cli.json {
        return Ok(None);
    }
    let interactive = !cli.dry_run && !cli.yes && interactive_terminal();
    let kind = match &cli.command {
        Command::App {
            command: None,
            project: None,
        } => Kind::Apps,
        Command::Config {
            command: None,
            project: None,
        } => Kind::Config,
        Command::Skill {
            command: None,
            project: None,
            details: false,
            only,
        } if only.is_empty() => Kind::Skills,
        Command::Agent {
            command: None,
            project: None,
        } => Kind::Agents,
        Command::History { query: None } if interactive => Kind::History,
        _ => return Ok(None),
    };
    let (mut session, view) = Session::load(ctx, kind)?;
    let view = if interactive {
        crate::terminal::browse(ctx, kind, &mut session, view, kind == Kind::Apps)?
    } else {
        view
    };
    Ok(Some(Output {
        text: (!interactive).then(|| plain(&view, kind)),
        incomplete: view.incomplete,
    }))
}

fn history_view(ctx: &Context) -> Result<View> {
    let (mut history, active_unreadable) = crate::operations::recovery_history(ctx)?;
    history.sort_by(|(a, _), (b, _)| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let note = if active_unreadable {
        "活动操作记录无法读取，仅显示历史"
    } else if history.is_empty() {
        "暂无操作记录"
    } else {
        "按开始时间排序"
    };
    Ok(View {
        scope: "global".into(),
        items: history
            .into_iter()
            .map(|(journal, state)| history_item(journal, state))
            .collect(),
        coverage: "observed",
        difference_coverage: "not_applicable",
        observed_at: Some(crate::operations::store::now()),
        note: note.into(),
        incomplete: active_unreadable,
    })
}

fn history_item(journal: crate::domain::Journal, recovery: RecoveryState) -> Item {
    use crate::{
        domain::resource::Management,
        domain::{Action, Phase},
    };
    let operation = match journal.plan.action {
        Action::Maintain => "维护",
        Action::Install => "安装",
        Action::Upgrade => "升级",
        Action::Uninstall => "卸载",
        Action::Update => "刷新索引",
        Action::Adopt => "纳入管理",
        Action::Cleanup => "清理",
        Action::Start => "启动",
        Action::Stop => "停止",
        Action::Apply => "应用配置",
        Action::Restore => "还原",
        Action::Reconcile => "核对",
    };
    let phase = match (journal.phase, recovery) {
        (Phase::Prepared, _) => "已准备",
        (Phase::Running, _) => "运行中",
        (Phase::Unknown, _) => "待核对",
        (Phase::Failed, _) => "失败",
        (Phase::Succeeded, RecoveryState::CodexPublication) => "原生安装完成，待登记",
        (Phase::Succeeded, RecoveryState::ClaudeFollowup) => "原生阶段完成，待核对后续步骤",
        (Phase::Succeeded, RecoveryState::Unverified) => "原生阶段完成，登记状态待查",
        (Phase::Succeeded, _) => "已完成",
    };
    let summary = format!("{operation} · {phase}");
    let mut details = vec![
        summary.clone(),
        format!("操作 {}", journal.id),
        format!("目标 {}", journal.plan.package.id),
        format!("原状态目录 {}", journal.state.display()),
    ];
    if recovery.can_confirm() {
        details.push("保留当前运行环境及作用域参数".into());
        details.push(format!("只读预演  mo recover {} --dry-run", journal.id));
        details.push(format!("核对原结果  mo recover {}", journal.id));
    }
    Item {
        id: journal.id,
        name: journal.plan.package.name,
        summary,
        managed: true,
        management: Management::Native,
        directory: false,
        path: None,
        source: None,
        agents: vec![],
        loaded: None,
        details,
        list: ListDetail::History {
            action: operation,
            result: phase,
        },
        inventory: None,
    }
}

pub(crate) use crate::output::sanitize;

fn plain(view: &View, kind: Kind) -> String {
    let rows = view
        .items
        .iter()
        .filter(|item| (kind != Kind::Skills || item.managed) && !item.directory)
        .map(|item| list_row(kind, item))
        .collect::<Vec<_>>();
    let note = sanitize(&view.note, false);
    if rows.is_empty() {
        return format!("{note}\n");
    }
    let table = crate::terminal::presentation::table(
        list_headers(kind),
        &rows,
        crate::terminal::presentation::table_width(),
    );
    format!("{note}\n{table}")
}

fn resource_view(ctx: &Context, filter: &str) -> Result<View> {
    Ok(view_from_snapshot(
        crate::catalog::resource_inventory::snapshot(ctx, Some(filter), None)?,
        false,
    ))
}

pub(crate) fn local_applications(ctx: &Context) -> Result<View> {
    let mut items: Vec<_> = crate::software::resource_software::recorded(ctx)?
        .into_iter()
        .filter_map(|resource| resource_item(resource, false))
        .collect();
    items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(View {
        scope: "global".into(),
        items,
        coverage: "partial",
        difference_coverage: "not_applicable",
        observed_at: None,
        note: "本机登记记录，状态待核验".into(),
        incomplete: true,
    })
}

pub(crate) fn probe_application_versions(ctx: &Context, view: &mut View) {
    use crate::agent::{ai_gate, ai_policy};
    for item in &mut view.items {
        let ListDetail::Application {
            scope,
            version_count,
            ..
        } = &mut item.list
        else {
            continue;
        };
        let Some(info) = &mut item.inventory else {
            continue;
        };
        if info.manager != "local-executable" || info.version.is_some() {
            continue;
        }
        let Some(tool) = ai_policy::TOOLS
            .into_iter()
            .find(|tool| ai_policy::binary(tool).is_ok_and(|binary| binary == item.name))
        else {
            continue;
        };
        let Ok(program) = ai_gate::executable(ctx, &item.name) else {
            continue;
        };
        if program == std::path::Path::new(scope)
            && let Ok(physical) = crate::platform::fs::read_path(&program)
            && !crate::software::resource_software::is_mise_dispatcher(ctx, &program, &physical)
            && let Ok(version) = ai_gate::version(ctx, &physical, tool)
        {
            item.details.push(format!("探测版本 {version}"));
            info.version = Some(version);
            *version_count = 1;
        }
    }
}

fn agent_view(ctx: &Context) -> Result<View> {
    use crate::domain::resource::Data;
    let mut snapshot = crate::catalog::resource_inventory::snapshot(ctx, Some("agents"), None)?;
    let (tools, versions) = crate::agent::ai::list_with_versions(ctx)?;
    for resource in &mut snapshot.resources {
        if let Data::Agent { runtime, .. } = &mut resource.data
            && runtime.version.is_none()
            && let Some(binary) = &runtime.binary
        {
            let mut matches = versions
                .iter()
                .filter_map(|(program, version)| (program == binary).then_some(version));
            if let Some(version) = matches.next()
                && matches.all(|other| other == version)
            {
                runtime.version = Some(version.clone());
            }
        }
    }
    let mut view = view_from_snapshot(snapshot, false);
    let rows = tools["tools"].as_array().map_or(&[][..], Vec::as_slice);
    view.items.extend(rows.iter().filter_map(|tool| {
        let name = tool["tool"].as_str()?;
        let binary = tool["binary"].as_str()?;
        let version = tool["version"].as_str().unwrap_or("未知");
        Some(Item {
            id: format!("ai:{name}"),
            name: name.into(),
            summary: format!("CLI · {version}"),
            managed: true,
            management: crate::domain::resource::Management::Native,
            directory: false,
            path: Some(PathBuf::from(binary)),
            source: None,
            agents: vec![],
            loaded: None,
            list: ListDetail::Agent {
                command: crate::agent::ai_policy::binary(name).ok()?.into(),
                version: tool["version"].as_str().map(str::to_owned),
                model: None,
                configuration: false,
            },
            inventory: None,
            details: vec![
                format!("程序 {binary}"),
                format!("版本 {version}"),
                format!(
                    "本机固定版本 {}",
                    tool["live_pin"].as_str().unwrap_or("未知")
                ),
                format!(
                    "源配置固定版本 {}",
                    tool["declared_pin"].as_str().unwrap_or("未知")
                ),
            ],
        })
    }));
    view.items
        .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    if tools["incomplete"] == true {
        view.incomplete = true;
        view.coverage = "partial";
        view.note = "Agent CLI 版本或档案证据不完整".into();
    }
    Ok(view)
}

pub(crate) fn adoption_view(ctx: &Context, name: Option<&str>) -> Result<View> {
    use crate::domain::resource::{Data, Management, State};
    let mut snapshot = crate::catalog::resource_inventory::applications(ctx, true)?;
    snapshot.resources.retain(|resource| {
        let Data::Application {
            manager,
            recipe_ref,
            ..
        } = &resource.data
        else {
            return false;
        };
        let native_adapter = match manager.as_str() {
            "brew" | "mise" => recipe_ref.is_some(),
            "npm" | "uv" => true,
            _ => false,
        };
        let observed = resource.status == State::Present && native_adapter;
        if let Some(name) = name {
            let declared_command = manager == "declared-recipe"
                && recipe_ref.is_some()
                && resource.status == State::Declared;
            return resource.matches_name(name) && (observed || declared_command);
        }
        observed && resource.management != Management::Adopted
    });
    let mut view = view_from_snapshot(snapshot, true);
    view.note = match (name, view.items.is_empty(), view.incomplete) {
        (Some(_), _, _) => "选择可纳入管理的同名安装项",
        (None, true, true) => "暂无已核验的可管理对象，盘点覆盖不完整",
        (None, true, false) => "暂无可纳入管理的软件",
        (None, false, true) => "选择要纳入管理的软件，盘点覆盖不完整",
        (None, false, false) => "选择要纳入管理的软件",
    }
    .into();
    Ok(view)
}

fn view_from_snapshot(snapshot: crate::domain::resource::Snapshot, show_scope: bool) -> View {
    let incomplete = snapshot.incomplete;
    let mut items = snapshot
        .resources
        .into_iter()
        .filter_map(|resource| resource_item(resource, show_scope))
        .collect::<Vec<_>>();
    items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    View {
        scope: "global".into(),
        items,
        coverage: if incomplete { "partial" } else { "observed" },
        difference_coverage: "not_applicable",
        observed_at: Some(crate::operations::store::now()),
        note: if incomplete {
            "盘点覆盖不完整，详情显示已核验的记录"
        } else {
            "本机盘点完成"
        }
        .into(),
        incomplete,
    }
}

fn release_detail(release: &crate::domain::resource::Release) -> String {
    let path = release
        .path
        .as_deref()
        .map_or_else(String::new, |path| path.display().to_string());
    format!(
        "版本 {} {} {path}",
        release.version,
        if release.active {
            "在用"
        } else {
            "已安装"
        }
    )
}

fn application_version(releases: &[crate::domain::resource::Release]) -> Option<String> {
    let has_active = releases.iter().any(|release| release.active);
    let mut seen = BTreeSet::new();
    let versions = releases
        .iter()
        .filter(|release| release.active || !has_active)
        .map(|release| release.version.as_str())
        .filter(|version| seen.insert(*version))
        .collect::<Vec<_>>();
    (!versions.is_empty()).then(|| versions.join(" / "))
}

fn resource_item(resource: crate::domain::resource::Resource, show_scope: bool) -> Option<Item> {
    use crate::domain::resource::{Data, Management};
    let mut name = resource.name;
    let mut inventory = None;
    let list;
    let (summary, path, agents, details) = match resource.data {
        Data::Application {
            manager,
            native_id,
            scope,
            releases,
            recipe_ref,
            issues,
            ..
        } => {
            let mut details = vec![
                format!("管理器 {manager}"),
                format!("原生标识 {native_id}"),
                format!("作用域 {scope}"),
                format!("观察 {:?}", resource.status),
                format!("登记配方 {}", recipe_ref.as_deref().unwrap_or("无")),
            ];
            details.extend(releases.iter().map(release_detail));
            details.extend(issues);
            let summary = if show_scope {
                format!("{manager} · {scope}")
            } else {
                format!("{manager} · {} 个版本", releases.len())
            };
            list = ListDetail::Application {
                scope: scope.clone(),
                version_count: releases.len(),
                recipe: recipe_ref,
            };
            inventory = Some(InventoryInfo {
                manager,
                groups: vec![],
                status: resource.status,
                version: application_version(&releases),
                aliases: resource.aliases,
                repository: None,
                native_record: None,
            });
            let path = releases.into_iter().find_map(|release| release.path);
            (summary, path, vec![], details)
        }
        Data::Agent {
            client,
            profile,
            config_dir,
            model,
            runtime,
        } => {
            let label = crate::agent::resource_agents::profile_label(&name, &config_dir);
            let command = resource
                .aliases
                .first()
                .cloned()
                .unwrap_or_else(|| name.clone());
            let summary = if label == name {
                format!("{command} · {client} · {profile}")
            } else {
                name = label;
                format!("{client} · 单独配置")
            };
            let details = vec![
                format!("命令 {command}"),
                format!("档案 {}", config_dir.display()),
                format!("运行时 {}", runtime.selector),
                format!(
                    "程序版本 {}",
                    runtime.version.as_deref().unwrap_or("待核验")
                ),
                format!("模型 {}", model.selected.as_deref().unwrap_or("未知")),
                format!("观察 {:?}", resource.status),
            ];
            list = ListDetail::profile(command, model.selected, runtime.version);
            (summary.clone(), Some(config_dir), vec![summary], details)
        }
        _ => return None,
    };
    Some(Item {
        id: resource.id,
        name,
        summary,
        managed: resource.management != Management::Untracked,
        management: resource.management,
        directory: false,
        path,
        source: None,
        agents,
        loaded: None,
        list,
        details,
        inventory,
    })
}

#[cfg(test)]
#[path = "../tests/unit/history_browser.rs"]
mod tests;
