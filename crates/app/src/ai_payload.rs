use crate::{
    domain::FileId,
    error::{Error, Result},
    paths::{self, Directory, EntryKind},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::Metadata,
    io::Read,
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_FILE: u64 = 512 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;
const MAX_DEPTH: usize = 48;

fn changed() -> Error {
    Error::attention(
        "ai_payload_changed",
        "探测期间安装根、载荷或执行文件的身份/内容改变，保留阻断",
    )
}
fn limit() -> Error {
    Error::attention("ai_payload_limit", "安装载荷超出有界只读验证预算")
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct Facts {
    identity: FileId,
    mode: u32,
    uid: u32,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
fn facts(metadata: &Metadata) -> Facts {
    Facts {
        identity: FileId {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        mode: metadata.mode() & 0o7777,
        uid: metadata.uid(),
        len: metadata.len(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        changed: (metadata.ctime(), metadata.ctime_nsec()),
    }
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
enum Entry {
    Directory(Facts),
    File(Facts, String),
    Link(FileId, u32, OsString),
}
struct Budget {
    bytes: u64,
    entries: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            bytes: MAX_BYTES,
            entries: MAX_ENTRIES,
        }
    }
    fn entry(&mut self) -> Result<()> {
        self.entries = self.entries.checked_sub(1).ok_or_else(limit)?;
        Ok(())
    }
}
fn file_stamp(dir: &Directory, name: &OsStr, budget: &mut Budget) -> Result<Entry> {
    let fd = rustix::fs::openat(
        dir.file(),
        name,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    let mut file = std::fs::File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(changed());
    }
    let before = facts(&metadata);
    if before.len > MAX_FILE || before.len > budget.bytes {
        return Err(limit());
    }
    let mut total = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        budget.bytes = budget.bytes.checked_sub(count as u64).ok_or_else(limit)?;
        if total > before.len {
            return Err(changed());
        }
        hasher.update(&buffer[..count]);
    }
    let current = dir.metadata(name)?.ok_or_else(changed)?;
    if total != before.len
        || facts(&file.metadata()?) != before
        || current.kind != EntryKind::File
        || current.identity != before.identity
        || current.mode != before.mode
    {
        return Err(changed());
    }
    dir.verify()?;
    Ok(Entry::File(before, format!("{:x}", hasher.finalize())))
}
fn walk(
    dir: &Directory,
    prefix: &Path,
    depth: usize,
    budget: &mut Budget,
    entries: &mut BTreeMap<PathBuf, Entry>,
    skip: Option<&Path>,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(limit());
    }
    dir.verify()?;
    let before = facts(&dir.file().metadata()?);
    for item in dir.names()? {
        let item = item.map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let name = OsString::from_vec(item.file_name().to_bytes().to_vec());
        if name == "." || name == ".." {
            continue;
        }
        budget.entry()?;
        let path = prefix.join(&name);
        if skip == Some(path.as_path()) {
            continue;
        }
        let meta = dir.metadata(&name)?.ok_or_else(changed)?;
        match meta.kind {
            EntryKind::Directory => {
                let child = dir.child(&name)?;
                walk(&child, &path, depth + 1, budget, entries, skip)?;
                dir.verify_child(&name, &child)?;
            }
            EntryKind::File => {
                entries.insert(path, file_stamp(dir, &name, budget)?);
            }
            EntryKind::Symlink => {
                let target = rustix::fs::readlinkat(dir.file(), &name, Vec::new())
                    .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
                entries.insert(
                    path,
                    Entry::Link(
                        meta.identity,
                        meta.mode,
                        OsString::from_vec(target.to_bytes().to_vec()),
                    ),
                );
            }
            EntryKind::Other => {
                return Err(Error::attention(
                    "ai_payload_unsupported",
                    "安装载荷包含特殊文件，拒绝执行",
                ));
            }
        }
    }
    dir.verify()?;
    if facts(&dir.file().metadata()?) != before {
        return Err(changed());
    }
    entries.insert(prefix.to_path_buf(), Entry::Directory(before));
    Ok(())
}
struct Tree {
    root: Directory,
    lineage: Vec<FileId>,
    entries: BTreeMap<PathBuf, Entry>,
}
impl Tree {
    fn capture(path: &Path, budget: &mut Budget) -> Result<Self> {
        let root = Directory::open(path)?;
        let lineage = root.lineage()?;
        let mut entries = BTreeMap::new();
        walk(&root, Path::new(""), 0, budget, &mut entries, None)?;
        if root.lineage()? != lineage {
            return Err(changed());
        }
        Ok(Self {
            root,
            lineage,
            entries,
        })
    }
    fn verify(&self, budget: &mut Budget) -> Result<()> {
        self.root.verify()?;
        let mut entries = BTreeMap::new();
        walk(&self.root, Path::new(""), 0, budget, &mut entries, None)?;
        if self.root.lineage()? != self.lineage || entries != self.entries {
            return Err(changed());
        }
        Ok(())
    }
}
struct File {
    requested: PathBuf,
    resolved: PathBuf,
    parent: Directory,
    lineage: Vec<FileId>,
    name: OsString,
    stamp: Entry,
}
impl File {
    fn capture(path: &Path, budget: &mut Budget) -> Result<Self> {
        let resolved = paths::read_path(path)?;
        let parent = Directory::open(resolved.parent().ok_or_else(changed)?)?;
        let lineage = parent.lineage()?;
        let name = resolved.file_name().ok_or_else(changed)?.to_os_string();
        let stamp = file_stamp(&parent, &name, budget)?;
        if parent.lineage()? != lineage {
            return Err(changed());
        }
        Ok(Self {
            requested: path.into(),
            resolved,
            parent,
            lineage,
            name,
            stamp,
        })
    }
    fn verify(&self, budget: &mut Budget) -> Result<()> {
        self.parent.verify()?;
        if paths::read_path(&self.requested)? != self.resolved
            || self.parent.lineage()? != self.lineage
            || file_stamp(&self.parent, &self.name, budget)? != self.stamp
        {
            return Err(changed());
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Evidence {
    trees: Vec<Tree>,
    files: Vec<File>,
}
impl Evidence {
    pub fn digest(&self) -> Result<String> {
        crate::domain::fingerprint(&(
            self.trees
                .iter()
                .map(|tree| (&tree.root.path, &tree.lineage, &tree.entries))
                .collect::<Vec<_>>(),
            self.files
                .iter()
                .map(|file| (&file.requested, &file.resolved, &file.lineage, &file.stamp))
                .collect::<Vec<_>>(),
        ))
    }
    pub fn tree(&mut self, path: &Path) -> Result<()> {
        self.trees.push(Tree::capture(path, &mut Budget::new())?);
        Ok(())
    }
    pub fn file(&mut self, path: &Path) -> Result<()> {
        if !self.files.iter().any(|file| file.requested == path) {
            self.files.push(File::capture(path, &mut Budget::new())?);
        }
        Ok(())
    }
    pub fn header(&mut self, path: &Path) -> Result<Vec<u8>> {
        self.file(path)?;
        let binding = self
            .files
            .iter()
            .find(|file| file.requested == path)
            .ok_or_else(changed)?;
        let fd = rustix::fs::openat(
            binding.parent.file(),
            &binding.name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let file = std::fs::File::from(fd);
        if !file.metadata()?.is_file() {
            return Err(changed());
        }
        let mut header = Vec::new();
        file.take(512).read_to_end(&mut header)?;
        Ok(header)
    }
    pub fn verify(&self) -> Result<()> {
        let mut budget = Budget::new();
        for tree in &self.trees {
            tree.verify(&mut budget).map_err(|_| changed())?;
        }
        for file in &self.files {
            file.verify(&mut budget).map_err(|_| changed())?;
        }
        Ok(())
    }
}

pub fn native_binary_digest(parent: &Directory, name: &OsStr) -> Result<String> {
    match file_stamp(parent, name, &mut Budget::new())? {
        Entry::File(facts, sha256) if facts.mode & 0o111 != 0 => Ok(sha256),
        _ => Err(Error::attention(
            "native_binary_unsupported",
            "需要明确的普通可执行文件",
        )),
    }
}

pub fn native_plugin_tree_digest(root: &Directory, skip: Option<&Path>) -> Result<String> {
    let mut entries = BTreeMap::new();
    walk(
        root,
        Path::new(""),
        0,
        &mut Budget::new(),
        &mut entries,
        skip,
    )?;
    let mut hasher = Sha256::new();
    for (path, entry) in entries {
        let encoded = path.as_os_str().as_encoded_bytes();
        hasher.update((encoded.len() as u64).to_le_bytes());
        hasher.update(encoded);
        match entry {
            Entry::Directory(facts) => {
                hasher.update(b"D");
                if skip.is_some() {
                    hasher.update(facts.mode.to_le_bytes());
                    hasher.update(facts.identity.device.to_le_bytes());
                    hasher.update(facts.identity.inode.to_le_bytes());
                }
            }
            Entry::File(facts, digest) => {
                hasher.update(b"F");
                hasher.update(facts.mode.to_le_bytes());
                hasher.update(digest.as_bytes());
            }
            Entry::Link(..) => {
                return Err(Error::attention(
                    "plugin_payload_symlink",
                    "插件载荷含有链接",
                ));
            }
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
#[path = "../tests/unit/ai_payload.rs"]
mod tests;
