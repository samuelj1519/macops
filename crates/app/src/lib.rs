mod ai;
mod ai_gate;
mod ai_payload;
mod ai_state;
mod ai_tx;
mod brew;
mod browser;
mod cache;
mod capabilities;
mod capability_guard;
mod capability_local;
mod capability_receipts;
mod capability_store;
mod capability_tx;
mod chezmoi_binding;
mod chezmoi_native;
mod cli;
mod config_inventory;
mod config_local;
mod config_tx;
mod context;
mod domain;
mod error;
mod governance;
mod identity;
mod index_store;
mod inventory;
mod invocation_inventory;
mod link_inventory;
mod manifest;
mod metadata;
mod migration;
mod migration_fs;
mod migration_scan;
mod migration_tx;
mod migration_writers;
mod native_plugin_tx;
mod native_plugins;
mod native_plugins_files;
mod native_runner;
mod native_skills;
mod native_skills_assets;
mod native_skills_files;
mod native_skills_steps;
mod operations;
mod output;
mod paths;
mod policy;
mod process;
mod profile_inventory;
mod providers;
mod recommend;
mod recommend_model;
mod recommend_upstream;
mod resource;
mod resource_agents;
mod resource_capabilities;
mod resource_configs;
mod resource_inventory;
mod resource_software;
mod service;
mod store;
mod target;
mod terminal;
mod updates;
mod uv_local;
mod uv_ownership;

use clap::{CommandFactory, Parser};
use serde_json::{Value, json};
use std::{io::Write, process::ExitCode};

fn run(cli: &cli::Cli, warnings: &mut Vec<String>) -> error::Result<Value> {
    if let Some(value) = run_worker(&cli.command)? {
        return Ok(value);
    }
    let chezmoi_requested = cli.chezmoi_source.is_some()
        || cli.chezmoi_destination.is_some()
        || cli.chezmoi_config.is_some();
    policy::cli::config_scope(&cli.command, chezmoi_requested)?;
    let ctx = context::Context::from_cli(cli, warnings)?;
    if let Some(result) = migration_command(cli, &ctx)? {
        return Ok(result);
    }
    if let Some(value) = run_scoped_entry(cli, &ctx, chezmoi_requested)? {
        return Ok(value);
    }
    if matches!(cli.command, cli::Command::Dirs { roots: true, .. }) {
        return Ok(
            json!({"home":ctx.home,"config":ctx.config,"data":ctx.data,"state":ctx.state,"cache":ctx.cache,"catalog":ctx.catalog,"settings":ctx.settings}),
        );
    }
    if let Some(value) = run_recovery(cli, &ctx)? {
        return Ok(value);
    }
    if let cli::Command::History { id } = &cli.command {
        return Ok(json!(
            store::history(&ctx)?
                .into_iter()
                .filter(|j| id.as_ref().is_none_or(|id| j.plan.package.id == *id))
                .collect::<Vec<_>>()
        ));
    }
    if let cli::Command::Credential { command } = &cli.command {
        return Ok(metadata::credentials(&ctx, command));
    }
    if let Some(value) = run_config_or_reconcile(cli, &ctx)? {
        return Ok(value);
    }
    match &cli.command {
        cli::Command::Service {
            command: cli::Service::Start { name },
        } => return service::control(&ctx, name, domain::Action::Start, cli.dry_run, cli.yes),
        cli::Command::Service {
            command: cli::Service::Stop { name },
        } => return service::control(&ctx, name, domain::Action::Stop, cli.dry_run, cli.yes),
        _ => (),
    }
    if let Some(value) = native_skills::run(&ctx, &cli.command, cli.dry_run, cli.yes)? {
        return Ok(value);
    }
    if let Some(value) = capability_tx::write(&ctx, &cli.command, cli.dry_run, cli.yes)? {
        return Ok(value);
    }
    if let Some(value) = run_source_adoption(cli, &ctx)? {
        return Ok(value);
    }
    if let Some(value) = capabilities::query(&ctx, &cli.command)? {
        return Ok(value);
    }
    if let Some(value) = ai::run(&ctx, &cli.command, cli.dry_run)? {
        return Ok(value);
    }
    if let Some(value) = resource_inventory::query(&ctx, &cli.command)? {
        return Ok(value);
    }
    let catalog = inventory::load(&ctx)?;
    match &cli.command {
        cli::Command::Doctor { id } => {
            let selected: Vec<_> = if let Some(id) = id {
                vec![catalog.select(id)?]
            } else {
                catalog.packages.iter().collect()
            };
            require_config_target(chezmoi_requested, &selected)?;
            target::diagnose(&ctx, &selected)
        }
        cli::Command::Dirs { id, .. } => {
            let selected: Vec<_> = if let Some(id) = id {
                vec![catalog.select(id)?]
            } else {
                catalog.packages.iter().collect()
            };
            Ok(json!(
                selected
                    .iter()
                    .map(|p| json!({"id":p.id,"paths":p.paths}))
                    .collect::<Vec<_>>()
            ))
        }
        cli::Command::Discover { manager } => discover_all(&ctx, manager.as_deref()),
        cli::Command::App { command } => app(&ctx, &catalog, command),
        cli::Command::Toolchain { command } => match command {
            cli::Toolchain::List { manager } => discover_all(&ctx, manager.as_deref()),
            cli::Toolchain::Info { manager } => inventory::discover(&ctx, manager),
        },
        cli::Command::Service { command } => service_read(&ctx, command),
        _ => run_operation(cli, &ctx, &catalog),
    }
}

fn run_scoped_entry(
    cli: &cli::Cli,
    ctx: &context::Context,
    chezmoi_requested: bool,
) -> error::Result<Option<Value>> {
    if let cli::Command::Recommend {
        product,
        evidence,
        github,
        model,
        keychain_account,
    } = &cli.command
    {
        return recommend::show(
            ctx,
            product,
            evidence.as_deref(),
            github.as_deref(),
            *model,
            keychain_account.as_deref(),
            cli.dry_run,
        )
        .map(Some);
    }
    if let cli::Command::Manifest { command } = &cli.command {
        return manifest::run(ctx, command, cli.dry_run, cli.yes).map(Some);
    }
    if let cli::Command::Adopt {
        id,
        bind_recipe: true,
    } = &cli.command
    {
        if chezmoi_requested {
            return Err(error::Error::new(
                "config_scope_unsupported",
                "chezmoi 目录参数不能用于 npm 配方",
            ));
        }
        return npm_adoption::run(ctx, id, cli.dry_run, cli.yes).map(Some);
    }
    brew::public_command(ctx, cli)
}

fn migration_command(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    if let cli::Command::Migrate { command } = &cli.command {
        return migration::run(ctx, command, cli.dry_run, cli.yes).map(Some);
    }
    if migration_tx::pending(ctx)? {
        return Err(error::Error::attention(
            "migration_recovery_required",
            "迁移尚未收口；先执行 migrate recover",
        ));
    }
    Ok(None)
}

fn run_recovery(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    if let cli::Command::Recover {
        operation_id,
        resolve_failed,
    } = &cli.command
    {
        return operations::recover(ctx, operation_id, cli.dry_run, *resolve_failed, cli.yes)
            .map(Some);
    }
    Ok(None)
}

fn run_worker(command: &cli::Command) -> error::Result<Option<Value>> {
    match command {
        cli::Command::Worker { journal } => operations::worker(journal)?,
        cli::Command::NativeWorker { journal } => native_runner::worker(
            journal,
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
    if policy::chezmoi_native::is_native(&journal.plan) {
        Ok((chezmoi_native::worker_guard, chezmoi_native::worker_after))
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
    let cli::Command::Adopt { id, .. } = &cli.command else {
        return Ok(None);
    };
    let Some(preview) = source_tx::adopt(ctx, id, true, cli.yes)? else {
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
    source_tx::adopt(ctx, id, false, cli.yes)?
        .map(Some)
        .ok_or_else(|| error::Error::new("source_selection_changed", "来源选择在采纳前改变"))
}

fn run_config_or_reconcile(cli: &cli::Cli, ctx: &context::Context) -> error::Result<Option<Value>> {
    if let cli::Command::Config {
        command: Some(command),
    } = &cli.command
        && let Some(value) = chezmoi_native::run(ctx, command, cli.dry_run, cli.yes)?
    {
        return Ok(Some(value));
    }
    match &cli.command {
        cli::Command::Skills {
            command: Some(cli::Skills::Plugins { command }),
        } => native_plugins::run(ctx, command, cli.dry_run, cli.yes).map(Some),
        cli::Command::Config {
            command: Some(cli::Config::Restore { operation_id }),
        } => config_tx::restore(ctx, operation_id, cli.dry_run, cli.yes).map(Some),
        cli::Command::Config {
            command: Some(cli::Config::Reconcile { id }),
        } => config_tx::reconcile(ctx, Some(id.as_str()), cli.dry_run, cli.yes).map(Some),
        cli::Command::Ai {
            command: cli::Ai::Reconcile { tool },
        } => config_tx::reconcile(ctx, tool.as_deref(), cli.dry_run, cli.yes).map(Some),
        _ => Ok(None),
    }
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

fn app(
    ctx: &context::Context,
    catalog: &domain::Catalog,
    command: &cli::App,
) -> error::Result<Value> {
    match command {
        cli::App::List => Ok(json!(
            catalog
                .packages
                .iter()
                .filter(|p| matches!(
                    p.kind,
                    domain::Kind::App | domain::Kind::Cli | domain::Kind::Manager
                ))
                .collect::<Vec<_>>()
        )),
        cli::App::Info { name } => Ok(
            json!({"package":catalog.select(name)?,"observation":inventory::observe(ctx,catalog.select(name)?)}),
        ),
        cli::App::Check { name } => Ok(json!(
            selected(catalog, name.as_deref())?
                .iter()
                .filter(|p| matches!(p.kind, domain::Kind::App))
                .map(|p| json!({"id":p.id,"observation":inventory::observe(ctx,p)}))
                .collect::<Vec<_>>()
        )),
        cli::App::Upgrade { .. } => Err(error::Error::new("route_error", "升级应进入明确后端")),
    }
}

fn service_read(ctx: &context::Context, command: &cli::Service) -> error::Result<Value> {
    let name = match command {
        cli::Service::List => "postgresql",
        cli::Service::Check { name } => name.as_deref().unwrap_or("postgresql"),
        _ => return Err(error::Error::new("route_error", "服务写入应进入明确后端")),
    };
    if name != "postgresql" {
        return Err(error::Error::new("undeclared_service", name));
    }
    let spec = domain::CommandSpec {
        program: process::executable("brew")?,
        args: vec![
            "services".into(),
            "info".into(),
            "postgresql@16".into(),
            "--json".into(),
        ],
    };
    let out = process::query(&ctx.scope, &spec)?;
    if out.code != Some(0) || out.uncertain {
        return Err(error::Error::new("service_unobserved", "服务状态查询失败"));
    }
    let value = inventory::parse_service(&out.stdout, "postgresql@16")?;
    Ok(
        json!({"service":value,"status":if value.running {"running"} else {"stopped"},"requires_running":matches!(command,cli::Service::Check {..})}),
    )
}

fn emit(value: &Value) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    out.write_all(b"\n")
}

pub fn main_entry() -> ExitCode {
    let raw: Vec<_> = std::env::args_os().collect();
    if raw.len() == 1 {
        return if cli::Cli::command().print_help().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let json_output = raw.iter().any(|v| v == "--json");
    let cli = match cli::Cli::try_parse_from(raw) {
        Ok(v) => v,
        Err(e) => {
            let code = if e.use_stderr() { 2 } else { 0 };
            if json_output && code != 0 {
                let _ = emit(
                    &json!({"schemaVersion":1,"status":"failed","error":{"code":"usage","message":e.to_string()}}),
                );
            } else {
                let _ = e.print();
            }
            return ExitCode::from(code);
        }
    };
    if let cli::Command::NpmObserve { npm, prefix, name } = &cli.command {
        let observed = target::probe(npm, prefix, name)
            .unwrap_or_else(|error| domain::Observation::Unknown { reason: error.code });
        return if serde_json::to_value(observed).map_or(true, |value| emit(&value).is_err()) {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    if let Some(result) = browser::entry(&cli) {
        return result;
    }
    let mut warnings = vec![];
    let result = run(&cli, &mut warnings);
    if !cli.json {
        for warning in &warnings {
            eprintln!("macops: {warning}");
        }
    }
    match result {
        Ok(data) => {
            let code = if attention(&cli.command, &data) { 3 } else { 0 };
            let data = policy::brew::public_value(&data);
            let value = json!({"schemaVersion":1,"status":if code==0 {"ok"} else {"attention"},"data":data,"warnings":warnings});
            let rendered = if cli.json {
                emit(&value)
            } else {
                std::io::stdout()
                    .lock()
                    .write_all(output::human(&data).as_bytes())
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
                eprintln!("macops: {e}");
            }
            ExitCode::from(e.exit)
        }
    }
}

fn run_operation(
    cli: &cli::Cli,
    ctx: &context::Context,
    catalog: &domain::Catalog,
) -> error::Result<Value> {
    match &cli.command {
        cli::Command::Install { id, version } => operations::execute(
            ctx,
            operations::plan(
                ctx,
                &target::resolve(ctx, id)?,
                domain::Action::Install,
                version.as_deref(),
                cli.dry_run,
            )?,
            cli.dry_run,
            cli.yes,
        ),
        cli::Command::Upgrade { id, version } => operations::execute(
            ctx,
            operations::plan(
                ctx,
                &target::resolve(ctx, id)?,
                domain::Action::Upgrade,
                version.as_deref(),
                cli.dry_run,
            )?,
            cli.dry_run,
            cli.yes,
        ),
        cli::Command::Uninstall { id } => operations::execute(
            ctx,
            operations::plan(
                ctx,
                &target::resolve(ctx, id)?,
                domain::Action::Uninstall,
                None,
                cli.dry_run,
            )?,
            cli.dry_run,
            cli.yes,
        ),
        cli::Command::Adopt { id, .. } => {
            let resolved = target::resolve(ctx, id)?;
            require_config_target(
                cli.chezmoi_source.is_some()
                    || cli.chezmoi_destination.is_some()
                    || cli.chezmoi_config.is_some(),
                &[&resolved],
            )?;
            operations::execute(
                ctx,
                operations::plan(ctx, &resolved, domain::Action::Adopt, None, cli.dry_run)?,
                cli.dry_run,
                cli.yes,
            )
        }
        cli::Command::Update { id } => {
            if let Some(key) = id
                && key.starts_with("application:")
            {
                let resolved = target::resolve(ctx, key)?;
                if policy::uv_local::is_uv(&resolved.provider) {
                    return updates::update(ctx, &[&resolved], cli.dry_run);
                }
            }
            updates::update(ctx, &selected(catalog, id.as_deref())?, cli.dry_run)
        }
        cli::Command::Outdated { id } => updates::outdated(ctx, &selected(catalog, id.as_deref())?),
        cli::Command::Cleanup { id } => {
            cache::cleanup(ctx, catalog.select(id)?, cli.dry_run, cli.yes)
        }
        _ => Err(error::Error::new(
            "unsupported_command",
            "该命令没有已验证的执行接口",
        )),
    }
}

use crate::policy::cli::{attention, selected};

mod npm_adoption;
mod npm_local;
#[cfg(test)]
#[path = "../tests/unit/support.rs"]
mod test_support;

#[cfg(test)]
#[path = "../tests/unit/native_plugins_support.rs"]
mod native_plugins_test_fixture;

#[cfg(test)]
#[path = "../tests/unit/profile_codec_regressions.rs"]
mod profile_codec_regressions;

mod source_tree;

mod source_git;

mod source_inventory;

mod source_tx;

mod source_payload;

mod source_files;

mod source_invocation;

mod source_plugins;
