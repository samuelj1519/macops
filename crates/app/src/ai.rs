use crate::{
    ai_gate as gate,
    ai_state::{self, Mise, Snapshot, Upgrade},
    cli::{Ai, App, Command},
    config_tx,
    context::Context,
    domain::{
        Action, CommandSpec, Kind, Observation, Package, Plan, Provider, QuerySpec, fingerprint,
    },
    error::{Error, Result},
    policy::ai as policy,
};
use serde_json::{Value, json};

pub fn run(ctx: &Context, command: &Command, dry_run: bool) -> Result<Option<Value>> {
    match command {
        Command::Ai { command: Ai::List } => list(ctx).map(Some),
        Command::Ai {
            command:
                Ai::Upgrade {
                    tool,
                    version,
                    force_live,
                },
        }
        | Command::App {
            command:
                App::Upgrade {
                    tool,
                    version,
                    force_live,
                },
        } => execute(ctx, tool, version.as_deref(), *force_live, dry_run, || {
            Ok(())
        })
        .map(Some),
        _ => Ok(None),
    }
}

pub fn unified_upgrade(
    ctx: &Context,
    command: &Command,
    dry_run: bool,
    explicit_config: bool,
) -> Result<Option<Value>> {
    let Command::Upgrade {
        id,
        version,
        force_live,
    } = command
    else {
        return Ok(None);
    };
    if let Some(tool) = policy::upgrade_target(id) {
        let package = crate::target::resolve(ctx, id)?;
        if matches!(&package.provider, Provider::Legacy { category, selector }
            if category == "ai" && selector == tool)
        {
            let selected = fingerprint(&package)?;
            return execute(ctx, tool, version.as_deref(), *force_live, dry_run, || {
                let current = crate::target::refresh(ctx, &package)?;
                if fingerprint(&current)? != selected {
                    return Err(Error::attention(
                        "plan_stale",
                        "AI 升级对象的配方在准备期间改变",
                    ));
                }
                Ok(())
            })
            .map(Some);
        }
        if explicit_config {
            return Err(Error::new(
                "config_scope_unsupported",
                "此对象由其他软件配方管理，不能使用 AI 对账范围",
            ));
        }
    }
    if *force_live {
        return Err(Error::new(
            "force_live_target_unsupported",
            "--force-live 仅适用于内置 AI 升级对象",
        ));
    }
    Ok(None)
}

fn execute(
    ctx: &Context,
    tool: &str,
    version: Option<&str>,
    force_live: bool,
    dry_run: bool,
    check: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    let upgrade = prepare(ctx, tool, version, force_live)?;
    let plan = plan(ctx, &upgrade)?;
    if dry_run {
        return Ok(
            json!({"backend":"rust", "mode":"dry-run", "plan":plan, "persistentWrites":false, "installerInvoked":false}),
        );
    }
    crate::ai_tx::execute(ctx, &upgrade, plan, check)
}

fn list(ctx: &Context) -> Result<Value> {
    let configs = config_tx::resolve_mise_paths(ctx).and_then(|(live, source, _)| {
        Ok((
            Snapshot::capture(&live)?.text,
            Snapshot::capture(&source)?.text,
        ))
    });
    let mut rows = Vec::new();
    let mut incomplete = false;
    for tool in policy::TOOLS {
        let binary = gate::executable(ctx, policy::binary(tool)?);
        let version = binary
            .as_ref()
            .ok()
            .and_then(|p| gate::version(ctx, p, tool).ok());
        incomplete |= binary.is_ok() && version.is_none();
        let pin = |source: bool| {
            configs.as_ref().ok().and_then(|(live, declared)| {
                policy::pin(if source { declared } else { live }, tool).ok()
            })
        };
        rows.push(json!({"tool":tool, "binary":binary.ok(), "version":version, "live_pin":pin(false), "declared_pin":pin(true),
            "profiles": if tool == "claude" { vec![".claude", ".claude-mi", ".claude-kimi", ".claude-glm", ".claude-zen"] } else { vec![] },
            "status": if version.is_some() { "present" } else { "unknown" }}));
    }
    Ok(
        json!({"backend":"rust", "incomplete":incomplete, "tools":rows, "persistentWrites":false, "config_coverage":if configs.is_ok() { "complete" } else { "unknown" }}),
    )
}

fn prepare(
    ctx: &Context,
    tool: &str,
    requested: Option<&str>,
    force_live: bool,
) -> Result<Upgrade> {
    let mut version = policy::version(tool, requested)?;
    let mise = if tool == "mimo" {
        None
    } else {
        let (live_path, source_path, editable) = config_tx::resolve_mise_paths(ctx)?;
        config_tx::check_mise_layers(
            live_path
                .parent()
                .ok_or_else(|| Error::new("invalid_config_target", "缺少 mise 目录"))?,
        )?;
        if !editable && !force_live {
            return Err(Error::attention(
                "unsupported_config_source",
                "源不是可写静态配置",
            ));
        }
        let live = Snapshot::capture(&live_path)?;
        let source = Snapshot::capture(&source_path)?;
        policy::require_sync(&live.text, &source.text, force_live)?;
        let prev_live_pin = policy::pin(&live.text, tool)?;
        if version.is_none() {
            let mise = gate::executable(ctx, "mise")?;
            let latest = gate::query(
                &ctx.scope,
                &mise,
                vec![
                    "latest".into(),
                    crate::policy::config_tx::ai_mise_spec(tool)?.into(),
                ],
            )?;
            version = policy::version(tool, Some(latest.trim()))?;
        }
        let target = version
            .as_deref()
            .ok_or_else(|| Error::new("exact_version_required", tool))?;
        Some(Mise {
            expected_live: policy::rewrite(&live.text, tool, target)?,
            expected_source: policy::rewrite(&source.text, tool, target)?,
            live,
            source,
            prev_live_pin,
            root_digest: config_tx::root_digest(ctx)?,
            roots: ctx.chezmoi.clone(),
        })
    };
    Ok(Upgrade {
        isolated: ctx.isolated,
        tool: tool.into(),
        version,
        force_live,
        mise,
        profiles: ai_state::profiles(ctx)?,
    })
}

fn upgrade_args(upgrade: &Upgrade) -> Result<Vec<String>> {
    Ok(if upgrade.mise.is_some() {
        vec![
            "use".into(),
            "--path".into(),
            upgrade
                .mise
                .as_ref()
                .ok_or_else(|| Error::new("ai_plan_invalid", "缺少冻结的 mise 目标"))?
                .live
                .anchor
                .requested
                .to_str()
                .ok_or_else(|| Error::new("invalid_path", "mise 目标不是 UTF-8"))?
                .into(),
            "--pin".into(),
            format!(
                "{}@{}",
                crate::policy::config_tx::ai_mise_spec(&upgrade.tool)?,
                upgrade
                    .version
                    .as_deref()
                    .ok_or_else(|| Error::new("exact_version_required", &upgrade.tool))?
            ),
        ]
    } else {
        let mut args = vec!["upgrade".into()];
        args.extend(upgrade.version.clone());
        args.extend(["--method".into(), "curl".into()]);
        args
    })
}

fn plan(ctx: &Context, upgrade: &Upgrade) -> Result<Plan> {
    let program = gate::executable(
        ctx,
        if upgrade.mise.is_some() {
            "mise"
        } else {
            "mimo"
        },
    )?;
    let args = upgrade_args(upgrade)?;
    let running = if let Some(mise) = &upgrade.mise {
        gate::managed_version(ctx, &program, &upgrade.tool, &mise.prev_live_pin)?
    } else {
        gate::version(ctx, &program, "mimo")?
    };
    if upgrade
        .mise
        .as_ref()
        .is_some_and(|mise| mise.prev_live_pin != running)
    {
        return Err(Error::attention(
            "ai_live_pin_not_exact",
            "升级前实际版本与本机 pin 不同，不能冻结精确回退版本",
        ));
    }
    let query = CommandSpec {
        program: program.clone(),
        args: vec![
            if upgrade.tool == "mimo" {
                "-v"
            } else {
                "--version"
            }
            .into(),
        ],
    };
    let package = Package {
        id: format!("ai-native:{}", upgrade.tool),
        name: upgrade.tool.clone(),
        kind: Kind::Cli,
        official_url: String::new(),
        provider: Provider::Command {
            query: query.clone(),
            available: None,
            install: None,
            upgrade: None,
            uninstall: None,
            synchronous: true,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let mut warnings = vec![format!("ai-native-v1:{}", fingerprint(upgrade)?)];
    if upgrade.force_live {
        warnings.push("--force-live 仅更新本机，不写静态源声明".into());
    }
    if upgrade.tool == "mimo" {
        warnings.push("mimo 为厂商工具，升级不可回滚；未知结果必须先恢复核验".into());
    }
    Ok(Plan {
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Upgrade,
        before: Observation::Present {
            evidence: None,
            version: running,
        },
        target: upgrade.version.clone(),
        command: Some(CommandSpec { program, args }),
        brew: None,
        npm_binding: None,
        uv_owner: None,
        query: Some(QuerySpec::Command(query)),
        warnings,
        synchronous: true,
        delegated: false,
    })
}
