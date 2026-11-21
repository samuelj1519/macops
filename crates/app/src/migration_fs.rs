use crate::{
    domain::{ConfigAnchor, FileId},
    error::{Error, Result},
    paths::{Bindings, Directory, EntryKind, ObservedRegular},
    policy::governance::{self as policy, DirectoryBinding, FileProof, Locations},
};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::File,
    io::Write,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub const LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct RootWitness {
    pub path: PathBuf,
    namespace: crate::policy::capability_tx::Namespace,
    present: Option<DirectoryBinding>,
}

impl RootWitness {
    pub fn capture(path: &Path) -> Result<Self> {
        let target = Bindings::new(64).target(path)?;
        let present = if target.metadata()?.is_some() {
            Some(binding(&directory(path)?)?)
        } else {
            None
        };
        Ok(Self {
            path: path.into(),
            namespace: crate::capability_store::namespace(path)?,
            present,
        })
    }
    pub fn create(&self, created: &mut BTreeMap<PathBuf, Directory>) -> Result<Directory> {
        if let Some(present) = &self.present {
            return verify_binding(present);
        }
        let mut dir = crate::capability_store::open(&self.namespace)?;
        for name in &self.namespace.remaining {
            let path = dir.path.join(name);
            let child = if let Some(known) = created.get(&path) {
                private(known)?;
                dir.verify_child(OsStr::new(name), known)?;
                known.clone()
            } else {
                if dir.metadata(OsStr::new(name))?.is_some() {
                    return Err(error());
                }
                dir.verify()?;
                rustix::fs::mkdirat(dir.file(), name.as_str(), Mode::RWXU).map_err(os)?;
                dir.file().sync_all()?;
                let child = dir.child(OsStr::new(name))?;
                private(&child)?;
                dir.verify_child(OsStr::new(name), &child)?;
                created.insert(path, child.clone());
                child
            };
            dir = child;
        }
        private(&dir)?;
        Ok(dir)
    }
}

// Each coordinate is the path's suffix from an observed ancestor, or the empty suffix
// from the existing leaf itself. This also represents missing descendants of an alias.
type Position = Vec<(FileId, PathBuf)>;

fn position(path: &Path) -> Result<Position> {
    if path == Path::new("/") {
        return Ok(vec![(
            Directory::open(path)?.identity().clone(),
            PathBuf::new(),
        )]);
    }
    let target = Bindings::new(64).target(path)?;
    let leaf = target.metadata()?;
    let parent = target.parent_directory()?;
    let mut coordinates = parent
        .lineage()?
        .into_iter()
        .rev()
        .zip(parent.path.ancestors())
        .map(|(id, ancestor)| {
            let suffix = path.strip_prefix(ancestor).map_err(|_| error())?;
            Ok((id, suffix.to_path_buf()))
        })
        .collect::<Result<Position>>()?;
    if let Some(meta) = leaf {
        coordinates.push((meta.identity, PathBuf::new()));
    }
    target.verify()?;
    Ok(coordinates)
}

fn physical_overlap(a: &Position, b: &Position) -> bool {
    a.iter().any(|(id, suffix)| {
        b.iter().any(|(other_id, other_suffix)| {
            id == other_id && policy::overlaps(suffix, other_suffix)
        })
    })
}

pub fn validate_new_roots(p: &Locations, roots: [&Path; 4]) -> Result<()> {
    if !policy::root_scope_valid(p, roots) {
        return Err(error());
    }
    let settings = position(&crate::paths::read_path(&p.settings)?)?;
    for path in policy::legacy_history_roots(p) {
        let history = position(&crate::paths::read_path(&path)?)?;
        if physical_overlap(&settings, &history) {
            return Err(error());
        }
    }
    let mut actual = Vec::with_capacity(roots.len());
    for path in roots {
        if crate::paths::read_path(path)? != path {
            return Err(error());
        }
        let current = position(path)?;
        if actual.iter().any(|other| physical_overlap(other, &current)) {
            return Err(error());
        }
        actual.push(current);
    }
    let (native, legacy) = source_positions(p)?;
    let defaults = policy::displaced_default_roots(p, roots)
        .map(|path| position(&crate::paths::read_path(&path)?))
        .collect::<Result<Vec<_>>>()?;
    if native
        .iter()
        .chain(&legacy)
        .chain(&defaults)
        .any(|protected| actual.iter().any(|root| physical_overlap(root, protected)))
    {
        return Err(error());
    }
    Ok(())
}

fn source_positions(p: &Locations) -> Result<(Vec<Position>, Vec<Position>)> {
    let native = policy::native_state_roots(p)
        .map(|path| {
            // Inspect the non-followed Native leaf through its existing ancestor.
            let parent = crate::paths::read_path(path.parent().ok_or_else(error)?)?;
            position(&parent.join(path.file_name().ok_or_else(error)?))
        })
        .collect::<Result<Vec<_>>>()?;
    let legacy = policy::legacy_control_roots(p)
        .map(|path| {
            let protected = position(&crate::paths::read_path(&path)?)?;
            if native.iter().any(|root| physical_overlap(root, &protected)) {
                return Err(error());
            }
            Ok(protected)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((native, legacy))
}

pub fn validate_legacy_sources(p: &Locations) -> Result<()> {
    source_positions(p).map(|_| ())
}

// Canonicalization is metadata-only. For a missing tail, retain its spelling
// relative to a proven ancestor. A dangling intermediate link or an unresolved
// parent component may alter the coordinates and is therefore rejected.
fn resolved_position(path: &Path) -> Result<Position> {
    let mut prefix = path;
    let mut tail = Vec::new();
    loop {
        match std::fs::canonicalize(prefix) {
            Ok(mut found) => {
                found.extend(tail.into_iter().rev());
                return position(&found);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::symlink_metadata(prefix) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    _ => return Err(error()),
                }
                tail.push(prefix.file_name().ok_or_else(error)?);
                prefix = prefix.parent().ok_or_else(error)?;
            }
            Err(_) => return Err(error()),
        }
    }
}

pub fn native_link_disjoint(p: &Locations, roots: [&Path; 4], text: &Path) -> Result<bool> {
    let target = policy::native_link_input(p, text).ok_or_else(error)?;
    let observed = resolved_position(&target)?;
    for protected in policy::link_protected_roots(p, roots) {
        if physical_overlap(&observed, &resolved_position(&protected)?) {
            return Ok(false);
        }
    }
    Ok(resolved_position(&target)? == observed)
}

pub fn validate_ledger_links(p: &Locations, ledger: &policy::CommittedMigration) -> Result<()> {
    for entry in &ledger.retained {
        if let Some(policy::RetainedEntry::Symlink { text }) = &entry.entry
            && (!native_link_disjoint(p, p.roots(), text).unwrap_or(false)
                || (p.cache != ledger.layout.cache
                    && !native_link_disjoint(p, ledger.layout.roots(), text).unwrap_or(false)))
        {
            return Err(policy::invalid());
        }
    }
    Ok(())
}

pub fn error() -> Error {
    Error::attention(
        "migration_input_changed",
        "迁移输入、落位目标或私有命名空间已改变",
    )
}

pub fn os(error: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn metadata_equal(a: &crate::paths::Metadata, b: &crate::paths::Metadata) -> bool {
    a.identity == b.identity && a.kind == b.kind && a.mode == b.mode
}

pub fn directory(path: &Path) -> Result<Directory> {
    let dir = Directory::open(path)?;
    if dir.path != path {
        return Err(error());
    }
    dir.verify()?;
    Ok(dir)
}

pub fn binding(dir: &Directory) -> Result<DirectoryBinding> {
    dir.verify()?;
    let meta = dir.file().metadata()?;
    Ok(DirectoryBinding {
        requested: dir.path.clone(),
        canonical: dir.path.clone(),
        identity: dir.identity().clone(),
        lineage: dir.lineage()?,
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
    })
}

pub fn verify_binding(expected: &DirectoryBinding) -> Result<Directory> {
    let dir = directory(&expected.requested)?;
    if binding(&dir)? != *expected {
        return Err(error());
    }
    private(&dir)?;
    Ok(dir)
}

pub fn private(dir: &Directory) -> Result<()> {
    let meta = dir.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
        return Err(Error::attention(
            "migration_private_directory_required",
            "自有迁移根必须属于当前用户且权限为0700；不自动修改既有目录权限",
        ));
    }
    dir.verify()
}

pub fn child_private(parent: &Directory, name: &OsStr) -> Result<Directory> {
    parent.verify()?;
    if parent.metadata(name)?.is_none() {
        rustix::fs::mkdirat(parent.file(), name, Mode::RWXU).map_err(os)?;
        parent.file().sync_all()?;
    }
    let child = parent.child(name)?;
    private(&child)?;
    parent.verify_child(name, &child)?;
    parent.verify()?;
    Ok(child)
}

pub fn snapshot(path: &Path) -> Result<Option<(Vec<u8>, FileProof)>> {
    bounded_snapshot(path, LIMIT)
}

pub fn bounded_snapshot(path: &Path, limit: u64) -> Result<Option<(Vec<u8>, FileProof)>> {
    let target = Bindings::new(64).target(path)?;
    match target.observe_regular(limit)? {
        ObservedRegular::Missing => Ok(None),
        ObservedRegular::NonRegular(_) => Err(error()),
        ObservedRegular::Regular { bytes, metadata } => {
            let parent = target.parent_directory()?;
            if path.parent() != Some(parent.path.as_path()) {
                return Err(error());
            }
            let name = path.file_name().and_then(OsStr::to_str).ok_or_else(error)?;
            let proof = FileProof {
                anchor: ConfigAnchor {
                    requested: path.into(),
                    canonical_parent: parent.path.clone(),
                    parent_identity: parent.identity().clone(),
                    name: name.into(),
                },
                identity: metadata.identity,
                lineage: parent.lineage()?,
                sha256: hash(&bytes),
                mode: metadata.mode,
            };
            Ok(Some((bytes, proof)))
        }
    }
}

pub fn names(dir: &Directory) -> Result<Vec<PathBuf>> {
    let mut names = Vec::new();
    for entry in dir.names()? {
        let entry = entry.map_err(os)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = std::str::from_utf8(name).map_err(|_| error())?;
        if names.len() >= 40_000 {
            return Err(Error::attention(
                "migration_archive_limit",
                "目录枚举超出条目预算",
            ));
        }
        names.push(PathBuf::from(name));
    }
    names.sort();
    dir.verify()?;
    Ok(names)
}

pub fn is_empty(dir: &Directory) -> Result<bool> {
    for entry in dir.names()? {
        let entry = entry.map_err(os)?;
        if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
            dir.verify()?;
            return Ok(false);
        }
    }
    dir.verify()?;
    Ok(true)
}

pub fn read_link(dir: &Directory, name: &OsStr) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let before = dir.metadata(name)?.ok_or_else(error)?;
    if before.kind != EntryKind::Symlink {
        return Err(error());
    }
    let text = rustix::fs::readlinkat(dir.file(), name, Vec::new()).map_err(os)?;
    if !dir
        .metadata(name)?
        .as_ref()
        .is_some_and(|m| metadata_equal(m, &before))
    {
        return Err(error());
    }
    dir.verify()?;
    Ok(std::ffi::OsString::from_vec(text.into_bytes()).into())
}

pub fn write_new(dir: &Directory, name: &str, bytes: &[u8]) -> Result<FileId> {
    dir.verify()?;
    let mut file = File::from(
        rustix::fs::openat(
            dir.file(),
            name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(os)?,
    );
    file.write_all(bytes)?;
    file.sync_all()?;
    let id = FileId::from_file(&file)?;
    dir.file().sync_all()?;
    dir.verify()?;
    Ok(id)
}

pub fn matches(
    dir: &Directory,
    name: &OsStr,
    id: &FileId,
    digest: &str,
    size: u64,
) -> Result<bool> {
    let Some(meta) = dir.metadata(name)? else {
        return Ok(false);
    };
    if meta.kind != EntryKind::File || meta.identity != *id || meta.mode != 0o600 {
        return Ok(false);
    }
    let (bytes, observed) = dir.regular_config_snapshot(name, LIMIT)?;
    let owner =
        rustix::fs::statat(dir.file(), name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).map_err(os)?;
    Ok(metadata_equal(&observed, &meta)
        && owner.st_uid == nix::unistd::geteuid().as_raw()
        && bytes.len() as u64 == size
        && hash(&bytes) == digest)
}

pub fn fill_own(dir: &Directory, name: &str, id: &FileId, bytes: &[u8]) -> Result<()> {
    dir.verify()?;
    let mut file = File::from(
        rustix::fs::openat(
            dir.file(),
            name,
            OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(os)?,
    );
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o7777 != 0o600
        || FileId::from_file(&file)? != *id
    {
        return Err(error());
    }
    file.set_len(0)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    dir.verify()?;
    if !matches(dir, OsStr::new(name), id, &hash(bytes), bytes.len() as u64)? {
        return Err(error());
    }
    Ok(())
}
