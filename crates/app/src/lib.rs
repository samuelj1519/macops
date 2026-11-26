mod agent;
mod browser;
mod capability;
mod catalog;
mod cli;
mod config;
mod context;
mod domain;
mod error;
mod migration;
mod operations;
mod output;
mod platform;
mod policy;
mod recommend;
mod skills;
mod software;
mod source;
mod terminal;

use agent::{
    ai, claude_install, codex_install, metadata, native_plugin_tx, native_plugins,
    plugin_marketplaces, resource_agents,
};
use capability::{capabilities, capability_tx};
use catalog::{cache, inventory, resource_inventory, target, updates};
use config::{chezmoi_native, config_tx};
use migration::migration_tx;
use operations::{manifest, native_runner, store};
use skills::native_skills;
use software::{brew, npm_adoption, service};
use source::source_tx;

use clap::{CommandFactory, Parser};
use serde_json::{Value, json};
use std::{io::Write, process::ExitCode};

pub(crate) fn scoped_retry<'a>(noun: &str, key: &'a str) -> Option<&'a str> {
    let (prefix, rest) = key.split_once(':')?;
    (prefix == noun && matches!(prefix, "app" | "config" | "skill" | "agent") && !rest.is_empty())
        .then_some(rest)
}

fn select_scoped<'a>(
    catalog: &'a domain::Catalog,
    noun: &str,
    key: &str,
) -> error::Result<&'a domain::Package> {
    match catalog.select(key) {
        Ok(package) => Ok(package),
        Err(error) if error.code == "not_found" => {
            scoped_retry(noun, key).map_or(Err(error), |rest| catalog.select(rest))
        }
        Err(error) => Err(error),
    }
}

fn resolve_scoped(ctx: &context::Context, noun: &str, key: &str) -> error::Result<domain::Package> {
    match target::resolve(ctx, key) {
        Ok(package) => Ok(package),
        Err(error) if error.code == "not_found" => {
            scoped_retry(noun, key).map_or(Err(error), |rest| target::resolve(ctx, rest))
        }
        Err(error) => Err(error),
    }
}

enum CommandOutput {
    Data(Value),
    Browser(browser::Output),
}

fn run(cli: &cli::Cli, warnings: &mut Vec<String>) -> error::Result<CommandOutput> {
    if let Some(value) = run_worker(&cli.command)? {
        return Ok(CommandOutput::Data(value));
    }
    let chezmoi_requested = cli.chezmoi_source.is_some()
        || cli.chezmoi_destination.is_some()
        || cli.chezmoi_config.is_some();
    policy::cli::config_scope(&cli.command, chezmoi_requested)?;
    validate_project_scope(&cli.command)?;
    native_skills::validate_recipe_argument(&cli.command)?;
    if matches!(cli.command, cli::Command::Recommend { setup: true, .. }) {
        let setup = context::RecommendSetupContext::from_cli(cli, warnings)?;
        return recommend::run_setup(&setup, cli.json, cli.dry_run).map(CommandOutput::Data);
    }
    let ctx = context::Context::from_cli(cli, warnings)?;
    if cli.json
        && !cli.dry_run
        && matches!(
            cli.command,
            cli::Command::Agent {
                command: Some(cli::AgentCmd::Launch { .. }),
                ..
            }
        )
    {
        return Err(error::Error::new(
            "launch_json_requires_preview",
            "使用 --dry-run 查看启动计划",
        ));
    }
    if let cli::Command::Migrate { command } = &cli.command {
        return migration::run(&ctx, command, cli.dry_run, cli.yes).map(CommandOutput::Data);
    }
    if migration_tx::pending(&ctx)? {
        return Err(migration_tx::recovery_required(&ctx));
    }
    if let Some(output) = browser::run(cli, &ctx)? {
        return Ok(CommandOutput::Browser(output));
    }
    if !cli.dry_run
        && writes_managed_state(&cli.command)
        && let Some(recovered) = recover_for_command(cli, &ctx)?
    {
        if repeats_recovered(&ctx, &cli.command, &recovered)? {
            return Ok(CommandOutput::Data(recovered.result));
        }
        if !cli.json {
            eprintln!("已检查上一次操作。现在准备本次操作。");
        }
    }
    run_scoped(cli, &ctx, chezmoi_requested).map(CommandOutput::Data)
}

fn recover_for_command(
    cli: &cli::Cli,
    ctx: &context::Context,
) -> error::Result<Option<operations::Recovered>> {
    let command = &cli.command;
    if let cli::Command::Agent {
        command: Some(cli::AgentCmd::Resume { operation_id }),
        ..
    } = command
    {
        return operations::recover_before_write_checked(ctx, None, |journal| {
            matches_resumed_stage(journal, operation_id)
        });
    }
    if cli.yes || local_agent(command).is_some() {
        return operations::recover_before_write(ctx, local_agent(command));
    }
    operations::recover_before_new_plan(ctx, false, |journal| {
        Ok(!may_request_confirmation(ctx, command)
            || repeats_resource(ctx, command, journal, None, false).unwrap_or(false))
    })
}

fn may_request_confirmation(ctx: &context::Context, command: &cli::Command) -> bool {
    use cli::{AppCmd, Command, Config, NativePlugins, Policy, SkillCmd};
    match command {
        Command::App {
            command:
                Some(
                    AppCmd::Install { id, .. }
                    | AppCmd::Upgrade { id: Some(id), .. }
                    | AppCmd::Refresh { id: Some(id) },
                ),
            ..
        } => brew_candidate(ctx, id).map_or(true, |package| package.is_some()),
        Command::App {
            command: Some(AppCmd::Upgrade { id: None, .. }),
            ..
        } => inventory::load(ctx).is_ok_and(|catalog| {
            catalog
                .packages
                .iter()
                .any(|package| matches!(package.provider, domain::Provider::BrewFormula { .. }))
        }),
        _ => matches!(
            command,
            Command::App {
                command: Some(
                    AppCmd::Uninstall { .. }
                        | AppCmd::Adopt { .. }
                        | AppCmd::Start { .. }
                        | AppCmd::Stop { .. }
                ),
                ..
            } | Command::Config {
                command: Some(
                    Config::Adopt { .. }
                        | Config::Forget { .. }
                        | Config::Apply { .. }
                        | Config::Reconcile { id: Some(_) }
                ),
                ..
            } | Command::Skill {
                command: Some(
                    SkillCmd::Adopt { .. }
                        | SkillCmd::Remove { .. }
                        | SkillCmd::Sync(_)
                        | SkillCmd::Policy {
                            command: Policy::Sync { .. }
                        }
                        | SkillCmd::Plugins {
                            command: NativePlugins::Upgrade { .. }
                        }
                ),
                ..
            } | Command::Cleanup { .. }
        ),
    }
}

fn writes_managed_state(command: &cli::Command) -> bool {
    use cli::{AgentCmd, AppCmd, Command, Config, NativePlugins, Policy, SkillCmd};
    matches!(
        command,
        Command::App {
            command: Some(
                AppCmd::Install { .. }
                    | AppCmd::Upgrade { .. }
                    | AppCmd::Uninstall { .. }
                    | AppCmd::Adopt {
                        target: Some(_),
                        ..
                    }
                    | AppCmd::Adopt { id: Some(_), .. }
                    | AppCmd::Start { .. }
                    | AppCmd::Stop { .. }
                    | AppCmd::Refresh { .. }
            ),
            ..
        } | Command::Config {
            command: Some(
                Config::Adopt { .. }
                    | Config::Forget { .. }
                    | Config::Enroll
                    | Config::Apply { .. }
                    | Config::Reconcile { id: Some(_) }
                    | Config::List { refresh: true }
            ),
            ..
        } | Command::Skill {
            command: Some(
                SkillCmd::Install { .. }
                    | SkillCmd::Upgrade(_)
                    | SkillCmd::Remove { .. }
                    | SkillCmd::Adopt { .. }
                    | SkillCmd::Refresh(_)
                    | SkillCmd::Sync(_)
                    | SkillCmd::Policy {
                        command: Policy::Sync { .. }
                    }
                    | SkillCmd::Plugins {
                        command: NativePlugins::Upgrade { .. }
                    }
            ),
            ..
        } | Command::Agent {
            command: Some(
                AgentCmd::Install { .. }
                    | AgentCmd::Upgrade { tool: Some(_), .. }
                    | AgentCmd::Resume { .. }
            ),
            ..
        } | Command::Cleanup { .. }
    )
}

fn local_agent(command: &cli::Command) -> Option<&str> {
    match command {
        cli::Command::Agent {
            command:
                Some(
                    cli::AgentCmd::Install {
                        tool,
                        force_live: true,
                        ..
                    }
                    | cli::AgentCmd::Upgrade {
                        tool: Some(tool),
                        force_live: true,
                    },
                ),
            ..
        } => Some(tool),
        _ => None,
    }
}

fn repeats_recovered(
    ctx: &context::Context,
    command: &cli::Command,
    recovered: &operations::Recovered,
) -> error::Result<bool> {
    use cli::{AgentCmd, Command};
    use domain::Action;
    if !recovered.succeeded {
        return Ok(false);
    }
    let (tool, version, local) = match command {
        Command::Agent {
            command:
                Some(AgentCmd::Install {
                    tool,
                    version,
                    force_live,
                }),
            ..
        } => (tool, version.as_deref(), *force_live),
        Command::Agent {
            command:
                Some(AgentCmd::Upgrade {
                    tool: Some(tool),
                    force_live,
                }),
            ..
        } => (tool, None, *force_live),
        _ => return repeats_resource(ctx, command, &recovered.original, recovered.local_ai, true),
    };
    Ok(recovered.local_ai == Some(local)
        && recovered.matches(Action::Upgrade, &format!("ai-native:{tool}"), version))
}

fn repeats_resource(
    ctx: &context::Context,
    command: &cli::Command,
    original: &domain::Journal,
    local_ai: Option<bool>,
    allow_removed_brew: bool,
) -> error::Result<bool> {
    use cli::{AppCmd, Command, Config, SkillCmd};
    use domain::Action;
    let (action, target, version, application) = match command {
        Command::Agent {
            command: Some(cli::AgentCmd::Resume { operation_id }),
            ..
        } => {
            return matches_resumed_stage(original, operation_id);
        }
        Command::Skill {
            command: Some(SkillCmd::Install { recipe }),
            ..
        } if original.plan.action == Action::Install => {
            return native_skills::matches_recipe_file(original, recipe);
        }
        Command::Skill {
            command: Some(SkillCmd::Upgrade(selection)),
            ..
        } if original.plan.action == Action::Upgrade => {
            return native_skills::matches_upgrade(ctx, original, selection);
        }
        Command::App {
            command: Some(AppCmd::Install { id, version }),
            ..
        } => (Action::Install, id.as_str(), version.as_deref(), true),
        Command::App {
            command:
                Some(AppCmd::Upgrade {
                    id: Some(id),
                    version,
                }),
            ..
        } => (Action::Upgrade, id.as_str(), version.as_deref(), true),
        Command::App {
            command: Some(AppCmd::Uninstall { id }),
            ..
        } => (Action::Uninstall, id.as_str(), None, true),
        Command::App {
            command:
                Some(AppCmd::Adopt {
                    target,
                    id,
                    bind_recipe,
                }),
            ..
        } if *bind_recipe == original.plan.npm_binding.is_some() => (
            Action::Adopt,
            id.as_deref().or(target.as_deref()).unwrap_or_default(),
            None,
            true,
        ),
        Command::App {
            command: Some(AppCmd::Start { id }),
            ..
        } => (Action::Start, service_selector(id), None, false),
        Command::App {
            command: Some(AppCmd::Stop { id }),
            ..
        } => (Action::Stop, service_selector(id), None, false),
        Command::Config {
            command: Some(Config::Apply { target }),
            ..
        } => (Action::Apply, target.as_str(), None, false),
        Command::Skill {
            command: Some(SkillCmd::Remove { id }),
            ..
        } => (Action::Uninstall, id.as_str(), None, false),
        Command::Cleanup { id } => (Action::Cleanup, id.as_str(), None, false),
        _ => return Ok(false),
    };
    let plan = &original.plan;
    if application && plan.brew.is_some() {
        if version.is_some() {
            return Ok(false);
        }
        return match brew_candidate(ctx, target)? {
            Some(package) => brew::matches_selected_recovery(original, &package, action),
            None => Ok(allow_removed_brew
                && action == Action::Uninstall
                && plan.action == action
                && (target == plan.package.id || target == plan.package.name)),
        };
    }
    if operations::matches_intent(original, action, target, version) {
        return Ok(true);
    }
    let key = scoped_retry("app", target).unwrap_or(target);
    if application {
        return repeats_application(ctx, original, local_ai, action, target, key, version);
    }
    Ok(match &plan.package.provider {
        domain::Provider::ConfigObserved { target: path, .. } => {
            action == Action::Apply && plan.action == action && path == std::path::Path::new(target)
        }
        domain::Provider::Service { .. } => plan.action == action && plan.package.name == target,
        _ => false,
    })
}

fn brew_candidate(ctx: &context::Context, key: &str) -> error::Result<Option<domain::Package>> {
    let is_formula = |package: &domain::Package| {
        matches!(package.provider, domain::Provider::BrewFormula { .. })
    };
    if let Some(package) = brew::candidate(ctx, key)?.filter(is_formula) {
        return Ok(Some(package));
    }
    scoped_retry("app", key).map_or(Ok(None), |key| {
        brew::candidate(ctx, key).map(|package| package.filter(is_formula))
    })
}

fn matches_resumed_stage(journal: &domain::Journal, operation_id: &str) -> error::Result<bool> {
    Ok(claude_install::matches_prior(journal, operation_id)?
        || codex_install::matches_prior(journal, operation_id)?)
}

fn repeats_application(
    ctx: &context::Context,
    original: &domain::Journal,
    local_ai: Option<bool>,
    action: domain::Action,
    target: &str,
    key: &str,
    version: Option<&str>,
) -> error::Result<bool> {
    let plan = &original.plan;
    if key.is_empty()
        || plan.action != action
        || version.is_some_and(|version| plan.target.as_deref() != Some(version))
    {
        return Ok(false);
    }
    match resolve_scoped(ctx, "app", target) {
        Ok(package) => {
            if action == domain::Action::Upgrade
                && let Some(tool) = ai::app_upgrade_tool(target, &package)
            {
                return Ok(local_ai == Some(false)
                    && operations::matches_intent(
                        original,
                        action,
                        &format!("ai-native:{tool}"),
                        version,
                    ));
            }
            Ok(operations::matches_intent(
                original,
                action,
                &package.id,
                version,
            ))
        }
        Err(error)
            if error.code == "not_found"
                && action == domain::Action::Uninstall
                && key == plan.package.name =>
        {
            Ok(true)
        }
        Err(_) if key == plan.package.name => Err(error::Error::attention(
            "operation_checked",
            "已检查上一次操作。请先在清单里确认这次要用的对象编号。",
        )),
        Err(_) => Ok(false),
    }
}

fn run_scoped(
    cli: &cli::Cli,
    ctx: &context::Context,
    chezmoi_requested: bool,
) -> error::Result<Value> {
    if let Some(value) = run_scoped_entry(cli, ctx, chezmoi_requested)? {
        return Ok(value);
    }
    if matches!(cli.command, cli::Command::Dirs { roots: true, .. }) {
        return Ok(
            json!({"home":ctx.home,"config":ctx.config,"data":ctx.data,"state":ctx.state,"cache":ctx.cache,"catalog":ctx.catalog,"settings":ctx.settings}),
        );
    }
    if let Some(value) = run_recovery(cli, ctx)? {
        return Ok(value);
    }
    if let cli::Command::History { query } = &cli.command {
        let mut history = store::history(ctx)?;
        if let Some(query) = query {
            if let Some(operation) = history.iter().position(|journal| journal.id == *query) {
                return Ok(json!([history.swap_remove(operation)]));
            }
            history.retain(|journal| journal.plan.package.id == *query);
        }
        return Ok(json!(history));
    }
    if let cli::Command::Search { query } = &cli.command {
        return run_search(ctx, query);
    }
    if let Some(value) = run_pipeline(cli, ctx)? {
        return Ok(value);
    }
    let catalog = inventory::load(ctx)?;
    catalog_query(cli, ctx, &catalog, chezmoi_requested)
}

fn validate_project_scope(command: &cli::Command) -> error::Result<()> {
    use cli::{AgentCmd, AppCmd, Command, Config, SkillCmd};
    let valid = match command {
        Command::App {
            project: Some(_),
            command,
        } => {
            matches!(command, None | Some(AppCmd::Resources | AppCmd::Detail(_)))
        }
        Command::Config {
            project: Some(_),
            command,
        } => {
            matches!(command, None | Some(Config::Resources | Config::Detail(_)))
        }
        Command::Skill {
            project: Some(_),
            command,
            details,
            only,
        } => {
            !details
                && only.is_empty()
                && matches!(
                    command,
                    None | Some(SkillCmd::Resources | SkillCmd::Detail(_))
                )
        }
        Command::Agent {
            project: Some(_),
            command,
        } => {
            matches!(
                command,
                None | Some(AgentCmd::Resources | AgentCmd::Detail(_))
            )
        }
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err(error::Error::new(
            "project_scope_unsupported",
            "--project 仅用于资源列表和详情",
        ))
    }
}

fn run_pipeline(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    if let Some(value) = run_config_or_reconcile(cli, ctx)? {
        return Ok(Some(value));
    }
    match &cli.command {
        cli::Command::App {
            project: _,
            command: Some(cli::AppCmd::Start { id }),
        } => {
            return service::control(
                ctx,
                service_selector(id),
                domain::Action::Start,
                cli.dry_run,
                cli.yes,
            )
            .map(Some);
        }
        cli::Command::App {
            project: _,
            command: Some(cli::AppCmd::Stop { id }),
        } => {
            return service::control(
                ctx,
                service_selector(id),
                domain::Action::Stop,
                cli.dry_run,
                cli.yes,
            )
            .map(Some);
        }
        _ => (),
    }
    if let Some(target) = policy::cli::adopt_target(&cli.command)
        && let Some(value) = native_skills::app_scoped(
            ctx,
            target.key,
            target.exact,
            None,
            domain::Action::Adopt,
            cli.dry_run,
            cli.yes,
        )?
    {
        return Ok(Some(value));
    }
    if let Some(value) = native_skills::run(ctx, &cli.command, cli.dry_run, cli.yes)? {
        return Ok(Some(value));
    }
    if let Some(value) = capability_tx::write(ctx, &cli.command, cli.dry_run, cli.yes)? {
        return Ok(Some(value));
    }
    if let Some(value) = run_source_adoption(cli, ctx)? {
        return Ok(Some(value));
    }
    if let Some(value) = resource_inventory::query(ctx, &cli.command)? {
        return Ok(Some(value));
    }
    if let Some(value) = capabilities::query(ctx, &cli.command)? {
        return Ok(Some(value));
    }
    if let Some(value) = ai::run(ctx, &cli.command, cli.dry_run)? {
        return Ok(Some(value));
    }
    Ok(None)
}

fn catalog_query(
    cli: &cli::Cli,
    ctx: &context::Context,
    catalog: &domain::Catalog,
    chezmoi_requested: bool,
) -> error::Result<Value> {
    match &cli.command {
        cli::Command::App {
            project,
            command: Some(command),
        } => run_application(
            cli,
            ctx,
            catalog,
            project.as_deref(),
            command,
            chezmoi_requested,
        ),
        cli::Command::Dirs { id, .. } => {
            let packages = selected(catalog, id.as_deref())?;
            Ok(json!(
                packages
                    .iter()
                    .map(|p| json!({"id":p.id,"paths":p.paths}))
                    .collect::<Vec<_>>()
            ))
        }
        cli::Command::Cleanup { id } => {
            cache::cleanup(ctx, catalog.select(id)?, cli.dry_run, cli.yes)
        }
        _ => Err(error::Error::new(
            "unsupported_command",
            "该命令没有已验证的执行接口",
        )),
    }
}

fn run_scoped_entry(
    cli: &cli::Cli,
    ctx: &context::Context,
    chezmoi_requested: bool,
) -> error::Result<Option<Value>> {
    if let Some(value) = ai::unified_upgrade(ctx, &cli.command, cli.dry_run, chezmoi_requested)? {
        return Ok(Some(value));
    }
    match &cli.command {
        cli::Command::Recommend {
            product,
            evidence,
            github,
            model,
            keychain_account,
            ..
        } => recommend::run(
            cli,
            ctx,
            product.as_deref(),
            evidence.as_deref(),
            github.as_deref(),
            *model,
            keychain_account.as_deref(),
        )
        .map(Some),
        cli::Command::Agent {
            command: Some(command),
            ..
        } => run_agent_entry(cli, ctx, command),
        cli::Command::Skill {
            command: Some(cli::SkillCmd::Market { profile, online }),
            ..
        } => run_market(cli, ctx, profile.as_deref(), *online).map(Some),
        cli::Command::Manifest { command } => {
            manifest::run(ctx, command, cli.dry_run, cli.yes).map(Some)
        }
        cli::Command::App {
            command: Some(cli::AppCmd::Adopt {
                bind_recipe: true, ..
            }),
            ..
        } => {
            if chezmoi_requested {
                return Err(error::Error::new(
                    "config_scope_unsupported",
                    "chezmoi 目录参数不能用于 npm 配方",
                ));
            }
            let target = policy::cli::adopt_target(&cli.command).ok_or_else(|| {
                error::Error::attention("adopt_target_required", "请指定要绑定 npm 配方的对象")
            })?;
            npm_adoption::run(ctx, target.key, cli.dry_run, cli.yes).map(Some)
        }
        _ => Ok(None),
    }
}

fn run_agent_entry(
    cli: &cli::Cli,
    ctx: &context::Context,
    command: &cli::AgentCmd,
) -> error::Result<Option<Value>> {
    match command {
        cli::AgentCmd::Resume { operation_id } => {
            let journal = operations::validated_recovery_journal(ctx, operation_id)?;
            if codex_install::is_install(&journal) {
                return codex_install::resume(
                    ctx,
                    &journal,
                    cli.dry_run,
                    cli.yes,
                    terminal::report_install_stage,
                )
                .map(Some);
            }
            claude_install::resume(
                ctx,
                operation_id,
                cli.dry_run,
                cli.yes,
                terminal::report_install_stage,
            )
            .map(Some)
        }
        cli::AgentCmd::Launch {
            profile,
            native_profile,
        } => codex_install::session::launch(
            ctx,
            profile,
            native_profile.as_deref(),
            cli.dry_run,
            cli.yes,
        )
        .map(Some),
        cli::AgentCmd::Uninstall { profile } => {
            resource_agents::uninstall(ctx, profile, cli.dry_run, cli.yes).map(Some)
        }
        cli::AgentCmd::Credential { command } => Ok(Some(metadata::credentials(ctx, command))),
        _ => Ok(None),
    }
}

fn run_market(
    cli: &cli::Cli,
    ctx: &context::Context,
    profile: Option<&str>,
    online: bool,
) -> error::Result<Value> {
    let interactive = browser::interactive_marketplaces(cli);
    if interactive && online && plugin_marketplaces::cursor_profile(profile) {
        eprintln!("正在查询 Cursor 账户市场…");
    }
    let data = plugin_marketplaces::list(ctx, profile, online, cli.dry_run)?;
    if interactive {
        browser::browse_marketplaces(ctx, &data)?;
    }
    Ok(data)
}

fn run_recovery(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    if let cli::Command::Recover {
        operation_id,
        resolve_failed,
    } = &cli.command
    {
        if let Some(operation_id) = operation_id {
            return operations::recover(ctx, operation_id, cli.dry_run, *resolve_failed, cli.yes)
                .map(Some);
        }
        let (pending, incomplete) = operations::pending_recovery(ctx)?;
        return Ok(Some(json!({
            "pending":pending,
            "incomplete":incomplete,
            "hint":"运行 mo recover <操作号> 检查那次操作的结果。确认要只记录失败时使用 --mark-failed。",
            "persistentWrites":false,
        })));
    }
    Ok(None)
}

fn run_search(ctx: &context::Context, query: &str) -> error::Result<Value> {
    let scope = query.split_once(':').and_then(|(noun, _)| match noun {
        "app" => Some("applications"),
        "config" => Some("configs"),
        "skill" => Some("capabilities"),
        "agent" => Some("agents"),
        _ => None,
    });
    let data = resource_inventory::snapshot(ctx, scope, None)?;
    let exact = data.resources.iter().find(|row| row.id == query);
    let candidates: Vec<_> = if let Some(row) = exact {
        vec![row]
    } else {
        data.resources
            .iter()
            .filter(|row| {
                row.matches_name(query)
                    || scope
                        .and_then(|_| query.split_once(':'))
                        .is_some_and(|(_, name)| row.matches_name(name))
            })
            .collect()
    };
    match candidates.as_slice() {
        [] => {
            if scope.is_none() || scope == Some("agents") {
                let key = scoped_retry("agent", query).unwrap_or(query);
                if let Some(tool) = ai::detail(ctx, key)? {
                    return Ok(tool);
                }
            }
            Err(error::Error::attention(
                "not_found",
                format!("没有发现对应对象：{query}"),
            ))
        }
        [resource] => {
            Ok(json!({"resource":resource,"coverage":data.coverage,"incomplete":data.incomplete}))
        }
        _ => {
            let choices: Vec<_> = candidates
                .iter()
                .map(|row| json!({"id":row.id,"name":row.name,"category":row.category()}))
                .collect();
            Ok(json!({"candidates":choices,"coverage":data.coverage,"incomplete":data.incomplete}))
        }
    }
}

fn run_worker(command: &cli::Command) -> error::Result<Option<Value>> {
    match command {
        cli::Command::Worker { journal } => operations::worker(journal)?,
        cli::Command::NativeWorker {
            journal,
            capture_progress,
        } => native_runner::worker(
            journal,
            *capture_progress,
            |journal, invocation| {
                let (guard, _) = native_callbacks(journal)?;
                guard(journal, invocation)
            },
            |journal, invocation, outcome| {
                let (_, after) = native_callbacks(journal)?;
                after(journal, invocation, outcome)
            },
        )?,
        _ => return Ok(None),
    }
    Ok(Some(Value::Null))
}

type NativeGuard = fn(&domain::Journal, &native_runner::FrozenInvocation) -> error::Result<()>;
type NativeAfter = fn(
    &domain::Journal,
    &native_runner::FrozenInvocation,
    &native_runner::NativeOutcome,
) -> error::Result<()>;

fn native_callbacks(journal: &domain::Journal) -> error::Result<(NativeGuard, NativeAfter)> {
    if config::rules::chezmoi_native::is_native(&journal.plan) {
        Ok((chezmoi_native::worker_guard, chezmoi_native::worker_after))
    } else if codex_install::is_install(journal) {
        Ok((codex_install::worker_guard, codex_install::worker_after))
    } else if claude_install::is_install(journal) {
        Ok((claude_install::worker_guard, claude_install::worker_after))
    } else if native_plugin_tx::is_plugin(journal) {
        Ok((
            native_plugin_tx::worker_guard,
            native_plugin_tx::worker_after,
        ))
    } else if journal.plan.brew.is_some() {
        Ok((brew::worker_guard, brew::worker_after))
    } else if native_skills::is_native(journal) {
        Ok((native_skills::worker_guard, native_skills::worker_after))
    } else {
        Err(error::Error::new(
            "native_adapter_mismatch",
            "原生操作未绑定适配器",
        ))
    }
}

fn run_source_adoption(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    let Some(target) = policy::cli::adopt_target(&cli.command) else {
        return Ok(None);
    };
    let Some(preview) = source_tx::adopt(ctx, target, true, cli.yes)? else {
        return Ok(None);
    };
    if cli.chezmoi_source.is_some()
        || cli.chezmoi_destination.is_some()
        || cli.chezmoi_config.is_some()
    {
        return Err(error::Error::new(
            "config_scope_unsupported",
            "chezmoi 目录参数不能用于非配置目标",
        ));
    }
    if cli.dry_run {
        return Ok(Some(preview));
    }
    source_tx::adopt(ctx, target, false, cli.yes)?
        .map(Some)
        .ok_or_else(|| error::Error::new("source_selection_changed", "来源选择在采纳前改变"))
}

fn run_config_or_reconcile(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    if let cli::Command::Config {
        command: Some(command),
        ..
    } = &cli.command
        && !matches!(command, cli::Config::Resources | cli::Config::Detail(_))
        && let Some(value) = chezmoi_native::run(ctx, command, cli.dry_run, cli.yes)?
    {
        return Ok(Some(value));
    }
    match &cli.command {
        cli::Command::Skill {
            project: _,
            details: _,
            only: _,
            command: Some(cli::SkillCmd::Plugins { command }),
        } => native_plugins::run(ctx, command, cli.dry_run, cli.yes).map(Some),
        cli::Command::Config {
            project: _,
            command: Some(cli::Config::Restore { operation_id }),
        } => config_tx::restore(ctx, operation_id, cli.dry_run, cli.yes).map(Some),
        cli::Command::Config {
            project: _,
            command: Some(cli::Config::Reconcile { id }),
        } => config_tx::reconcile(ctx, id.as_deref(), cli.dry_run, cli.yes).map(Some),
        cli::Command::Agent {
            project: _,
            command: Some(cli::AgentCmd::Upgrade { tool: None, .. }),
        } => config_tx::reconcile(ctx, None, cli.dry_run, cli.yes).map(Some),
        _ => Ok(None),
    }
}

fn discover_all(ctx: &context::Context, manager: Option<&str>) -> error::Result<Value> {
    if let Some(manager) = manager {
        return inventory::discover(ctx, manager);
    }
    Ok(json!(
        [
            "brew", "mise", "uv", "npm", "bun", "cargo", "rustup", "conda"
        ]
        .iter()
        .map(|m| match inventory::discover(ctx, m) {
            Ok(v) => v,
            Err(e) => json!({"manager":m,"status":"unknown","error":e}),
        })
        .collect::<Vec<_>>()
    ))
}

fn service_selector(key: &str) -> &str {
    let key = scoped_retry("app", key).unwrap_or(key);
    key.strip_prefix("service:").unwrap_or(key)
}

fn service_read(ctx: &context::Context, command: &cli::ServiceCmd) -> error::Result<Value> {
    let name = match command {
        cli::ServiceCmd::List => "postgresql",
        cli::ServiceCmd::Check { name } => name.as_deref().unwrap_or("postgresql"),
    };
    if name != "postgresql" {
        return Err(error::Error::new("undeclared_service", name));
    }
    let spec = domain::CommandSpec {
        program: crate::operations::command::executable("brew")?,
        args: vec![
            "services".into(),
            "info".into(),
            "postgresql@16".into(),
            "--json".into(),
        ],
    };
    let out = crate::operations::command::query(&ctx.scope, &spec)?;
    if out.code != Some(0) || out.uncertain {
        return Err(error::Error::new("service_unobserved", "服务状态查询失败"));
    }
    let value = inventory::parse_service(&out.stdout, "postgresql@16")?;
    Ok(
        json!({"service":value,"status":if value.running {"running"} else {"stopped"},"requires_running":matches!(command,cli::ServiceCmd::Check {..})}),
    )
}

fn require_config_target(
    chezmoi_requested: bool,
    packages: &[&domain::Package],
) -> error::Result<()> {
    if chezmoi_requested
        && packages
            .iter()
            .any(|p| !matches!(p.provider, domain::Provider::ConfigObserved { .. }))
    {
        return Err(error::Error::new(
            "config_scope_unsupported",
            "chezmoi 目录参数不能用于非配置目标",
        ));
    }
    Ok(())
}

fn object_action(
    cli: &cli::Cli,
    ctx: &context::Context,
    key: &str,
    action: domain::Action,
    version: Option<&str>,
) -> error::Result<Value> {
    for id in [Some(key), scoped_retry("app", key)].into_iter().flatten() {
        let request = brew::ObjectRequest {
            action,
            id: id.to_owned(),
            version: version.map(str::to_owned),
            doctor: false,
        };
        if let Some(value) = brew::route_object(ctx, &request, cli.dry_run, cli.yes, None)? {
            return Ok(value);
        }
        if action != domain::Action::Install
            && let Some(value) =
                native_skills::app_scoped(ctx, id, false, version, action, cli.dry_run, cli.yes)?
        {
            return Ok(value);
        }
    }
    let package = resolve_scoped(ctx, "app", key)?;
    operations::execute(
        ctx,
        operations::plan(ctx, &package, action, version, cli.dry_run)?,
        cli.dry_run,
        cli.yes,
    )
}

fn upgrade_all(
    cli: &cli::Cli,
    ctx: &context::Context,
    catalog: &domain::Catalog,
) -> error::Result<Value> {
    use domain::{Kind, Provider};
    let (explicit, candidates): (Vec<_>, Vec<_>) = catalog
        .packages
        .iter()
        .filter(|package| {
            matches!(package.kind, Kind::App | Kind::Cli | Kind::Runtime)
                && (matches!(package.provider, Provider::BrewFormula { .. })
                    || policy::operation::commands::available_query(&ctx.home, package).is_ok())
        })
        .partition(|package| package.exact_version_required);
    let (native, indexed): (Vec<_>, Vec<_>) = candidates
        .into_iter()
        .partition(|package| matches!(package.provider, Provider::BrewFormula { .. }));
    let explicit = explicit
        .into_iter()
        .map(|package| json!({"id":package.id,"status":"explicit_version_required"}));
    if cli.dry_run {
        let mut results = upgrade_rows(ctx, &indexed)?;
        results.extend(explicit);
        results.extend(
            native
                .iter()
                .map(|package| json!({"id":package.id,"status":"native_query_required"})),
        );
        return Ok(
            json!({"mode":"dry-run","results":results,"refresh":"not-run","persistentWrites":false}),
        );
    }
    let mut results = refresh_upgrade_sources(cli, ctx, &indexed, &native)?;
    let mut stopped = false;
    let mut unattempted = Vec::new();
    for row in &mut results {
        if row["status"] != "different" || row["installed"]["version"].is_null() {
            continue;
        }
        let (Some(id), Some(version)) = (row["id"].as_str(), row["available"].as_str()) else {
            continue;
        };
        if stopped {
            unattempted.push(id.to_owned());
            continue;
        }
        let outcome = if let Some(package) = indexed.iter().find(|package| package.id == id) {
            upgrade_indexed(cli, ctx, package, version)
        } else {
            object_action(cli, ctx, id, domain::Action::Upgrade, None)
        };
        match outcome {
            Ok(value) => {
                let incomplete = attention(&cli.command, &value);
                *row = json!({"id":id,"status":if incomplete {"unknown"} else {"upgraded"},"operation":value});
                stopped = incomplete;
            }
            Err(error) => {
                stopped = true;
                *row = json!({"id":id,"status":"unknown","error":error});
            }
        }
    }
    results.extend(explicit);
    if unattempted.is_empty() {
        Ok(json!({"results":results}))
    } else {
        Ok(json!({"results":results,"unattempted":unattempted}))
    }
}

fn upgrade_indexed(
    cli: &cli::Cli,
    ctx: &context::Context,
    package: &domain::Package,
    version: &str,
) -> error::Result<Value> {
    let explicit = (!matches!(package.provider, domain::Provider::Brew { .. })).then_some(version);
    let plan = operations::plan(ctx, package, domain::Action::Upgrade, explicit, false)?;
    if plan.target.as_deref() != Some(version) {
        return Err(error::Error::attention(
            "plan_stale",
            "查询到的版本已改变，请重新检查",
        ));
    }
    operations::execute(ctx, plan, false, cli.yes)
}

fn upgrade_rows(
    ctx: &context::Context,
    packages: &[&domain::Package],
) -> error::Result<Vec<Value>> {
    let mut rows: Vec<Value> = serde_json::from_value(updates::outdated(ctx, packages)?)?;
    for row in &mut rows {
        if row["installed"]["status"] == "missing" {
            row["status"] = json!("not_installed");
        } else if row["installed"]["version"].is_null() {
            row["status"] = json!("unknown");
        }
    }
    Ok(rows)
}

fn refresh_upgrade_sources(
    cli: &cli::Cli,
    ctx: &context::Context,
    indexed: &[&domain::Package],
    native: &[&domain::Package],
) -> error::Result<Vec<Value>> {
    let mut rows = if indexed.is_empty() {
        vec![]
    } else {
        let refreshed = updates::update(ctx, indexed, false)?;
        let mut rows = upgrade_rows(ctx, indexed)?;
        for (row, refresh) in rows
            .iter_mut()
            .zip(refreshed["results"].as_array().into_iter().flatten())
        {
            if refresh["status"] != "updated" {
                row["status"] = json!("unknown");
                row["error"] = refresh["error"].clone();
            }
        }
        rows
    };
    for package in native {
        let request = brew::ObjectRequest {
            action: domain::Action::Update,
            id: package.id.clone(),
            version: None,
            doctor: false,
        };
        let row = match brew::route_object(ctx, &request, false, cli.yes, None) {
            Ok(Some(value)) => {
                let available = &value["results"][0]["version"];
                let installed = &value["observed"];
                let status = if installed["status"] == "brew-missing" {
                    "not_installed"
                } else if installed["version"] == *available {
                    "current"
                } else {
                    "different"
                };
                json!({"id":package.id,"status":status,"available":available,"installed":installed})
            }
            Ok(None) => json!({"id":package.id,"status":"unknown"}),
            Err(error) if error.code == "cancelled" => return Err(error),
            Err(error) => json!({"id":package.id,"status":"unknown","error":error}),
        };
        rows.push(row);
    }
    Ok(rows)
}

fn scoped_packages<'a>(
    catalog: &'a domain::Catalog,
    key: Option<&str>,
) -> error::Result<Vec<&'a domain::Package>> {
    key.map_or_else(
        || selected(catalog, None),
        |key| Ok(vec![select_scoped(catalog, "app", key)?]),
    )
}

fn run_application(
    cli: &cli::Cli,
    ctx: &context::Context,
    catalog: &domain::Catalog,
    project: Option<&std::path::Path>,
    command: &cli::AppCmd,
    chezmoi_requested: bool,
) -> error::Result<Value> {
    match command {
        cli::AppCmd::Install { id, version } => {
            object_action(cli, ctx, id, domain::Action::Install, version.as_deref())
        }
        cli::AppCmd::Upgrade { id, version, .. } => match id {
            Some(id) => object_action(cli, ctx, id, domain::Action::Upgrade, version.as_deref()),
            None => upgrade_all(cli, ctx, catalog),
        },
        cli::AppCmd::Uninstall { id } => {
            object_action(cli, ctx, id, domain::Action::Uninstall, None)
        }
        cli::AppCmd::Adopt { .. } => run_adoption(cli, ctx),
        cli::AppCmd::Refresh { id } => {
            if let Some(id) = id {
                for key in [Some(id.as_str()), scoped_retry("app", id)]
                    .into_iter()
                    .flatten()
                {
                    let request = brew::ObjectRequest {
                        action: domain::Action::Update,
                        id: key.to_owned(),
                        version: None,
                        doctor: false,
                    };
                    if let Some(value) =
                        brew::route_object(ctx, &request, cli.dry_run, cli.yes, None)?
                    {
                        return Ok(value);
                    }
                }
            }
            if let Some(key) = id
                && key.starts_with("application:")
            {
                let resolved = target::resolve(ctx, key)?;
                if software::rules::uv_local::is_uv(&resolved.provider) {
                    return updates::update(ctx, &[&resolved], cli.dry_run);
                }
            }
            updates::update(ctx, &scoped_packages(catalog, id.as_deref())?, cli.dry_run)
        }
        cli::AppCmd::Outdated { id } => {
            updates::outdated(ctx, &scoped_packages(catalog, id.as_deref())?)
        }
        cli::AppCmd::Detail(detail) => application_detail(ctx, catalog, project, &detail.target),
        cli::AppCmd::Doctor { id } => {
            application_doctor(cli, ctx, catalog, id.as_deref(), chezmoi_requested)
        }
        cli::AppCmd::Discover { manager } => discover_all(ctx, manager.as_deref()),
        cli::AppCmd::Service { command } => service_read(ctx, command),
        _ => Err(error::Error::new(
            "unsupported_command",
            "该命令没有已验证的执行接口",
        )),
    }
}

fn application_detail(
    ctx: &context::Context,
    catalog: &domain::Catalog,
    project: Option<&std::path::Path>,
    term: &str,
) -> error::Result<Value> {
    let exact = scoped_retry("app", term).unwrap_or(term);
    if project.is_none() && !exact.starts_with("application:") {
        let declared = inventory::declared_catalog(ctx)?;
        if let Some(package) = declared
            .packages
            .iter()
            .find(|package| package.id == term)
            .or_else(|| declared.packages.iter().find(|package| package.id == exact))
        {
            return Ok(package_detail(ctx, package));
        }
    }
    let snapshot = resource_inventory::snapshot(ctx, Some("applications"), project)?;
    let detail = resource_inventory::snapshot_detail(&snapshot, "applications", term);
    if project.is_some() || term.starts_with("application:") {
        return detail;
    }
    if detail
        .as_ref()
        .is_err_and(|error| error.code != "not_found")
    {
        return detail;
    }
    let declared = inventory::declared_catalog(ctx)?;
    match select_scoped(&declared, "app", term) {
        Ok(package) => return Ok(package_detail(ctx, package)),
        Err(error) if error.code != "not_found" => return Err(error),
        Err(_) => (),
    }
    if let Ok(package) = select_scoped(catalog, "app", term)
        && matches!(package.provider, domain::Provider::ObservedApp { .. })
    {
        return Ok(package_detail(ctx, package));
    }
    detail
}

fn package_detail(ctx: &context::Context, package: &domain::Package) -> Value {
    json!({"package":package,"observation":inventory::observe(ctx, package)})
}

fn application_doctor(
    cli: &cli::Cli,
    ctx: &context::Context,
    catalog: &domain::Catalog,
    id: Option<&str>,
    chezmoi_requested: bool,
) -> error::Result<Value> {
    if let Some(id) = id {
        for key in [Some(id), scoped_retry("app", id)].into_iter().flatten() {
            let request = brew::ObjectRequest {
                action: domain::Action::Upgrade,
                id: key.to_owned(),
                version: None,
                doctor: true,
            };
            if let Some(value) = brew::route_object(ctx, &request, cli.dry_run, cli.yes, None)? {
                return Ok(value);
            }
        }
    }
    let packages = scoped_packages(catalog, id)?;
    require_config_target(chezmoi_requested, &packages)?;
    target::diagnose(ctx, &packages)
}

fn emit(value: &Value) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    out.write_all(b"\n")
}

fn root_help() -> String {
    use std::fmt::Write as _;
    let mut text = String::from(concat!(
        "本机软件、技能与配置\n\n",
        "用法  mo [全局参数] <类型> [对象] [动作]\n\n",
        "四类入口\n",
        "  mo app [应用]         应用：安装、升级、卸载、纳入管理\n",
        "  mo config [子命令]    配置：chezmoi 归属与治理\n",
        "  mo skill [技能]       技能与插件：安装、升级、检查\n",
        "  mo agent [子命令]     Agent CLI、档案、凭据与启动\n\n",
        "常用动作\n",
        "  mo <名称>             跨类型查询对象\n",
        "  mo recommend <目标>   查看来源和安装建议\n",
        "  mo recommend --setup  设置推荐模型\n",
        "  mo app adopt [名称]   将现有软件纳入管理\n",
        "  mo skill market       浏览插件市场\n",
        "  mo recover [操作号]   查看待核对操作或核对原结果\n\n",
        "其他入口\n",
        "  mo dirs --roots       查看当前目录与状态范围\n",
        "  mo manifest --help   导出清单、比较差异和预演重放\n",
        "  mo cleanup macops:cache --dry-run  查看可清理的缓存\n",
        "  mo migrate --help    预检或核对旧数据转换\n\n",
        "可用命令\n",
    ));
    let command = cli::Cli::command();
    let mut commands: Vec<_> = command
        .get_subcommands()
        .filter(|item| !item.is_hide_set())
        .collect();
    commands.sort_by_key(|item| item.get_name());
    for item in commands {
        let _ = writeln!(
            text,
            "  {:<13} {}",
            item.get_name(),
            item.get_about()
                .map_or_else(String::new, ToString::to_string)
        );
    }
    text.push_str(concat!(
        "\n常用全局参数\n",
        "  --json                  输出结构化 JSON\n",
        "  --dry-run               预演操作\n",
        "  --timeout SECONDS       每次运行外部程序最多等多少秒\n",
        "  --yes                   跳过交互确认\n",
        "  -V, --version           查看版本\n",
        "  -h, --help              查看帮助\n\n",
        "mo help <命令> 或 mo <命令> --help 查看该命令的参数。\n",
        "mo help advanced 查看全部全局参数和恢复入口。\n",
    ));
    text
}

fn parse_entry(mut args: Vec<std::ffi::OsString>) -> Result<cli::Cli, clap::Error> {
    let result = cli::Cli::try_parse_from(&args);
    match result {
        Ok(cli) => {
            let Some((terms, actions)) = target_args(&cli.command) else {
                return Ok(cli);
            };
            let index = args.len() - terms.len();
            let action = terms.get(1).filter(|arg| actions.contains(&arg.as_str()));
            if action.is_some() {
                args.swap(index, index + 1);
            } else {
                args.insert(index, "__detail".into());
            }
            cli::Cli::try_parse_from(args)
        }
        Err(error) if error.kind() == clap::error::ErrorKind::InvalidSubcommand => {
            args.insert(1, "search".into());
            cli::Cli::try_parse_from(args).map_err(|_| error)
        }
        Err(error) => Err(error),
    }
}

fn target_args(command: &cli::Command) -> Option<(&[String], &'static [&'static str])> {
    use cli::{AgentCmd, AppCmd, Command, Config, SkillCmd};
    match command {
        Command::App {
            command: Some(AppCmd::Target(terms)),
            ..
        } => Some((
            terms,
            &[
                "install",
                "upgrade",
                "uninstall",
                "adopt",
                "doctor",
                "start",
                "stop",
                "refresh",
                "outdated",
            ],
        )),
        Command::Config {
            command: Some(Config::Target(terms)),
            ..
        } => Some((
            terms,
            &[
                "adopt",
                "forget",
                "source-path",
                "status",
                "verify",
                "diff",
                "apply",
                "reconcile",
            ],
        )),
        Command::Skill {
            command: Some(SkillCmd::Target(terms)),
            ..
        } => Some((
            terms,
            &[
                "adopt",
                "upgrade",
                "uninstall",
                "doctor",
                "sync",
                "refresh",
                "outdated",
            ],
        )),
        Command::Agent {
            command: Some(AgentCmd::Target(terms)),
            ..
        } => Some((terms, &["install", "upgrade", "launch"])),
        _ => None,
    }
}

pub fn main_entry() -> ExitCode {
    let mut raw: Vec<_> = std::env::args_os().collect();
    if raw.len() == 1 && browser::interactive_terminal() {
        raw.push("app".into());
    }
    if matches!(&raw[1..], [help, advanced] if help == "help" && advanced == "advanced") {
        return if cli::Cli::command().print_long_help().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if raw.len() == 1
        || matches!(&raw[1..], [arg] if arg == "--help" || arg == "-h" || arg == "help")
    {
        return if std::io::stdout()
            .lock()
            .write_all(root_help().as_bytes())
            .is_ok()
        {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let json_output = raw.iter().any(|v| v == "--json");
    let cli = match parse_entry(raw) {
        Ok(v) => v,
        Err(e) => {
            let code = if e.use_stderr() { 2 } else { 0 };
            if json_output && code != 0 {
                let _ = emit(
                    &json!({"schemaVersion":1,"status":"failed","error":{"code":"usage","message":e.to_string()}}),
                );
            } else {
                if code != 0 {
                    use clap::error::ErrorKind;
                    let problem = match e.kind() {
                        ErrorKind::UnknownArgument => "有个选项不能在这里用。",
                        ErrorKind::MissingRequiredArgument => "这条命令还少了一项。",
                        ErrorKind::InvalidValue | ErrorKind::ValueValidation => {
                            "有一项的值不合要求。"
                        }
                        ErrorKind::ArgumentConflict => "有两个选项不能一起用。",
                        _ => "这条命令有个地方需要改。",
                    };
                    if let Some(hint) = skill_install_usage(&e) {
                        eprintln!("{hint}");
                    } else {
                        eprintln!("{problem}运行 mo --help 看命令怎么写。");
                    }
                }
                let _ = e.print();
            }
            return ExitCode::from(code);
        }
    };
    dispatch(&cli)
}

fn skill_install_usage(error: &clap::Error) -> Option<String> {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let usage = error.get(ContextKind::Usage)?.to_string();
    let usage = usage.strip_prefix("Usage:").unwrap_or(&usage);
    if !usage
        .split_ascii_whitespace()
        .take(3)
        .eq(["mo", "skill", "install"])
    {
        return None;
    }
    match error.kind() {
        ErrorKind::UnknownArgument => {
            let ContextValue::String(value) = error.get(ContextKind::InvalidArg)? else {
                return None;
            };
            let github = value.strip_prefix("github.com/").unwrap_or(value);
            let repo = policy::public_source::repository_subject(github)?;
            Some(format!(
                "GitHub 仓库请先用 mo recommend {repo} 查看安装方式。已有本机配方时，用 mo skill install --recipe /绝对路径/recipe.json。"
            ))
        }
        ErrorKind::MissingRequiredArgument => {
            Some("请用 --recipe 指定本机配方的绝对路径。示例见 mo skill install --help。".into())
        }
        _ => None,
    }
}

fn dispatch(cli: &cli::Cli) -> ExitCode {
    if matches!(
        cli.command,
        cli::Command::Worker { .. }
            | cli::Command::NativeWorker {
                capture_progress: true,
                ..
            }
    ) {
        return match run_worker(&cli.command) {
            Ok(_) => ExitCode::SUCCESS,
            Err(error) => ExitCode::from(error.exit),
        };
    }
    if let cli::Command::NpmObserve { npm, prefix, name } = &cli.command {
        let observed = target::probe(npm, prefix, name)
            .unwrap_or_else(|error| domain::Observation::Unknown { reason: error.code });
        return if serde_json::to_value(observed).map_or(true, |value| emit(&value).is_err()) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    let mut warnings = vec![];
    let result = run(cli, &mut warnings);
    if !cli.json {
        for warning in &warnings {
            eprintln!("macops: {warning}");
        }
    }
    match result {
        Ok(CommandOutput::Browser(browser::Output { text, incomplete })) => {
            if let Some(text) = text
                && std::io::stdout().lock().write_all(text.as_bytes()).is_err()
            {
                return ExitCode::FAILURE;
            }
            ExitCode::from(if incomplete { 3 } else { 0 })
        }
        Ok(CommandOutput::Data(data)) => {
            let code = if attention(&cli.command, &data) { 3 } else { 0 };
            if recommend::interactive(cli) || browser::interactive_marketplaces(cli) {
                if data["mode"] == "cancelled" {
                    eprintln!("已取消。");
                }
                return ExitCode::from(code);
            }
            let data = software::rules::brew::public_value(&data);
            let value = json!({"schemaVersion":1,"status":if code==0 {"ok"} else {"attention"},"data":data,"warnings":warnings});
            let rendered = if cli.json {
                emit(&value)
            } else {
                let text = match cli.command {
                    cli::Command::Skill {
                        project: _,
                        details: _,
                        only: _,
                        command: Some(cli::SkillCmd::Market { .. }),
                    } => plugin_marketplaces::human(&data),
                    cli::Command::Recover { .. } => operations::recovery_text(&data, cli.dry_run),
                    _ if policy::cli::continuation(&data).is_some() => {
                        operations::recovery_text(&data, false)
                    }
                    _ => output::command_human(&cli.command, &data),
                };
                std::io::stdout().lock().write_all(text.as_bytes())
            };
            match rendered {
                Ok(()) => ExitCode::from(code),
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(e) => {
            if cli.json {
                let _ = emit(
                    &json!({"schemaVersion":1,"status":"failed","data":null,"error":e,"warnings":warnings}),
                );
            } else {
                eprintln!("{}", output::error_text(&e));
            }
            ExitCode::from(e.exit)
        }
    }
}

fn choose_adoption(
    ctx: &context::Context,
    cli: &cli::Cli,
    chezmoi_requested: bool,
) -> error::Result<Option<domain::Package>> {
    let interactive = !chezmoi_requested
        && !cli.json
        && !cli.dry_run
        && !cli.yes
        && browser::interactive_terminal();
    let name = if let Some(target) = policy::cli::adopt_target(&cli.command) {
        let selected = target::resolve_adoption(ctx, target, cli.dry_run).or_else(|error| {
            if error.code != "not_found" {
                return Err(error);
            }
            scoped_retry("app", target.key).map_or(Err(error), |key| {
                target::resolve_adoption(
                    ctx,
                    cli::AdoptTarget {
                        key,
                        exact: target.exact,
                    },
                    cli.dry_run,
                )
            })
        });
        match selected {
            Ok(package) => return Ok(Some(package)),
            Err(mut error) if error.code == "ambiguous" && !target.exact => {
                if !interactive {
                    error
                        .message
                        .push_str("。请用 mo app adopt --id <ID> 指定，或在 mo app 中选中目标");
                    return Err(error);
                }
                Some(scoped_retry("app", target.key).unwrap_or(target.key))
            }
            Err(error) => return Err(error),
        }
    } else if interactive {
        None
    } else {
        return Err(error::Error::attention(
            "adopt_target_required",
            "请指定软件名称或使用 mo app adopt --id <ID>；在终端运行 mo app adopt 可选择软件",
        ));
    };
    let view = browser::adoption_view(ctx, name)?;
    if name.is_some() && view.items.is_empty() {
        return Err(error::Error::attention(
            "ambiguous",
            "同名候选已变化或不在软件清单中，请确认目标后使用 mo app adopt --id <ID>",
        ));
    }
    let Some(id) = terminal::pick_adoption(view)? else {
        return Ok(None);
    };
    target::resolve_adoption_resource(ctx, &id).map(Some)
}

fn run_adoption(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Value> {
    let chezmoi_requested = cli.chezmoi_source.is_some()
        || cli.chezmoi_destination.is_some()
        || cli.chezmoi_config.is_some();
    let Some(package) = choose_adoption(ctx, cli, chezmoi_requested)? else {
        return Ok(json!({"status":"cancelled","persistentWrites":false}));
    };
    require_config_target(chezmoi_requested, &[&package])?;
    if matches!(
        package.provider,
        domain::Provider::BrewFormula { .. } | domain::Provider::Brew { cask: false, .. }
    ) {
        return brew::execute_selected(ctx, &package, domain::Action::Adopt, cli.dry_run, cli.yes);
    }
    operations::execute(
        ctx,
        operations::plan(ctx, &package, domain::Action::Adopt, None, cli.dry_run)?,
        cli.dry_run,
        cli.yes,
    )
}

use crate::policy::cli::{attention, selected};

#[cfg(test)]
#[path = "../tests/unit/support.rs"]
mod test_support;

#[cfg(test)]
#[path = "../tests/unit/native_plugins_support.rs"]
mod native_plugins_test_fixture;

#[cfg(test)]
#[path = "../tests/unit/profile_codec_regressions.rs"]
mod profile_codec_regressions;
