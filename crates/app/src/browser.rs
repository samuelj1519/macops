use crate::{
    chezmoi_native::ConfigBrowser,
    cli::{Cli, Command},
    context::Context,
    error::{Error, Result},
};
use serde::Serialize;
use serde_json::json;
use std::{
    io::{IsTerminal, Write},
    path::PathBuf,
    process::ExitCode,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Config,
    Skills,
    Capabilities,
    Apps,
    Agents,
}

impl Kind {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Config => "配置",
            Self::Skills => "技能",
            Self::Capabilities => "能力",
            Self::Apps => "软件",
            Self::Agents => "Agent 档案",
        }
    }

    pub(crate) const TABS: [Self; 4] = [Self::Apps, Self::Capabilities, Self::Agents, Self::Config];
}

#[derive(Debug, Serialize)]
pub(crate) struct Item {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub managed: bool,
    #[serde(skip)]
    pub management: crate::resource::Management,
    pub directory: bool,
    pub path: Option<PathBuf>,
    pub source: Option<PathBuf>,
    pub agents: Vec<String>,
    pub loaded: Option<bool>,
    pub details: Vec<String>,
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
}

impl Session {
    pub(crate) fn load(ctx: &Context, kind: Kind) -> Result<(Self, View)> {
        match kind {
            Kind::Config => {
                let browser = ConfigBrowser::load(ctx)?;
                let view = browser.view();
                Ok((Self::Config(Box::new(browser)), view))
            }
            Kind::Skills => Ok((Self::Skills, crate::capabilities::browse(ctx, false)?)),
            Kind::Capabilities => Ok((Self::Capabilities, crate::capabilities::browse(ctx, true)?)),
            Kind::Apps => Ok((Self::Apps, resource_view(ctx, "applications")?)),
            Kind::Agents => Ok((Self::Agents, resource_view(ctx, "agents")?)),
        }
    }

    pub(crate) fn refresh(&mut self, ctx: &Context) -> Result<View> {
        match self {
            Self::Config(browser) => {
                browser.refresh(ctx)?;
                Ok(browser.view())
            }
            Self::Skills => crate::capabilities::browse(ctx, false),
            Self::Capabilities => crate::capabilities::browse(ctx, true),
            Self::Apps => resource_view(ctx, "applications"),
            Self::Agents => resource_view(ctx, "agents"),
        }
    }

    pub(crate) fn observed(
        &self,
        id: &str,
        source: bool,
    ) -> Result<crate::chezmoi_native::ViewerFile> {
        match self {
            Self::Config(browser) => browser.select_file(id, source),
            Self::Skills | Self::Capabilities | Self::Apps | Self::Agents => Err(Error::attention(
                "file_unavailable",
                "此入口提供技能管理详情",
            )),
        }
    }
}

pub(crate) fn interactive_terminal() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| !term.is_empty() && term != "dumb")
}

pub(crate) fn entry(cli: &Cli) -> Option<ExitCode> {
    let interactive = !cli.json && !cli.dry_run && interactive_terminal();
    let unified = interactive
        && !cli.yes
        && matches!(
            cli.command,
            Command::List {
                kind: None,
                project: None
            }
        );
    let kind = match &cli.command {
        Command::List { .. } if unified => Kind::Apps,
        Command::Config { command: None } => Kind::Config,
        Command::Skills { command: None } => Kind::Skills,
        Command::Apps => Kind::Apps,
        Command::Agents => Kind::Agents,
        _ => return None,
    };
    let mut warnings = Vec::new();
    let result = execute(cli, kind, &mut warnings, interactive, unified);
    if !cli.json {
        for warning in &warnings {
            eprintln!("macops: {warning}");
        }
    }
    let code = match result {
        Ok(view) => {
            let output = if cli.json {
                crate::emit(
                    &json!({"schemaVersion":1,"status":if view.incomplete {"attention"} else {"ok"},
                    "data":view,"warnings":warnings}),
                )
            } else if interactive {
                Ok(())
            } else {
                std::io::stdout()
                    .lock()
                    .write_all(plain(&view, kind).as_bytes())
            };
            if output.is_err() {
                1
            } else if view.incomplete {
                3
            } else {
                0
            }
        }
        Err(error) => {
            if cli.json {
                let _ = crate::emit(
                    &json!({"schemaVersion":1,"status":"failed","data":null,"error":error,"warnings":warnings}),
                );
            } else {
                eprintln!("macops: {error}");
            }
            error.exit
        }
    };
    Some(ExitCode::from(code))
}

fn execute(
    cli: &Cli,
    kind: Kind,
    warnings: &mut Vec<String>,
    interactive: bool,
    unified: bool,
) -> Result<View> {
    crate::policy::cli::config_scope(
        &cli.command,
        cli.chezmoi_source.is_some()
            || cli.chezmoi_destination.is_some()
            || cli.chezmoi_config.is_some(),
    )?;
    let ctx = Context::from_cli(cli, warnings)?;
    let (mut session, view) = Session::load(&ctx, kind)?;
    if interactive {
        crate::terminal::browse(&ctx, kind, &mut session, view, unified)
    } else {
        Ok(view)
    }
}

pub(crate) fn sanitize(value: &str, multiline: bool) -> String {
    let mut clean = String::with_capacity(value.len());
    for ch in value.chars() {
        if (ch != '\n' || !multiline)
            && (ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        {
            clean.extend(ch.escape_default());
        } else {
            clean.push(ch);
        }
    }
    clean
}

fn plain(view: &View, kind: Kind) -> String {
    use std::fmt::Write as _;
    let mut text = format!("{}\n", sanitize(&view.note, false));
    for item in view
        .items
        .iter()
        .filter(|item| (kind != Kind::Skills || item.managed) && !item.directory)
    {
        let _ = writeln!(
            text,
            "{}\t{}",
            sanitize(&item.name, false),
            sanitize(&item.summary, false)
        );
    }
    text
}

fn resource_view(ctx: &Context, filter: &str) -> Result<View> {
    let snapshot = crate::resource_inventory::snapshot(ctx, Some(filter), None)?;
    let incomplete = snapshot.incomplete;
    let mut items = snapshot
        .resources
        .into_iter()
        .filter_map(resource_item)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(View {
        scope: "global".into(),
        items,
        coverage: if incomplete { "partial" } else { "observed" },
        difference_coverage: "not_applicable",
        observed_at: Some(crate::store::now()),
        note: if incomplete {
            "盘点覆盖不完整，详情显示已核验的记录"
        } else {
            "本机盘点完成"
        }
        .into(),
        incomplete,
    })
}

fn resource_item(resource: crate::resource::Resource) -> Option<Item> {
    use crate::resource::{Data, Management};
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
            details.extend(releases.iter().map(|release| {
                let path = release
                    .path
                    .as_deref()
                    .map_or_else(String::new, |p| p.display().to_string());
                format!(
                    "版本 {} {} {path}",
                    release.version,
                    if release.active { "在用" } else { "" }
                )
            }));
            details.extend(issues);
            let summary = format!("{manager} · {} 个版本", releases.len());
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
            let summary = format!("{client} · {profile}");
            let details = vec![
                format!("档案 {}", config_dir.display()),
                format!("运行时 {}", runtime.selector),
                format!("模型 {}", model.selected.as_deref().unwrap_or("未知")),
                format!("观察 {:?}", resource.status),
            ];
            (summary.clone(), Some(config_dir), vec![summary], details)
        }
        _ => return None,
    };
    Some(Item {
        id: resource.id,
        name: resource.name,
        summary,
        managed: resource.management != Management::Untracked,
        management: resource.management,
        directory: false,
        path,
        source: None,
        agents,
        loaded: None,
        details,
    })
}
