use crate::{
    catalog::inventory,
    context::Context,
    domain::{
        Action, Journal, LocalNpmQuery, NpmAnchor, NpmBindingProof, Observation, Package, Phase,
        Plan, Provider, QuerySpec, Receipts, fingerprint,
    },
    error::{Error, Result},
    operations,
    operations::providers,
    operations::store::{CommitFailure, Store},
};
use serde_json::{Value, json};
use std::path::Path;

pub fn run(ctx: &Context, id: &str, dry_run: bool, yes: bool) -> Result<Value> {
    let plan = prepare_plan(ctx, id, &crate::catalog::target::receipts(ctx)?)?;
    let mut journal = operations::new_journal(ctx, plan)?;
    if dry_run {
        return Ok(json!({
            "mode":"dry-run","backend":"rust","plan":journal.plan,"persistentWrites":false
        }));
    }
    operations::confirm(
        &format!("将已采纳的 npm 实例 {id} 绑定到声明的配方。此操作只更新管理记录。"),
        yes,
    )?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    recheck(ctx, &store, &journal)?;
    store.save(&journal)?;
    commit(ctx, &store, &journal)?;
    journal.phase = Phase::Succeeded;
    journal.message = "已将 npm 实例绑定到显式配方".into();
    store.save(&journal)?;
    Ok(json!(journal))
}

fn recheck(ctx: &Context, store: &Store, journal: &Journal) -> Result<()> {
    let current = prepare_plan(ctx, &journal.plan.package.id, &store.receipts()?)?;
    if fingerprint(&current)? != journal.plan_sha256 {
        return Err(Error::attention(
            "plan_stale",
            "已确认的 npm 配方、收据或安装在绑定前变化",
        ));
    }
    Ok(())
}

fn commit(ctx: &Context, store: &Store, journal: &Journal) -> Result<()> {
    store
        .commit_checked(journal, journal.plan.before.clone(), || {
            recheck(ctx, store, journal)
        })
        .map_err(|failure| match failure {
            CommitFailure::Check(error) | CommitFailure::Outcome(error) => error,
        })
}

fn declared_recipe(ctx: &Context, id: &str) -> Result<(Package, QuerySpec)> {
    let catalog = inventory::declared_catalog(ctx)?;
    let recipe = catalog
        .packages
        .iter()
        .find(|package| package.id == id)
        .ok_or_else(recipe_required)?;
    let Provider::Npm {
        authority: Some(_), ..
    } = &recipe.provider
    else {
        return Err(recipe_required());
    };
    let query = providers::capture_query(ctx, recipe, Some(Action::Adopt))?;
    let native = local_query(&query)?;
    for other in &catalog.packages {
        if other.id != recipe.id && same_instance(native, other)? {
            return Err(Error::attention(
                "owner_conflict",
                "同一 npm 实例有多个 catalog ID，先核对配方归属",
            ));
        }
    }
    Ok((recipe.clone(), query))
}

fn local_query(query: &QuerySpec) -> Result<&LocalNpmQuery> {
    let QuerySpec::LocalNpm(local) = query else {
        return Err(recipe_required());
    };
    Ok(local)
}

fn same_instance(query: &LocalNpmQuery, package: &Package) -> Result<bool> {
    match &package.provider {
        Provider::Npm {
            package, prefix, ..
        }
        | Provider::NpmObserved {
            name: package,
            prefix,
        } => same_prefix(query, package, prefix),
        _ => match crate::policy::catalog::targets::descriptor(package)? {
            Some(legacy) => same_prefix(query, &legacy.name, &legacy.prefix),
            None => Ok(false),
        },
    }
}

fn same_prefix(query: &LocalNpmQuery, name: &str, prefix: &Path) -> Result<bool> {
    if name != query.name {
        return Ok(false);
    }
    let NpmAnchor::ExistingPrefix { identity, .. } = &query.namespace.anchor else {
        return Err(recipe_required());
    };
    let canonical = crate::platform::fs::read_path(prefix)?;
    let directory = match crate::platform::fs::Directory::open(&canonical) {
        Ok(directory) => directory,
        Err(error) if error.code == "path_missing" => return Ok(false),
        Err(error) => return Err(error),
    };
    directory.verify()?;
    Ok(directory.identity() == identity)
}

fn recipe_required() -> Error {
    Error::attention(
        "npm_binding_recipe_required",
        "请在所选 catalog 中为同一实例 ID 声明显式 npm 写入配方",
    )
}

fn prepare_plan(ctx: &Context, id: &str, receipts: &Receipts) -> Result<Plan> {
    let (package, query) = declared_recipe(ctx, id)?;
    let new_npm = local_query(&query)?;
    let old = receipts
        .installations
        .get(id)
        .filter(|receipt| matches!(receipt.package.provider, Provider::NpmObserved { .. }))
        .ok_or_else(|| {
            Error::attention(
                "npm_binding_observation_required",
                "请先用 adopt 采纳同一 npm 实例的只读基线",
            )
        })?;
    for (key, receipt) in &receipts.installations {
        if key != id && same_instance(new_npm, &receipt.package)? {
            return Err(Error::attention(
                "owner_conflict",
                "同一 npm 实例已由另一收据登记，保留原归属",
            ));
        }
    }
    let owner_mismatch = || {
        Error::attention(
            "manager_scope_changed",
            "npm 配方与只读收据的作用域、安装根身份或包名不一致",
        )
    };
    let Some(old_query @ QuerySpec::LocalNpm(old_npm)) = &old.query else {
        return Err(owner_mismatch());
    };
    if old.package.id != id
        || old.scope.as_ref() != Some(&ctx.scope)
        || old_npm.namespace != new_npm.namespace
        || old_npm.name != new_npm.name
    {
        return Err(owner_mismatch());
    }
    crate::catalog::target::check_owner(ctx, &package)?;
    let previous = providers::observe_query(&old.package, old_query, &ctx.scope, false)?;
    let before = providers::observe_query(&package, &query, &ctx.scope, false)?;
    let same_installation = matches!(
        (&previous, &before),
        (
            Observation::NativePackage { version: old_version, source_fingerprint, .. },
            Observation::Present { version, evidence: Some(evidence) }
        ) if old_version == version && source_fingerprint == &evidence.fingerprint
    );
    if previous != old.observation || !same_installation {
        return Err(Error::attention(
            "installation_drift",
            "npm 安装与已采纳基线不一致，须先核对安装",
        ));
    }
    Ok(Plan {
        brew: None,
        uv_owner: None,
        npm_binding: Some(NpmBindingProof {
            old_receipt_sha256: fingerprint(old)?,
        }),
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Adopt,
        target: before.version().map(str::to_owned),
        before,
        command: None,
        query: Some(query),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    })
}

pub fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    mark_failed: bool,
) -> Result<Value> {
    let unknown = || {
        Error::attention(
            "npm_binding_outcome_unknown",
            "无法证明 npm 绑定收据已提交或原收据仍完整，保留操作阻断",
        )
    };
    let proof = journal.plan.npm_binding.as_ref().ok_or_else(unknown)?;
    if !crate::policy::operation::state::pending(journal.phase) {
        return Ok(json!(journal));
    }
    if mark_failed {
        return Err(Error::attention(
            "mark_failed_unsupported",
            format!(
                "这次操作要靠安装记录判断结果，不能手动记为失败。\n运行 mo recover {} 检查那次的结果。请带上原来的目录选项。",
                journal.id
            ),
        ));
    }
    if journal.state != ctx.state || journal.home != ctx.home {
        return Err(unknown());
    }
    let receipts = store.receipts().map_err(|_| unknown())?;
    let receipt = receipts
        .installations
        .get(&journal.plan.package.id)
        .ok_or_else(unknown)?;
    if receipt.operation_id == journal.id
        && receipt.scope.as_ref() == Some(&journal.scope)
        && receipt.query == journal.plan.query
        && receipt.observation == journal.plan.before
        && fingerprint(&receipt.package)? == journal.plan.recipe_sha256
    {
        journal.phase = Phase::Succeeded;
        journal.message = "已核验本次 npm 配方绑定收据".into();
    } else if fingerprint(receipt)? == proof.old_receipt_sha256 {
        journal.phase = Phase::Failed;
        journal.message = "未提交 npm 配方绑定，原只读收据完整".into();
    } else {
        return Err(unknown());
    }
    store.save(journal)?;
    Ok(json!(journal))
}

#[cfg(test)]
#[path = "../../tests/unit/npm_adoption.rs"]
mod tests;
