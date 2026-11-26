use super::rules::{self, Classification};
use crate::{
    context::Context,
    error::{Error, Result},
    migration::migration_fs as fs,
    migration::migration_writers::Fence,
    platform::fs::{Bindings, BoundTarget, Directory, EntryKind, Metadata},
    policy::governance::{
        self as policy, Archive, ArchiveKind, CommittedMigration, DATASETS, Dataset, FileProof,
        IndexedEntry, Layout, LegacyLink, RetainKind, Retained, RetainedEntry, RetainedShape,
        Transfer,
    },
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub struct Input {
    pub dataset: Dataset,
    pub bytes: Vec<u8>,
    pub proof: FileProof,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Issue {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Debug)]
pub struct Scan {
    pub roots: Vec<fs::RootWitness>,
    pub writers: Option<Fence>,
    pub inputs: Vec<Input>,
    pub converted: BTreeMap<Dataset, Transfer>,
    pub archives: Vec<Archive>,
    pub retained: Vec<Retained>,
    pub links: Vec<LegacyLink>,
    pub blocked: Vec<Issue>,
}

impl Scan {
    fn block(&mut self, path: &Path, reason: impl Into<String>) {
        self.blocked.push(Issue {
            path: path.into(),
            reason: reason.into(),
        });
    }
    pub fn require_ready(&self) -> Result<()> {
        if self.blocked.is_empty() {
            return Ok(());
        }
        Err(Error::attention(
            "migration_preflight_blocked",
            format!(
                "{} 项预检阻断；使用 migrate preflight --json 查看路径与原因",
                self.blocked.len()
            ),
        ))
    }

    pub fn take_writer_fence(&mut self) -> Result<Fence> {
        self.require_ready()?;
        let writers = self.writers.take().ok_or_else(fs::error)?;
        writers.verify()?;
        Ok(writers)
    }
    pub fn report(&self) -> Value {
        json!({"phase":if self.blocked.is_empty(){"succeeded"}else{"blocked"},"mode":"preflight", "ready":self.blocked.is_empty(),"incomplete":!self.blocked.is_empty(),"converted":self.converted,"archives":self.archives.iter().map(|a| json!({"kind":a.kind,"root":a.root,"entries":a.entries.len()})).collect::<Vec<_>>(),"retained":self.retained,"legacy_links":self.links,"blocked":self.blocked})
    }
}

pub fn capture(ctx: &Context, conflicts: bool) -> Result<Scan> {
    capture_with(ctx, conflicts, None)
}

fn capture_with(ctx: &Context, conflicts: bool, legacy_layout: Option<&Layout>) -> Result<Scan> {
    let p = crate::operations::governance::locations(ctx);
    if legacy_layout.is_some() {
        fs::validate_legacy_sources(&p)?;
    } else {
        fs::validate_new_roots(&p, p.roots())?;
    }
    let mut scan = Scan {
        roots: vec![],
        writers: None,
        inputs: vec![],
        converted: BTreeMap::new(),
        archives: vec![],
        retained: vec![],
        links: vec![],
        blocked: vec![],
    };
    if conflicts {
        match Fence::capture(&ctx.home) {
            Ok(writers) => scan.writers = Some(writers),
            Err(issue) => scan.block(&issue.0, issue.error().code),
        }
        check_placement_roots(&p)?;
        for path in [
            &ctx.config,
            &ctx.data,
            &ctx.state,
            &ctx.cache,
            &ctx.config.join("manifests"),
        ] {
            scan.roots.push(fs::RootWitness::capture(path)?);
        }
    }
    if let Some((bytes, _)) = fs::snapshot(&ctx.state.join("active.json"))? {
        match serde_json::from_slice::<crate::domain::Journal>(&bytes) {
            Ok(journal) if !crate::policy::operation::state::pending(journal.phase) => (),
            _ => scan.block(
                &ctx.state.join("active.json"),
                "migration_existing_operation_pending",
            ),
        }
    }
    for dataset in DATASETS {
        if !conflicts && dataset == Dataset::ManagedUpdates {
            scan.converted.insert(dataset, Transfer::Missing);
            continue;
        }
        if let Err(e) = dataset_input(ctx, &mut scan, dataset, conflicts) {
            scan.block(&dataset.source(&p), e.code);
        }
    }
    scan_children(ctx, &mut scan, &ctx.home.join(".agents"), &[])?;
    let historical_runtime = legacy_layout.map(Layout::roots);
    let skip = historical_runtime
        .as_ref()
        .map_or(&[][..], |roots| roots.as_slice());
    scan_children(ctx, &mut scan, &ctx.home.join(".local/state"), skip)?;
    for path in [
        &ctx.settings,
        &ctx.home.join(".local/share"),
        &ctx.home.join(".local/bin"),
    ] {
        classify_entry(ctx, &mut scan, path)?;
    }
    let home = fs::directory(&ctx.home)?;
    for name in fs::names(&home)? {
        let path = ctx.home.join(name);
        if matches!(
            rules::classify(&p, &path),
            Classification::RetainInPlace(policy::RetainKind::Harness)
        ) {
            classify_entry(ctx, &mut scan, &path)?;
        }
    }
    if scan.blocked.iter().any(|issue| {
        matches!(
            issue.reason.as_str(),
            "migration_existing_operation_pending"
                | "migration_old_writer_pending"
                | "migration_old_writer_active"
        )
    }) {
        scan.retained
            .retain(|entry| entry.owner != RetainKind::EmptyLegacyLockContainer);
    }
    scan.retained.sort_by(|a, b| a.path.cmp(&b.path));
    scan.retained.dedup_by(|a, b| a.path == b.path);
    Ok(scan)
}

fn check_placement_roots(p: &policy::Locations) -> Result<()> {
    let mut devices = std::collections::BTreeSet::new();
    for path in p.roots() {
        let target = Bindings::new(64).target(path)?;
        let device = if target.metadata()?.is_some() {
            let root = fs::directory(path)?;
            fs::private(&root)?;
            root.identity().device
        } else {
            target.parent_directory()?.identity().device
        };
        devices.insert(device);
    }
    if devices.len() != 1 {
        return Err(Error::attention(
            "migration_cross_device",
            "所选自有根跨文件系统，未执行任何迁移写入",
        ));
    }
    Ok(())
}

fn dataset_input(ctx: &Context, scan: &mut Scan, dataset: Dataset, conflicts: bool) -> Result<()> {
    let p = crate::operations::governance::locations(ctx);
    let layout = Layout {
        config: ctx.config.clone(),
        data: ctx.data.clone(),
        state: ctx.state.clone(),
        cache: ctx.cache.clone(),
    };
    let source = dataset.source(&p);
    let target = dataset.target(&layout);
    if conflicts && Bindings::new(64).target(&target)?.metadata()?.is_some() {
        scan.block(&target, "migration_target_exists");
    }
    match fs::snapshot(&source)? {
        None => {
            scan.converted.insert(dataset, Transfer::Missing);
        }
        Some((bytes, proof)) => {
            rules::validate_dataset(&ctx.home, dataset, &bytes, nix::unistd::geteuid().as_raw())?;
            scan.converted.insert(
                dataset,
                Transfer::Copied {
                    source,
                    target,
                    original: Box::new(proof.clone()),
                    target_sha256: proof.sha256.clone(),
                    target_mode: 0o600,
                },
            );
            scan.inputs.push(Input {
                dataset,
                bytes,
                proof,
            });
        }
    }
    Ok(())
}

fn scan_children(ctx: &Context, scan: &mut Scan, path: &Path, skip: &[&Path]) -> Result<()> {
    if Bindings::new(64).target(path)?.metadata()?.is_none() {
        return Ok(());
    }
    let dir = fs::directory(path)?;
    for name in fs::names(&dir)? {
        let child = path.join(name);
        if !skip.contains(&child.as_path()) {
            classify_entry(ctx, scan, &child)?;
        }
    }
    dir.verify()
}

fn classify_entry(ctx: &Context, scan: &mut Scan, path: &Path) -> Result<()> {
    let target = Bindings::new(64).target(path)?;
    let Some(metadata) = target.metadata()? else {
        return Ok(());
    };
    let locations = crate::operations::governance::locations(ctx);
    match rules::classify(&locations, path) {
        Classification::Convert(_) | Classification::Runtime | Classification::LegacyWriter => (),
        Classification::RetainInPlace(_) => {
            match retained_entry(&locations, path, &target, &metadata)? {
                Some(retained) => scan.retained.push(retained),
                None => scan.block(path, "migration_retained_entry_invalid"),
            }
        }
        Classification::Container if metadata.kind == EntryKind::Directory => {
            scan_children(ctx, scan, path, &[])?;
        }
        Classification::Container => scan.block(path, "migration_container_invalid"),
        Classification::ProfilesLink => {
            if let Err(e) = profile_link(ctx, scan, path) {
                scan.block(path, e.code);
            }
        }
        Classification::ArchiveIndexed(kind) => match archive(path, kind) {
            Ok(archive) => scan.archives.push(archive),
            Err(e) => scan.block(path, e.code),
        },
        Classification::Block => {
            let reason = if path == ctx.home.join(".agents/invocation-policy.pending.json") {
                "migration_old_writer_pending"
            } else {
                "migration_unknown_entry"
            };
            scan.block(path, reason);
        }
    }
    Ok(())
}

fn retained_entry(
    locations: &policy::Locations,
    path: &Path,
    target: &BoundTarget,
    metadata: &Metadata,
) -> Result<Option<Retained>> {
    let (owner, shape) = policy::retained_rule(locations, path).ok_or_else(fs::error)?;
    let entry = match (shape, &metadata.kind) {
        (RetainedShape::Historical, _) => None,
        (RetainedShape::Directory | RetainedShape::NativeState, EntryKind::Directory) => {
            Some(RetainedEntry::Directory)
        }
        (RetainedShape::File, EntryKind::File) => Some(RetainedEntry::File),
        (RetainedShape::NativeState, EntryKind::Symlink) => {
            let name = path.file_name().ok_or_else(fs::error)?;
            let text = fs::read_link(target.parent_directory()?, name)?;
            if !fs::native_link_disjoint(locations, locations.roots(), &text).unwrap_or(false) {
                return Ok(None);
            }
            Some(RetainedEntry::Symlink { text })
        }
        (RetainedShape::EmptyDirectory, EntryKind::Directory) => {
            let dir = fs::directory(path)?;
            if dir.identity() != &metadata.identity {
                return Err(fs::error());
            }
            if !fs::is_empty(&dir)? {
                return Ok(None);
            }
            Some(RetainedEntry::EmptyDirectory)
        }
        _ => return Ok(None),
    };
    if target.metadata()?.is_none_or(|current| {
        current.identity != metadata.identity || current.kind != metadata.kind
    }) {
        return Err(fs::error());
    }
    let retained = Retained {
        path: path.into(),
        owner,
        entry,
    };
    Ok(policy::retained_entry_valid(locations, locations.roots(), &retained).then_some(retained))
}

fn profile_link(ctx: &Context, scan: &mut Scan, path: &Path) -> Result<()> {
    let root = fs::directory(&ctx.home.join(".agents"))?;
    let text = fs::read_link(&root, OsStr::new("skill-profiles.json"))?;
    let target = crate::policy::paths::normalize_link(&root.path.join(&text))?;
    let expected = Dataset::SkillProfiles.source(&crate::operations::governance::locations(ctx));
    if target != expected
        || !matches!(
            scan.converted.get(&Dataset::SkillProfiles),
            Some(Transfer::Copied { .. })
        )
    {
        return Err(fs::error());
    }
    scan.links.push(LegacyLink {
        path: path.into(),
        text,
        target,
    });
    Ok(())
}

fn archive(path: &Path, kind: ArchiveKind) -> Result<Archive> {
    let root = fs::directory(path)?;
    let binding = fs::binding(&root)?;
    if matches!(kind, ArchiveKind::Backups | ArchiveKind::RebuildState) && binding.mode != 0o700 {
        return Err(fs::error());
    }
    let mut archive = Archive {
        kind,
        root: path.into(),
        mode: binding.mode,
        entries: BTreeMap::new(),
    };
    let mut budget = 256 * 1024 * 1024;
    archive_walk(&root, &root, &mut archive, &mut budget, 0)?;
    root.verify()?;
    Ok(archive)
}

fn archive_walk(
    root: &Directory,
    dir: &Directory,
    archive: &mut Archive,
    budget: &mut u64,
    depth: usize,
) -> Result<()> {
    if depth > 48 {
        return Err(Error::attention(
            "migration_archive_limit",
            "档案超出深度预算",
        ));
    }
    for name in fs::names(dir)? {
        if archive.entries.len() >= 40_000 {
            return Err(Error::attention(
                "migration_archive_limit",
                "档案超出条目预算",
            ));
        }
        let path = dir.path.join(&name);
        let relative = path
            .strip_prefix(&root.path)
            .map_err(|_| fs::error())?
            .to_path_buf();
        let meta = dir.metadata(name.as_os_str())?.ok_or_else(fs::error)?;
        let bun_lock = archive_entry_gate(dir, archive.kind, &relative, &meta)?;
        if archive.kind == ArchiveKind::Settings
            && relative.parent() == Some(Path::new("manifests"))
            && meta.kind != EntryKind::File
        {
            return Err(Error::attention(
                "migration_unknown_manifest",
                "旧治理声明必须为普通文件；未跟随链接或目录",
            ));
        }
        let entry = match meta.kind {
            EntryKind::Directory => {
                let child = dir.child(name.as_os_str())?;
                archive_walk(root, &child, archive, budget, depth + 1)?;
                dir.verify_child(name.as_os_str(), &child)?;
                IndexedEntry::Directory { mode: meta.mode }
            }
            EntryKind::File => {
                let (bytes, after) =
                    dir.regular_config_snapshot(name.as_os_str(), fs::LIMIT.min(*budget))?;
                if after.identity != meta.identity
                    || after.kind != meta.kind
                    || after.mode != meta.mode
                {
                    return Err(fs::error());
                }
                if bun_lock && !bun_lock_header(&bytes) {
                    return Err(old_writer_pending());
                }
                *budget = budget
                    .checked_sub(bytes.len() as u64)
                    .ok_or_else(fs::error)?;
                IndexedEntry::File {
                    sha256: fs::hash(&bytes),
                    mode: meta.mode,
                    size: bytes.len() as u64,
                }
            }
            EntryKind::Symlink => IndexedEntry::Link {
                text: fs::read_link(dir, name.as_os_str())?,
                mode: meta.mode,
            },
            EntryKind::Other => {
                return Err(Error::attention(
                    "migration_special_file",
                    "档案包含特殊文件；未打开该文件",
                ));
            }
        };
        archive.entries.insert(relative, entry);
    }
    dir.verify()
}

fn archived_bun_lock_path(dir: &Directory, relative: &Path, meta: &Metadata) -> Result<bool> {
    let segments: Vec<_> = relative.iter().collect();
    let [backup, managed, _, objects, index, ..] = segments.as_slice() else {
        return Ok(false);
    };
    if meta.kind != EntryKind::File
        || !backup.as_encoded_bytes().starts_with(b"SKILLS-")
        || *managed != "managed-assets"
        || *objects != "objects"
        || !index.as_encoded_bytes().iter().all(u8::is_ascii_digit)
    {
        return Ok(false);
    }
    Ok(dir
        .metadata(OsStr::new("package.json"))?
        .is_some_and(|entry| entry.kind == EntryKind::File))
}

fn bun_lock_header(bytes: &[u8]) -> bool {
    bytes
        .trim_ascii_start()
        .strip_prefix(b"{")
        .and_then(|body| {
            body.trim_ascii_start()
                .strip_prefix(b"\"lockfileVersion\":")
        })
        .and_then(|value| value.trim_ascii_start().strip_prefix(b"1"))
        .is_some_and(|tail| tail.trim_ascii_start().starts_with(b","))
}

fn old_writer_pending() -> Error {
    Error::attention(
        "migration_old_writer_pending",
        "发现旧锁或未决操作；先审查旧写入口",
    )
}

fn archive_entry_gate(
    dir: &Directory,
    kind: ArchiveKind,
    relative: &Path,
    meta: &Metadata,
) -> Result<bool> {
    let name = relative
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(fs::error)?;
    let lock = relative
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lock"));
    let old_writer = if kind == ArchiveKind::Settings {
        relative.starts_with(".git") && lock
    } else {
        lock || name == "active.json"
    };
    if old_writer {
        if kind == ArchiveKind::Backups
            && name == "bun.lock"
            && archived_bun_lock_path(dir, relative, meta)?
        {
            return Ok(true);
        }
        return Err(old_writer_pending());
    }
    if kind == ArchiveKind::Settings
        && relative.parent() == Some(Path::new("manifests"))
        && !rules::known_manifest(name)
    {
        return Err(Error::attention(
            "migration_unknown_manifest",
            "发现未识别的旧治理声明",
        ));
    }
    Ok(false)
}

pub fn verify_sources(ctx: &Context, ledger: &CommittedMigration) -> Result<()> {
    verify_source_scope(ctx, ledger, None)
}

pub fn verify_committed_sources(ctx: &Context, ledger: &CommittedMigration) -> Result<()> {
    let legacy_layout =
        (ledger.schema_version == policy::LEGACY_LEDGER_VERSION).then_some(&ledger.layout);
    verify_source_scope(ctx, ledger, legacy_layout)
}

fn verify_source_scope(
    ctx: &Context,
    ledger: &CommittedMigration,
    legacy_layout: Option<&Layout>,
) -> Result<()> {
    fs::validate_ledger_links(&crate::operations::governance::locations(ctx), ledger)?;
    let current = capture_with(ctx, false, legacy_layout)?;
    current.require_ready()?;
    for dataset in DATASETS {
        if dataset != Dataset::ManagedUpdates
            && current.converted.get(&dataset) != ledger.converted.get(&dataset)
        {
            return Err(diverged());
        }
    }
    if current.archives != ledger.archives || current.links != ledger.legacy_links {
        return Err(diverged());
    }
    Ok(())
}

pub fn diverged() -> Error {
    Error::attention(
        "migration_source_diverged",
        "旧源自提交以来发生改变；未覆盖新权威数据",
    )
}
