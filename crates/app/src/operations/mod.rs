pub(crate) mod command;
pub(crate) mod governance;
pub(crate) mod identity;
pub(crate) mod manifest;
pub(crate) mod native_runner;

pub(crate) mod providers;
pub(crate) mod store;

mod recovery;
pub(crate) use recovery::{
    Recovered, RecoveryPreview, RecoveryState, lock_after_recovery, matches_intent,
    pending_recovery, recover, recover_before_new_plan, recover_before_write,
    recover_before_write_checked, recovery_history, recovery_text, require_recovery_roots,
    validated_recovery_journal, verify_recovery_preview,
};

use crate::{
    catalog::inventory,
    context::Context,
    domain::{Action, Observation, Package, Plan, Provider, QuerySpec, Receipts, fingerprint},
    error::{Error, Result},
    operations::store::{Journal, Phase, Store},
    platform::{fs as paths, process},
};
use serde_json::{Value, json};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

pub fn confirm(message: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    crate::terminal::confirm(message)
}

pub fn plan(
    ctx: &Context,
    package: &Package,
    action: Action,
    version: Option<&str>,
    dry_run: bool,
) -> Result<Plan> {
    crate::software::rules::uv_local::action(&package.provider, action)?;
    if matches!(package.provider, Provider::Uv { binding: None, .. }) {
        return Err(Error::attention(
            "legacy_uv_query_unbound",
            "旧 uv 声明未绑定 namespace，不能采纳或启动原生查询",
        ));
    }
    crate::policy::operation::state::version_request(package, action, version)?;
    let frozen = local_query(ctx, package, action)?;
    let before = match &frozen {
        Some(query) => providers::observe_query(package, query, &ctx.scope, false)?,
        None => inventory::observe(ctx, package),
    };
    crate::policy::operation::state::plan_inputs(package, action, &before, version)?;
    if action == Action::Adopt {
        return adoption_plan(ctx, package, before, frozen);
    }
    check_before(ctx, package, action, &before, frozen.as_ref())?;
    let cached = if dry_run && version.is_none() && action != Action::Uninstall {
        crate::policy::catalog::cache::indexed_version(
            &crate::catalog::updates::read(ctx)?,
            package,
            &ctx.scope,
        )?
    } else {
        None
    };
    let resolution = crate::policy::operation::state::version_resolution(
        action,
        dry_run,
        version,
        cached.as_deref(),
    )?;
    if resolution == crate::policy::operation::state::Resolution::Unresolved {
        return Ok(Plan {
            brew: None,
            npm_binding: None,
            uv_owner: None,
            timeout_secs: ctx.timeout_secs,
            package: package.clone(),
            action,
            before,
            target: None,
            command: None,
            query: None,
            recipe_sha256: fingerprint(package)?,
            warnings: vec![
                "目标尚未解析；预演不联网。先运行 mo app refresh 或提供 --version，才能显示完整执行命令".into(),
            ],
            synchronous: providers::synchronous(package),
            delegated: false,
        });
    }
    let retirement = frozen
        .as_ref()
        .and_then(crate::software::mise_owner::retirement::query_witness);
    let target = match resolution {
        crate::policy::operation::state::Resolution::Explicit(v)
        | crate::policy::operation::state::Resolution::Cached(v) => Some(v),
        crate::policy::operation::state::Resolution::Latest => {
            Some(crate::catalog::updates::latest(ctx, package)?)
        }
        crate::policy::operation::state::Resolution::Absent
        | crate::policy::operation::state::Resolution::Unresolved => {
            retirement.map(|release| release.version.clone())
        }
    };
    let command = native_write_spec(ctx, package, action, target.as_deref(), frozen.as_ref())?;
    let query = match frozen {
        Some(query) => query,
        None => providers::query_spec(ctx, package)?,
    };
    Ok(Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        package: package.clone(),
        action,
        before,
        target,
        command: Some(command),
        query: Some(query),
        recipe_sha256: fingerprint(package)?,
        warnings: if matches!(package.provider, Provider::Brew { .. }) {
            vec!["brew 无通用固定版本参数；若解析后版本变化，后置验证将停止并报告 unknown".into()]
        } else if let Provider::Npm { authority, .. } = &package.provider {
            crate::software::rules::npm_local::write_warnings(authority.as_ref(), action)
        } else {
            vec![]
        },
        synchronous: providers::synchronous(package),
        delegated: false,
    })
}

fn native_write_spec(
    ctx: &Context,
    package: &Package,
    action: Action,
    target: Option<&str>,
    frozen: Option<&QuerySpec>,
) -> Result<crate::domain::CommandSpec> {
    let native_version = if matches!(package.provider, Provider::Brew { .. }) {
        None
    } else {
        target
    };
    let mut command = if let Some(release) =
        frozen.and_then(crate::software::mise_owner::retirement::query_witness)
    {
        crate::software::mise_owner::retirement::write_spec(
            package,
            action,
            native_version,
            release,
        )?
    } else {
        providers::mutation_spec(ctx, package, action, native_version)?
    };
    if let Some(QuerySpec::LocalNpm(query)) = frozen {
        crate::software::npm_local::verify_plain_namespace_for_write(query, action)?;
        crate::software::rules::npm_local::bind_canonical_command_paths(
            &mut command,
            &query.namespace.canonical_prefix,
        )?;
    }
    if ctx.isolated && !paths::read_path(&command.program)?.starts_with(&ctx.home) {
        return Err(Error::new(
            "global_write_in_fixture",
            "fixture 只能运行 fixture 内声明的写程序",
        ));
    }
    Ok(command)
}

pub fn new_journal(ctx: &Context, plan: Plan) -> Result<Journal> {
    new_journal_with_id(ctx, plan, store::operation_id())
}

pub fn new_journal_with_id(ctx: &Context, plan: Plan, id: String) -> Result<Journal> {
    let journal = Journal {
        scope_sha256: fingerprint(&ctx.scope)?,
        scope: ctx.scope.clone(),
        schema_version: 1,
        id,
        phase: Phase::Prepared,
        plan_sha256: fingerprint(&plan)?,
        plan,
        home: ctx.home.clone(),
        state: ctx.state.clone(),
        cache: ctx.cache.clone(),
        settings: ctx.settings.clone(),
        process_group: None,
        started_at: store::now(),
        message: String::new(),
    };
    store::validate_journal(&journal)?;
    Ok(journal)
}

pub fn execute(ctx: &Context, plan: Plan, dry_run: bool, yes: bool) -> Result<Value> {
    execute_checked(ctx, plan, dry_run, yes, || Ok(()))
}

pub(crate) fn execute_checked(
    ctx: &Context,
    plan: Plan,
    dry_run: bool,
    yes: bool,
    check: impl FnOnce() -> Result<()>,
) -> Result<Value> {
    crate::software::rules::uv_local::validate_plan(&plan)?;
    if dry_run {
        return Ok(json!({"mode":"dry-run","backend":"rust","plan":plan,"persistentWrites":false}));
    }
    if matches!(
        plan.action,
        Action::Uninstall | Action::Adopt | Action::Start | Action::Stop
    ) {
        confirm(&confirmation(ctx, &plan), yes)?;
        if let Some(previous) = recover_before_write(ctx, None)?
            && previous.original.plan.recipe_sha256 == plan.recipe_sha256
            && previous.matches(plan.action, &plan.package.id, plan.target.as_deref())
        {
            return Ok(previous.result);
        }
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    store.receipts()?;
    let frozen = plan.query.as_ref().filter(|query| checked_query(query));
    let observed = match frozen {
        Some(query) => providers::observe_query(&plan.package, query, &ctx.scope, false)?,
        None => inventory::observe(ctx, &plan.package),
    };
    let current = crate::catalog::target::refresh(ctx, &plan.package)?;
    crate::policy::operation::state::require_fresh_plan(&plan, &current, &observed)?;
    check_before(ctx, &plan.package, plan.action, &observed, frozen)?;
    if let Some(QuerySpec::LocalNpm(query)) = &plan.query {
        crate::software::npm_local::verify_plain_namespace_for_write(query, plan.action)?;
    }
    if plan.uv_owner.is_some() {
        crate::software::uv_ownership::check(ctx, &plan, None)?;
    }
    check()?;
    if crate::software::mise_owner::applies(&plan)
        && let Some(spec) = &plan.command
    {
        let mut command = crate::operations::command::command(spec, &ctx.scope)?;
        mise_admission(ctx, &plan, &mut command)?;
    }
    let mut journal = new_journal(ctx, plan)?;
    store.save(&journal)?;
    if journal.plan.action == Action::Adopt {
        if journal.plan.uv_owner.is_some() {
            store
                .commit_checked(&journal, journal.plan.before.clone(), || {
                    crate::software::uv_ownership::check(ctx, &journal.plan, None)?;
                    let query = journal
                        .plan
                        .query
                        .as_ref()
                        .ok_or_else(|| Error::new("invalid_uv_plan", "缺少冻结 uv 查询"))?;
                    let current = providers::observe_query(
                        &journal.plan.package,
                        query,
                        &journal.scope,
                        false,
                    )?;
                    crate::policy::operation::state::require_fresh_plan(
                        &journal.plan,
                        &journal.plan.package,
                        &current,
                    )
                })
                .map_err(|e| match e {
                    store::CommitFailure::Check(e) | store::CommitFailure::Outcome(e) => e,
                })?;
        } else {
            store.commit(&journal, journal.plan.before.clone())?;
        }
        journal.phase = Phase::Succeeded;
        store.save(&journal)?;
        return Ok(json!(journal));
    }
    let result = launch(&store, &mut journal)?;
    if let Some(message) = crate::policy::operation::state::completion(
        result.code,
        result.uncertain,
        journal.process_group.is_some_and(process::group_alive),
    ) {
        journal.phase = Phase::Unknown;
        journal.message = message;
        store.save(&journal)?;
        return Ok(json!(journal));
    }
    if let Err(error) = crate::software::mise_owner::record_completion(&store, &journal) {
        journal.phase = Phase::Unknown;
        journal.message = error.to_string();
        store.save(&journal)?;
        return Ok(json!(journal));
    }
    finish(&store, &mut journal)?;
    Ok(json!(journal))
}

fn confirmation(ctx: &Context, plan: &Plan) -> String {
    if crate::software::mise_owner::retirement::bound_query(plan).is_some() {
        format!(
            "卸载 {}\n{}",
            crate::browser::sanitize(&plan.package.name, false),
            crate::terminal::plan::plan_scope(ctx, plan)
        )
    } else if plan.action == Action::Adopt {
        format!(
            "纳入管理 {}\n{}",
            crate::browser::sanitize(&plan.package.name, false),
            crate::terminal::plan::plan_scope(ctx, plan)
        )
    } else {
        format!("{:?} {}", plan.action, plan.package.id)
    }
}

fn mise_admission(ctx: &Context, plan: &Plan, command: &mut std::process::Command) -> Result<()> {
    crate::software::mise_owner::bind_mutation(&ctx.scope, plan, command)?;
    if plan.action == Action::Uninstall {
        crate::software::mise_owner::retirement::check_receipt(ctx, plan)
    } else {
        crate::software::mise_owner::check_command(ctx, command)
    }
}

fn observe_frozen(
    store: &Store,
    journal: &Journal,
) -> Result<(Observation, Option<crate::domain::MiseQuery>)> {
    let receipts = store.receipts()?;
    let committed = receipts
        .installations
        .get(&journal.plan.package.id)
        .map(|receipt| crate::software::rules::npm_local::committed_install_query(journal, receipt))
        .transpose()?
        .flatten();
    let spec = journal
        .plan
        .query
        .as_ref()
        .ok_or_else(|| Error::new("verification_missing", "原操作没有冻结验证查询"))?;
    if let QuerySpec::Mise(query) = spec {
        let final_query = crate::software::mise_owner::completion_query(store, journal, query)?;
        let finalized = QuerySpec::Mise(final_query.clone());
        if receipts
            .installations
            .get(&journal.plan.package.id)
            .is_some_and(|known| {
                known.operation_id == journal.id && known.query.as_ref() != Some(&finalized)
            })
        {
            return Err(Error::attention(
                "commit_conflict",
                "已登记的 mise 安装目录身份发生变化",
            ));
        }
        let observed =
            providers::observe_query(&journal.plan.package, &finalized, &journal.scope, true)?;
        return Ok((observed, Some(final_query)));
    }
    let observed = providers::observe_query(
        &journal.plan.package,
        committed.as_ref().unwrap_or(spec),
        &journal.scope,
        true,
    )?;
    Ok((observed, None))
}

fn finish(store: &Store, journal: &mut Journal) -> Result<()> {
    let (observed, mise) = observe_frozen(store, journal).unwrap_or_else(|error| {
        (
            Observation::Unknown {
                reason: error.to_string(),
            },
            None,
        )
    });
    if expected(journal, &observed) {
        let committed = if let Some(query) = mise {
            store.commit_mise(journal, observed, query)
        } else {
            store.commit(journal, observed)
        };
        match committed {
            Ok(()) => {
                journal.phase = Phase::Succeeded;
                journal.message = "安装状态通过冻结契约验证".into();
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "commit_conflict" | "mise_scope_changed" | "mise_release_changed"
                ) =>
            {
                journal.phase = Phase::Unknown;
                journal.message = error.to_string();
            }
            Err(error) => return Err(error),
        }
    } else {
        journal.phase = Phase::Unknown;
        journal.message = format!(
            "未验证预期结果：{}；保留阻断",
            serde_json::to_string(&observed)?
        );
    }
    store.save(journal)
}

pub fn launch(store: &Store, journal: &mut Journal) -> Result<process::Output> {
    if crate::software::rules::uv_local::is_uv(&journal.plan.package.provider) {
        return Err(crate::software::rules::uv_local::blocked());
    }
    let mut output_file = tempfile::tempfile()?;
    let mut error_file = tempfile::tempfile()?;
    let mut progress = if journal.plan.package.kind == crate::domain::Kind::Config {
        None
    } else {
        crate::terminal::progress::LiveProgress::open()
    };
    let (mut mise_progress, stdout, stderr) =
        launch_streams(journal, progress.is_some(), &output_file, &error_file)?;
    let path = store
        .path
        .join("operations")
        .join(format!("{}.json", journal.id));
    let binary = std::env::current_exe()?;
    let mut child = std::process::Command::new(binary)
        .arg("__worker")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    journal.phase = Phase::Running;
    journal.process_group = Some(child.id());
    if let Err(e) = store.save(journal) {
        process::kill_group(child.id());
        let _ = child.wait();
        return Err(e);
    }
    let mut gate = child
        .stdin
        .take()
        .ok_or_else(|| Error::new("worker_gate_missing", "worker stdin 未建立"))?;
    if let Some(progress) = &mut progress {
        progress.announce(journal, None);
    }
    if let Err(e) = gate.write_all(b"RUN\n") {
        process::kill_group(child.id());
        let _ = child.wait();
        journal.phase = Phase::Unknown;
        journal.message = e.to_string();
        store.save(journal)?;
        return Err(e.into());
    }
    drop(gate);
    let start = Instant::now();
    let mut uncertain = false;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if start.elapsed() > Duration::from_secs(journal.plan.timeout_secs)
            || output_file.metadata()?.len() + error_file.metadata()?.len() > 4 * 1024 * 1024
        {
            uncertain = true;
            process::kill_group(child.id());
            break child.wait()?;
        }
        if relay_launch(
            &mut progress,
            &mut mise_progress,
            &mut output_file,
            &error_file,
            false,
        )
        .is_err()
        {
            uncertain = true;
            process::kill_group(child.id());
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    uncertain |= relay_launch(
        &mut progress,
        &mut mise_progress,
        &mut output_file,
        &error_file,
        true,
    )
    .is_err();
    uncertain |= output_file.metadata()?.len() > 2 * 1024 * 1024
        || error_file.metadata()?.len() > 2 * 1024 * 1024;
    output_file.seek(SeekFrom::Start(0))?;
    error_file.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    let mut diagnostic = Vec::new();
    output_file.take(2 * 1024 * 1024).read_to_end(&mut output)?;
    error_file
        .take(2 * 1024 * 1024)
        .read_to_end(&mut diagnostic)?;
    Ok(process::Output {
        code: status.code(),
        stdout: String::from_utf8_lossy(&output).into_owned(),
        stderr: String::from_utf8_lossy(&diagnostic).into_owned(),
        uncertain,
    })
}

fn launch_streams(
    journal: &Journal,
    live: bool,
    output: &std::fs::File,
    error: &std::fs::File,
) -> std::io::Result<(
    Option<crate::terminal::mise_progress::MiseProgress>,
    Stdio,
    Stdio,
)> {
    if live
        && crate::agent::ai_tx::is_native(journal)
        && journal.plan.command.as_ref().is_some_and(|command| {
            command
                .program
                .file_name()
                .is_some_and(|file| file == "mise")
                && command.args.first().is_some_and(|arg| arg == "use")
        })
        && let Some((pty, stdout, stderr)) = crate::terminal::mise_progress::MiseProgress::open()
    {
        return Ok((Some(pty), stdout, stderr));
    }
    Ok((
        None,
        Stdio::from(output.try_clone()?),
        Stdio::from(error.try_clone()?),
    ))
}

fn relay_launch(
    display: &mut Option<crate::terminal::progress::LiveProgress>,
    pty: &mut Option<crate::terminal::mise_progress::MiseProgress>,
    output: &mut std::fs::File,
    error: &std::fs::File,
    draining: bool,
) -> std::io::Result<()> {
    if let Some(display) = display {
        if let Some(pty) = pty {
            let result = pty.relay(display, output, draining);
            if draining {
                pty.finish(display);
            }
            return result;
        }
        display.relay([output, error], draining);
    }
    Ok(())
}

pub fn worker(path: &Path) -> Result<()> {
    let mut gate = [0_u8; 4];
    std::io::stdin().read_exact(&mut gate)?;
    crate::policy::operation::state::worker_gate(gate)?;
    let journal: Journal = paths::regular_json(path)?;
    let error = prepare_worker(
        &journal,
        path,
        std::process::id(),
        nix::unistd::getpgrp().as_raw(),
    )?
    .exec();
    Err(error.into())
}

fn prepare_worker(
    journal: &Journal,
    path: &Path,
    pid: u32,
    group: i32,
) -> Result<std::process::Command> {
    store::validate_journal(journal)?;
    if crate::skills::native_skills::is_native(journal) {
        return Err(Error::attention(
            "native_worker_required",
            "Native Skills 只能使用冻结的原生 supervisor 协议",
        ));
    }
    if crate::software::rules::uv_local::is_uv(&journal.plan.package.provider) {
        return Err(crate::software::rules::uv_local::blocked());
    }
    if crate::agent::native_plugin_tx::is_plugin(journal)
        || crate::agent::codex_install::is_install(journal)
        || crate::agent::claude_install::is_install(journal)
    {
        return Err(crate::agent::native_plugins::refusal(
            "native_plugin_worker_required",
        ));
    }
    if crate::config::rules::chezmoi_native::is_native(&journal.plan) || journal.plan.brew.is_some()
    {
        return Err(Error::attention(
            "native_worker_required",
            "此计划必须使用已绑定原生执行器",
        ));
    }
    crate::policy::operation::state::worker_identity(journal, path, pid, group)?;
    if let Some(query) = journal
        .plan
        .query
        .as_ref()
        .filter(|query| checked_query(query))
    {
        let current =
            providers::observe_query(&journal.plan.package, query, &journal.scope, false)?;
        crate::policy::operation::state::require_fresh_plan(
            &journal.plan,
            &journal.plan.package,
            &current,
        )?;
        if let QuerySpec::LocalNpm(npm_query) = query {
            crate::software::npm_local::verify_plain_namespace_for_write(
                npm_query,
                journal.plan.action,
            )?;
        }
    }
    let spec = journal
        .plan
        .command
        .as_ref()
        .ok_or_else(|| Error::new("worker_command_missing", "没有写命令"))?;
    let native = crate::agent::ai_tx::worker_command(journal)?;
    let spec = native.as_ref().unwrap_or(spec);
    let mut command = crate::operations::command::command(spec, &journal.scope)?;
    if crate::software::mise_owner::applies(&journal.plan) {
        let ctx = crate::software::mise_owner::worker_context(journal)?;
        mise_admission(&ctx, &journal.plan, &mut command)?;
    }
    Ok(command)
}

fn checked_query(query: &QuerySpec) -> bool {
    matches!(
        query,
        QuerySpec::Mise(_)
            | QuerySpec::LocalUvTool(_)
            | QuerySpec::LocalNpm(_)
            | QuerySpec::LocalConfig(_)
            | QuerySpec::LocalCapability(_)
    )
}

#[cfg(test)]
#[path = "../../tests/unit/operation_worker.rs"]
mod worker_tests;

pub(crate) fn locked_journal(store: &Store, id: &str) -> Result<Journal> {
    let history = Store::read::<Journal>(&store.operations, &format!("{id}.json"))?;
    let active = Store::read::<Journal>(&store.root, "active.json")?;
    crate::policy::operation::state::recovery_journal(id, active, history)
}

fn check_before(
    ctx: &Context,
    package: &Package,
    action: Action,
    before: &Observation,
    frozen: Option<&QuerySpec>,
) -> Result<()> {
    check_existing_owner(ctx, package)?;
    crate::policy::operation::state::require_install_slot(action, before)?;
    if matches!(action, Action::Upgrade | Action::Uninstall) {
        let receipts_file = ctx.state.join("receipts.json");
        let receipts: Receipts = if paths::read_path(&receipts_file)?.exists() {
            paths::regular_json(&receipts_file)?
        } else {
            Receipts {
                schema_version: 1,
                installations: std::collections::BTreeMap::default(),
            }
        };
        let receipt = receipts.installations.get(&package.id).ok_or_else(|| {
            Error::attention("adoption_required", "先 adopt 采纳已安装基线，再升级或卸载")
        })?;
        let mut query = match frozen {
            Some(query) => query.clone(),
            None => providers::query_spec(ctx, package)?,
        };
        if let QuerySpec::Mise(query) = &mut query {
            query.retirement = None;
        }
        crate::policy::operation::state::receipt_matches(
            &ctx.scope, package, before, &query, receipt,
        )?;
    }
    Ok(())
}

fn check_existing_owner(ctx: &Context, package: &Package) -> Result<()> {
    if crate::software::rules::uv_local::is_uv(&package.provider) {
        return crate::software::uv_ownership::snapshot(ctx, package).map(|_| ());
    }
    crate::catalog::target::check_owner(ctx, package)?;
    let path = ctx.state.join("receipts.json");
    if !paths::read_path(&path)?.exists() {
        return Ok(());
    }
    let receipts: Receipts = paths::regular_json(&path)?;
    crate::policy::operation::state::existing_owner(
        &ctx.scope,
        package,
        receipts.installations.get(&package.id),
    )
}

fn adoption_plan(
    ctx: &Context,
    package: &Package,
    before: Observation,
    frozen: Option<QuerySpec>,
) -> Result<Plan> {
    crate::agent::codex_install::require_ordinary_adoption(package)?;
    crate::agent::claude_install::require_ordinary_adoption(package)?;
    if matches!(before, Observation::Missing { .. }) {
        return Err(Error::attention("not_installed", &package.id));
    }
    check_existing_owner(ctx, package)?;
    Ok(Plan {
        brew: None,
        npm_binding: None,
        uv_owner: if crate::software::rules::uv_local::is_uv(&package.provider) {
            Some(crate::software::uv_ownership::snapshot(ctx, package)?)
        } else {
            None
        },
        timeout_secs: ctx.timeout_secs,
        package: package.clone(),
        action: Action::Adopt,
        target: if crate::software::rules::uv_local::is_uv(&package.provider) {
            None
        } else {
            before.version().map(str::to_owned)
        },
        before,
        command: None,
        query: frozen.or_else(|| providers::query_spec(ctx, package).ok()),
        recipe_sha256: fingerprint(package)?,
        warnings: vec![],
        synchronous: true,
        delegated: false,
    })
}

use crate::policy::operation::state::expected;

#[cfg(test)]
#[path = "../../tests/unit/npm_operations.rs"]
mod npm_tests;

#[cfg(test)]
#[path = "../../tests/unit/uv_operations.rs"]
mod uv_tests;

fn local_query(ctx: &Context, package: &Package, action: Action) -> Result<Option<QuerySpec>> {
    if matches!(
        package.provider,
        Provider::ConfigObserved {
            authority: crate::domain::ConfigAuthority::ChezmoiNative,
            ..
        }
    ) {
        return Err(Error::attention(
            "config_native_explicit_entry_required",
            "原生配置仅通过明确 config 命令执行",
        ));
    }
    Ok(
        if matches!(
            package.provider,
            Provider::Mise { .. }
                | Provider::Uv {
                    binding: Some(_),
                    ..
                }
                | Provider::UvObserved { .. }
                | Provider::Npm { .. }
                | Provider::NpmObserved { .. }
                | Provider::ConfigObserved { .. }
                | Provider::CapabilityObserved { .. }
        ) {
            Some(providers::capture_query(ctx, package, Some(action))?)
        } else {
            None
        },
    )
}
