pub(crate) mod install;
pub(crate) mod mise_progress;
pub(crate) mod picker;
pub(crate) mod plan;
pub(crate) mod presentation;
pub(crate) mod progress;

use crate::{
    agent::profile_install::Stage,
    browser::{Item, Kind, Session, View, sanitize},
    config::chezmoi_native::ViewerFile,
    context::Context,
    domain::resource::Management,
    domain::{Action, Package, Plan, Provider},
    error::{Error, Result},
    operations::RecoveryPreview,
};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use nix::sys::signal::{SigSet, Signal};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    widgets::{Block, Borders, Paragraph, Row, Table, TableState, Wrap},
};
use std::{
    fmt::Write as _,
    io::{self, Stdout},
};

#[derive(Debug)]
pub(crate) struct SignalHold {
    suspended: SigSet,
}

impl SignalHold {
    pub(crate) fn enter() -> Result<Self> {
        let mask = |_| Error::new("terminal_signal_mask", "无法挂起全屏会话期间的终止信号");
        let suspended = SigSet::thread_get_mask().map_err(mask)?;
        SigSet::from_iter([
            Signal::SIGINT,
            Signal::SIGTERM,
            Signal::SIGHUP,
            Signal::SIGQUIT,
        ])
        .thread_block()
        .map_err(mask)?;
        Ok(Self { suspended })
    }
}

impl Drop for SignalHold {
    fn drop(&mut self) {
        let _ = self.suspended.thread_set_mask();
    }
}

#[derive(Debug)]
struct Restore {
    _hold: SignalHold,
    screen: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        if self.screen {
            let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        }
    }
}

fn screen() -> Result<(Terminal<CrosstermBackend<Stdout>>, Restore)> {
    let mut restore = Restore {
        _hold: SignalHold::enter()?,
        screen: false,
    };
    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    restore.screen = true;
    Ok((Terminal::new(CrosstermBackend::new(io::stdout()))?, restore))
}

pub(crate) fn feedback(kind: crate::output::TerminalLine, text: &str) {
    eprintln!(
        "{}",
        crate::output::terminal_line(kind, text, presentation::color())
    );
}

pub(crate) fn confirm(message: &str) -> Result<()> {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() {
        presentation::panel("确认操作", message);
    }
    let selected = picker::select(
        &["取消".into(), "确认执行".into()],
        0,
        false,
        "",
        |typed| (typed == "yes").then_some(vec![1]),
    )?;
    if selected == [1] {
        Ok(())
    } else {
        Err(Error::attention("cancelled", "操作已取消"))
    }
}

pub(crate) fn report_install_stage(stage: Stage) {
    use std::io::IsTerminal as _;
    let human = std::io::stderr().is_terminal() && !std::env::args_os().any(|arg| arg == "--json");
    if let Some(text) = install_stage_text(stage, human)
        && !progress::install_stage(text)
    {
        feedback(crate::output::TerminalLine::Step, text);
    }
}

fn install_stage_text(stage: Stage, human: bool) -> Option<&'static str> {
    if !human {
        return None;
    }
    Some(match stage {
        Stage::VerifySource => "核对固定提交与插件文件",
        Stage::RegisterMarketplace => "在隔离目录登记插件市场",
        Stage::InstallPlugin => "在隔离目录安装插件",
        Stage::CheckActivation => "核对插件启用设置",
        Stage::Publish => "校验后写入选定配置",
    })
}

enum Page {
    List,
    Detail(Box<Details>),
    File { path: String, text: String },
    Operation(Box<Plan>, Option<String>),
    Recovery(Box<RecoveryPreview>),
}

struct Details {
    actual: Option<Result<ViewerFile>>,
    source: Option<Result<ViewerFile>>,
    recovery: Option<Box<RecoveryPreview>>,
    agent_session: Option<crate::agent::codex_install::session::Details>,
    agent_removable: bool,
    skills_native: bool,
    target: DetailTarget,
}

#[derive(Default)]
enum DetailTarget {
    App {
        adopt: bool,
        install: bool,
        upgrade: bool,
        uninstall: bool,
    },
    SharedSkill,
    Plugin {
        claude: bool,
    },
    AgentCli(String),
    Config {
        path: std::path::PathBuf,
        forget: bool,
    },
    #[default]
    Other,
}

enum NativeAction {
    Brew {
        action: Action,
        package: Box<Package>,
    },
    AppVersion {
        package: Box<Package>,
        action: Action,
    },
    AgentPreference(String),
    AgentLaunch(String),
    AgentUninstall(String),
    Skills {
        action: Action,
        id: String,
    },
    SharedSkill {
        action: Action,
        id: String,
    },
    Plugin {
        id: String,
        action: crate::domain::resource::CapabilityAction,
    },
    Command {
        command: Box<crate::cli::Command>,
        title: &'static str,
        confirm: Option<String>,
        report: bool,
    },
}

enum After {
    Native(NativeAction),
    FinishOperation,
    FinishRecovery,
    Refresh,
}

struct Browser {
    view: View,
    row: usize,
    search: String,
    typing: bool,
    show_unregistered: bool,
    show_directories: bool,
    kind: Kind,
    group: Option<String>,
    page: Page,
    notice: String,
    offset: usize,
    page_rows: isize,
}

fn detail_target(
    ctx: &Context,
    session: &Session,
    kind: Kind,
    item: &Item,
    skills_native: bool,
) -> DetailTarget {
    let scoped = ctx.chezmoi.source.is_some()
        || ctx.chezmoi.destination.is_some()
        || ctx.chezmoi.config.is_some();
    let manager = item
        .inventory
        .as_ref()
        .map_or("", |data| data.manager.as_str());
    match kind {
        Kind::Apps => application_detail_target(ctx, item, manager),
        Kind::Skills | Kind::Capabilities if !scoped && !skills_native => {
            if !item.inventory.as_ref().is_some_and(|data| {
                use crate::domain::resource::State;
                data.status == State::Present
                    || (manager == "codex-config" && data.status == State::Declared)
            }) {
                return DetailTarget::Other;
            }
            if manager == "skills-cli:github" {
                DetailTarget::SharedSkill
            } else if !item.agents.is_empty() && matches!(manager, "claude-plugin" | "codex-config")
            {
                DetailTarget::Plugin {
                    claude: manager == "claude-plugin",
                }
            } else {
                DetailTarget::Other
            }
        }
        Kind::Agents => crate::agent::ai_policy::upgrade_target(&item.id)
            .map_or(DetailTarget::Other, |tool| {
                DetailTarget::AgentCli(tool.to_owned())
            }),
        Kind::Config if !item.directory && session.observed(&item.id, true).is_ok() => item
            .path
            .as_ref()
            .map_or(DetailTarget::Other, |path| DetailTarget::Config {
                path: path.clone(),
                forget: !scoped
                    && matches!(session, Session::Config(browser) if browser.supports_forget(ctx)),
            }),
        _ => DetailTarget::Other,
    }
}

fn application_detail_target(ctx: &Context, item: &Item, manager: &str) -> DetailTarget {
    let recipe = match &item.list {
        crate::browser::ListDetail::Application { recipe, .. } => recipe.as_deref(),
        _ => None,
    };
    let provider = recipe
        .and_then(|id| {
            crate::catalog::inventory::load(ctx)
                .ok()
                .and_then(|catalog| {
                    catalog
                        .packages
                        .into_iter()
                        .find(|package| package.id == id)
                })
        })
        .or_else(|| {
            (manager == "brew")
                .then(|| {
                    crate::software::brew::candidate(ctx, &item.id)
                        .ok()
                        .flatten()
                })
                .flatten()
                .filter(|package| {
                    package.id == item.id
                        && matches!(package.provider, Provider::BrewFormula { .. })
                })
        })
        .map(|package| package.provider);
    let (install, upgrade, uninstall) = match &provider {
        Some(Provider::Command {
            install,
            upgrade,
            uninstall,
            ..
        }) => (install.is_some(), upgrade.is_some(), uninstall.is_some()),
        Some(
            Provider::BrewFormula { .. }
            | Provider::Brew { cask: true, .. }
            | Provider::Mise { .. }
            | Provider::Cargo { .. }
            | Provider::Bun { .. },
        ) => (true, true, true),
        Some(Provider::Npm {
            authority: Some(authority),
            ..
        }) => (
            authority.registry.is_some(),
            authority.registry.is_some(),
            authority.allow_remove,
        ),
        _ => (false, false, false),
    };
    let install = install
        && item.inventory.as_ref().is_some_and(|data| {
            use crate::domain::resource::State;
            matches!(data.status, State::Missing | State::Declared)
        })
        && recipe.is_some_and(|id| {
            crate::catalog::inventory::declared_catalog(ctx)
                .is_ok_and(|catalog| catalog.packages.iter().any(|package| package.id == id))
        });
    DetailTarget::App {
        adopt: upgrade
            || uninstall
            || matches!(manager, "npm" | "uv")
            || matches!(provider, Some(Provider::Command { .. })),
        upgrade,
        uninstall,
        install,
    }
}

fn group_keys(kind: Kind, item: &Item) -> Vec<&str> {
    match kind {
        Kind::Apps => vec![
            item.inventory
                .as_ref()
                .map_or("来源未核验", |info| info.manager.as_str()),
        ],
        Kind::Skills | Kind::Capabilities => item
            .inventory
            .as_ref()
            .filter(|info| !info.groups.is_empty())
            .map_or_else(
                || vec!["未核验"],
                |info| info.groups.iter().map(String::as_str).collect(),
            ),
        _ => vec![],
    }
}

pub(crate) fn group_label(key: &str) -> &str {
    match key {
        "brew" => "Homebrew",
        "codex:default" => "Codex",
        "claude:default" => "Claude Code",
        "cursor:default" => "Cursor",
        "shared" => "共享技能",
        "declared-recipe" => "声明配方",
        _ => key,
    }
}

fn matches(item: &Item, search: &str, agent_filters: bool) -> bool {
    let meta = format!("{} {} {}", item.name, item.summary, item.details.join(" ")).to_lowercase();
    search.split_whitespace().all(|term| {
        if let Some(agent) = term
            .strip_prefix('@')
            .filter(|agent| agent_filters && !agent.contains('/'))
        {
            item.agents
                .iter()
                .any(|name| name.to_lowercase().contains(agent))
        } else {
            meta.contains(term)
        }
    })
}

impl Browser {
    fn new(view: View, kind: Kind) -> Self {
        Self {
            view,
            row: 0,
            search: String::new(),
            typing: false,
            show_unregistered: true,
            show_directories: false,
            kind,
            group: None,
            page: Page::List,
            notice: String::new(),
            offset: 0,
            page_rows: 1,
        }
    }

    fn groups(&self) -> Vec<&str> {
        self.view
            .items
            .iter()
            .flat_map(|item| group_keys(self.kind, item))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn switch_group(&mut self, forward: bool) {
        let groups = self.groups();
        if groups.is_empty() {
            return;
        }
        let active = groups
            .iter()
            .position(|key| Some(*key) == self.group.as_deref())
            .map_or(0, |index| index + 1);
        let count = groups.len() + 1;
        let next = (active + if forward { 1 } else { count - 1 }) % count;
        let group = next.checked_sub(1).map(|index| groups[index].to_owned());
        let id = self.selected();
        self.group = group;
        self.preserve(id.as_deref());
    }

    fn group_tabs(&self, columns: usize, max_rows: usize) -> Vec<String> {
        if max_rows == 0 {
            return Vec::new();
        }
        let mode = match self.kind {
            Kind::Apps => "来源",
            Kind::Skills | Kind::Capabilities => "安装位置",
            _ => return Vec::new(),
        };
        let heading = format!("←/→ {mode} ·");
        let mut lines = Vec::new();
        let mut line = heading;
        let mut selected_row = 0;
        for group in std::iter::once(None).chain(self.groups().into_iter().map(Some)) {
            let label = sanitize(group.map_or("全部", group_label), false);
            let label = presentation::fit(&label, columns.saturating_sub(4));
            let selected = group == self.group.as_deref();
            let tab = if selected {
                format!("[{label}]")
            } else {
                format!(" {label} ")
            };
            if presentation::width(&line) + 1 + presentation::width(&tab) > columns {
                lines.push(std::mem::replace(&mut line, String::from(" ")));
            }
            if selected {
                selected_row = lines.len();
            }
            let _ = write!(line, " {tab}");
        }
        lines.push(line);
        let first = selected_row.saturating_sub(max_rows - 1);
        lines.drain(..first);
        lines.truncate(max_rows);
        lines
    }

    fn visible(&self) -> Vec<&Item> {
        let search = self.search.to_lowercase();
        let agent_filters = matches!(self.kind, Kind::Skills | Kind::Capabilities);
        self.view
            .items
            .iter()
            .filter(|item| {
                (self.show_unregistered || item.managed)
                    && (self.show_directories || !item.directory)
                    && self
                        .group
                        .as_deref()
                        .is_none_or(|key| group_keys(self.kind, item).contains(&key))
                    && (search.is_empty() || matches(item, &search, agent_filters))
            })
            .collect()
    }

    fn selected(&self) -> Option<String> {
        self.visible().get(self.row).map(|item| item.id.clone())
    }

    fn preserve(&mut self, id: Option<&str>) {
        let visible = self.visible();
        self.row = visible
            .iter()
            .position(|item| Some(item.id.as_str()) == id)
            .unwrap_or_else(|| self.row.min(visible.len().saturating_sub(1)));
    }

    fn move_by(&mut self, step: isize) {
        if matches!(self.page, Page::List) {
            let max = self.visible().len().saturating_sub(1);
            self.row = self.row.saturating_add_signed(step).min(max);
        } else {
            self.offset = self.offset.saturating_add_signed(step);
        }
    }

    fn enter(&mut self, ctx: &Context, session: &Session, kind: Kind) {
        if !matches!(self.page, Page::List) {
            return;
        }
        let Some(id) = self.selected() else {
            return;
        };
        self.notice.clear();
        self.offset = 0;
        let agent_session = if kind == Kind::Agents {
            self.visible()
                .get(self.row)
                .filter(|item| item.management == Management::Adopted)
                .and_then(|item| crate::agent::codex_install::session::details(ctx, &item.id))
        } else {
            None
        };
        let skills_native = matches!(kind, Kind::Skills | Kind::Capabilities)
            && self
                .visible()
                .get(self.row)
                .is_some_and(|item| item.management == Management::Adopted)
            && crate::skills::native_skills::accepts_inventory_maintenance(ctx, &id);
        let target = self
            .visible()
            .get(self.row)
            .map_or(DetailTarget::Other, |item| {
                detail_target(ctx, session, kind, item, skills_native)
            });
        self.page = Page::Detail(Box::new(Details {
            actual: (kind == Kind::Config).then(|| session.observed(&id, false)),
            source: (kind == Kind::Config).then(|| session.observed(&id, true)),
            recovery: (kind == Kind::History)
                .then(|| RecoveryPreview::read(ctx, &id).ok().map(Box::new))
                .flatten(),
            agent_session,
            agent_removable: kind == Kind::Agents
                && crate::agent::resource_agents::resolve_session_profile(ctx, &id).is_ok(),
            skills_native,
            target,
        }));
    }

    fn open(&mut self, source: bool) {
        let Page::Detail(details) = &self.page else {
            return;
        };
        let file = if source {
            &details.source
        } else {
            &details.actual
        };
        let Some(file) = file else {
            return;
        };
        let result = match file {
            Ok(file) => file
                .read()
                .map(|text| (file.path().display().to_string(), text))
                .map_err(|err| err.to_string()),
            Err(err) => Err(err.to_string()),
        };
        match result {
            Ok((path, text)) => {
                self.offset = 0;
                self.notice.clear();
                self.page = Page::File { path, text };
            }
            Err(err) => self.notice = err,
        }
    }

    fn back(&mut self, ctx: &Context, kind: Kind, session: &Session) {
        let was_child = matches!(
            self.page,
            Page::File { .. } | Page::Operation(..) | Page::Recovery(_)
        );
        self.page = Page::List;
        self.notice.clear();
        self.offset = 0;
        if was_child {
            self.enter(ctx, session, kind);
        }
    }

    fn preview_operation(&mut self, ctx: &Context, action: Action) -> Option<NativeAction> {
        if ctx.chezmoi.source.is_some()
            || ctx.chezmoi.destination.is_some()
            || ctx.chezmoi.config.is_some()
        {
            self.notice = "错误表现　无法维护软件或技能。\n错误原因　当前指定了 chezmoi 配置目录（config_scope_unsupported）。\n正确用法　去掉 chezmoi 目录选项后再运行 mo。".into();
            return None;
        }
        if action == Action::Adopt
            && let Page::Detail(details) = &self.page
            && matches!(details.target, DetailTarget::App { adopt: false, .. })
        {
            self.notice = "这份安装暂无可登记的维护入口。请查看详情中的渠道和安装范围。".into();
            return None;
        }
        let id = self.selected()?;
        let resolve = || {
            if let Some(package) = crate::software::brew::candidate(ctx, &id)?
                && matches!(package.provider, Provider::BrewFormula { .. })
            {
                return Ok(package);
            }
            crate::catalog::target::resolve(ctx, &id)
        };
        let package = match resolve() {
            Ok(package) => package,
            Err(error) => {
                self.notice = crate::output::error_text(&error);
                return None;
            }
        };
        if matches!(package.provider, Provider::BrewFormula { .. }) {
            return Some(NativeAction::Brew {
                action,
                package: Box::new(package),
            });
        }
        if matches!(action, Action::Install | Action::Upgrade)
            && (package.exact_version_required
                || matches!(
                    package.provider,
                    Provider::Cargo { .. }
                        | Provider::Command {
                            available: None,
                            ..
                        }
                ))
        {
            return Some(NativeAction::AppVersion {
                package: Box::new(package),
                action,
            });
        }
        let prepare = || {
            if matches!(package.provider, Provider::Mise { .. }) {
                return crate::operations::plan(
                    ctx,
                    &package,
                    action,
                    None,
                    action == Action::Adopt,
                );
            }
            let supported = if action == Action::Adopt {
                matches!(
                    package.provider,
                    Provider::Npm { .. }
                        | Provider::NpmObserved { .. }
                        | Provider::Uv { .. }
                        | Provider::UvObserved { .. }
                        | Provider::CapabilityObserved { .. }
                        | Provider::Bun { .. }
                        | Provider::Cargo { .. }
                        | Provider::Command { .. }
                        | Provider::Brew { cask: true, .. }
                )
            } else {
                matches!(
                    package.provider,
                    Provider::Npm { .. }
                        | Provider::Bun { .. }
                        | Provider::Cargo { .. }
                        | Provider::Command { .. }
                        | Provider::Brew { cask: true, .. }
                )
            };
            if !supported {
                return Err(Error::attention(
                    "inventory_adapter_unavailable",
                    "该来源需使用对应的原生维护入口（mo help advanced）",
                ));
            }
            crate::operations::plan(ctx, &package, action, None, action == Action::Adopt)
        };
        self.preview_plan(prepare(), None);
        None
    }

    fn preview_plan(&mut self, result: Result<Plan>, version: Option<String>) {
        match result {
            Ok(plan) => {
                self.notice.clear();
                self.offset = 0;
                self.page = Page::Operation(Box::new(plan), version);
            }
            Err(error) => self.notice = crate::output::error_text(&error),
        }
    }

    fn prompt_app_version(&mut self, ctx: &Context, package: &Package, action: Action) {
        let mut version = String::new();
        let result = (|| {
            eprintln!(
                "输入 {} 的目标版本（直接按 Enter 取消）",
                sanitize(&package.name, false)
            );
            std::io::stdin().read_line(&mut version)?;
            let version = version.trim();
            if version.is_empty() {
                return Err(Error::attention("cancelled", "已取消操作"));
            }
            crate::operations::plan(ctx, package, action, Some(version), false)
        })();
        self.preview_plan(result, Some(version.trim().to_owned()));
    }

    fn finish_operation(&mut self, ctx: &Context, session: &mut Session) {
        let Page::Operation(plan, version) = std::mem::replace(&mut self.page, Page::List) else {
            return;
        };
        let action = plan.action;
        let result = match crate::operations::recover_before_write(ctx, None) {
            Ok(Some(recovered))
                if recovered.matches(action, &plan.package.id, plan.target.as_deref())
                    && recovered.original.plan.recipe_sha256 == plan.recipe_sha256 =>
            {
                Ok(recovered.result)
            }
            Ok(Some(_)) => {
                match crate::catalog::target::refresh(ctx, &plan.package).and_then(|package| {
                    crate::operations::plan(
                        ctx,
                        &package,
                        action,
                        version.as_deref(),
                        action == Action::Adopt,
                    )
                }) {
                    Ok(current) => {
                        self.page = Page::Operation(Box::new(current), version);
                        self.notice = "已检查上一次操作。请确认重新准备的计划。".into();
                        return;
                    }
                    Err(error) => Err(error),
                }
            }
            Ok(None) => crate::operations::execute(ctx, *plan, false, true),
            Err(error) => Err(error),
        };
        self.operation_result(ctx, session, action, result);
    }

    fn agent_action(&self, key: char) -> Option<NativeAction> {
        let Page::Detail(details) = &self.page else {
            return None;
        };
        let selector = self.visible().get(self.row)?.id.clone();
        if key == 'x' {
            return details
                .agent_removable
                .then_some(NativeAction::AgentUninstall(selector));
        }
        details.agent_session.as_ref()?;
        Some(if key == 'e' {
            NativeAction::AgentPreference(selector)
        } else {
            NativeAction::AgentLaunch(selector)
        })
    }

    fn detail_action(&self, key: char) -> Option<NativeAction> {
        use crate::{
            cli::{AgentCmd, Command, Config, SkillCmd},
            domain::resource::CapabilityAction,
        };
        let Page::Detail(details) = &self.page else {
            return None;
        };
        let id = self.selected()?;
        let (command, title, confirm, report) = match (&details.target, key) {
            (DetailTarget::SharedSkill, 'u' | 'x') => {
                return Some(NativeAction::SharedSkill {
                    id,
                    action: if key == 'u' {
                        Action::Upgrade
                    } else {
                        Action::Uninstall
                    },
                });
            }
            (DetailTarget::SharedSkill, 'a') => (
                Command::Skill {
                    project: None,
                    details: false,
                    only: vec![],
                    command: Some(SkillCmd::Adopt { id, recipe: None }),
                },
                "技能管理结果",
                None,
                false,
            ),
            (DetailTarget::Plugin { claude }, key) => {
                let action = match key {
                    'u' if *claude => CapabilityAction::Upgrade,
                    'e' if *claude => CapabilityAction::Enable,
                    'd' if *claude => CapabilityAction::Disable,
                    'x' => CapabilityAction::Uninstall,
                    _ => return None,
                };
                return Some(NativeAction::Plugin { id, action });
            }
            (DetailTarget::AgentCli(tool), 'u') => (
                Command::Agent {
                    project: None,
                    command: Some(AgentCmd::Upgrade {
                        tool: Some(tool.clone()),
                        force_live: false,
                    }),
                },
                "Agent CLI 升级结果",
                Some(format!(
                    "升级 Agent CLI {tool}？使用此程序的配置都会使用新版本。"
                )),
                false,
            ),
            (DetailTarget::Config { path, forget }, key) => {
                let target = || crate::policy::config_local::config_id(path).ok();
                let (command, title, report) = match key {
                    'd' => (
                        Config::Diff {
                            path: Some(path.clone()),
                        },
                        "配置差异",
                        true,
                    ),
                    'v' => (
                        Config::Verify {
                            targets: vec![path.clone()],
                        },
                        "配置核验结果",
                        true,
                    ),
                    'p' => (Config::Apply { target: target()? }, "配置应用结果", false),
                    'x' if *forget => (Config::Forget { target: target()? }, "停止管理结果", false),
                    _ => return None,
                };
                (
                    Command::Config {
                        project: None,
                        command: Some(command),
                    },
                    title,
                    None,
                    report,
                )
            }
            _ => return None,
        };
        Some(NativeAction::Command {
            command: Box::new(command),
            title,
            confirm,
            report,
        })
    }

    fn detail_shortcut(&self, event: &KeyEvent) -> Option<NativeAction> {
        let KeyCode::Char(key) = event.code else {
            return None;
        };
        if !event.modifiers.is_empty() {
            return None;
        }
        if self.kind == Kind::Agents && matches!(key, 'e' | 'l' | 'x') {
            self.agent_action(key)
        } else {
            self.detail_action(key)
        }
    }

    fn maintenance_key(&mut self, ctx: &Context, key: char) -> Option<NativeAction> {
        let Page::Detail(detail) = &self.page else {
            return None;
        };
        let action = match key {
            'i' => Action::Install,
            'u' => Action::Upgrade,
            _ => Action::Uninstall,
        };
        if detail.skills_native && action != Action::Install {
            return Some(NativeAction::Skills {
                action,
                id: self.selected()?,
            });
        }
        if self.kind == Kind::Apps {
            if matches!(detail.target, DetailTarget::App { install, upgrade, uninstall, .. }
                if !(match action { Action::Install => install, Action::Upgrade => upgrade, _ => uninstall }))
            {
                self.notice = "这份安装暂无已核验的维护入口。请用详情中的原安装工具维护。".into();
                return None;
            }
            return self.preview_operation(ctx, action);
        }
        None
    }

    fn run_native(&mut self, ctx: &Context, session: &mut Session, action: NativeAction) {
        if let NativeAction::AgentUninstall(label) = &action {
            let result = crate::agent::resource_agents::uninstall(ctx, label, false, false);
            self.refresh(ctx, session);
            self.notice = match result {
                Ok(data) => format!(
                    "单独配置已归档 · {}",
                    data["archive"].as_str().unwrap_or("")
                ),
                Err(error) => crate::output::error_text(&error),
            };
            return;
        }
        let result = match action {
            NativeAction::AppVersion { package, action } => {
                self.prompt_app_version(ctx, &package, action);
                return;
            }
            NativeAction::Plugin { id, action } => {
                let result = crate::skills::portfolio::maintain_from_inventory(ctx, &id, action);
                match result {
                    Ok(value) => {
                        self.reload_detail(ctx, session);
                        let rows = value["results"].as_array().map_or(&[][..], Vec::as_slice);
                        self.notice =
                            crate::skills::portfolio::batch_text(rows, presentation::table_width());
                    }
                    Err(error) => self.notice = crate::output::error_text(&error),
                }
                return;
            }
            NativeAction::Command {
                command,
                title,
                confirm,
                report,
            } => {
                let result = (|| {
                    if let Some(message) = confirm {
                        crate::operations::confirm(&message, false)?;
                    }
                    crate::run_inventory_command(ctx, *command)
                })();
                match result {
                    Ok(text) if report => {
                        self.notice.clear();
                        self.offset = 0;
                        self.page = Page::File {
                            path: title.into(),
                            text,
                        };
                    }
                    Ok(text) => {
                        self.reload_detail(ctx, session);
                        self.notice = format!("{title}\n{}", text.trim_end());
                    }
                    Err(error) => self.notice = crate::output::error_text(&error),
                }
                return;
            }
            NativeAction::Brew { action, package } => {
                let result =
                    crate::software::brew::execute_selected(ctx, &package, action, false, false);
                self.operation_result(ctx, session, action, result);
                return;
            }
            NativeAction::Skills { action, id } => {
                let result =
                    crate::skills::native_skills::maintain_from_inventory(ctx, &id, action);
                self.operation_result(ctx, session, action, result);
                return;
            }
            NativeAction::SharedSkill { action, id } => {
                let result =
                    crate::skills::native_skills::maintain_shared_from_inventory(ctx, &id, action);
                self.operation_result(ctx, session, action, result);
                return;
            }
            NativeAction::AgentPreference(label) => {
                crate::agent::codex_install::session::configure(ctx, &label)
            }
            NativeAction::AgentLaunch(label) => {
                crate::agent::codex_install::session::launch(ctx, &label, None, false, false)
                    .map(|_| ())
            }
            NativeAction::AgentUninstall(_) => return,
        };
        self.refresh(ctx, session);
        self.notice = match result {
            Ok(()) => "已保存下次由 mo 启动时的插件设置".into(),
            Err(error) => crate::output::error_text(&error),
        };
    }

    fn reload_detail(&mut self, ctx: &Context, session: &mut Session) {
        let id = self.selected();
        if self.kind == Kind::Config {
            // Reopen the cache after mutation without refreshing Native or publishing metadata.
            if let Ok((fresh, view)) = Session::load(ctx, self.kind) {
                *session = fresh;
                self.view = view;
            }
            self.page = Page::List;
            self.preserve(id.as_deref());
        } else {
            self.refresh(ctx, session);
        }
        if id.is_some() && self.selected() == id {
            self.enter(ctx, session, self.kind);
        }
    }

    fn operation_result(
        &mut self,
        ctx: &Context,
        session: &mut Session,
        action: Action,
        result: Result<serde_json::Value>,
    ) {
        let label = operation_label(action);
        self.refresh(ctx, session);
        self.notice = match result {
            Ok(value) if value["status"] == "current" => "已是目标版本。".into(),
            Ok(value) if value["status"] == "changed" => format!("{label}结果 已完成"),
            Ok(value) => {
                let receipt = value.get("journal").unwrap_or(&value);
                let mut notice = format!(
                    "{label}结果 {}",
                    crate::output::phase_text(receipt["phase"].as_str().unwrap_or_default())
                );
                if let Some(hint) = crate::output::recovery_hint(&value) {
                    let _ = write!(notice, "\n{hint}");
                }
                notice
            }
            Err(error) => crate::output::error_text(&error),
        };
    }

    fn preview_recovery(&mut self) {
        if let Page::Detail(details) = &mut self.page
            && let Some(preview) = details.recovery.take()
        {
            self.notice.clear();
            self.offset = 0;
            self.page = Page::Recovery(preview);
        }
    }

    fn finish_recovery(&mut self, ctx: &Context, session: &mut Session) {
        let Page::Recovery(preview) = std::mem::replace(&mut self.page, Page::List) else {
            return;
        };
        let result = preview.confirm(ctx);
        self.refresh(ctx, session);
        self.notice = match result {
            Ok(value) => recovery_notice(&value),
            Err(error) => crate::output::error_text(&error),
        };
    }

    fn type_key(&mut self, code: KeyCode) {
        let id = self.selected();
        match code {
            KeyCode::Enter => self.typing = false,
            KeyCode::Esc => {
                self.search.clear();
                self.typing = false;
            }
            KeyCode::Backspace => {
                self.search.pop();
            }
            KeyCode::Char(ch) if self.search.len() < 256 => self.search.push(ch),
            _ => {}
        }
        self.preserve(id.as_deref());
    }

    fn refresh(&mut self, ctx: &Context, session: &mut Session) {
        let id = self.selected();
        self.notice.clear();
        self.page = Page::List;
        match session.refresh(ctx) {
            Ok(view) => self.view = view,
            Err(error) => {
                if let Session::Config(browser) = session {
                    self.view = browser.view();
                }
                self.notice = crate::output::error_text(&error);
            }
        }
        if self
            .group
            .as_deref()
            .is_some_and(|key| !self.groups().contains(&key))
        {
            self.group = None;
        }
        self.preserve(id.as_deref());
    }

    fn refresh_from_key(&mut self, ctx: &Context, session: &mut Session) {
        if self.kind == Kind::Config
            && let Err(error) = crate::operations::recover_before_write(ctx, None)
        {
            self.notice = crate::output::error_text(&error);
            return;
        }
        self.refresh(ctx, session);
    }

    fn header_lines(
        &self,
        count: usize,
        unified: bool,
        width: usize,
        height: usize,
    ) -> Vec<String> {
        let filter = match self.kind {
            Kind::Skills | Kind::Capabilities if self.show_unregistered => " · 全部发现",
            Kind::Skills | Kind::Capabilities => " · 已登记筛选",
            _ => "",
        };
        let total = self.view.items.len().to_string();
        let digits = total.len();
        let title = format!(
            "{} · {count:>digits$} / {total}{filter} · {}",
            self.kind.title(),
            sanitize(&self.view.note, false),
        );
        let mut lines = Vec::new();
        if unified {
            let tabs = Kind::TABS.map(|tab| {
                if tab == self.kind {
                    format!("[{}]", tab.title())
                } else {
                    format!(" {} ", tab.title())
                }
            });
            lines.push(format!("Tab 切换 · {}", tabs.join("  ")));
        }
        lines.push(title);
        lines.extend(self.group_tabs(width, height.saturating_sub(lines.len() + 1)));
        let query = sanitize(&self.search, false);
        lines.push(if self.typing || !query.is_empty() {
            let cursor = if self.typing { "▏" } else { "" };
            format!("/ 搜索 · {query}{cursor}")
        } else {
            "/ 搜索".into()
        });
        lines
    }

    fn draw(&mut self, ctx: &Context, frame: &mut Frame, kind: Kind, unified: bool) {
        let visible = self.visible();
        let inset = if matches!(self.page, Page::List) {
            3
        } else {
            2
        };
        let below_header = inset + 2;
        let lines = self.header_lines(
            visible.len(),
            unified,
            usize::from(frame.area().width),
            usize::from(frame.area().height.saturating_sub(below_header)),
        );
        let header_height = u16::try_from(lines.len())
            .unwrap_or(u16::MAX)
            .min(frame.area().height.saturating_sub(2));
        let [header, main, footer] =
            layout_with_notice(frame, &self.notice, header_height, below_header);
        let page_rows = isize::try_from(main.height.saturating_sub(inset).max(1)).unwrap_or(1);
        let text = lines
            .iter()
            .take(usize::from(header.height))
            .map(|line| presentation::fit(line, usize::from(header.width)))
            .collect::<Vec<_>>()
            .join("\n");
        frame.render_widget(Paragraph::new(text), header);
        let mut offset = self.offset;
        match &self.page {
            Page::Recovery(preview) => detail(
                frame,
                main,
                "核对原操作",
                &recovery_summary(preview),
                &mut offset,
            ),
            Page::Operation(plan, _) => detail(
                frame,
                main,
                &format!("{}预览", operation_label(plan.action)),
                &format!(
                    "{}\n当前版本 {}\n目标版本 {}\n{}\n{}",
                    sanitize(&plan.package.name, false),
                    sanitize(plan.before.version().unwrap_or("未知"), false),
                    sanitize(plan.target.as_deref().unwrap_or("—"), false),
                    plan::plan_scope(ctx, plan),
                    if plan.action == Action::Adopt {
                        "记录现有安装基线，保留文件和版本。".into()
                    } else {
                        String::new()
                    }
                ),
                &mut offset,
            ),
            Page::File { path, text } => {
                detail(
                    frame,
                    main,
                    &sanitize(path, false),
                    &sanitize(text, true),
                    &mut offset,
                );
            }
            Page::Detail(selection) => {
                if let Some(item) = visible.get(self.row) {
                    detail(
                        frame,
                        main,
                        &sanitize(&item.name, false),
                        &item_details(item, selection.agent_session.as_ref()),
                        &mut offset,
                    );
                }
            }
            Page::List if main.width >= 100 => {
                let [list, side] =
                    Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                        .areas(main);
                self.list(frame, list, &visible);
                if let Some(item) = visible.get(self.row) {
                    detail(frame, side, "管理详情", &details(item), &mut offset);
                }
            }
            Page::List => self.list(frame, main, &visible),
        }
        frame.render_widget(Paragraph::new(keys(kind, &self.page, unified)), footer);
        self.page_rows = page_rows;
        self.offset = offset;
    }

    fn draw_adoption(&self, frame: &mut Frame) {
        let [header, list, selection, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let visible = self.visible();
        let search = if self.typing { " · 搜索 " } else { " " };
        let title = format!(
            "纳入管理 · {} / {} · {}{search}{}",
            visible.len(),
            self.view.items.len(),
            self.view.note,
            sanitize(&self.search, false)
        );
        frame.render_widget(Paragraph::new(title), header);
        self.list(frame, list, &visible);
        if let Some(item) = visible.get(self.row) {
            detail(frame, selection, "所选安装项", &details(item), &mut 0);
        }
        frame.render_widget(
            Paragraph::new("j/k 滚动  / 搜索  Enter 选择  Esc 取消"),
            footer,
        );
    }

    fn list(&self, frame: &mut Frame, area: Rect, visible: &[&Item]) {
        let headers = crate::browser::list_headers(self.kind);
        let rows = visible
            .iter()
            .map(|item| {
                crate::browser::list_row(self.kind, item)
                    .into_iter()
                    .map(|value| sanitize(&value, false))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        let budget = usize::from(area.width.saturating_sub(4));
        let (headers, widths) = presentation::visible_columns(headers, &rows, budget);
        let widths = widths
            .into_iter()
            .map(|width| Constraint::Length(u16::try_from(width).unwrap_or(area.width)));
        let mut state = TableState::default().with_selected((!rows.is_empty()).then_some(self.row));
        let values = rows
            .into_iter()
            .map(|row| Row::new(row.into_iter().take(headers.len())));
        let list = Table::new(values, widths)
            .header(Row::new(headers.iter().copied()).style(ratatui::style::Modifier::BOLD))
            .column_spacing(2)
            .block(Block::default().borders(Borders::ALL))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }
}

fn recovery_summary(preview: &RecoveryPreview) -> String {
    let journal = &preview.journal;
    format!(
        "{}\n目标 {}\n原状态目录 {}\n根据原证据补写记录，或回退该操作的改动。安装器不会重新启动。",
        sanitize(&journal.id, false),
        sanitize(&journal.plan.package.id, false),
        sanitize(&journal.state.display().to_string(), false),
    )
}

fn detail(frame: &mut Frame, area: Rect, title: &str, text: &str, offset: &mut usize) {
    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title.to_owned()),
        )
        .wrap(Wrap { trim: false });
    let lines = paragraph.line_count(area.width.saturating_sub(2));
    let maximum = lines
        .saturating_sub(usize::from(area.height))
        .min(usize::from(u16::MAX));
    *offset = (*offset).min(maximum);
    frame.render_widget(
        paragraph.scroll((u16::try_from(*offset).unwrap_or(u16::MAX), 0)),
        area,
    );
}

fn details(item: &Item) -> String {
    item.details
        .iter()
        .map(|line| sanitize(line, false))
        .collect::<Vec<_>>()
        .join("\n")
}

fn item_details(
    item: &Item,
    session: Option<&crate::agent::codex_install::session::Details>,
) -> String {
    let mut text = details(item);
    if let Some(session) = session {
        use std::fmt::Write;
        let _ = write!(
            text,
            "\n档案 ID {}\n基础模型 {}\n插件 {}\n下次由 mo 启动时 {}\n会话载入 未知",
            item.id,
            sanitize(session.observed_model.as_deref().unwrap_or("默认"), false),
            sanitize(&session.plugin, false),
            session.preference
        );
    }
    text
}

fn layout_with_notice(
    frame: &mut Frame,
    message: &str,
    header_height: u16,
    below_header: u16,
) -> [Rect; 3] {
    let limit = frame
        .area()
        .height
        .saturating_sub(header_height.saturating_add(below_header))
        .min(4);
    let notice = Paragraph::new(sanitize(message, true)).wrap(Wrap { trim: true });
    let height = if message.is_empty() {
        0
    } else {
        u16::try_from(notice.line_count(frame.area().width))
            .unwrap_or(limit)
            .min(limit)
    };
    let [header, main, feedback, footer] = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(height),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    if height > 0 {
        frame.render_widget(notice, feedback);
    }
    [header, main, footer]
}

fn recovery_notice(value: &serde_json::Value) -> String {
    use crate::policy::cli::{Continuation, continuation};
    if value["reason"] == "operation_busy" {
        return "另一个操作正在使用记录。等它结束后再检查。".into();
    }
    match continuation(value) {
        Some(Continuation::Ready) => {
            return value["continuationCommand"].as_str().map_or_else(
                || "原阶段已核对。运行 mo recover 查看下一步。".into(),
                |command| format!("原阶段已核对。继续请运行 {}", sanitize(command, false)),
            );
        }
        Some(Continuation::Unverified) => return "核对结果 后续步骤待核对".into(),
        None => {}
    }
    let journal = value.get("journal").unwrap_or(value);
    if value["recoveryBlocker"].is_string()
        && let Some(message) = journal["message"].as_str()
    {
        return format!("仍待核对 · {message}");
    }
    let phase = journal
        .get("phase")
        .or_else(|| value.get("status"))
        .and_then(serde_json::Value::as_str);
    let status = match phase {
        Some("succeeded") => "已完成",
        Some("failed") => "已记录失败",
        _ => "仍待核对",
    };
    format!("核对结果 {status}")
}

fn operation_label(action: Action) -> &'static str {
    match action {
        Action::Install => "安装",
        Action::Adopt => "纳入管理",
        Action::Upgrade => "升级",
        _ => "卸载",
    }
}

fn keys(kind: Kind, page: &Page, unified: bool) -> std::borrow::Cow<'static, str> {
    if let Page::Detail(details) = page
        && let DetailTarget::App {
            adopt,
            install,
            upgrade,
            uninstall,
        } = details.target
        && (adopt || install || upgrade || uninstall)
    {
        return [
            (install, "i 安装"),
            (adopt, "a 检查管理资格"),
            (upgrade, "u 升级"),
            (uninstall, "x 卸载"),
            (true, "Esc 返回  q 退出"),
        ]
        .into_iter()
        .filter_map(|(enabled, label)| enabled.then_some(label))
        .collect::<Vec<_>>()
        .join("  ")
        .into();
    }
    let text = match page {
        Page::List if matches!(kind, Kind::Skills | Kind::Capabilities) => {
            "j/k 滚动  / 搜索（@档案）  Enter 详情  a 切换发现项  r 刷新  q 退出"
        }
        Page::List if kind == Kind::Config => {
            "j/k 滚动  / 搜索  Enter 详情  d 切换目录  r 刷新  q 退出"
        }
        Page::List if kind == Kind::Marketplaces => "j/k 滚动  / 搜索  Enter 详情  q 退出",
        Page::List => "j/k 滚动  / 搜索  Enter 详情  r 刷新  q 退出",
        Page::Operation(plan, _) => match plan.action {
            Action::Install => "Enter 确认安装  Esc 返回  q 退出",
            Action::Adopt => "Enter 确认纳入管理  Esc 返回  q 退出",
            Action::Upgrade => "Enter 确认升级  Esc 返回  q 退出",
            _ => "Enter 确认卸载  Esc 返回  q 退出",
        },
        Page::Recovery(_) => "Enter 确认核对  Esc 返回  q 退出",
        Page::Detail(details) if details.recovery.is_some() => "c 核对原操作  Esc 返回  q 退出",
        Page::Detail(details) if details.agent_session.is_some() => {
            "e 插件设置  l 启动档案  x 卸载  Esc 返回  q 退出"
        }
        Page::Detail(details) if details.agent_removable => "x 卸载  Esc 返回  q 退出",
        Page::Detail(details) if details.skills_native => "u 升级  x 卸载  Esc 返回  q 退出",
        Page::Detail(details) if matches!(details.target, DetailTarget::SharedSkill) => {
            "u 升级  x 卸载  a 接管技能  Esc 返回  q 退出"
        }
        Page::Detail(details)
            if matches!(details.target, DetailTarget::Plugin { claude: true }) =>
        {
            "e 启用  d 停用  u 升级  x 卸载  Esc 返回  q 退出"
        }
        Page::Detail(details)
            if matches!(details.target, DetailTarget::Plugin { claude: false }) =>
        {
            "x 卸载插件  Esc 返回  q 退出"
        }
        Page::Detail(details) if matches!(details.target, DetailTarget::AgentCli(_)) => {
            "u 升级 Agent CLI  Esc 返回  q 退出"
        }
        Page::Detail(details)
            if matches!(details.target, DetailTarget::Config { forget: true, .. }) =>
        {
            "o 打开实际文件  s 打开管理源  d 差异  v 核验  p 应用  x 停止管理  Esc 返回  q 退出"
        }
        Page::Detail(details) if matches!(details.target, DetailTarget::Config { .. }) => {
            "o 打开实际文件  s 打开管理源  d 差异  v 核验  p 应用  Esc 返回  q 退出"
        }
        Page::Detail(_) if kind == Kind::Config => "o 打开实际文件  s 打开管理源  Esc 返回  q 退出",
        Page::Detail(_) if kind == Kind::Apps => "只读 · 请用原安装工具维护  Esc 返回  q 退出",
        Page::Detail(_) if unified && kind == Kind::Capabilities => {
            "a 检查管理资格  Esc 返回  q 退出"
        }
        Page::Detail(_) | Page::File { .. } => "j/k 滚动  Esc 返回  q 退出",
    };
    text.into()
}

fn control(event: &KeyEvent, key: char) -> bool {
    event.code == KeyCode::Char(key) && event.modifiers.contains(KeyModifiers::CONTROL)
}

fn switch_tab(ctx: &Context, session: &mut Session, kind: Kind, forward: bool) -> Result<Browser> {
    let tabs = Kind::TABS;
    let index = tabs.iter().position(|tab| *tab == kind).unwrap_or_default();
    let step = if forward { 1 } else { tabs.len() - 1 };
    let next = tabs[(index + step) % tabs.len()];
    let (loaded, view) = Session::load(ctx, next)?;
    *session = loaded;
    Ok(Browser::new(view, next))
}

pub(crate) fn pick_adoption(view: View) -> Result<Option<String>> {
    let mut browser = Browser::new(view, Kind::Apps);
    let (mut terminal, _restore) = screen()?;
    loop {
        terminal.draw(|frame| browser.draw_adoption(frame))?;
        let Event::Key(event) = event::read()? else {
            continue;
        };
        if event.kind == KeyEventKind::Release {
            continue;
        }
        if control(&event, 'c') {
            return Ok(None);
        }
        if browser.typing {
            browser.type_key(event.code);
            continue;
        }
        match event.code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
            KeyCode::Enter => {
                if let Some(id) = browser.selected() {
                    return Ok(Some(id));
                }
            }
            KeyCode::Char('/') => browser.typing = true,
            KeyCode::Char('j') | KeyCode::Down => browser.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => browser.move_by(-1),
            _ => {}
        }
    }
}

pub(crate) fn browse(
    ctx: &Context,
    kind: Kind,
    session: &mut Session,
    view: View,
    unified: bool,
) -> Result<View> {
    let mut browser = Browser::new(view, kind);
    loop {
        let (mut terminal, restore) = screen()?;
        let after = loop {
            let kind = browser.kind;
            terminal.draw(|frame| browser.draw(ctx, frame, kind, unified))?;
            let Event::Key(event) = event::read()? else {
                continue;
            };
            if event.kind == KeyEventKind::Release {
                continue;
            }
            if control(&event, 'c') {
                return Ok(browser.view);
            }
            if browser.typing {
                browser.type_key(event.code);
                continue;
            }
            if let Some(action) = browser.detail_shortcut(&event) {
                break After::Native(action);
            }
            let rows = browser.page_rows;
            match event.code {
                KeyCode::Tab | KeyCode::BackTab if unified => {
                    match switch_tab(ctx, session, kind, event.code == KeyCode::Tab) {
                        Ok(next) => browser = next,
                        Err(error) => browser.notice = crate::output::error_text(&error),
                    }
                }
                KeyCode::Char('q') => return Ok(browser.view),
                KeyCode::Esc => browser.back(ctx, kind, session),
                KeyCode::Char('/') if matches!(browser.page, Page::List) => browser.typing = true,
                KeyCode::Left | KeyCode::Right if matches!(browser.page, Page::List) => {
                    browser.switch_group(event.code == KeyCode::Right);
                }
                KeyCode::Enter if matches!(browser.page, Page::Operation(..)) => {
                    break After::FinishOperation;
                }
                KeyCode::Enter if matches!(browser.page, Page::Recovery(_)) => {
                    break After::FinishRecovery;
                }
                KeyCode::Enter => browser.enter(ctx, session, kind),
                KeyCode::Char('c') if kind == Kind::History => browser.preview_recovery(),
                KeyCode::Char('o') => browser.open(false),
                KeyCode::Char('s') => browser.open(true),
                KeyCode::Char('j') | KeyCode::Down => browser.move_by(1),
                KeyCode::Char('k') | KeyCode::Up => browser.move_by(-1),
                KeyCode::PageDown => browser.move_by(rows),
                KeyCode::PageUp => browser.move_by(-rows),
                KeyCode::Char('d') if control(&event, 'd') => browser.move_by((rows / 2).max(1)),
                KeyCode::Char('u') if control(&event, 'u') => browser.move_by(-(rows / 2).max(1)),
                KeyCode::Char('a')
                    if matches!(kind, Kind::Skills | Kind::Capabilities)
                        && matches!(browser.page, Page::List) =>
                {
                    let id = browser.selected();
                    browser.show_unregistered = !browser.show_unregistered;
                    browser.preserve(id.as_deref());
                }
                KeyCode::Char('a')
                    if (kind == Kind::Apps || (unified && kind == Kind::Capabilities))
                        && matches!(browser.page, Page::Detail(_)) =>
                {
                    if let Some(action) = browser.preview_operation(ctx, Action::Adopt) {
                        break After::Native(action);
                    }
                }
                KeyCode::Char(key @ ('i' | 'u' | 'x')) => {
                    if let Some(action) = browser.maintenance_key(ctx, key) {
                        break After::Native(action);
                    }
                }
                KeyCode::Char('d')
                    if kind == Kind::Config && matches!(browser.page, Page::List) =>
                {
                    let id = browser.selected();
                    browser.show_directories = !browser.show_directories;
                    browser.preserve(id.as_deref());
                }
                KeyCode::Char('r')
                    if kind != Kind::Marketplaces && matches!(browser.page, Page::List) =>
                {
                    break After::Refresh;
                }
                _ => {}
            }
        };
        drop(terminal);
        drop(restore);
        match after {
            After::Native(action) => browser.run_native(ctx, session, action),
            After::FinishOperation => browser.finish_operation(ctx, session),
            After::FinishRecovery => browser.finish_recovery(ctx, session),
            After::Refresh => browser.refresh_from_key(ctx, session),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/terminal_agent.rs"]
mod agent_tests;
