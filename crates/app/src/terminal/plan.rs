use crate::{context::Context, domain::Provider, policy::public_source::domain_link};

pub(crate) fn native_command(
    command: &crate::domain::CommandSpec,
    scope: Option<(&str, &str)>,
) -> String {
    fn quote(arg: &str) -> String {
        if !arg.is_empty()
            && arg
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@=-".contains(&byte))
        {
            arg.into()
        } else {
            format!("'{}'", arg.replace('\'', "'\\''"))
        }
    }
    let mut parts = Vec::new();
    if let Some((key, value)) = scope {
        parts.push(format!("{key}={}", quote(value)));
    }
    parts.push(quote(&command.program.display().to_string()));
    parts.extend(command.args.iter().map(|arg| quote(arg)));
    parts.join(" ")
}

pub(crate) fn native_banner(
    journal: &crate::domain::Journal,
    command: &crate::domain::CommandSpec,
    env: Option<&std::collections::BTreeMap<String, String>>,
) -> String {
    use super::presentation::{self, PanelLine};
    use crate::output::{TerminalLine, sanitize, terminal_line};

    let scope = env.and_then(|env| {
        ["CLAUDE_CONFIG_DIR", "CODEX_HOME"]
            .into_iter()
            .find_map(|key| env.get(key).map(|value| (key, value.as_str())))
    });
    let trusted = if crate::skills::native_skills::is_native(journal) {
        Some("Skills CLI")
    } else if crate::agent::plugin_lifecycle::owns(journal)
        || crate::agent::claude_install::is_install(journal)
        || crate::agent::codex_install::is_install(journal)
    {
        Some(if matches!(scope, Some(("CODEX_HOME", _))) {
            "Codex"
        } else {
            "Claude Code"
        })
    } else if journal.plan.brew.is_some() {
        Some("Homebrew")
    } else if crate::agent::ai_tx::is_native(journal) {
        command
            .program
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| matches!(*name, "mise" | "mimo"))
    } else {
        None
    };
    let tool = trusted.unwrap_or(match journal.plan.package.provider {
        Provider::Brew { .. } | Provider::BrewFormula { .. } | Provider::Service { .. } => {
            "Homebrew"
        }
        Provider::Cargo { .. } => "Cargo",
        Provider::Bun { .. } => "Bun",
        Provider::Mise { .. } => "mise",
        Provider::Npm { .. } => "npm",
        _ => "本机安装器",
    });
    let command = if trusted.is_some() {
        PanelLine::Command(native_command(command, scope))
    } else {
        PanelLine::Note(format!(
            "{} · 使用配方中的参数",
            sanitize(&command.program.display().to_string(), false)
        ))
    };
    let color = presentation::color();
    let width = presentation::columns().saturating_sub(1).min(96);
    let mut output = presentation::section_text(
        "原生命令",
        &[command],
        (width >= 24).then_some(width),
        color,
        false,
    );
    output.push_str(&terminal_line(
        TerminalLine::Section,
        &format!("原工具输出 · {tool}"),
        color,
    ));
    output.push('\n');
    output
}

pub(crate) fn source_label(value: &str) -> &str {
    let public = value.split(['?', '#']).next().unwrap_or_default();
    if domain_link(public) {
        public
    } else {
        "待核对"
    }
}

pub(crate) fn plan_scope(ctx: &Context, plan: &crate::domain::Plan) -> String {
    use crate::{domain::QuerySpec, output::sanitize};
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
            _ => crate::software::resource_software::mise_scope(ctx),
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
    if let Some(release) = crate::software::mise_owner::retirement::bound_query(plan) {
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
