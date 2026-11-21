use crate::{
    context::Context,
    domain::FileId,
    error::{Error, Result},
    migration_fs as fs, migration_tx,
    paths::{Bindings, BoundTarget, Directory, ObservedRegular},
    policy::governance::{
        self as policy, AuthorityPaths, CommittedMigration, Dataset, DirectoryBinding, Locations,
    },
};
use sha2::{Digest, Sha256};
use std::{os::unix::fs::MetadataExt, path::Path};

const LEDGER_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(PartialEq, Eq)]
struct LedgerStamp {
    identity: FileId,
    mode: u32,
    digest: String,
}

#[derive(PartialEq, Eq)]
struct PendingStamp {
    identity: FileId,
    kind: crate::paths::EntryKind,
    mode: u32,
}

pub struct Governance {
    state: std::path::PathBuf,
    pub paths: AuthorityPaths,
    ledger: BoundTarget,
    stamp: Option<LedgerStamp>,
    roots: Vec<(DirectoryBinding, Directory)>,
    pending: BoundTarget,
    pending_stamp: Option<PendingStamp>,
}

fn invalid() -> Error {
    Error::attention(
        "invalid_migration_ledger",
        "迁移权威账本不可用或持久根身份已改变",
    )
}

pub fn locations(ctx: &Context) -> Locations {
    Locations {
        home: ctx.home.clone(),
        settings: ctx.settings.clone(),
        config: ctx.config.clone(),
        data: ctx.data.clone(),
        state: ctx.state.clone(),
        cache: ctx.cache.clone(),
    }
}

fn stamp(observed: ObservedRegular) -> Result<Option<LedgerStamp>> {
    match observed {
        ObservedRegular::Missing => Ok(None),
        ObservedRegular::NonRegular(_) => Err(invalid()),
        ObservedRegular::Regular { bytes, metadata } => Ok(Some(LedgerStamp {
            identity: metadata.identity,
            mode: metadata.mode,
            digest: format!("{:x}", Sha256::digest(bytes)),
        })),
    }
}

fn pending_stamp(target: &BoundTarget) -> Result<Option<PendingStamp>> {
    Ok(target.metadata()?.map(|meta| PendingStamp {
        identity: meta.identity,
        kind: meta.kind,
        mode: meta.mode,
    }))
}

fn verify_root(binding: &DirectoryBinding) -> Result<Directory> {
    let root = Directory::open(&binding.requested).map_err(|_| invalid())?;
    let actual = root.file().metadata()?;
    if root.path != binding.canonical
        || root.identity() != &binding.identity
        || root.lineage()? != binding.lineage
        || actual.uid() != binding.uid
        || actual.uid() != nix::unistd::geteuid().as_raw()
        || actual.mode() & 0o7777 != 0o700
    {
        return Err(invalid());
    }
    root.verify().map_err(|_| invalid())?;
    Ok(root)
}

pub fn capture(ctx: &Context) -> Result<Governance> {
    if crate::migration_tx::pending(ctx)? {
        return Err(Error::attention(
            "migration_recovery_required",
            "一次性迁移尚未收口；先执行 migrate recover",
        ));
    }
    capture_snapshot(ctx, false).map(|(authority, _)| authority)
}

pub fn capture_committed(ctx: &Context) -> Result<(Governance, CommittedMigration)> {
    let (authority, ledger) = capture_snapshot(ctx, true)?;
    let ledger =
        ledger.ok_or_else(|| Error::attention("migration_not_committed", "尚无已提交迁移账本"))?;
    Ok((authority, ledger))
}

fn capture_snapshot(
    ctx: &Context,
    migration_verify: bool,
) -> Result<(Governance, Option<CommittedMigration>)> {
    let locations = locations(ctx);
    let scope = if migration_verify {
        fs::validate_new_roots(&locations, locations.roots())
    } else {
        Ok(())
    };
    let name = Path::new("migration-ledger.json");
    let mut bindings = Bindings::new(64);
    let ledger = if scope.is_err() {
        let root = fs::directory(&ctx.state)?;
        fs::private(&root)?;
        bindings.relative(&root, name)?
    } else {
        bindings
            .target(&ctx.state.join(name))
            .map_err(|_| invalid())?
    };
    let observed = ledger
        .observe_regular(LEDGER_LIMIT)
        .map_err(|_| invalid())?;
    let parsed = match &observed {
        ObservedRegular::Missing => None,
        ObservedRegular::NonRegular(_) => return Err(invalid()),
        ObservedRegular::Regular { bytes, .. } => Some(policy::parse(&locations, bytes)?),
    };
    let historic = parsed
        .as_ref()
        .is_some_and(|ledger| ledger.schema_version == policy::LEGACY_LEDGER_VERSION);
    if migration_verify && historic {
        fs::validate_legacy_sources(&locations)?;
    } else {
        scope?;
    }
    let roots = if let Some(parsed) = &parsed {
        if parsed.schema_version == policy::LEDGER_VERSION {
            if !migration_verify {
                fs::validate_new_roots(&locations, locations.roots()).map_err(|_| invalid())?;
            }
            fs::validate_ledger_links(&locations, parsed)?;
        }
        [
            &parsed.root_bindings.config,
            &parsed.root_bindings.data,
            &parsed.root_bindings.state,
        ]
        .into_iter()
        .map(|binding| verify_root(binding).map(|root| (binding.clone(), root)))
        .collect::<Result<Vec<_>>>()?
    } else {
        vec![]
    };
    if migration_verify && migration_tx::pending(ctx)? {
        return Err(migration_tx::recovery_required(ctx));
    }
    let paths = policy::select(&locations, parsed.as_ref())?;
    let pending = Bindings::new(64).target(&paths.pending)?;
    let pending_stamp = pending_stamp(&pending)?;
    let authority = Governance {
        state: ctx.state.clone(),
        paths,
        stamp: stamp(observed)?,
        ledger,
        roots,
        pending,
        pending_stamp,
    };
    authority.verify()?;
    Ok((authority, parsed))
}

impl Governance {
    pub fn ensure_settled(&self) -> Result<()> {
        if self.pending_stamp.is_some() {
            return Err(Error::attention(
                "capability_recovery_required",
                "能力包批次仍未收口；先核验 pending 与备份",
            ));
        }
        Ok(())
    }
    pub fn verify(&self) -> Result<()> {
        if crate::migration_tx::pending_in(&self.state)? {
            return Err(Error::attention(
                "migration_recovery_required",
                "一次性迁移在本次观察期间出现；先核验原迁移",
            ));
        }
        for (binding, root) in &self.roots {
            root.verify().map_err(|_| invalid())?;
            let actual = root.file().metadata()?;
            if root.lineage()? != binding.lineage
                || actual.uid() != binding.uid
                || actual.uid() != nix::unistd::geteuid().as_raw()
                || actual.mode() & 0o7777 != 0o700
            {
                return Err(invalid());
            }
        }
        let current = stamp(
            self.ledger
                .observe_regular(LEDGER_LIMIT)
                .map_err(|_| invalid())?,
        )?;
        if pending_stamp(&self.pending)? != self.pending_stamp {
            return Err(Error::attention(
                "governance_pending_changed",
                "能力包事务状态在查询期间改变",
            ));
        }
        if current != self.stamp {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn read(&self, dataset: Dataset, limit: u64) -> Result<Option<Vec<u8>>> {
        self.verify()?;
        self.ensure_settled()?;
        let target = Bindings::new(64).target(&self.paths.dataset(dataset))?;
        let value = match target.observe_regular(limit)? {
            ObservedRegular::Missing if self.paths.required.contains(&dataset) => {
                return Err(Error::attention(
                    "missing_governance_dataset",
                    "已转换的持久治理数据缺失",
                ));
            }
            ObservedRegular::Missing => None,
            ObservedRegular::NonRegular(_) => {
                return Err(Error::attention(
                    "invalid_governance_dataset",
                    "治理数据必须为普通文件",
                ));
            }
            ObservedRegular::Regular { bytes, .. } => Some(bytes),
        };
        self.verify()?;
        Ok(value)
    }
}

fn with_authority<T>(
    ctx: &Context,
    settled: bool,
    reader: impl FnOnce(&Governance) -> Result<T>,
) -> Result<T> {
    let authority = capture(ctx)?;
    if settled {
        authority.ensure_settled()?;
    }
    let result = reader(&authority);
    authority.verify()?;
    result
}

pub fn read<T>(ctx: &Context, reader: impl FnOnce(&Governance) -> Result<T>) -> Result<T> {
    with_authority(ctx, true, reader)
}

pub fn inspect<T>(ctx: &Context, reader: impl FnOnce(&Governance) -> Result<T>) -> Result<T> {
    with_authority(ctx, false, reader)
}

#[cfg(test)]
#[path = "../tests/unit/governance.rs"]
mod tests;
