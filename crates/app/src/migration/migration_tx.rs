use super::rules::{self, Placement};
use crate::{
    context::Context,
    domain::FileId,
    error::{Error, Result},
    migration::migration_fs as fs,
    migration::migration_scan::{self, Scan},
    migration::migration_writers::Fence,
    platform::fs::Directory,
    policy::governance::{
        self as policy, CommittedMigration, DirectoryBinding, DurableBindings, Layout,
        MigrationPhase,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

const MARKER: &str = "migration.pending.json";
const MANIFEST: &str = "placement-manifest.json";
const LEDGER_SLOT: &str = "migration-ledger.candidate";
const MARKER_SLOT: &str = "migration-marker.candidate";
const ACTIVE: &str = "migration-active.json";
const ACTIVE_SLOT: &str = "migration-active.candidate";
const MARKER_DONE: &str = "migration-marker.complete";
const ACTIVE_DONE: &str = "migration-active.complete";

#[derive(Debug, PartialEq, Eq)]
enum Position {
    Live,
    Staged,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    id: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    stage: DirectoryBinding,
    manifest_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    ledger: CommittedMigration,
    #[serde(deserialize_with = "crate::policy::json::object")]
    stage: DirectoryBinding,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    parents: Vec<DirectoryBinding>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    placements: Vec<Placement>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    ledger_identity: FileId,
    ledger_sha256: String,
    ledger_size: u64,
    #[serde(deserialize_with = "crate::policy::json::object")]
    marker_identity: FileId,
    #[serde(deserialize_with = "crate::policy::json::object")]
    active_identity: FileId,
}

#[derive(Debug)]
pub struct Batch {
    manifest: Manifest,
    pending: Pending,
    pending_identity: FileId,
    root: Directory,
    stage: Directory,
    parents: BTreeMap<PathBuf, Directory>,
    writers: Option<Fence>,
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn directories(
    ctx: &Context,
    roots: &[fs::RootWitness],
) -> Result<(Directory, Directory, Directory, Directory, Directory)> {
    let mut created = BTreeMap::new();
    let mut found = BTreeMap::new();
    for witness in roots {
        found.insert(witness.path.clone(), witness.create(&mut created)?);
    }
    let mut take = |path: &Path| found.remove(path).ok_or_else(fs::error);
    let config = take(&ctx.config)?;
    let data = take(&ctx.data)?;
    let state = take(&ctx.state)?;
    let cache = take(&ctx.cache)?;
    let manifests = take(&ctx.config.join("manifests"))?;
    if [&config, &state, &cache, &manifests]
        .iter()
        .any(|d| d.identity().device != data.identity().device)
    {
        return Err(Error::attention(
            "migration_cross_device",
            "本版迁移要求所选自有根位于同一文件系统",
        ));
    }
    Ok((config, data, state, cache, manifests))
}

fn make_ledger(
    ctx: &Context,
    id: &str,
    scan: &mut Scan,
    root_bindings: DurableBindings,
    cache: DirectoryBinding,
) -> CommittedMigration {
    CommittedMigration {
        schema_version: policy::LEDGER_VERSION,
        phase: MigrationPhase::Committed,
        migration_id: id.into(),
        home: ctx.home.clone(),
        legacy_settings: ctx.settings.clone(),
        layout: Layout {
            config: ctx.config.clone(),
            data: ctx.data.clone(),
            state: ctx.state.clone(),
            cache: ctx.cache.clone(),
        },
        root_bindings,
        cache_audit: Some(cache),
        converted: std::mem::take(&mut scan.converted),
        legacy_links: std::mem::take(&mut scan.links),
        archives: std::mem::take(&mut scan.archives),
        retained: std::mem::take(&mut scan.retained),
    }
}

pub fn prepare(ctx: &Context, scan: Scan, id: &str) -> Result<Batch> {
    prepare_with(ctx, scan, id, || Ok(()))
}

fn prepare_with(
    ctx: &Context,
    mut scan: Scan,
    id: &str,
    after_active: impl FnOnce() -> Result<()>,
) -> Result<Batch> {
    let writers = scan.take_writer_fence()?;
    let (config, data, root, cache, manifests) = directories(ctx, &scan.roots)?;
    let parent = fs::child_private(&data, OsStr::new("migrations"))?;
    if parent.metadata(OsStr::new(id))?.is_some() {
        return Err(fs::error());
    }
    rustix::fs::mkdirat(parent.file(), id, rustix::fs::Mode::RWXU).map_err(fs::os)?;
    parent.file().sync_all()?;
    let stage = parent.child(OsStr::new(id))?;
    fs::private(&stage)?;
    let bindings = DurableBindings {
        config: fs::binding(&config)?,
        data: fs::binding(&data)?,
        state: fs::binding(&root)?,
    };
    let ledger = make_ledger(ctx, id, &mut scan, bindings, fs::binding(&cache)?);
    policy::validate(&crate::operations::governance::locations(ctx), &ledger)?;
    let mut parents = BTreeMap::new();
    for dir in [root.clone(), cache, manifests] {
        parents.insert(dir.path.clone(), dir);
    }
    let mut placements = vec![];
    for input in scan.inputs {
        let slot = format!("{:?}.candidate", input.dataset);
        let identity = fs::write_new(&stage, &slot, &input.bytes)?;
        placements.push(Placement {
            dataset: input.dataset,
            target: input.dataset.target(&ledger.layout),
            slot,
            identity,
            sha256: input.proof.sha256,
            size: input.bytes.len() as u64,
        });
    }
    let ledger_bytes = json(&ledger)?;
    let ledger_identity = fs::write_new(&stage, LEDGER_SLOT, &ledger_bytes)?;
    let marker_identity = fs::write_new(&stage, MARKER_SLOT, &[])?;
    let active_identity = fs::write_new(&stage, ACTIVE_SLOT, &[])?;
    let manifest = Manifest {
        schema_version: 1,
        ledger,
        stage: fs::binding(&stage)?,
        parents: parents.values().map(fs::binding).collect::<Result<_>>()?,
        placements,
        ledger_identity,
        ledger_sha256: fs::hash(&ledger_bytes),
        ledger_size: ledger_bytes.len() as u64,
        marker_identity: marker_identity.clone(),
        active_identity: active_identity.clone(),
    };
    validate_manifest(&manifest)?;
    let mut frozen_locations = crate::operations::governance::locations(ctx);
    frozen_locations
        .cache
        .clone_from(&manifest.ledger.layout.cache);
    rules::validate_placements(&frozen_locations, &manifest.placements)?;
    let bytes = json(&manifest)?;
    fs::write_new(&stage, MANIFEST, &bytes)?;
    let pending = Pending {
        schema_version: 1,
        id: id.into(),
        stage: manifest.stage.clone(),
        manifest_sha256: fs::hash(&bytes),
    };
    fs::fill_own(&stage, MARKER_SLOT, &marker_identity, &json(&pending)?)?;
    fs::fill_own(&stage, ACTIVE_SLOT, &active_identity, &json(&pending)?)?;
    rustix::fs::renameat_with(
        stage.file(),
        ACTIVE_SLOT,
        root.file(),
        ACTIVE,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(fs::os)?;
    stage.file().sync_all()?;
    root.file().sync_all()?;
    after_active()?;
    rustix::fs::renameat_with(
        stage.file(),
        MARKER_SLOT,
        root.file(),
        MARKER,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(fs::os)?;
    stage.file().sync_all()?;
    root.file().sync_all()?;
    let pending_identity = marker_identity;
    let batch = Batch {
        manifest,
        pending,
        pending_identity,
        root,
        stage,
        parents,
        writers: Some(writers),
    };
    batch.guard()?;
    Ok(batch)
}

pub fn load(ctx: &Context, id: &str) -> Result<Batch> {
    if !crate::policy::operation::state::valid_id(id) {
        return Err(fs::error());
    }
    let root = fs::directory(&ctx.state)?;
    fs::private(&root)?;
    let (bytes, meta) = root.regular_config_snapshot(OsStr::new(ACTIVE), fs::LIMIT)?;
    let crate::policy::json::Object(pending): crate::policy::json::Object<Pending> =
        serde_json::from_value(crate::policy::json::parse(&bytes)?)?;
    if pending.schema_version != 1
        || pending.id != id
        || pending.stage.requested != ctx.data.join("migrations").join(id)
    {
        return Err(fs::error());
    }
    let stage = fs::verify_binding(&pending.stage)?;
    fs::private(&stage)?;
    let (bytes, _) = stage.regular_config_snapshot(OsStr::new(MANIFEST), fs::LIMIT)?;
    if fs::hash(&bytes) != pending.manifest_sha256 {
        return Err(fs::error());
    }
    let crate::policy::json::Object(manifest): crate::policy::json::Object<Manifest> =
        serde_json::from_value(crate::policy::json::parse(&bytes)?)
            .map_err(|_| policy::invalid())?;
    let locations = crate::operations::governance::locations(ctx);
    policy::validate(&locations, &manifest.ledger)?;
    fs::validate_ledger_links(&locations, &manifest.ledger)?;
    validate_manifest(&manifest)?;
    let mut frozen_locations = crate::operations::governance::locations(ctx);
    frozen_locations
        .cache
        .clone_from(&manifest.ledger.layout.cache);
    rules::validate_placements(&frozen_locations, &manifest.placements)?;
    if manifest.schema_version != 1
        || manifest.ledger.migration_id != id
        || manifest.stage != pending.stage
        || meta.identity != manifest.active_identity
    {
        return Err(fs::error());
    }
    let expected = [
        ctx.state.clone(),
        manifest.ledger.layout.cache.clone(),
        ctx.config.join("manifests"),
    ];
    if manifest.parents.len() != 3
        || !expected
            .iter()
            .all(|p| manifest.parents.iter().any(|b| b.requested == *p))
    {
        return Err(fs::error());
    }
    let committed = ledger_live(&manifest, &root)?;
    let parents = manifest
        .parents
        .iter()
        .filter(|b| !(b.requested == manifest.ledger.layout.cache && committed))
        .map(|b| fs::verify_binding(b).map(|d| (d.path.clone(), d)))
        .collect::<Result<_>>()?;
    let pending_identity = manifest.marker_identity.clone();
    let batch = Batch {
        manifest,
        pending,
        pending_identity,
        root,
        stage,
        parents,
        writers: None,
    };
    batch.guard()?;
    Ok(batch)
}

impl Batch {
    pub fn id(&self) -> &str {
        &self.pending.id
    }

    fn guard(&self) -> Result<()> {
        fs::verify_binding(&self.manifest.stage)?;
        let committed = ledger_live(&self.manifest, &self.root)?;
        for binding in &self.manifest.parents {
            if committed && binding.requested == self.manifest.ledger.layout.cache {
                continue;
            }
            fs::verify_binding(binding)?;
        }
        for binding in [
            &self.manifest.ledger.root_bindings.config,
            &self.manifest.ledger.root_bindings.data,
            &self.manifest.ledger.root_bindings.state,
        ] {
            fs::verify_binding(binding)?;
        }
        self.registry()?;
        let (manifest, _) = self
            .stage
            .regular_config_snapshot(OsStr::new(MANIFEST), fs::LIMIT)?;
        if fs::hash(&manifest) != self.pending.manifest_sha256 {
            return Err(fs::error());
        }
        self.verify_pairs()?;
        Ok(())
    }

    fn position(&self, live: &str, staged: &str, complete: &str, id: &FileId) -> Result<Position> {
        let bytes = json(&self.pending)?;
        let hash = fs::hash(&bytes);
        let size = bytes.len() as u64;
        let locations = [
            (&self.root, live, Position::Live),
            (&self.stage, staged, Position::Staged),
            (&self.stage, complete, Position::Complete),
        ];
        let mut found = None;
        for (dir, name, position) in locations {
            if dir.metadata(OsStr::new(name))?.is_some() {
                if found.is_some() || !fs::matches(dir, OsStr::new(name), id, &hash, size)? {
                    return Err(fs::error());
                }
                found = Some(position);
            }
        }
        found.ok_or_else(fs::error)
    }

    fn registry(&self) -> Result<()> {
        if self.position(
            ACTIVE,
            ACTIVE_SLOT,
            ACTIVE_DONE,
            &self.manifest.active_identity,
        )? != Position::Live
        {
            return Err(fs::error());
        }
        let marker = self.position(MARKER, MARKER_SLOT, MARKER_DONE, &self.pending_identity)?;
        if marker == Position::Staged {
            for item in &self.manifest.placements {
                if self
                    .parent(item)?
                    .metadata(item.target.file_name().ok_or_else(fs::error)?)?
                    .is_some()
                {
                    return Err(fs::error());
                }
            }
        }
        Ok(())
    }

    fn verify_pairs(&self) -> Result<()> {
        let committed = ledger_live(&self.manifest, &self.root)?;
        for item in &self.manifest.placements {
            if committed && item.dataset == policy::Dataset::ManagedUpdates {
                continue;
            }
            let parent = self.parent(item)?;
            let name = item.target.file_name().ok_or_else(fs::error)?;
            let staged = fs::matches(
                &self.stage,
                OsStr::new(&item.slot),
                &item.identity,
                &item.sha256,
                item.size,
            )?;
            let live = fs::matches(parent, name, &item.identity, &item.sha256, item.size)?;
            if !((staged && parent.metadata(name)?.is_none())
                || (live && self.stage.metadata(OsStr::new(&item.slot))?.is_none()))
            {
                return Err(fs::error());
            }
        }
        let staged = fs::matches(
            &self.stage,
            OsStr::new(LEDGER_SLOT),
            &self.manifest.ledger_identity,
            &self.manifest.ledger_sha256,
            self.manifest.ledger_size,
        )?;
        let live = fs::matches(
            &self.root,
            OsStr::new("migration-ledger.json"),
            &self.manifest.ledger_identity,
            &self.manifest.ledger_sha256,
            self.manifest.ledger_size,
        )?;
        if !((staged
            && self
                .root
                .metadata(OsStr::new("migration-ledger.json"))?
                .is_none())
            || (live && self.stage.metadata(OsStr::new(LEDGER_SLOT))?.is_none()))
        {
            return Err(fs::error());
        }
        Ok(())
    }

    fn parent(&self, item: &Placement) -> Result<&Directory> {
        item.target
            .parent()
            .and_then(|p| self.parents.get(p))
            .ok_or_else(fs::error)
    }

    fn publish(&self, item: &Placement) -> Result<()> {
        self.publish_with(item, || Ok(()))
    }

    fn publish_with(
        &self,
        item: &Placement,
        before_rename: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.guard()?;
        if self.position(MARKER, MARKER_SLOT, MARKER_DONE, &self.pending_identity)?
            != Position::Live
        {
            return Err(fs::error());
        }
        let parent = self.parent(item)?;
        let name = item.target.file_name().ok_or_else(fs::error)?;
        if !fs::matches(
            &self.stage,
            OsStr::new(&item.slot),
            &item.identity,
            &item.sha256,
            item.size,
        )? {
            return Err(fs::error());
        }
        before_rename()?;
        rustix::fs::renameat_with(
            self.stage.file(),
            item.slot.as_str(),
            parent.file(),
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(fs::os)?;
        self.stage.file().sync_all()?;
        parent.file().sync_all()?;
        if !fs::matches(parent, name, &item.identity, &item.sha256, item.size)? {
            return Err(fs::error());
        }
        self.guard()
    }

    pub fn place(&self) -> Result<()> {
        self.writer_fence()?.verify()?;
        for item in &self.manifest.placements {
            self.publish(item)?;
        }
        Ok(())
    }

    fn writer_fence(&self) -> Result<&Fence> {
        self.writers.as_ref().ok_or_else(fs::error)
    }

    fn all_placed(&self) -> Result<()> {
        self.guard()?;
        let committed = ledger_live(&self.manifest, &self.root)?;
        for item in &self.manifest.placements {
            if committed && item.dataset == policy::Dataset::ManagedUpdates {
                continue;
            }
            let name = item.target.file_name().ok_or_else(fs::error)?;
            if self.stage.metadata(OsStr::new(&item.slot))?.is_some()
                || !fs::matches(
                    self.parent(item)?,
                    name,
                    &item.identity,
                    &item.sha256,
                    item.size,
                )?
            {
                return Err(fs::error());
            }
        }
        Ok(())
    }

    pub fn commit(&self, ctx: &Context) -> Result<()> {
        self.publish_ledger(ctx)?;
        self.finish_committed(ctx)
    }

    fn publish_ledger(&self, ctx: &Context) -> Result<()> {
        self.all_placed()?;
        migration_scan::verify_sources(ctx, &self.manifest.ledger)?;
        self.guard()?;
        if !fs::matches(
            &self.stage,
            OsStr::new(LEDGER_SLOT),
            &self.manifest.ledger_identity,
            &self.manifest.ledger_sha256,
            self.manifest.ledger_size,
        )? {
            return Err(fs::error());
        }
        self.writer_fence()?.verify()?;
        rustix::fs::renameat_with(
            self.stage.file(),
            LEDGER_SLOT,
            self.root.file(),
            "migration-ledger.json",
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(fs::os)?;
        self.stage.file().sync_all()?;
        self.root.file().sync_all()?;
        Ok(())
    }

    pub fn finish_committed(&self, ctx: &Context) -> Result<()> {
        self.all_placed()?;
        if !fs::matches(
            &self.root,
            OsStr::new("migration-ledger.json"),
            &self.manifest.ledger_identity,
            &self.manifest.ledger_sha256,
            self.manifest.ledger_size,
        )? {
            return Err(fs::error());
        }
        migration_scan::verify_sources(ctx, &self.manifest.ledger)?;
        Fence::capture(&ctx.home)
            .map_err(|issue| issue.error())?
            .verify()?;
        self.clear()
    }

    fn clear(&self) -> Result<()> {
        self.clear_with(|| Ok(()))
    }

    fn clear_with(&self, after_marker: impl FnOnce() -> Result<()>) -> Result<()> {
        self.guard()?;
        self.complete_record(MARKER, MARKER_SLOT, MARKER_DONE, &self.pending_identity)?;
        after_marker()?;
        self.guard()?;
        self.complete_record(
            ACTIVE,
            ACTIVE_SLOT,
            ACTIVE_DONE,
            &self.manifest.active_identity,
        )?;
        fs::verify_binding(&self.manifest.stage)?;
        Ok(())
    }

    fn complete_record(&self, live: &str, staged: &str, complete: &str, id: &FileId) -> Result<()> {
        let position = self.position(live, staged, complete, id)?;
        if position == Position::Complete {
            return Ok(());
        }
        let (dir, name) = if position == Position::Live {
            (&self.root, live)
        } else {
            (&self.stage, staged)
        };
        rustix::fs::renameat_with(
            dir.file(),
            name,
            self.stage.file(),
            complete,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(fs::os)?;
        dir.file().sync_all()?;
        self.stage.file().sync_all()?;
        if self.position(live, staged, complete, id)? != Position::Complete {
            return Err(fs::error());
        }
        Ok(())
    }

    pub fn rollback(&self) -> Result<()> {
        self.guard()?;
        if self
            .root
            .metadata(OsStr::new("migration-ledger.json"))?
            .is_some()
        {
            return Err(fs::error());
        }
        for item in self.manifest.placements.iter().rev() {
            self.restore(item)?;
        }
        for item in &self.manifest.placements {
            if self
                .parent(item)?
                .metadata(item.target.file_name().ok_or_else(fs::error)?)?
                .is_some()
            {
                return Err(fs::error());
            }
        }
        self.clear()
    }

    fn restore(&self, item: &Placement) -> Result<()> {
        self.restore_with(item, || Ok(()))
    }

    fn restore_with(
        &self,
        item: &Placement,
        before_rename: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        self.guard()?;
        let parent = self.parent(item)?;
        let name = item.target.file_name().ok_or_else(fs::error)?;
        if fs::matches(
            &self.stage,
            OsStr::new(&item.slot),
            &item.identity,
            &item.sha256,
            item.size,
        )? {
            if parent.metadata(name)?.is_some() {
                return Err(fs::error());
            }
            return Ok(());
        }
        if self.stage.metadata(OsStr::new(&item.slot))?.is_some()
            || !fs::matches(parent, name, &item.identity, &item.sha256, item.size)?
        {
            return Err(fs::error());
        }
        before_rename()?;
        rustix::fs::renameat_with(
            parent.file(),
            name,
            self.stage.file(),
            item.slot.as_str(),
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(fs::os)?;
        parent.file().sync_all()?;
        self.stage.file().sync_all()?;
        if !fs::matches(
            &self.stage,
            OsStr::new(&item.slot),
            &item.identity,
            &item.sha256,
            item.size,
        )? {
            return Err(fs::error());
        }
        self.guard()
    }
}

fn ledger_live(manifest: &Manifest, root: &Directory) -> Result<bool> {
    fs::matches(
        root,
        OsStr::new("migration-ledger.json"),
        &manifest.ledger_identity,
        &manifest.ledger_sha256,
        manifest.ledger_size,
    )
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    let copies = manifest
        .ledger
        .converted
        .values()
        .filter(|v| matches!(v, policy::Transfer::Copied { .. }))
        .count();
    let ledger = json(&manifest.ledger)?;
    if copies != manifest.placements.len()
        || manifest.ledger_sha256 != fs::hash(&ledger)
        || manifest.ledger_size != ledger.len() as u64
    {
        return Err(fs::error());
    }
    for item in &manifest.placements {
        match manifest.ledger.converted.get(&item.dataset) {
            Some(policy::Transfer::Copied {
                target,
                target_sha256,
                target_mode,
                ..
            }) if target == &item.target
                && target_sha256 == &item.sha256
                && *target_mode == 0o600 => {}
            _ => return Err(fs::error()),
        }
    }
    Ok(())
}

pub fn pending(ctx: &Context) -> Result<bool> {
    pending_in(&ctx.state)
}

pub fn pending_in(state: &Path) -> Result<bool> {
    for name in [ACTIVE, MARKER] {
        if crate::platform::fs::Bindings::new(64)
            .target(&state.join(name))?
            .metadata()?
            .is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn recovery_required(ctx: &Context) -> Error {
    let message = if let Some(id) = pending_id(ctx) {
        format!("迁移 {id} 尚待核对。运行 mo migrate recover {id} 核对原迁移。")
    } else {
        "迁移尚待核对，但无法识别迁移编号。请检查原状态目录中的迁移记录。".into()
    };
    Error::attention("migration_recovery_required", message)
}

fn pending_id(ctx: &Context) -> Option<String> {
    let (bytes, _) = fs::snapshot(&ctx.state.join(ACTIVE)).ok().flatten()?;
    let value = crate::policy::json::parse(&bytes).ok()?;
    let crate::policy::json::Object(record) =
        serde_json::from_value::<crate::policy::json::Object<Pending>>(value).ok()?;
    (record.schema_version == 1
        && crate::policy::operation::state::valid_id(&record.id)
        && record.id.len() <= 128
        && record.stage.requested == ctx.data.join("migrations").join(&record.id))
    .then_some(record.id)
}

pub fn committed(ctx: &Context) -> Result<Option<CommittedMigration>> {
    fs::snapshot(&ctx.state.join("migration-ledger.json"))?
        .map(|(bytes, _)| policy::parse(&crate::operations::governance::locations(ctx), &bytes))
        .transpose()
}

#[cfg(test)]
#[path = "../../tests/unit/migration_tx.rs"]
mod tests;
