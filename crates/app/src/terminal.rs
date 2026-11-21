use crate::{
    browser::{Item, Kind, Session, View, sanitize},
    chezmoi_native::ViewerFile,
    context::Context,
    domain::{Action, Plan, Provider},
    error::{Error, Result},
    resource::Management,
};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Rect},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::io::{self, Stdout};

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    }
}

fn screen() -> Result<(Terminal<CrosstermBackend<Stdout>>, Restore)> {
    terminal::enable_raw_mode()?;
    let restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen)?;
    Ok((Terminal::new(CrosstermBackend::new(io::stdout()))?, restore))
}

enum Page {
    List,
    Detail(Box<Details>),
    File { path: String, text: String },
    Operation(Box<Plan>),
}

struct Details {
    actual: Option<Result<ViewerFile>>,
    source: Option<Result<ViewerFile>>,
}

struct Browser {
    view: View,
    row: usize,
    search: String,
    typing: bool,
    show_unregistered: bool,
    show_directories: bool,
    kind: Kind,
    page: Page,
    notice: String,
    offset: usize,
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
            show_unregistered: kind != Kind::Skills,
            show_directories: false,
            kind,
            page: Page::List,
            notice: String::new(),
            offset: 0,
        }
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

    fn enter(&mut self, session: &Session, kind: Kind) {
        if !matches!(self.page, Page::List) {
            return;
        }
        let Some(id) = self.selected() else {
            return;
        };
        self.notice.clear();
        self.offset = 0;
        self.page = Page::Detail(Box::new(Details {
            actual: (kind == Kind::Config).then(|| session.observed(&id, false)),
            source: (kind == Kind::Config).then(|| session.observed(&id, true)),
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

    fn back(&mut self, kind: Kind, session: &Session) {
        let was_child = matches!(self.page, Page::File { .. } | Page::Operation(_));
        self.page = Page::List;
        self.notice.clear();
        self.offset = 0;
        if was_child {
            self.enter(session, kind);
        }
    }

    fn preview_operation(&mut self, ctx: &Context, action: Action) {
        if ctx.chezmoi.source.is_some()
            || ctx.chezmoi.destination.is_some()
            || ctx.chezmoi.config.is_some()
        {
            self.notice = "config_scope_unsupported · 已限定配置源，不能维护软件或能力".into();
            return;
        }
        let Some(id) = self.selected() else { return };
        let prepare = || {
            let package = crate::target::resolve(ctx, &id)?;
            let supported = if action == Action::Adopt {
                matches!(
                    package.provider,
                    Provider::Npm { .. }
                        | Provider::NpmObserved { .. }
                        | Provider::Uv { .. }
                        | Provider::UvObserved { .. }
                        | Provider::CapabilityObserved { .. }
                )
            } else {
                crate::policy::manifest::supports_replay(&package.provider)
            };
            if !supported {
                return Err(Error::attention(
                    "inventory_adapter_unavailable",
                    "该来源需使用对应的原生维护入口（mo help advanced）",
                ));
            }
            crate::operations::plan(ctx, &package, action, None, action == Action::Adopt)
        };
        match prepare() {
            Ok(plan) => {
                self.notice.clear();
                self.offset = 0;
                self.page = Page::Operation(Box::new(plan));
            }
            Err(error) => self.notice = error.to_string(),
        }
    }

    fn finish_operation(&mut self, ctx: &Context, session: &mut Session) {
        let Page::Operation(plan) = std::mem::replace(&mut self.page, Page::List) else {
            return;
        };
        let label = operation_label(plan.action);
        let result = crate::operations::execute(ctx, *plan, false, true);
        self.refresh(ctx, session);
        self.notice = match result {
            Ok(value) => {
                let receipt = value.get("journal").unwrap_or(&value);
                format!(
                    "{label}结果 {} · {}",
                    receipt["phase"].as_str().unwrap_or("待核对"),
                    receipt["id"].as_str().unwrap_or("待查")
                )
            }
            Err(error) => error.to_string(),
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
                self.notice = error.to_string();
            }
        }
        self.preserve(id.as_deref());
    }

    fn draw(&mut self, frame: &mut Frame, kind: Kind, unified: bool) {
        let [header, main, footer] = Layout::vertical([
            Constraint::Length(if unified { 2 } else { 1 }),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let visible = self.visible();
        let search = if self.typing { "搜索 " } else { "" };
        let filter = match kind {
            Kind::Skills | Kind::Capabilities if self.show_unregistered => " · 全部发现",
            Kind::Skills | Kind::Capabilities => " · 已登记筛选",
            _ => "",
        };
        let title = format!(
            "{} · {} / {}{filter} · {} {search}{}",
            kind.title(),
            visible.len(),
            self.view.items.len(),
            sanitize(&self.view.note, false),
            sanitize(&self.search, false)
        );
        let text = if unified {
            let tabs = Kind::TABS.map(|tab| {
                if tab == kind {
                    format!("[{}]", tab.title())
                } else {
                    tab.title().to_owned()
                }
            });
            format!("Tab 切换 · {}\n{title}", tabs.join("  "))
        } else {
            title
        };
        frame.render_widget(Paragraph::new(text), header);
        let mut offset = self.offset;
        match &self.page {
            Page::Operation(plan) => detail(
                frame,
                main,
                &format!("{}预览", operation_label(plan.action)),
                &format!(
                    "{}\n当前版本 {}\n目标版本 {}\n{}\n{}",
                    sanitize(&plan.package.name, false),
                    sanitize(plan.before.version().unwrap_or("未知"), false),
                    sanitize(plan.target.as_deref().unwrap_or("—"), false),
                    crate::recommend::plan_scope(plan),
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
            Page::Detail(_) => {
                if let Some(item) = visible.get(self.row) {
                    detail(
                        frame,
                        main,
                        &sanitize(&item.name, false),
                        &details(item),
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
        let help = if self.notice.is_empty() {
            keys(kind, &self.page, unified)
        } else {
            &self.notice
        };
        frame.render_widget(Paragraph::new(sanitize(help, false)), footer);
        self.offset = offset;
    }

    fn list(&self, frame: &mut Frame, area: Rect, visible: &[&Item]) {
        let items: Vec<_> = visible
            .iter()
            .map(|item| {
                let status = match item.management {
                    Management::Declared => "有声明",
                    Management::Native => "原生登记",
                    Management::Adopted => "受管",
                    Management::Untracked => "已发现",
                };
                ListItem::new(format!(
                    "{}  {}  {}",
                    sanitize(&item.name, false),
                    status,
                    sanitize(&item.summary, false)
                ))
            })
            .collect();
        let mut state = ListState::default().with_selected((!items.is_empty()).then_some(self.row));
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, area, &mut state);
    }
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

fn operation_label(action: Action) -> &'static str {
    match action {
        Action::Adopt => "纳管",
        Action::Upgrade => "升级",
        _ => "卸载",
    }
}

fn keys(kind: Kind, page: &Page, unified: bool) -> &'static str {
    match page {
        Page::List if matches!(kind, Kind::Skills | Kind::Capabilities) => {
            "j/k 滚动  / 搜索（@档案）  Enter 详情  a 切换发现项  r 刷新  q 退出"
        }
        Page::List if kind == Kind::Config => {
            "j/k 滚动  / 搜索  Enter 详情  d 切换目录  r 刷新  q 退出"
        }
        Page::List => "j/k 滚动  / 搜索  Enter 详情  r 刷新  q 退出",
        Page::Operation(plan) => match plan.action {
            Action::Adopt => "Enter 确认纳管  Esc 返回  q 退出",
            Action::Upgrade => "Enter 确认升级  Esc 返回  q 退出",
            _ => "Enter 确认卸载  Esc 返回  q 退出",
        },
        Page::Detail(_) if kind == Kind::Config => "o 打开实际文件  s 打开管理源  Esc 返回  q 退出",
        Page::Detail(_) if unified && kind == Kind::Apps => {
            "a 检查纳管  u 升级  x 卸载  Esc 返回  q 退出"
        }
        Page::Detail(_) if unified && kind == Kind::Capabilities => "a 检查纳管  Esc 返回  q 退出",
        Page::Detail(_) | Page::File { .. } => "j/k 滚动  Esc 返回  q 退出",
    }
}

fn control(event: &KeyEvent, key: char) -> bool {
    event.code == KeyCode::Char(key) && event.modifiers.contains(KeyModifiers::CONTROL)
}

pub(crate) fn browse(
    ctx: &Context,
    mut kind: Kind,
    session: &mut Session,
    view: View,
    unified: bool,
) -> Result<View> {
    let mut browser = Browser::new(view, kind);
    loop {
        let (mut terminal, restore) = screen()?;
        loop {
            terminal.draw(|frame| browser.draw(frame, kind, unified))?;
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
            let rows =
                isize::try_from(terminal.size()?.height.saturating_sub(4).max(1)).unwrap_or(1);
            match event.code {
                KeyCode::Tab | KeyCode::BackTab if unified => {
                    let tabs = Kind::TABS;
                    let index = tabs.iter().position(|tab| *tab == kind).unwrap_or_default();
                    let step = if event.code == KeyCode::Tab {
                        1
                    } else {
                        tabs.len() - 1
                    };
                    let next = tabs[(index + step) % tabs.len()];
                    match Session::load(ctx, next) {
                        Ok((loaded, view)) => {
                            *session = loaded;
                            kind = next;
                            browser = Browser::new(view, kind);
                        }
                        Err(error) => browser.notice = error.to_string(),
                    }
                }
                KeyCode::Char('q') => return Ok(browser.view),
                KeyCode::Esc => browser.back(kind, session),
                KeyCode::Char('/') if matches!(browser.page, Page::List) => browser.typing = true,
                KeyCode::Enter if matches!(browser.page, Page::Operation(_)) => {
                    browser.finish_operation(ctx, session);
                }
                KeyCode::Enter => browser.enter(session, kind),
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
                    if unified
                        && matches!(kind, Kind::Apps | Kind::Capabilities)
                        && matches!(browser.page, Page::Detail(_)) =>
                {
                    browser.preview_operation(ctx, Action::Adopt);
                }
                KeyCode::Char(key @ ('u' | 'x'))
                    if unified && kind == Kind::Apps && matches!(browser.page, Page::Detail(_)) =>
                {
                    let action = if key == 'u' {
                        Action::Upgrade
                    } else {
                        Action::Uninstall
                    };
                    browser.preview_operation(ctx, action);
                }
                KeyCode::Char('d')
                    if kind == Kind::Config && matches!(browser.page, Page::List) =>
                {
                    let id = browser.selected();
                    browser.show_directories = !browser.show_directories;
                    browser.preserve(id.as_deref());
                }
                KeyCode::Char('r') if matches!(browser.page, Page::List) => break,
                _ => {}
            }
        }
        drop(terminal);
        drop(restore);
        browser.refresh(ctx, session);
    }
}
