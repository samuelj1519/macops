pub(crate) mod migration_fs;
pub(crate) mod migration_scan;
pub(crate) mod migration_tx;
pub(crate) mod migration_writers;

use crate::{
    cli::Migrate,
    context::Context,
    error::{Error, Result},
    migration::migration_fs as fs,
    operations::store::{self, Store},
    policy::governance::{Dataset, Transfer},
};
use serde_json::{Value, json};

pub fn run(ctx: &Context, command: &Migrate, dry_run: bool, yes: bool) -> Result<Value> {
    if !matches!(command, Migrate::Verify) {
        let locations = crate::operations::governance::locations(ctx);
        fs::validate_new_roots(&locations, locations.roots())?;
    }
    match command {
        Migrate::Preflight => preflight(ctx),
        Migrate::Verify => verify(ctx),
        Migrate::Run if dry_run => preflight(ctx),
        Migrate::Run => execute(ctx, yes),
        Migrate::Recover { migration_id } => recover(ctx, migration_id, dry_run, yes),
    }
}

fn preflight(ctx: &Context) -> Result<Value> {
    if migration_tx::committed(ctx)?.is_some() {
        return verify(ctx);
    }
    if migration_tx::pending(ctx)? {
        return Err(migration_tx::recovery_required(ctx));
    }
    let mut value = migration_scan::capture(ctx, true)?.report();
    value["read_only"] = Value::Bool(true);
    Ok(value)
}

fn execute(ctx: &Context, yes: bool) -> Result<Value> {
    if migration_tx::committed(ctx)?.is_some() {
        return verify(ctx);
    }
    if migration_tx::pending(ctx)? {
        return Err(migration_tx::recovery_required(ctx));
    }
    let before = migration_scan::capture(ctx, true)?;
    before.require_ready()?;
    crate::operations::confirm(
        "将旧治理数据复制到新权威根；保留全部旧源与索引。提交后禁止继续使用旧写入口。",
        yes,
    )?;
    before.writers.as_ref().ok_or_else(fs::error)?.verify()?;
    let locations = crate::operations::governance::locations(ctx);
    fs::validate_new_roots(&locations, locations.roots())?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let current = migration_scan::capture(ctx, true)?;
    current.require_ready()?;
    let batch = migration_tx::prepare(ctx, current, &store::operation_id())?;
    if let Err(error) = batch.place().and_then(|()| batch.commit(ctx)) {
        if migration_tx::committed(ctx)?.is_some() {
            return Err(error);
        }
        if batch.rollback().is_err() {
            return Err(Error::attention(
                "migration_recovery_required",
                format!(
                    "迁移 {} 结果未知；保留落位与证据，先执行 migrate recover",
                    batch.id()
                ),
            ));
        }
        return Err(error);
    }
    Ok(
        json!({"phase":"succeeded","migration_id":batch.id(),"old_sources_retained":true,"authority":"migrated"}),
    )
}

pub fn verify(ctx: &Context) -> Result<Value> {
    let (governance, ledger) = crate::operations::governance::capture_committed(ctx)?;
    governance.ensure_settled()?;
    migration_scan::verify_committed_sources(ctx, &ledger)?;
    let cache_relocated = ctx.cache != ledger.layout.cache;
    let mut cache_miss = cache_relocated;
    for dataset in crate::policy::governance::DATASETS {
        if dataset == Dataset::ManagedUpdates && cache_relocated {
            continue;
        }
        let path = governance.paths.dataset(dataset);
        let checked = match fs::snapshot(&path) {
            Ok(Some((bytes, _))) => crate::policy::migration::validate_dataset(
                &ctx.home,
                dataset,
                &bytes,
                nix::unistd::geteuid().as_raw(),
            ),
            Ok(None) if matches!(ledger.converted.get(&dataset), Some(Transfer::Missing)) => Ok(()),
            Ok(None) => Err(fs::error()),
            Err(error) => Err(error),
        };
        if dataset == Dataset::ManagedUpdates {
            cache_miss |= checked.is_err();
        } else {
            checked?;
        }
    }
    governance.verify()?;
    migration_scan::verify_committed_sources(ctx, &ledger)?;
    Ok(
        json!({"phase":"succeeded","mode":"verify","migration_id":ledger.migration_id,"authority":"migrated","cache_miss":cache_miss,"old_sources_retained":true}),
    )
}

fn recover(ctx: &Context, id: &str, dry_run: bool, yes: bool) -> Result<Value> {
    if !crate::policy::state::valid_id(id) || id.len() > 128 {
        return Err(fs::error());
    }
    if !migration_tx::pending(ctx)? {
        if let Some(ledger) = migration_tx::committed(ctx)? {
            if ledger.migration_id != id {
                return Err(Error::attention(
                    "migration_not_found",
                    "该ID不是本次已提交迁移",
                ));
            }
            return verify(ctx);
        }
        return Ok(
            json!({"phase":"succeeded","mode":"recover","migration_id":id,"result":"not-needed"}),
        );
    }
    migration_tx::load(ctx, id)?;
    if dry_run {
        return Ok(json!({"mode":"dry-run","migration_id":id,"pending":true}));
    }
    crate::operations::confirm("仅核验并收口原迁移；不重跑安装器，不覆盖既有目标。", yes)?;
    let locations = crate::operations::governance::locations(ctx);
    fs::validate_new_roots(&locations, locations.roots())?;
    let _store = Store::lock(ctx)?;
    let batch = migration_tx::load(ctx, id)?;
    let committed = migration_tx::committed(ctx)?.is_some();
    if committed {
        batch.finish_committed(ctx)?;
    } else {
        batch.rollback()?;
    }
    Ok(
        json!({"phase":"succeeded","mode":"recover","migration_id":id,"result":if committed {"committed"}else{"rolled-back"}}),
    )
}
