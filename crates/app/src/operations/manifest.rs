use crate::{
    catalog::inventory,
    cli::Manifest as Command,
    context::Context,
    domain::{Action, Package, Plan, Receipt, fingerprint},
    error::{Error, Result},
    operations::{self, providers},
    platform::fs::Directory,
    policy::operation::manifest::{self as policy, Entry, Manifest},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

const FILE_LIMIT: u64 = 8 * 1024 * 1024;

struct Current {
    entries: BTreeMap<String, Entry>,
    replayable: BTreeSet<String>,
    selected_scope: String,
    skipped_capabilities: Vec<String>,
}

pub fn run(ctx: &Context, command: &Command, dry_run: bool, yes: bool) -> Result<Value> {
    match command {
        Command::Export => {
            let local = current(ctx)?;
            let manifest = Manifest {
                schema_version: 1,
                entries: local.entries.into_values().collect(),
            };
            policy::validate(&manifest)?;
            Ok(json!({
                "manifest":manifest,"source":"local-records",
                "capabilityClaimsToReview":local.skipped_capabilities,"persistentWrites":false
            }))
        }
        Command::Plan { file } => {
            let input = load(file)?;
            let local = current(ctx)?;
            Ok(json!({
                "entries":policy::plan(&input,&local.entries,&local.replayable,&local.selected_scope),
                "source":"local-records",
                "persistentWrites":false,
            }))
        }
        Command::Replay { file, id } => replay(ctx, &load(file)?, id, dry_run, yes),
    }
}

fn load(file: &Path) -> Result<Manifest> {
    let absolute = if file.is_absolute() {
        file.to_path_buf()
    } else {
        std::env::current_dir()?.join(file)
    };
    let bad_path = || Error::new("manifest_invalid", "需要一个可读取的普通清单文件");
    let parent = Directory::open(absolute.parent().ok_or_else(bad_path)?)?;
    let leaf = absolute.file_name().ok_or_else(bad_path)?;
    let (bytes, _) = parent.regular_config_snapshot(leaf, FILE_LIMIT)?;
    let crate::policy::json::Object(manifest) = serde_json::from_value::<
        crate::policy::json::Object<Manifest>,
    >(crate::policy::json::parse(&bytes)?)?;
    policy::validate(&manifest)?;
    Ok(manifest)
}

fn current(ctx: &Context) -> Result<Current> {
    let catalog = inventory::declared_catalog(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let mut current = BTreeMap::new();
    let mut replayable = BTreeSet::new();
    let scope = fingerprint(&ctx.scope)?;
    for package in &catalog.packages {
        if policy::supports_replay(&package.provider) {
            replayable.insert(package.id.clone());
        }
        let receipt = receipts.installations.get(&package.id);
        current.insert(package.id.clone(), entry(&scope, package, receipt)?);
    }
    for package in crate::config::chezmoi_native::cached_packages(ctx)? {
        if !current.contains_key(&package.id) {
            let receipt = receipts.installations.get(&package.id);
            current.insert(package.id.clone(), entry(&scope, &package, receipt)?);
        }
    }
    let (capabilities, skipped_capabilities) =
        crate::capability::capability_local::registered_packages(ctx)?;
    for package in capabilities {
        if !current.contains_key(&package.id) {
            let receipt = receipts.installations.get(&package.id);
            current.insert(package.id.clone(), entry(&scope, &package, receipt)?);
        }
    }
    for (id, receipt) in &receipts.installations {
        if id != &receipt.package.id {
            return Err(Error::attention(
                "manifest_receipt_mismatch",
                "收据键与内部身份不一致",
            ));
        }
        if !current.contains_key(id) {
            current.insert(id.clone(), entry(&scope, &receipt.package, Some(receipt))?);
        }
    }
    Ok(Current {
        entries: current,
        replayable,
        selected_scope: scope,
        skipped_capabilities,
    })
}

fn entry(selected_scope: &str, package: &Package, receipt: Option<&Receipt>) -> Result<Entry> {
    let recipe = fingerprint(package)?;
    let receipt = if let Some(receipt) = receipt {
        (fingerprint(&receipt.package)? == recipe).then_some(receipt)
    } else {
        None
    };
    let scope = match receipt {
        Some(found) => found.scope.as_ref().map(fingerprint).transpose()?,
        None => Some(selected_scope.to_owned()),
    };
    let version = receipt
        .and_then(|found| found.observation.version())
        .filter(|v| policy::valid_version(v))
        .map(str::to_owned);
    Ok(Entry {
        id: package.id.clone(),
        kind: package.kind,
        channel: policy::channel(&package.provider),
        recipe_sha256: recipe,
        scope_sha256: scope,
        version,
    })
}

fn replay(ctx: &Context, manifest: &Manifest, id: &str, dry_run: bool, yes: bool) -> Result<Value> {
    let intent = manifest
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| Error::attention("manifest_entry_missing", "所选 ID 不在清单中"))?;
    let scope = fingerprint(&ctx.scope)?;
    if intent.scope_sha256.as_ref() != Some(&scope) {
        return Err(Error::attention(
            "manifest_scope_changed",
            "清单缺少作用域证据，或当前执行作用域与期望不同",
        ));
    }
    if dry_run {
        check_pending_read_only(ctx)?;
    }
    let version = intent
        .version
        .as_deref()
        .ok_or_else(|| Error::attention("manifest_version_required", "重放需要明确的目标版本"))?;
    if !policy::concrete_npm_version(version) {
        return Err(Error::attention(
            "manifest_version_required",
            "需要一个明确 npm SemVer 版本，例如 1.2.3 或 1.2.3-beta.1",
        ));
    }
    let recovered = if dry_run {
        None
    } else {
        checked_package(ctx, intent)?;
        operations::recover_before_new_plan(ctx, yes, |journal| {
            Ok(journal.plan.recipe_sha256 == intent.recipe_sha256
                && [Action::Install, Action::Upgrade].iter().any(|action| {
                    operations::matches_intent(journal, *action, &intent.id, Some(version))
                }))
        })
        .map_err(|mut error| {
            if error.code == "recovery_required" {
                error.code = "operation_recovery_required".into();
            }
            error
        })?
    };
    if let Some(recovered) = &recovered
        && recovered.original.plan.recipe_sha256 == intent.recipe_sha256
        && [Action::Install, Action::Upgrade]
            .iter()
            .any(|action| recovered.matches(*action, &intent.id, Some(version)))
    {
        return Ok(recovered.result.clone());
    }
    let planned = replay_plan(ctx, intent, version)?;
    if let Some(plan) = planned {
        if !dry_run {
            operations::confirm(
                &format!("按清单重放 {} 的 npm 版本 {version}", intent.id),
                yes,
            )?;
        }
        return operations::execute_checked(ctx, plan, dry_run, true, || {
            checked_package(ctx, intent).map(|_| ())
        });
    }
    if !dry_run {
        let store = crate::operations::store::Store::lock(ctx)?;
        store.check_pending()?;
        if replay_plan(ctx, intent, version)?.is_some() {
            return Err(Error::attention(
                "manifest_replay_changed",
                "当前状态已变化，请重新预演",
            ));
        }
    }
    Ok(json!({
        "id":intent.id,"status":"unchanged","version":version,"persistentWrites":recovered.is_some()
    }))
}

fn checked_package(ctx: &Context, intent: &Entry) -> Result<Package> {
    let catalog = inventory::declared_catalog(ctx)?;
    let package = catalog
        .packages
        .into_iter()
        .find(|item| item.id == intent.id)
        .ok_or_else(|| Error::attention("manifest_recipe_required", "正式 catalog 没有所选配方"))?;
    if fingerprint(&package)? != intent.recipe_sha256
        || package.kind != intent.kind
        || policy::channel(&package.provider) != intent.channel
    {
        return Err(Error::attention(
            "manifest_recipe_changed",
            "正式配方与导出摘要不同",
        ));
    }
    if !policy::supports_replay(&package.provider) {
        return Err(Error::attention(
            "manifest_replay_unsupported",
            "当前重放范围需要带注册表授权的 npm 配方",
        ));
    }
    Ok(package)
}

fn replay_plan(ctx: &Context, intent: &Entry, version: &str) -> Result<Option<Plan>> {
    let package = checked_package(ctx, intent)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let existing = receipts.installations.get(&intent.id);
    if let Some(receipt) = existing {
        if receipt.scope.as_ref() != Some(&ctx.scope) {
            return Err(Error::attention(
                "manifest_scope_changed",
                "已有收据的作用域不同",
            ));
        }
        if receipt.observation.version() == Some(version) {
            crate::catalog::target::check_owner(ctx, &package)?;
            let query = providers::capture_query(ctx, &package, Some(Action::Upgrade))?;
            let native = providers::observe_query(&package, &query, &ctx.scope, false)?;
            crate::policy::operation::state::receipt_matches(
                &ctx.scope, &package, &native, &query, receipt,
            )?;
            return Ok(None);
        }
    }
    let action = if existing.is_some() {
        Action::Upgrade
    } else {
        Action::Install
    };
    operations::plan(ctx, &package, action, Some(version), true).map(Some)
}

fn check_pending_read_only(ctx: &Context) -> Result<()> {
    let safe = crate::platform::fs::write_path(&ctx.state)?;
    let state = match Directory::open(&safe) {
        Ok(state) => state,
        Err(error) if error.code == "path_missing" => return Ok(()),
        Err(error) => return Err(error),
    };
    let active = crate::operations::store::Store::read::<crate::operations::store::Journal>(
        state.file(),
        "active.json",
    )?;
    if let Some(journal) = active {
        crate::operations::store::validate_journal(&journal)?;
        if crate::policy::operation::state::pending(journal.phase) {
            return Err(Error::attention(
                "operation_recovery_required",
                "上一次操作的结果还没确认。运行 mo recover 查看待检查的操作。",
            ));
        }
    }
    state.verify()
}
