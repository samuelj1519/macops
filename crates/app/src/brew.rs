use crate::{
    context::Context,
    domain::{
        BrewContextBinding, BrewContextRecord, BrewContextRegistry, BrewFormulaKey, BrewQuerySpec,
        Observation,
    },
    error::{Error, Result},
    policy::brew as policy,
    store::Store,
};
use std::collections::{BTreeMap, BTreeSet};

const REGISTRY: &str = "brew-contexts-v1.json";

pub fn read_registry(store: &Store) -> Result<BrewContextRegistry> {
    store.verify_namespace()?;
    let value = read_registry_at(&store.root)?;
    store.verify_namespace()?;
    Ok(value)
}
fn read_registry_at(root: &std::fs::File) -> Result<BrewContextRegistry> {
    match rustix::fs::statat(root, REGISTRY, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(meta)
            if meta.st_uid != nix::unistd::geteuid().as_raw()
                || meta.st_mode & 0o170_000 != 0o100_000
                || meta.st_mode & 0o077 != 0 =>
        {
            return Err(Error::attention(
                "brew_registry_not_private",
                "Brew registry 必须是当前用户的私有普通文件",
            ));
        }
        Ok(_) | Err(rustix::io::Errno::NOENT) => (),
        Err(error) => return Err(std::io::Error::from_raw_os_error(error.raw_os_error()).into()),
    }
    let registry = match Store::read::<crate::policy::json::UniqueValue>(root, REGISTRY)? {
        Some(value) => {
            serde_json::from_value::<crate::policy::json::Object<BrewContextRegistry>>(value.0)?.0
        }
        None => BrewContextRegistry {
            schema_version: 1,
            contexts: BTreeMap::new(),
        },
    };
    policy::validate_registry(&registry)?;
    Ok(registry)
}
fn persist(store: &Store, registry: &BrewContextRegistry) -> Result<()> {
    policy::validate_registry(registry)?;
    store.verify_namespace()?;
    Store::atomic(&store.root, REGISTRY, registry)?;
    store.verify_namespace()
}
fn binding_scope(ctx: &Context, store: &Store, binding: &BrewContextBinding) -> Result<()> {
    if ctx.state != store.path || ctx.home != binding.metadata_scope.home {
        return Err(Error::attention(
            "brew_registry_scope_changed",
            "Brew context 与原 HOME/状态范围不匹配",
        ));
    }
    policy::validate_binding(binding)
}
pub fn register_context(
    ctx: &Context,
    store: &Store,
    binding: BrewContextBinding,
    declaration_keys: BTreeMap<String, BrewFormulaKey>,
) -> Result<BrewContextRecord> {
    binding_scope(ctx, store, &binding)?;
    store.check_pending()?;
    let mut registry = read_registry(store)?;
    if registry.contexts.contains_key(&binding.context_id) {
        return Err(Error::attention(
            "brew_context_exists",
            "context 已存在；变更必须显式 rebind",
        ));
    }
    let record = BrewContextRecord {
        epoch: 0,
        binding,
        observation_valid: false,
        writer_scope_confirmed: false,
        declaration_keys,
    };
    registry
        .contexts
        .insert(record.binding.context_id.clone(), record.clone());
    persist(store, &registry)?;
    Ok(record)
}
pub fn rebind_context(
    ctx: &Context,
    store: &Store,
    binding: BrewContextBinding,
    continuity_verified: bool,
) -> Result<BrewContextRecord> {
    binding_scope(ctx, store, &binding)?;
    store.check_pending()?;
    let mut registry = read_registry(store)?;
    let previous = registry
        .contexts
        .get(&binding.context_id)
        .ok_or_else(|| Error::attention("brew_context_missing", "没有原 context 可重绑定"))?;
    let next = policy::rebind(previous, binding, continuity_verified, &BTreeSet::new())?;
    registry
        .contexts
        .insert(next.binding.context_id.clone(), next.clone());
    persist(store, &registry)?;
    Ok(next)
}
pub fn record_observation(
    ctx: &Context,
    store: &Store,
    query: &BrewQuerySpec,
    observed: &Observation,
) -> Result<()> {
    binding_scope(ctx, store, &query.binding)?;
    let mut registry = read_registry(store)?;
    let previous = registry
        .contexts
        .get(&query.binding.context_id)
        .ok_or_else(|| Error::attention("brew_context_missing", "没有查询所属 context"))?;
    let next = policy::accept_observation(previous, query, observed)?;
    registry
        .contexts
        .insert(next.binding.context_id.clone(), next);
    persist(store, &registry)
}

fn helper_directory(store: &Store) -> Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, mkdirat, openat};
    use std::os::unix::fs::MetadataExt;
    let mut directory = store.root.try_clone()?;
    for name in [
        "native-brew",
        "query-scripts",
        "v1",
        &crate::policy::cache::digest(policy::QUERY_SCRIPT),
    ] {
        match mkdirat(&directory, name, Mode::from_bits_truncate(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(e) => return Err(std::io::Error::from_raw_os_error(e.raw_os_error()).into()),
        }
        directory = std::fs::File::from(
            openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?,
        );
        let meta = directory.metadata()?;
        if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o777 != 0o700 {
            return Err(Error::attention(
                "brew_helper_not_private",
                "Brew 查询 helper 目录不是私有目录",
            ));
        }
        store.verify_namespace()?;
    }
    Ok(directory)
}
fn materialize_script(store: &Store) -> Result<crate::domain::BrewQueryScript> {
    use rustix::fs::{Mode, OFlags, openat};
    use std::io::Write;
    let directory = helper_directory(store)?;
    match openat(
        &directory,
        "query.rb",
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o400),
    ) {
        Ok(fd) => {
            let mut file = std::fs::File::from(fd);
            file.write_all(policy::QUERY_SCRIPT)?;
            file.sync_all()?;
            directory.sync_all()?;
        }
        Err(rustix::io::Errno::EXIST) => (),
        Err(e) => return Err(std::io::Error::from_raw_os_error(e.raw_os_error()).into()),
    }
    let absolute = store
        .path
        .join("native-brew/query-scripts/v1")
        .join(crate::policy::cache::digest(policy::QUERY_SCRIPT))
        .join("query.rb");
    let stamp = query::file_stamp(&absolute)?;
    if stamp.sha256 != crate::policy::cache::digest(policy::QUERY_SCRIPT)
        || stamp.node.mode & 0o777 != 0o400
    {
        return Err(Error::attention(
            "brew_query_script_changed",
            "Brew 固定查询 helper 不匹配",
        ));
    }
    store.verify_namespace()?;
    let executable = crate::paths::read_path(&std::env::current_exe()?)?;
    Ok(crate::domain::BrewQueryScript {
        origin: crate::domain::BrewScriptOrigin::MacopsBuiltinV1,
        absolute_path: absolute.clone(),
        canonical_path: crate::paths::read_path(&absolute)?,
        file_id: stamp.node.identity,
        sha256: stamp.sha256,
        size: stamp.node.size,
        product_build_id: query::file_stamp(&executable)?.sha256,
    })
}
fn targeted(command: &crate::cli::Command) -> Option<(&str, bool)> {
    use crate::cli::Command;
    match command {
        Command::Doctor { id: Some(id) } => Some((id, true)),
        Command::Update { id: Some(id) }
        | Command::Upgrade { id, .. }
        | Command::Install { id, .. }
        | Command::Uninstall { id }
        | Command::Adopt { id, .. } => Some((id, false)),
        _ => None,
    }
}

fn action(command: &crate::cli::Command) -> crate::domain::Action {
    use crate::{cli::Command, domain::Action};
    match command {
        Command::Install { .. } => Action::Install,
        Command::Uninstall { .. } => Action::Uninstall,
        Command::Adopt { .. } => Action::Adopt,
        _ => Action::Upgrade,
    }
}
fn confirm_scope(query: &BrewQuerySpec, description: &str, yes: bool) -> Result<()> {
    let summary = format!(
        "{description}\nprogram={}\nprefix={}\nCellar={}\nformula={}/{}\nNative 保留依赖/dependents；卸载为全部匹配版本，并保留Native pin/alias housekeeping。",
        query.binding.invocation.display(),
        query.binding.prefix.display(),
        query.binding.cellar.display(),
        query.selection.tap,
        query.selection.name
    );
    if yes {
        eprintln!("{summary}");
    }
    crate::operations::confirm(&summary, yes)
}
fn prepared_plan(
    ctx: &Context,
    package: &crate::domain::Package,
    query: BrewQuerySpec,
    observed: Observation,
    owner: crate::domain::BrewOwnerSnapshot,
    action: crate::domain::Action,
    consent: crate::domain::BrewConsent,
) -> Result<crate::domain::Plan> {
    use crate::domain::{
        Action, BrewContextAuthority, BrewExecution, BrewPlanData, BrewScopeConfirmation,
        Observation, Plan, QuerySpec,
    };
    let (goal, target) = policy::resolve_goal(&query, action, &observed)?;
    let execution = if action == Action::Adopt {
        BrewExecution::BaselineOnly
    } else if (action == Action::Upgrade && policy::upgrade_is_current(&query, &observed)?)
        || (action == Action::Uninstall && matches!(observed, Observation::BrewMissing { .. }))
    {
        BrewExecution::VerifiedNoop
    } else {
        BrewExecution::NativeMutator
    };
    let consent = if execution == BrewExecution::VerifiedNoop
        && action == Action::Upgrade
        && consent != crate::domain::BrewConsent::Yes
    {
        crate::domain::BrewConsent::ExistingContext
    } else {
        consent
    };
    let resolved = policy::resolved_package(package, &query)?;
    let data = BrewPlanData {
        schema_version: 1,
        owner,
        context: BrewContextAuthority {
            context_id: query.binding.context_id.clone(),
            epoch: query.context_epoch,
            binding_fingerprint: crate::domain::fingerprint(&query.binding)?,
        },
        goal,
        execution: execution.clone(),
        confirmation: BrewScopeConfirmation {
            context_id: query.binding.context_id.clone(),
            epoch: query.context_epoch,
            logical_owner_id: policy::logical_id(&query.key)?,
            action,
            effect_contract: policy::EFFECT_CONTRACT.into(),
            consent,
        },
        env_capture_id: query.env_capture_id.clone(),
    };
    let command = if execution == BrewExecution::NativeMutator {
        Some(policy::native_command(&query, action)?)
    } else {
        None
    };
    let plan = Plan {
        brew: Some(Box::new(data)),
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        package: resolved.clone(),
        action,
        before: observed,
        target,
        command,
        query: Some(QuerySpec::BrewFormula(Box::new(query))),
        recipe_sha256: crate::domain::fingerprint(&resolved)?,
        warnings: vec!["Native 维护没有原子回滚；未知结果只查询原操作，不重跑。".into()],
        synchronous: true,
        delegated: true,
    };
    policy::checked_plan(&plan)?
        .ok_or_else(|| Error::attention("invalid_brew_plan", "Brew 计划缺少投影"))?;
    Ok(plan)
}
fn finish_without_mutator(
    ctx: &Context,
    store: &Store,
    plan: crate::domain::Plan,
    env: &query::ConfirmedEnvironment,
) -> Result<serde_json::Value> {
    let query = match &plan.query {
        Some(crate::domain::QuerySpec::BrewFormula(q)) => q.as_ref().clone(),
        _ => return Err(Error::attention("invalid_brew_plan", "缺少Brew查询")),
    };
    let mut journal = crate::operations::new_journal(ctx, plan)?;
    store.save(&journal)?;
    let observed = match query::observe(store, &query, env, journal.plan.timeout_secs) {
        Ok(value) => value,
        Err(error) => Observation::Unknown { reason: error.code },
    };
    if !crate::policy::state::expected(&journal, &observed) {
        journal.phase = crate::domain::Phase::Unknown;
        journal.message = "Brew 基线/Noop 后置核验不匹配；没有执行Native mutator".into();
        store.save(&journal)?;
        return Ok(serde_json::json!(journal));
    }
    let commit_observation = observed.clone();
    let committed = store.commit_checked(&journal, observed, || {
        query::verify(&query)?;
        let registry = read_registry(store)?;
        let record = registry
            .contexts
            .get(&query.binding.context_id)
            .ok_or_else(|| Error::attention("brew_context_missing", "提交缺少原Brew context"))?;
        policy::accept_observation(record, &query, &commit_observation)?;
        let view = policy::checked_plan(&journal.plan)?
            .ok_or_else(|| Error::attention("invalid_brew_plan", "缺少Brew投影"))?;
        let current = policy::owner_snapshot(
            &query.key,
            &crate::inventory::declared_catalog(ctx)?,
            &store.receipts()?,
            &registry,
        )?;
        if current != view.data.owner {
            return Err(Error::attention(
                "brew_owner_changed",
                "Brew owner在提交前变化",
            ));
        }
        Ok(())
    });
    if let Err(failure) = committed {
        let error = match failure {
            crate::store::CommitFailure::Check(e) | crate::store::CommitFailure::Outcome(e) => e,
        };
        journal.phase = crate::domain::Phase::Unknown;
        journal.message = format!("Brew 状态提交未确认：{}；未执行Native mutator", error.code);
        store.save(&journal)?;
        return Ok(serde_json::json!(journal));
    }
    journal.phase = crate::domain::Phase::Succeeded;
    journal.message = "Brew 当前状态已按原scope核验；未运行Native mutator".into();
    store.save(&journal)?;
    Ok(serde_json::json!(journal))
}

fn current_context(
    ctx: &Context,
    store: &Store,
    original: &BrewQuerySpec,
    doctor: bool,
    requested: crate::domain::Action,
    yes: bool,
) -> Result<(BrewContextRegistry, bool)> {
    let mut registry = read_registry(store)?;
    let new_context = !registry.contexts.contains_key(&original.binding.context_id);
    let first_use = registry
        .contexts
        .get(&original.binding.context_id)
        .is_none_or(|record| !record.writer_scope_confirmed);
    if new_context {
        if doctor {
            return Err(Error::attention(
                "brew_context_unregistered",
                "此具体Brew context尚未明确确认，未运行Native查询",
            ));
        }
        confirm_scope(
            original,
            "首次信任此具体Native程序及查询范围；将重新核验Native实际安装身份",
            yes,
        )?;
        register_context(ctx, store, original.binding.clone(), BTreeMap::new())?;
        registry = read_registry(store)?;
    }
    let record = registry
        .contexts
        .get(&original.binding.context_id)
        .ok_or_else(|| Error::attention("brew_context_unregistered", "没有对应Brew context"))?;
    if record.epoch != original.context_epoch || record.binding != original.binding {
        if doctor || requested != crate::domain::Action::Adopt {
            return Err(Error::attention(
                "brew_query_stale",
                "原绑定已变化，必须显式rebind，未重指向旧查询",
            ));
        }
        confirm_scope(
            original,
            "显式重绑定此已有context，逻辑owner和旧操作历史保留",
            yes,
        )?;
        rebind_context(ctx, store, original.binding.clone(), false)?;
        registry = read_registry(store)?;
    }
    Ok((registry, first_use))
}
fn candidate(ctx: &Context, id: &str) -> Result<Option<crate::domain::Package>> {
    if [
        "config:",
        "capability:",
        "skill:",
        "plugin:",
        "ai:",
        "service:",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix))
        || std::path::Path::new(id).is_absolute()
    {
        return Ok(None);
    }
    let mut catalog = crate::inventory::declared_catalog(ctx)?;
    if !catalog.packages.iter().any(|p| p.id == id || p.name == id)
        && id.starts_with("application:")
        && let Some(receipt) = crate::target::receipts(ctx)?.installations.get(id)
        && matches!(
            receipt.package.provider,
            crate::domain::Provider::BrewFormula { .. }
        )
    {
        catalog.packages.push(receipt.package.clone());
    }
    Ok(catalog.select(id).ok().cloned())
}
fn unbound_formula(
    dry_run: bool,
    package: &crate::domain::Package,
) -> Result<Option<serde_json::Value>> {
    if matches!(
        package.provider,
        crate::domain::Provider::Brew { cask: false, .. }
    ) {
        if dry_run {
            return Ok(Some(
                serde_json::json!({"mode":"dry-run","backend":"brew-native","id":package.id,"target":null,"unresolved":true,"reason":"brew_context_binding_required","nativeRuns":0,"persistentWrites":false}),
            ));
        }
        return Err(Error::attention(
            "brew_context_binding_required",
            "此旧声明没有可验证的program/prefix/Cellar和exact full_name绑定；需核对明确context，未查询或接管",
        ));
    }
    Ok(None)
}
fn confirm_action(view: &policy::BrewPlan<'_>, yes: bool) -> Result<()> {
    let versions: Vec<_> = report_of(&view.plan.before)?
        .rack
        .kegs
        .iter()
        .filter(|k| k.tap == view.query.selection.tap)
        .map(|k| &k.version)
        .collect();
    confirm_scope(
        view.query,
        &format!(
            "确认已核验的Native作用范围：action={:?} execution={:?} target={:?} selected_versions={versions:?}",
            view.plan.action, view.data.execution, view.plan.target
        ),
        yes,
    )
}
fn record_writer_scope(ctx: &Context, store: &Store, view: &policy::BrewPlan<'_>) -> Result<()> {
    if view.data.execution != crate::domain::BrewExecution::NativeMutator
        || view.data.confirmation.consent == crate::domain::BrewConsent::ExistingContext
    {
        return Err(Error::attention(
            "brew_writer_scope_unconfirmed",
            "仅明确确认的NativeMutator可记录写范围授权",
        ));
    }
    binding_scope(ctx, store, &view.query.binding)?;
    query::verify(view.query)?;
    let mut registry = read_registry(store)?;
    let current = registry
        .contexts
        .get(&view.query.binding.context_id)
        .ok_or_else(native_invalid)?;
    let mut confirmed = policy::accept_observation(current, view.query, &view.plan.before)?;
    confirmed.writer_scope_confirmed = true;
    registry
        .contexts
        .insert(view.query.binding.context_id.clone(), confirmed);
    persist(store, &registry)
}
fn check_version_request(command: &crate::cli::Command) -> Result<()> {
    if matches!(
        command,
        crate::cli::Command::Install {
            version: Some(_),
            ..
        } | crate::cli::Command::Upgrade {
            version: Some(_),
            ..
        }
    ) {
        return Err(Error::attention(
            "brew_version_unsupported",
            "Brew 不接受任意历史版本，请使用明确受审配方",
        ));
    }
    Ok(())
}
pub fn public_command(ctx: &Context, cli: &crate::cli::Cli) -> Result<Option<serde_json::Value>> {
    route_command(ctx, &cli.command, cli.dry_run, cli.yes, None)
}

pub(crate) fn execute_confirmed(
    ctx: &Context,
    selected: &crate::domain::Package,
    action: crate::domain::Action,
) -> Result<serde_json::Value> {
    use crate::{cli::Command, domain::Action};
    let id = selected.id.clone();
    let command = match action {
        Action::Install => Command::Install { id, version: None },
        Action::Upgrade => Command::Upgrade { id, version: None },
        Action::Uninstall => Command::Uninstall { id },
        Action::Adopt => Command::Adopt {
            id,
            bind_recipe: false,
        },
        _ => return Err(native_invalid()),
    };
    route_command(ctx, &command, false, false, Some(selected))?.ok_or_else(native_invalid)
}

fn selected_recipe(
    ctx: &Context,
    command: &crate::cli::Command,
    selected: Option<&crate::domain::Package>,
) -> Result<Option<(crate::domain::Package, bool)>> {
    let Some((id, doctor)) = targeted(command) else {
        return Ok(None);
    };
    if let Some(expected) = selected {
        let catalog = crate::inventory::declared_catalog(ctx)?;
        let requested = action(command);
        let receipts = if requested == crate::domain::Action::Install {
            crate::domain::Receipts::default()
        } else {
            crate::target::receipts(ctx)?
        };
        require_selected_recipe(&catalog, &receipts, expected, requested)?;
        return Ok(Some((expected.clone(), doctor)));
    }
    let Some(package) = candidate(ctx, id)? else {
        return Ok(None);
    };
    Ok(Some((package, doctor)))
}

fn require_selected_recipe(
    catalog: &crate::domain::Catalog,
    receipts: &crate::domain::Receipts,
    expected: &crate::domain::Package,
    action: crate::domain::Action,
) -> Result<()> {
    let stale = || Error::attention("plan_stale", "已选择的 Homebrew 配方发生变化");
    let receipt = if action != crate::domain::Action::Install
        && expected.id.starts_with("application:")
        && !catalog.packages.iter().any(|pkg| pkg.name == expected.id)
    {
        receipts.installations.get(&expected.id)
    } else {
        None
    };
    let current = catalog
        .packages
        .iter()
        .find(|pkg| pkg.id == expected.id)
        .or(receipt.map(|entry| &entry.package))
        .ok_or_else(stale)?;
    if crate::domain::fingerprint(current)? != crate::domain::fingerprint(expected)? {
        return Err(stale());
    }
    Ok(())
}

fn route_command(
    ctx: &Context,
    command: &crate::cli::Command,
    dry_run: bool,
    yes: bool,
    selected: Option<&crate::domain::Package>,
) -> Result<Option<serde_json::Value>> {
    use serde_json::json;
    let Some((package, doctor)) = selected_recipe(ctx, command, selected)? else {
        return Ok(None);
    };
    if let Some(value) = unbound_formula(dry_run, &package)? {
        return Ok(Some(value));
    }
    let crate::domain::Provider::BrewFormula {
        query: original, ..
    } = &package.provider
    else {
        return Ok(None);
    };
    check_version_request(command)?;
    if dry_run {
        return Ok(Some(
            json!({"mode":"dry-run","backend":"brew-native","id":package.id,"scope":{"program":original.binding.invocation,"prefix":original.key.canonical_prefix,"cellar":original.binding.cellar,"full_name":original.key.full_name},"target":null,"unresolved":true,"reason":"仅使用已登记/缓存证据；预演不运行 Native 查询","nativeRuns":0,"persistentWrites":false}),
        ));
    }
    let exists = match std::fs::symlink_metadata(ctx.state.join(REGISTRY)) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    if !exists && doctor {
        return Ok(Some(
            json!([{"id":package.id,"observation":{"status":"unknown","reason":"brew_context_unregistered"}}]),
        ));
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let (registry, first_use) =
        current_context(ctx, &store, original, doctor, action(command), yes)?;
    let record = registry
        .contexts
        .get(&original.binding.context_id)
        .ok_or_else(|| Error::attention("brew_context_unregistered", "没有对应Brew context"))?;
    let catalog = crate::inventory::declared_catalog(ctx)?;
    let receipts = store.receipts()?;
    let requested = action(command);
    if let Some(expected) = selected {
        require_selected_recipe(&catalog, &receipts, expected, requested)?;
    }
    let owner = policy::owner_snapshot(&original.key, &catalog, &receipts, &registry)?;
    let env = query::ConfirmedEnvironment::capture(ctx, record)?;
    let mut current = original.as_ref().clone();
    current.context_epoch = record.epoch;
    current.env_capture_id = env.capture_id().into();
    current.script = materialize_script(&store)?;
    let observed = query::observe(&store, &current, &env, ctx.timeout_secs)?;
    if matches!(
        observed,
        Observation::BrewInstalled { .. } | Observation::BrewMissing { .. }
    ) {
        record_observation(ctx, &store, &current, &observed)?;
    }
    if doctor {
        return Ok(Some(
            json!([{"id":package.id,"observation":observed,"nativeQuery":true}]),
        ));
    }
    if matches!(command, crate::cli::Command::Update { .. }) {
        let report = report_of(&observed)?;
        let info = policy::formula_identity(&current, report)?;
        return crate::updates::update_queried(ctx, &store, &package, &info.pkg_version).map(Some);
    }
    let mandatory_action_confirmation = selected.is_some()
        || matches!(
            requested,
            crate::domain::Action::Adopt | crate::domain::Action::Uninstall
        );
    let consent = if yes {
        crate::domain::BrewConsent::Yes
    } else if first_use || mandatory_action_confirmation {
        crate::domain::BrewConsent::Interactive
    } else {
        crate::domain::BrewConsent::ExistingContext
    };
    let plan = prepared_plan(ctx, &package, current, observed, owner, requested, consent)?;
    let view = policy::checked_plan(&plan)?
        .ok_or_else(|| Error::attention("invalid_brew_plan", "缺少Brew投影"))?;
    if mandatory_action_confirmation
        || (first_use && view.data.execution == crate::domain::BrewExecution::NativeMutator)
    {
        confirm_action(&view, yes)?;
        if view.data.execution == crate::domain::BrewExecution::NativeMutator {
            record_writer_scope(ctx, &store, &view)?;
        }
    }
    if view.data.execution != crate::domain::BrewExecution::NativeMutator {
        return finish_without_mutator(ctx, &store, plan, &env).map(Some);
    }
    execute_native(ctx, &store, plan, &env).map(Some)
}

pub(super) struct BrewDiscovery {
    pub binding: BrewContextBinding,
    pub book: crate::policy::resources::BrewBook,
}
pub(super) fn discovery_books(
    ctx: &Context,
) -> Result<(Vec<BrewDiscovery>, Vec<std::path::PathBuf>)> {
    use std::os::unix::process::CommandExt;
    match std::fs::symlink_metadata(ctx.state.join(REGISTRY)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((vec![], vec![])),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    let directory = crate::paths::Directory::open(&ctx.state)?;
    let state = crate::capability_store::binding(&directory, &ctx.state)?;
    let registry = read_registry_at(directory.file())?;
    let mut results = vec![];
    let mut unverified = vec![];
    for record in registry
        .contexts
        .values()
        .filter(|r| r.binding.metadata_scope.home == ctx.home)
    {
        if !record.observation_valid {
            unverified.push(record.binding.prefix.clone());
            continue;
        }
        let env = query::ConfirmedEnvironment::capture(ctx, record)?;
        let guard = || {
            let root = original_state(&state)?;
            let current = read_registry_at(root.file())?;
            if current.contexts.get(&record.binding.context_id) != Some(record) {
                return Err(native_invalid());
            }
            query::verify_context(&record.binding)
        };
        guard()?;
        let mut command = std::process::Command::new(&record.binding.invocation);
        command
            .args(["info", "--json=v2", "--installed"])
            .env_clear()
            .envs(env.values())
            .current_dir(&ctx.home)
            .stdin(std::process::Stdio::null())
            .process_group(0);
        let captured = crate::process::capture_bytes_guarded_process_in(
            &mut command,
            std::time::Duration::from_secs(ctx.timeout_secs.min(60)),
            4 * 1024 * 1024,
            &directory.path,
            guard,
        );
        let output = query::owned_output(captured)?;
        if output.uncertain || output.code != Some(0) {
            return Err(Error::attention(
                "brew_query_failed",
                "已绑定Brew发现查询未确认",
            ));
        }
        let book = crate::policy::resources::brew_book(&output.stdout)?;
        results.push(BrewDiscovery {
            binding: record.binding.clone(),
            book,
        });
    }
    Ok((results, unverified))
}
pub fn discover(ctx: &Context) -> Result<serde_json::Value> {
    let (packages, coverage) = crate::resource_software::brew(ctx)?;
    let unresolved = !matches!(
        coverage.state,
        crate::resource::CoverageState::Complete | crate::resource::CoverageState::Absent
    );
    Ok(
        serde_json::json!({"manager":"brew","packages":packages,"coverage":coverage,"adopted":false,"unresolved":unresolved}),
    )
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrewRunState {
    schema_version: u32,
    operation_id: String,
    plan_sha256: String,
    state: crate::policy::governance::DirectoryBinding,
    catalog: std::path::PathBuf,
    expected_loader_paths: [std::path::PathBuf; 3],
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrewAfterProof {
    operation_id: String,
    plan_sha256: String,
    invocation_sha256: String,
    outcome_sha256: String,
    observation: Observation,
}
fn native_invalid() -> Error {
    Error::attention(
        "brew_native_evidence_invalid",
        "Brew 原操作的范围或核验证据不匹配；不重跑Native",
    )
}
fn run_name(journal: &crate::domain::Journal, suffix: &str) -> String {
    format!("brew-native-{}-{suffix}.json", journal.id)
}
fn original_state(
    binding: &crate::policy::governance::DirectoryBinding,
) -> Result<crate::paths::Directory> {
    crate::capability_store::open(&crate::policy::capability_tx::Namespace {
        ancestor: binding.clone(),
        remaining: vec![".brew-native-query".into()],
    })
}
fn run_state(journal: &crate::domain::Journal) -> Result<BrewRunState> {
    crate::store::validate_journal(journal)?;
    policy::checked_plan(&journal.plan)?.ok_or_else(native_invalid)?;
    let root = crate::paths::Directory::open(&journal.state)?;
    let run: BrewRunState =
        Store::read(root.file(), &run_name(journal, "input"))?.ok_or_else(native_invalid)?;
    if run.schema_version != 2
        || run.operation_id != journal.id
        || run.plan_sha256 != journal.plan_sha256
        || run.state.requested != journal.state
        || !run.catalog.is_absolute()
        || run.expected_loader_paths
            != query::expected_loader_paths(
                &policy::checked_plan(&journal.plan)?
                    .ok_or_else(native_invalid)?
                    .query
                    .binding,
                std::path::Path::new("/etc/homebrew/brew.env"),
            )
    {
        return Err(native_invalid());
    }
    original_state(&run.state)?;
    Ok(run)
}
fn execution_owner(journal: &crate::domain::Journal, run: &BrewRunState) -> Result<()> {
    let view = policy::checked_plan(&journal.plan)?.ok_or_else(native_invalid)?;
    let root = original_state(&run.state)?;
    let registry = read_registry_at(root.file())?;
    let record = registry
        .contexts
        .get(&view.query.binding.context_id)
        .ok_or_else(native_invalid)?;
    policy::accept_observation(record, view.query, &journal.plan.before)?;
    if view.data.execution == crate::domain::BrewExecution::NativeMutator
        && !record.writer_scope_confirmed
    {
        return Err(Error::attention(
            "brew_writer_scope_unconfirmed",
            "原context未确认Native写作用范围；不启动或重跑",
        ));
    }
    if journal.home != view.query.binding.metadata_scope.home || journal.scope.home != journal.home
    {
        return Err(native_invalid());
    }
    let catalog = match std::fs::symlink_metadata(&run.catalog) {
        Ok(_) => {
            let value = crate::policy::json::parse(&crate::paths::regular_bytes(&run.catalog)?)?;
            let crate::policy::json::Object(catalog) = serde_json::from_value::<
                crate::policy::json::Object<crate::domain::Catalog>,
            >(value)?;
            catalog
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => crate::domain::Catalog {
            schema_version: 1,
            packages: vec![],
        },
        Err(e) => return Err(e.into()),
    };
    catalog.validate()?;
    let receipts = Store::read(root.file(), "receipts.json")?.unwrap_or(crate::domain::Receipts {
        schema_version: 1,
        installations: BTreeMap::new(),
    });
    crate::policy::state::receipt_schema(&receipts)?;
    if policy::owner_snapshot(&view.query.key, &catalog, &receipts, &registry)? != view.data.owner {
        return Err(Error::attention(
            "brew_owner_changed",
            "原Brew owner集合已经变化",
        ));
    }
    query::verify(view.query)
}
fn invocation_guard(
    journal: &crate::domain::Journal,
    invocation: &crate::native_runner::FrozenInvocation,
) -> Result<()> {
    let view = policy::checked_plan(&journal.plan)?.ok_or_else(native_invalid)?;
    let projection = policy::invocation_projection(&view)?;
    if invocation.command != projection.command
        || invocation.cwd != projection.cwd
        || invocation.timeout_secs != projection.timeout_secs
        || invocation.env.get("HOME")
            != view
                .query
                .binding
                .metadata_scope
                .home
                .to_str()
                .map(str::to_owned)
                .as_ref()
    {
        return Err(native_invalid());
    }
    Ok(())
}
fn report_of(observed: &Observation) -> Result<&crate::domain::BrewNativeReport> {
    match observed {
        Observation::BrewInstalled { evidence, .. } => Ok(&evidence.report),
        Observation::BrewMissing { evidence } => Ok(&evidence.report),
        _ => Err(native_invalid()),
    }
}
fn same_native_observation(before: &Observation, current: &Observation) -> Result<()> {
    let mut frozen = report_of(before)?.clone();
    let actual = report_of(current)?;
    frozen.query_id.clone_from(&actual.query_id);
    if frozen != *actual {
        return Err(Error::attention(
            "brew_before_changed",
            "Native当前安装与冻结计划不同",
        ));
    }
    Ok(())
}
pub fn worker_guard(
    journal: &crate::domain::Journal,
    invocation: &crate::native_runner::FrozenInvocation,
) -> Result<()> {
    let result = worker_before(journal, invocation);
    if let Err(error) = &result {
        let run = run_state(journal)?;
        let root = original_state(&run.state)?;
        Store::atomic_new(
            root.file(),
            &run_name(journal, "before-error"),
            &serde_json::json!({"operation_id":journal.id,"plan_sha256":journal.plan_sha256,"reason":error.code}),
        )?;
    }
    result
}
fn worker_before(
    journal: &crate::domain::Journal,
    invocation: &crate::native_runner::FrozenInvocation,
) -> Result<()> {
    invocation_guard(journal, invocation)?;
    let run = run_state(journal)?;
    execution_owner(journal, &run)?;
    let view = policy::checked_plan(&journal.plan)?.ok_or_else(native_invalid)?;
    let observed = query::observe_at(
        view.query,
        &invocation.env,
        invocation.timeout_secs,
        &run.state,
    )?;
    same_native_observation(&journal.plan.before, &observed)?;
    execution_owner(journal, &run)
}
pub fn worker_after(
    journal: &crate::domain::Journal,
    invocation: &crate::native_runner::FrozenInvocation,
    outcome: &crate::native_runner::NativeOutcome,
) -> Result<()> {
    invocation_guard(journal, invocation)?;
    let run = run_state(journal)?;
    execution_owner(journal, &run)?;
    let view = policy::checked_plan(&journal.plan)?.ok_or_else(native_invalid)?;
    let observation = match query::observe_at(
        view.query,
        &invocation.env,
        invocation.timeout_secs,
        &run.state,
    ) {
        Ok(value) => value,
        Err(error) => Observation::Unknown { reason: error.code },
    };
    let root = original_state(&run.state)?;
    let proof = BrewAfterProof {
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        invocation_sha256: crate::domain::fingerprint(invocation)?,
        outcome_sha256: crate::domain::fingerprint(outcome)?,
        observation,
    };
    Store::atomic_new(root.file(), &run_name(journal, "after"), &proof)?;
    execution_owner(journal, &run)
}
fn verify_report_files(observed: &Observation) -> Result<()> {
    let report = report_of(observed)?;
    if query::node(&std::fs::symlink_metadata(&report.cellar)?) != report.rack.cellar_after {
        return Err(native_invalid());
    }
    let rack = match std::fs::symlink_metadata(&report.rack.path) {
        Ok(meta) => Some(query::node(&meta)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if rack != report.rack.rack_after {
        return Err(native_invalid());
    }
    for keg in &report.rack.kegs {
        if query::file_stamp(&keg.path.join("INSTALL_RECEIPT.json"))? != keg.receipt {
            return Err(native_invalid());
        }
    }
    Ok(())
}
fn finish_native(
    ctx: &Context,
    store: &Store,
    journal: &mut crate::domain::Journal,
    binding: &crate::native_runner::RunBinding,
) -> Result<serde_json::Value> {
    use crate::{domain::Phase, native_runner::NativeOutcome};
    let outcome = crate::native_runner::observe(store, journal, binding)?;
    let result = (|| {
        if outcome
            != (NativeOutcome::Exited {
                code: Some(0),
                signal: None,
            })
        {
            return Err(native_invalid());
        }
        let run = run_state(journal)?;
        let proof: BrewAfterProof =
            Store::read(&store.root, &run_name(journal, "after"))?.ok_or_else(native_invalid)?;
        if proof.operation_id != journal.id
            || proof.plan_sha256 != journal.plan_sha256
            || proof.invocation_sha256 != crate::domain::fingerprint(&binding.invocation)?
            || proof.outcome_sha256 != crate::domain::fingerprint(&outcome)?
        {
            return Err(native_invalid());
        }
        let view = policy::checked_plan(&journal.plan)?.ok_or_else(native_invalid)?;
        if !policy::expected(&view, &proof.observation) {
            return Err(native_invalid());
        }
        store
            .commit_checked(journal, proof.observation.clone(), || {
                execution_owner(journal, &run)?;
                verify_report_files(&proof.observation)
            })
            .map_err(|e| match e {
                crate::store::CommitFailure::Check(e) | crate::store::CommitFailure::Outcome(e) => {
                    e
                }
            })?;
        Ok::<(), Error>(())
    })();
    journal.phase = if matches!(outcome, NativeOutcome::Running) {
        Phase::Running
    } else if result.is_ok() {
        Phase::Succeeded
    } else {
        Phase::Unknown
    };
    journal.message = match result {
        Ok(()) => "Brew Native原退出0，原范围后置证据与owner核验通过".into(),
        Err(e) => format!("Brew Native结果未确认：{}；未重跑Native", e.code),
    };
    store.save(journal)?;
    let _ = ctx;
    Ok(serde_json::json!({"journal":journal,"nativeOutcome":outcome}))
}
fn execute_native(
    ctx: &Context,
    store: &Store,
    plan: crate::domain::Plan,
    env: &query::ConfirmedEnvironment,
) -> Result<serde_json::Value> {
    let view = policy::checked_plan(&plan)?.ok_or_else(native_invalid)?;
    env.matches(view.query)?;
    let projection = policy::invocation_projection(&view)?;
    let invocation = crate::native_runner::FrozenInvocation {
        command: projection.command,
        env: env.values().clone(),
        cwd: projection.cwd,
        timeout_secs: projection.timeout_secs,
    };
    let mut journal = crate::operations::new_journal(ctx, plan)?;
    let directory = crate::paths::Directory::open(&store.path)?;
    let run = BrewRunState {
        schema_version: 2,
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        state: crate::capability_store::binding(&directory, &store.path)?,
        catalog: ctx.catalog.clone(),
        expected_loader_paths: query::expected_loader_paths(
            &policy::checked_plan(&journal.plan)?
                .ok_or_else(native_invalid)?
                .query
                .binding,
            std::path::Path::new("/etc/homebrew/brew.env"),
        ),
    };
    Store::atomic_new(&store.root, &run_name(&journal, "input"), &run)?;
    store.save(&journal)?;
    let binding = crate::native_runner::prepare(store, &journal, &invocation)?;
    let launched = crate::native_runner::launch(store, &mut journal, &binding);
    if let Err(error) = launched {
        journal.phase = crate::domain::Phase::Unknown;
        journal.message = format!("Brew Native启动或原证据未确认：{}；不重试", error.code);
        store.save(&journal)?;
        return Ok(serde_json::json!(journal));
    }
    finish_native(ctx, store, &mut journal, &binding)
}
pub fn recover(
    ctx: &Context,
    original: &crate::domain::Journal,
    dry_run: bool,
    resolve_failed: bool,
    yes: bool,
    expected: Option<&str>,
) -> Result<serde_json::Value> {
    if original.home != ctx.home || original.state != ctx.state || original.scope != ctx.scope {
        return Err(native_invalid());
    }
    if dry_run || !crate::policy::state::pending(original.phase) {
        return Ok(serde_json::json!({"journal":original,"persistentWrites":false}));
    }
    let store = match Store::lock(ctx) {
        Ok(store) => store,
        Err(error) if error.code == "operation_busy" => {
            return Ok(
                serde_json::json!({"journal":original,"status":"running_or_unknown","reason":error.code,"persistentWrites":false}),
            );
        }
        Err(error) => return Err(error),
    };
    store.check_migration_pending()?;
    let mut journal = crate::operations::locked_journal(&store, &original.id)?;
    crate::operations::verify_recovery_preview(expected, &journal)?;
    if journal.id != original.id
        || journal.plan_sha256 != original.plan_sha256
        || journal.scope != original.scope
        || journal.state != original.state
        || journal.cache != original.cache
        || journal.settings != original.settings
        || journal.started_at != original.started_at
    {
        return Err(native_invalid());
    }
    if !crate::policy::state::pending(journal.phase) {
        return Ok(serde_json::json!({"journal":journal,"persistentWrites":false}));
    }
    if let Some(terminal) = store.publish_existing_terminal(&journal)? {
        return Ok(serde_json::json!({"journal":terminal,"persistentWrites":true}));
    }
    if resolve_failed {
        crate::operations::confirm(
            "确认已人工核对原Brew操作与外部进程；只记录失败并解除阻断，不重跑、不回滚、不验收成功。",
            yes,
        )?;
        journal.phase = crate::domain::Phase::Failed;
        journal.message = "用户明确确认人工核对后记录失败；未运行Native或回滚".into();
        store.save(&journal)?;
        return Ok(serde_json::json!({"journal":journal,"persistentWrites":true}));
    }
    let binding = match crate::native_runner::binding(&store, &journal) {
        Ok(binding) => binding,
        Err(error) => {
            journal.phase = crate::domain::Phase::Unknown;
            journal.message = error.code;
            store.save(&journal)?;
            return Ok(serde_json::json!(journal));
        }
    };
    finish_native(ctx, &store, &mut journal, &binding)
}

#[cfg(test)]
#[path = "../tests/unit/brew_registry.rs"]
mod tests;

mod query {
    use crate::{
        context::Context,
        domain::{BrewContextRecord, BrewEvidenceStamp, BrewQuerySpec, Observation, fingerprint},
        error::{Error, Result},
        paths,
        policy::brew as policy,
        process,
        store::{self, Store},
    };
    use std::{
        collections::BTreeMap,
        io::Read,
        os::unix::{fs::MetadataExt, process::CommandExt},
        path::Path,
        process::{Command, Stdio},
        time::Duration,
    };

    pub struct ConfirmedEnvironment {
        capture_id: String,
        context_id: String,
        epoch: u64,
        binding_fingerprint: String,
        values: BTreeMap<String, String>,
    }
    impl std::fmt::Debug for ConfirmedEnvironment {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("ConfirmedEnvironment")
                .field("capture_id", &self.capture_id)
                .field("context_id", &self.context_id)
                .field("epoch", &self.epoch)
                .field("env_keys", &self.values.keys().collect::<Vec<_>>())
                .finish_non_exhaustive()
        }
    }
    impl ConfirmedEnvironment {
        pub fn capture(ctx: &Context, record: &BrewContextRecord) -> Result<Self> {
            Self::from_values(ctx, record, std::env::vars_os())
        }
        pub(super) fn from_values(
            ctx: &Context,
            record: &BrewContextRecord,
            values: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
        ) -> Result<Self> {
            if ctx.home != record.binding.metadata_scope.home {
                return Err(changed());
            }
            policy::validate_binding(&record.binding)?;
            let mut env = BTreeMap::new();
            for (key, value) in values {
                let key = key.into_string().map_err(|_| changed())?;
                let value = value.into_string().map_err(|_| changed())?;
                env.insert(key, value);
            }
            env.insert("HOME".into(), ctx.home.to_str().ok_or_else(changed)?.into());
            for (key, value) in &ctx.scope.paths {
                if let Some(value) = value {
                    env.insert(key.clone(), value.to_str().ok_or_else(changed)?.into());
                } else {
                    env.remove(key);
                }
            }
            for (key, expected) in [
                (
                    "XDG_CONFIG_HOME",
                    &record.binding.metadata_scope.xdg_config_home,
                ),
                (
                    "HOMEBREW_XDG_CONFIG_HOME",
                    &record.binding.metadata_scope.homebrew_xdg_config_home,
                ),
            ] {
                let actual = env
                    .get(key)
                    .filter(|value| !value.is_empty())
                    .map(std::path::PathBuf::from);
                if &actual != expected {
                    return Err(changed());
                }
            }
            for key in [
                "HOMEBREW_NO_AUTO_UPDATE",
                "HOMEBREW_NO_INSTALL_CLEANUP",
                "HOMEBREW_NO_AUTOREMOVE",
                "HOMEBREW_NO_BOOTSNAP",
            ] {
                env.insert(key.into(), "1".into());
            }
            Ok(Self {
                capture_id: format!("ENV-{}", store::operation_id()),
                context_id: record.binding.context_id.clone(),
                epoch: record.epoch,
                binding_fingerprint: fingerprint(&record.binding)?,
                values: env,
            })
        }
        pub fn capture_id(&self) -> &str {
            &self.capture_id
        }
        pub fn values(&self) -> &BTreeMap<String, String> {
            &self.values
        }
        pub fn matches(&self, query: &BrewQuerySpec) -> Result<()> {
            if self.capture_id != query.env_capture_id
                || self.context_id != query.binding.context_id
                || self.epoch != query.context_epoch
                || self.binding_fingerprint != fingerprint(&query.binding)?
            {
                return Err(changed());
            }
            Ok(())
        }
    }
    fn changed() -> Error {
        Error::attention(
            "brew_query_scope_changed",
            "Brew 查询的冻结环境、程序、根或配置已经变化",
        )
    }
    pub(super) fn node(meta: &std::fs::Metadata) -> crate::domain::BrewNodeStamp {
        crate::domain::BrewNodeStamp {
            identity: crate::domain::FileId {
                device: meta.dev(),
                inode: meta.ino(),
            },
            mode: meta.mode(),
            size: meta.len(),
            modified_ns: u64::try_from(
                i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
            )
            .unwrap_or(0),
        }
    }
    pub(super) fn file_stamp(path: &Path) -> Result<crate::domain::BrewFileStamp> {
        let canonical = paths::read_path(path)?;
        let mut file = std::fs::File::open(&canonical)?;
        let before = file.metadata()?;
        if !before.is_file() || before.len() > 256 * 1024 * 1024 {
            return Err(changed());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if node(&before) != node(&file.metadata()?) {
            return Err(changed());
        }
        Ok(crate::domain::BrewFileStamp {
            node: node(&before),
            sha256: crate::policy::cache::digest(&bytes),
        })
    }
    pub fn verify(query: &BrewQuerySpec) -> Result<()> {
        verify_scoped(query, Path::new("/etc/homebrew/brew.env"))
    }
    pub(super) fn verify_scoped(query: &BrewQuerySpec, system_slot: &Path) -> Result<()> {
        policy::validate_query(query)?;
        verify_context_scoped(&query.binding, system_slot)?;
        let script = file_stamp(&query.script.absolute_path)?;
        if paths::read_path(&query.script.absolute_path)? != query.script.canonical_path
            || script.node.identity != query.script.file_id
            || script.node.mode & 0o777 != 0o400
            || script.node.size != query.script.size
            || script.sha256 != query.script.sha256
        {
            return Err(changed());
        }
        Ok(())
    }
    pub(super) fn verify_context(binding: &crate::domain::BrewContextBinding) -> Result<()> {
        verify_context_scoped(binding, Path::new("/etc/homebrew/brew.env"))
    }
    fn verify_context_scoped(
        binding: &crate::domain::BrewContextBinding,
        system_slot: &Path,
    ) -> Result<()> {
        policy::validate_binding(binding)?;
        verify_loader_metadata(binding, system_slot)?;
        if paths::read_path(&binding.invocation)? != binding.canonical_program
            || file_stamp(&binding.canonical_program)? != binding.program_stamp
        {
            return Err(changed());
        }
        if paths::Directory::open(&binding.prefix)?.identity() != &binding.prefix_id
            || paths::Directory::open(&binding.cellar)?.identity() != &binding.cellar_id
        {
            return Err(changed());
        }
        for config in &binding.metadata_scope.config_files {
            if let crate::domain::BrewConfigStamp::Present { path, stamp } = config
                && file_stamp(path)? != *stamp
            {
                return Err(changed());
            }
        }
        Ok(())
    }
    pub(super) fn expected_loader_paths(
        binding: &crate::domain::BrewContextBinding,
        system_slot: &Path,
    ) -> [std::path::PathBuf; 3] {
        [
            system_slot.into(),
            binding.prefix.join("etc/homebrew/brew.env"),
            binding
                .metadata_scope
                .actual_user_config_home
                .join("brew.env"),
        ]
    }
    fn loader_changed() -> Error {
        Error::attention(
            "brew_loader_slot_mismatch",
            "Brew环境stamp未覆盖固定loader的完整实际路径/状态",
        )
    }
    fn nearest_parent(path: &Path) -> Result<paths::Directory> {
        let mut parent = path.parent().ok_or_else(loader_changed)?;
        loop {
            match std::fs::symlink_metadata(parent) {
                Ok(meta) if meta.is_dir() => return paths::Directory::open(parent),
                Ok(_) => return Err(loader_changed()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    parent = parent.parent().ok_or_else(loader_changed)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    fn verify_loader_metadata(
        binding: &crate::domain::BrewContextBinding,
        system_slot: &Path,
    ) -> Result<()> {
        let expected = expected_loader_paths(binding, system_slot);
        let mut seen = std::collections::BTreeSet::new();
        if paths::read_path(&binding.prefix)? != binding.prefix {
            return Err(loader_changed());
        }
        for (expected, stamp) in expected.iter().zip(&binding.metadata_scope.config_files) {
            let physical = paths::read_path(expected)?;
            let declared = match stamp {
                crate::domain::BrewConfigStamp::Absent { path, .. }
                | crate::domain::BrewConfigStamp::Present { path, .. } => path,
            };
            if paths::read_path(declared)? != physical || !seen.insert(physical.clone()) {
                return Err(loader_changed());
            }
            let metadata = match std::fs::symlink_metadata(&physical) {
                Ok(meta) => Some(meta),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            };
            match (stamp, metadata) {
                (crate::domain::BrewConfigStamp::Absent { anchor, parent, .. }, None) => {
                    let nearest = nearest_parent(&physical)?;
                    let meta = nearest.file().metadata()?;
                    if paths::read_path(anchor)? != nearest.path
                        || nearest.identity() != &parent.identity
                        || meta.mode() != parent.mode
                    {
                        return Err(loader_changed());
                    }
                }
                (crate::domain::BrewConfigStamp::Present { stamp, .. }, Some(meta))
                    if meta.is_file() && node(&meta) == stamp.node => {}
                _ => return Err(loader_changed()),
            }
        }
        Ok(())
    }
    pub(super) fn owned_output(
        captured: process::CapturedProcess,
    ) -> Result<process::BinaryOutput> {
        if let Some(pid) = captured.pid
            && process::group_alive(pid)
        {
            while process::group_alive(pid) {
                std::thread::sleep(Duration::from_millis(15));
            }
            if captured.result.is_ok() {
                return Err(Error::attention(
                    "brew_query_group_not_quiescent",
                    "原查询组仍有子进程，未接受查询结果",
                ));
            }
        }
        captured.result
    }
    pub fn observe(
        store: &Store,
        query: &BrewQuerySpec,
        env: &ConfirmedEnvironment,
        timeout_secs: u64,
    ) -> Result<Observation> {
        env.matches(query)?;
        store.verify_namespace()?;
        let directory = paths::Directory::open(&store.path)?;
        let state = crate::capability_store::binding(&directory, &store.path)?;
        let result = observe_at(query, env.values(), timeout_secs, &state);
        store.verify_namespace()?;
        result
    }
    pub(super) fn observe_at(
        query: &BrewQuerySpec,
        env: &BTreeMap<String, String>,
        timeout_secs: u64,
        state: &crate::policy::governance::DirectoryBinding,
    ) -> Result<Observation> {
        let guard = || {
            let directory = super::original_state(state)?;
            verify(query)?;
            let registry = super::read_registry_at(directory.file())?;
            if !registry
                .contexts
                .get(&query.binding.context_id)
                .is_some_and(|r| r.epoch == query.context_epoch && r.binding == query.binding)
            {
                return Err(changed());
            }
            Ok(())
        };
        guard()?;
        let query_id = format!("QUERY-{}", store::operation_id());
        let mut command = Command::new(&query.binding.invocation);
        command
            .args(["ruby", "--"])
            .arg(&query.script.absolute_path)
            .args([
                "observe",
                &format!("{}/{}", query.selection.tap, query.selection.name),
                &query_id,
            ]);
        command
            .env_clear()
            .envs(env)
            .current_dir(&query.binding.metadata_scope.home)
            .stdin(Stdio::null())
            .process_group(0);
        let directory = super::original_state(state)?;
        let captured = process::capture_bytes_guarded_process_in(
            &mut command,
            Duration::from_secs(timeout_secs.min(60)),
            4 * 1024 * 1024,
            &directory.path,
            guard,
        );
        let output = match owned_output(captured) {
            Ok(output) => output,
            Err(error) => return Ok(Observation::Unknown { reason: error.code }),
        };
        if output.uncertain || output.code != Some(0) {
            return Ok(Observation::Unknown {
                reason: "brew_native_query_failed".into(),
            });
        }
        let report = policy::parse_report(&output.stdout)?;
        let stamp = BrewEvidenceStamp {
            context_id: query.binding.context_id.clone(),
            context_epoch: query.context_epoch,
            binding_fingerprint: fingerprint(&query.binding)?,
            env_capture_id: query.env_capture_id.clone(),
            query_id,
        };
        policy::observation(query, stamp, report)
    }
}

#[cfg(test)]
#[path = "../tests/unit/brew_query.rs"]
mod query_tests;
