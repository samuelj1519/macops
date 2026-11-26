use crate::error::{Error, Result};
use std::path::{Path, PathBuf};

pub fn read_path(path: &Path) -> Result<PathBuf> {
    let mut pending = lexical(path)?;
    for _ in 0..40 {
        let mut current = PathBuf::new();
        let components: Vec<_> = pending.components().collect();
        let mut changed = false;
        for (i, part) in components.iter().enumerate() {
            current.push(part.as_os_str());
            lexical(&current)?;
            match std::fs::symlink_metadata(&current) {
                Ok(m) if m.file_type().is_symlink() => {
                    let link = std::fs::read_link(&current)?;
                    let mut target = if link.is_absolute() {
                        link
                    } else {
                        current.parent().unwrap_or(Path::new("/")).join(link)
                    };
                    for rest in &components[i + 1..] {
                        target.push(rest.as_os_str());
                    }
                    pending = normalize_link(&target).map_err(|mut error| {
                        if error.code == "excluded_path" {
                            error.message = format!("{} 的{}", current.display(), error.message);
                        }
                        error
                    })?;
                    changed = true;
                    break;
                }
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
        if !changed {
            return Ok(pending);
        }
    }
    Err(Error::new("symlink_loop", path.display().to_string()))
}

pub fn write_path(path: &Path) -> Result<PathBuf> {
    let clean = lexical(path)?;
    let mut current = PathBuf::new();
    for c in clean.components() {
        current.push(c.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(Error::new(
                    "write_symlink_forbidden",
                    current.display().to_string(),
                ));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(clean)
}

pub fn regular_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&regular_bytes(path)?)?)
}

pub fn regular_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let safe = read_path(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
        .open(safe)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
        return Err(Error::new("invalid_input_file", path.display().to_string()));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(Error::new(
            "invalid_input_file",
            "输入在读取期间超过大小限制",
        ));
    }
    Ok(bytes)
}

use crate::policy::paths::{lexical, normalize_link};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}
impl EntryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}
#[derive(Clone, Debug)]
pub enum ObservedRegular {
    Missing,
    NonRegular(Metadata),
    Regular { bytes: Vec<u8>, metadata: Metadata },
}
#[derive(Clone, Debug)]
pub struct Metadata {
    pub identity: crate::domain::FileId,
    pub kind: EntryKind,
    pub mode: u32,
}
#[derive(Clone, Debug)]
pub struct Directory {
    file: std::sync::Arc<std::fs::File>,
    requested: PathBuf,
    pub path: PathBuf,
    identity: crate::domain::FileId,
}

fn fs_error(error: rustix::io::Errno) -> Error {
    if error == rustix::io::Errno::NOENT {
        return Error::new("path_missing", "路径不存在");
    }
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}
fn number<T: TryInto<u64>>(value: T) -> Result<u64> {
    value
        .try_into()
        .map_err(|_| Error::new("invalid_file_metadata", "文件元信息无法表示"))
}
fn metadata(stat: &rustix::fs::Stat) -> Result<Metadata> {
    use rustix::fs::FileType;
    Ok(Metadata {
        identity: crate::domain::FileId {
            device: number(stat.st_dev)?,
            inode: number(stat.st_ino)?,
        },
        kind: match FileType::from_raw_mode(stat.st_mode) {
            FileType::RegularFile => EntryKind::File,
            FileType::Directory => EntryKind::Directory,
            FileType::Symlink => EntryKind::Symlink,
            _ => EntryKind::Other,
        },
        mode: u32::try_from(number(stat.st_mode)?)
            .map_err(|_| Error::new("invalid_file_metadata", "文件模式无法表示"))?
            & 0o7777,
    })
}
fn component(name: &std::ffi::OsStr) -> Result<()> {
    let mut parts = Path::new(name).components();
    if !matches!(parts.next(), Some(std::path::Component::Normal(part)) if part == name)
        || parts.next().is_some()
    {
        return Err(Error::new("invalid_path_component", "需要单一安全文件名"));
    }
    Ok(())
}
fn directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}

fn open_directory(path: &Path) -> Result<std::fs::File> {
    walk_directory(path, |_| Ok(()))
}

fn walk_directory(
    path: &Path,
    mut visit: impl FnMut(&std::fs::File) -> Result<()>,
) -> Result<std::fs::File> {
    use rustix::fs::{Mode, open, openat};
    lexical(path)?;
    let mut file =
        std::fs::File::from(open("/", directory_flags(), Mode::empty()).map_err(fs_error)?);
    visit(&file)?;
    for part in path.components().skip(1) {
        file = std::fs::File::from(
            openat(&file, part.as_os_str(), directory_flags(), Mode::empty()).map_err(fs_error)?,
        );
        visit(&file)?;
    }
    Ok(file)
}

impl Directory {
    pub fn open(path: &Path) -> Result<Self> {
        let safe = read_path(path)?;
        let file = open_directory(&safe)?;
        let identity = crate::domain::FileId::from_file(&file)?;
        Ok(Self {
            file: std::sync::Arc::new(file),
            requested: path.to_path_buf(),
            path: safe,
            identity,
        })
    }
    pub fn metadata(&self, name: &std::ffi::OsStr) -> Result<Option<Metadata>> {
        component(name)?;
        lexical(&self.path.join(name))?;
        match rustix::fs::statat(&self.file, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(metadata(&stat)?)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(fs_error(error)),
        }
    }
    pub fn names(&self) -> Result<rustix::fs::Dir> {
        rustix::fs::Dir::read_from(&self.file).map_err(fs_error)
    }
    pub fn child(&self, name: &std::ffi::OsStr) -> Result<Self> {
        component(name)?;
        lexical(&self.path.join(name))?;
        let file = std::fs::File::from(
            rustix::fs::openat(
                &self.file,
                name,
                directory_flags(),
                rustix::fs::Mode::empty(),
            )
            .map_err(fs_error)?,
        );
        let identity = crate::domain::FileId::from_file(&file)?;
        Ok(Self {
            file: std::sync::Arc::new(file),
            requested: self.requested.join(name),
            path: self.path.join(name),
            identity,
        })
    }
    pub fn verify_child(&self, name: &std::ffi::OsStr, child: &Self) -> Result<()> {
        if self
            .metadata(name)?
            .is_none_or(|meta| meta.kind != EntryKind::Directory || meta.identity != child.identity)
        {
            return Err(Error::attention(
                "directory_changed",
                "目录在观察期间改变，丢弃该枝结果",
            ));
        }
        Ok(())
    }
    pub fn verify(&self) -> Result<()> {
        if read_path(&self.requested)? != self.path
            || crate::domain::FileId::from_file(&open_directory(&self.path)?)? != self.identity
        {
            return Err(Error::attention(
                "directory_changed",
                "观察根在查询期间改变，丢弃结果",
            ));
        }
        Ok(())
    }
    pub fn read_regular(&self, name: &std::ffi::OsStr, limit: u64) -> Result<Option<Vec<u8>>> {
        use rustix::fs::{Mode, OFlags};
        use std::io::Read;
        component(name)?;
        lexical(&self.path.join(name))?;
        let fd = match rustix::fs::openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => {
                self.verify()?;
                return Ok(None);
            }
            Err(error) => return Err(fs_error(error)),
        };
        let file = std::fs::File::from(fd);
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > limit {
            return Err(Error::attention(
                "invalid_config_input",
                "需要限额内的普通配置元信息文件",
            ));
        }
        let mut bytes = vec![];
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len())
            .map_err(|_| Error::new("invalid_config_input", "输入大小无法表示"))?
            > limit
        {
            return Err(Error::attention(
                "invalid_config_input",
                "输入在读取期间超过限额",
            ));
        }
        self.verify()?;
        Ok(Some(bytes))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/paths_fd.rs"]
mod tests;

#[derive(Debug)]
pub struct Bindings {
    directories: std::collections::BTreeMap<PathBuf, Directory>,
    limit: usize,
}
#[derive(Clone, Debug)]
pub struct BoundTarget {
    chain: Vec<Directory>,
    name: std::ffi::OsString,
    missing: Option<std::ffi::OsString>,
}
impl Bindings {
    pub fn new(limit: usize) -> Self {
        Self {
            directories: std::collections::BTreeMap::new(),
            limit,
        }
    }
    pub fn root(&mut self, path: &Path) -> Result<Directory> {
        let key = lexical(path)?;
        if let Some(directory) = self.directories.get(&key) {
            directory.verify()?;
            return Ok(directory.clone());
        }
        self.capacity()?;
        let directory = Directory::open(&key)?;
        self.directories.insert(key, directory.clone());
        Ok(directory)
    }
    fn capacity(&self) -> Result<()> {
        if self.directories.len() >= self.limit {
            Err(Error::attention(
                "metadata_binding_limit",
                "目录绑定数量达到上限，未重新按路径读取",
            ))
        } else {
            Ok(())
        }
    }
    fn child(&mut self, parent: &Directory, name: &std::ffi::OsStr) -> Result<Directory> {
        component(name)?;
        let key = parent.requested.join(name);
        if let Some(child) = self.directories.get(&key) {
            parent.verify_child(name, child)?;
            return Ok(child.clone());
        }
        self.capacity()?;
        let child = parent.child(name)?;
        parent.verify_child(name, &child)?;
        self.directories.insert(key, child.clone());
        Ok(child)
    }
    pub fn target(&mut self, path: &Path) -> Result<BoundTarget> {
        let path = lexical(path)?;
        let mut parent = path
            .parent()
            .ok_or_else(|| Error::new("invalid_config_target", "目标缺少父目录"))?
            .to_path_buf();
        loop {
            match self.root(&parent) {
                Ok(root) => {
                    return self.relative(
                        &root,
                        path.strip_prefix(&parent)
                            .map_err(|_| Error::new("invalid_config_target", "目标不在绑定范围"))?,
                    );
                }
                Err(error) if error.code == "path_missing" => {
                    if !parent.pop() {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }
    }
    pub fn relative(&mut self, root: &Directory, path: &Path) -> Result<BoundTarget> {
        let name = path
            .file_name()
            .ok_or_else(|| Error::new("invalid_config_target", "目标缺少文件名"))?;
        component(name)?;
        let mut chain = vec![root.clone()];
        let mut missing = None;
        for part in path.parent().unwrap_or(Path::new("")).components() {
            let std::path::Component::Normal(name) = part else {
                return Err(Error::new("invalid_config_target", "需要安全相对目标"));
            };
            let parent = chain
                .last()
                .ok_or_else(|| Error::new("invalid_config_target", "目录绑定缺失"))?;
            match self.child(parent, name) {
                Ok(child) => chain.push(child),
                Err(error) if error.code == "path_missing" => {
                    missing = Some(name.to_os_string());
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(BoundTarget {
            chain,
            name: name.to_os_string(),
            missing,
        })
    }
}
impl BoundTarget {
    pub fn verify(&self) -> Result<()> {
        let root = self
            .chain
            .first()
            .ok_or_else(|| Error::new("invalid_config_target", "目录绑定缺失"))?;
        root.verify()?;
        for pair in self.chain.windows(2) {
            let child = &pair[1];
            pair[0].verify_child(
                child
                    .path
                    .file_name()
                    .ok_or_else(|| Error::new("invalid_config_target", "子目录缺少名称"))?,
                child,
            )?;
        }
        Ok(())
    }
    pub fn missing_descendant(&self, path: &Path) -> Result<Self> {
        if self.metadata()?.is_some() {
            return Err(Error::attention(
                "directory_changed",
                "原缺失目录已出现，未绑定新命名空间",
            ));
        }
        let mut target = self.clone();
        target.missing = Some(self.missing.clone().unwrap_or_else(|| self.name.clone()));
        target.name = path
            .file_name()
            .ok_or_else(|| Error::new("invalid_config_target", "目标缺少文件名"))?
            .to_os_string();
        component(&target.name)?;
        Ok(target)
    }
    pub fn metadata(&self) -> Result<Option<Metadata>> {
        self.verify()?;
        let parent = self
            .chain
            .last()
            .ok_or_else(|| Error::new("invalid_config_target", "目录绑定缺失"))?;
        let result = if let Some(missing) = &self.missing {
            if parent.metadata(missing)?.is_some() {
                return Err(Error::attention(
                    "directory_changed",
                    "查询期间原缺失目录已改变",
                ));
            }
            None
        } else {
            parent.metadata(&self.name)?
        };
        self.verify()?;
        Ok(result)
    }
    pub fn bound_directory(&self) -> Result<Option<Directory>> {
        let Some(meta) = self.metadata()? else {
            return Ok(None);
        };
        if meta.kind != EntryKind::Directory {
            return Err(Error::attention(
                "expected_directory",
                "需要普通目录，拒绝跟随链接",
            ));
        }
        let child = self.parent_directory()?.child(&self.name)?;
        let current = self.metadata()?;
        if current.as_ref().map(|row| &row.identity) != Some(child.identity())
            || &meta.identity != child.identity()
        {
            return Err(Error::attention("directory_changed", "目录在绑定期间改变"));
        }
        Ok(Some(child))
    }

    pub fn parent_directory(&self) -> Result<&Directory> {
        self.chain
            .last()
            .ok_or_else(|| Error::new("invalid_config_target", "目录绑定缺失"))
    }
    pub fn observe_regular(&self, limit: u64) -> Result<ObservedRegular> {
        self.verify()?;
        let parent = self
            .chain
            .last()
            .ok_or_else(|| Error::new("invalid_config_target", "目录绑定缺失"))?;
        let observed = if let Some(missing) = &self.missing {
            if parent.metadata(missing)?.is_some() {
                return Err(Error::attention(
                    "directory_changed",
                    "查询期间原缺失目录已改变",
                ));
            }
            ObservedRegular::Missing
        } else {
            match parent.metadata(&self.name)? {
                None => ObservedRegular::Missing,
                Some(meta) if meta.kind != EntryKind::File => ObservedRegular::NonRegular(meta),
                Some(_) => {
                    let (bytes, metadata) = parent.regular_config_snapshot(&self.name, limit)?;
                    ObservedRegular::Regular { bytes, metadata }
                }
            }
        };
        self.verify()?;
        Ok(observed)
    }

    pub fn read_selected(&self, identity: &crate::domain::FileId, limit: u64) -> Result<Vec<u8>> {
        self.verify()?;
        if self.missing.is_some() {
            return Err(Error::attention("file_unavailable", "所选文件已不存在"));
        }
        let (bytes, _) = self.parent_directory()?.snapshot_with_codes(
            &self.name,
            limit,
            ("file_unavailable", "仅可打开普通文件"),
            ("config_file_limit", "所选文件超过查看限制"),
            ("config_file_changed", "所选文件身份或内容已改变"),
            Some(identity),
        )?;
        self.verify()?;
        Ok(bytes)
    }
}

impl Directory {
    pub fn identity(&self) -> &crate::domain::FileId {
        &self.identity
    }

    pub fn lineage(&self) -> Result<Vec<crate::domain::FileId>> {
        let mut ids = vec![];
        walk_directory(&self.path, |file| {
            ids.push(crate::domain::FileId::from_file(file)?);
            Ok(())
        })?;
        if ids.last() != Some(&self.identity) {
            return Err(Error::attention("directory_changed", "目录身份已改变"));
        }
        Ok(ids)
    }

    pub fn regular_snapshot(
        &self,
        name: &std::ffi::OsStr,
        limit: u64,
    ) -> Result<(Vec<u8>, crate::domain::FileId)> {
        let (bytes, meta) = self.snapshot_with_codes(
            name,
            limit,
            ("unsupported_npm_manifest", "需要普通 npm manifest 文件"),
            ("npm_manifest_limit", "npm manifest 超出读取预算"),
            ("npm_manifest_changed", "npm manifest 在读取期间改变"),
            None,
        )?;
        Ok((bytes, meta.identity))
    }

    pub fn regular_config_snapshot(
        &self,
        name: &std::ffi::OsStr,
        limit: u64,
    ) -> Result<(Vec<u8>, Metadata)> {
        self.snapshot_with_codes(
            name,
            limit,
            ("unsupported_config_file", "需要普通配置文件"),
            ("config_file_limit", "配置文件超出读取预算"),
            ("config_file_changed", "配置文件在读取期间改变"),
            None,
        )
    }

    pub(crate) fn file(&self) -> &std::fs::File {
        &self.file
    }

    pub fn atomic_replace_regular(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
        mode: u32,
        check_cas: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        self.atomic_replace_regular_verified(name, bytes, mode, |dir, _| check_cas(dir))
            .map(|_| ())
    }

    pub fn atomic_replace_regular_verified(
        &self,
        name: &std::ffi::OsStr,
        bytes: &[u8],
        mode: u32,
        check_cas: impl FnOnce(&Self, &Metadata) -> Result<()>,
    ) -> Result<Metadata> {
        use rustix::fs::{AtFlags, Mode, OFlags, fchmod, openat, renameat, unlinkat};
        use std::io::Write;
        component(name)?;
        lexical(&self.path.join(name))?;
        self.verify()?;
        let tmp_name = format!(".{}.cfg.tmp", crate::operations::store::operation_id());
        let fd = openat(
            &self.file,
            tmp_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(fs_error)?;
        let mut tmp_file = std::fs::File::from(fd);
        let outcome = (|| {
            tmp_file.write_all(bytes)?;
            fchmod(&tmp_file, Mode::from_raw_mode((mode & 0o7777) as _)).map_err(fs_error)?;
            tmp_file.sync_all()?;
            self.verify()?;
            let written = metadata(&rustix::fs::fstat(&tmp_file).map_err(fs_error)?)?;
            check_cas(self, &written)?;
            if let Some(existing) = self.metadata(name)?
                && existing.kind != EntryKind::File
            {
                return Err(Error::attention(
                    "unsupported_config_file",
                    "配置目标位置不是普通文件，拒绝覆盖",
                ));
            }
            renameat(&self.file, tmp_name.as_str(), &self.file, name).map_err(fs_error)?;
            self.file.sync_all()?;
            self.verify()?;
            Ok(written)
        })();
        if outcome.is_err() {
            let _ = unlinkat(&self.file, tmp_name.as_str(), AtFlags::empty());
        }
        outcome
    }

    pub fn unlink_regular(
        &self,
        name: &std::ffi::OsStr,
        check_cas: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        use rustix::fs::{AtFlags, unlinkat};
        component(name)?;
        lexical(&self.path.join(name))?;
        self.verify()?;
        check_cas(self)?;
        if let Some(existing) = self.metadata(name)? {
            if existing.kind != EntryKind::File {
                return Err(Error::attention(
                    "unsupported_config_file",
                    "拒绝删除非普通配置文件",
                ));
            }
            unlinkat(&self.file, name, AtFlags::empty()).map_err(fs_error)?;
            self.file.sync_all()?;
        }
        Ok(())
    }

    fn snapshot_with_codes(
        &self,
        name: &std::ffi::OsStr,
        limit: u64,
        bad_kind: (&str, &str),
        over_limit: (&str, &str),
        changed_code: (&str, &str),
        identity: Option<&crate::domain::FileId>,
    ) -> Result<(Vec<u8>, Metadata)> {
        use rustix::fs::{AtFlags, Mode, OFlags, fstat, openat, statat};
        use std::io::Read;
        component(name)?;
        lexical(&self.path.join(name))?;
        let stat = || statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW).map_err(fs_error);
        let stamp = |s: &rustix::fs::Stat| {
            (
                s.st_dev,
                s.st_ino,
                s.st_size,
                s.st_mtime,
                s.st_mtime_nsec,
                s.st_ctime,
                s.st_ctime_nsec,
            )
        };
        let before = stat()?;
        let initial = metadata(&before)?;
        let changed = || Error::attention(changed_code.0, changed_code.1);
        if identity.is_some_and(|expected| expected != &initial.identity) {
            return Err(changed());
        }
        if initial.kind != EntryKind::File {
            return Err(Error::attention(bad_kind.0, bad_kind.1));
        }
        if number(before.st_size)? > limit {
            return Err(Error::attention(over_limit.0, over_limit.1));
        }
        let fd = openat(
            &self.file,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(fs_error)?;
        let file = std::fs::File::from(fd);
        if stamp(&fstat(&file).map_err(fs_error)?) != stamp(&before) {
            return Err(changed());
        }
        let mut bytes = Vec::new();
        (&file).take(limit).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != number(before.st_size)?
            || stamp(&fstat(&file).map_err(fs_error)?) != stamp(&before)
            || stamp(&stat()?) != stamp(&before)
        {
            return Err(changed());
        }
        Ok((bytes, initial))
    }
}
