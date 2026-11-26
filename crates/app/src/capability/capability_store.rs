use crate::{
    capability::rules::capability_tx::{Blob, Namespace, State},
    error::{Error, Result},
    operations::store::Store,
    platform::fs::{Directory, EntryKind},
    policy::governance::DirectoryBinding,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn changed() -> Error {
    Error::attention("capability_namespace_changed", "能力包事务命名空间身份改变")
}
fn io(error: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}

pub fn binding(root: &Directory, requested: &Path) -> Result<DirectoryBinding> {
    let meta = root.file().metadata()?;
    Ok(DirectoryBinding {
        requested: requested.into(),
        canonical: root.path.clone(),
        identity: root.identity().clone(),
        lineage: root.lineage()?,
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
    })
}

pub fn namespace(path: &Path) -> Result<Namespace> {
    let path = crate::policy::paths::lexical(path)?;
    let mut parent = path.parent().ok_or_else(changed)?.to_path_buf();
    loop {
        match Directory::open(&parent) {
            Ok(root) => {
                let tail = path.strip_prefix(&parent).map_err(|_| changed())?;
                let remaining = tail
                    .components()
                    .map(|part| {
                        part.as_os_str()
                            .to_str()
                            .map(str::to_owned)
                            .ok_or_else(changed)
                    })
                    .collect::<Result<Vec<_>>>()?;
                return Ok(Namespace {
                    ancestor: binding(&root, &parent)?,
                    remaining,
                });
            }
            Err(error) if error.code == "path_missing" => {
                parent = parent.parent().ok_or_else(changed)?.to_path_buf();
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn open(namespace: &Namespace) -> Result<Directory> {
    let root = Directory::open(&namespace.ancestor.requested)?;
    let meta = root.file().metadata()?;
    if root.path != namespace.ancestor.canonical
        || root.identity() != &namespace.ancestor.identity
        || root.lineage()? != namespace.ancestor.lineage
        || meta.uid() != namespace.ancestor.uid
        || meta.mode() & 0o7777 != namespace.ancestor.mode
    {
        return Err(changed());
    }
    root.verify()?;
    Ok(root)
}

pub fn parent(namespace: &Namespace) -> Result<Option<Directory>> {
    let mut root = open(namespace)?;
    let (_, parents) = namespace.remaining.split_last().ok_or_else(changed)?;
    for name in parents {
        match root.child(OsStr::new(name)) {
            Ok(child) => root = child,
            Err(error) if error.code == "path_missing" => return Ok(None),
            Err(error) => return Err(error),
        }
    }
    Ok(Some(root))
}

pub fn observe(namespace: &Namespace, limit: u64) -> Result<(State, Option<Vec<u8>>)> {
    let Some(root) = parent(namespace)? else {
        return Ok((State::Missing, None));
    };
    let name = namespace.remaining.last().ok_or_else(changed)?;
    let Some(meta) = root.metadata(OsStr::new(name))? else {
        root.verify()?;
        return Ok((State::Missing, None));
    };
    match meta.kind {
        EntryKind::File => {
            let (bytes, verified) = root.regular_config_snapshot(OsStr::new(name), limit)?;
            root.verify()?;
            Ok((
                State::File {
                    identity: verified.identity,
                    sha256: hash(&bytes),
                    mode: verified.mode,
                },
                Some(bytes),
            ))
        }
        EntryKind::Symlink => {
            let text =
                rustix::fs::readlinkat(root.file(), name.as_str(), Vec::new()).map_err(io)?;
            let text = crate::capability::rules::delivery::LinkText::from_bytes(text.as_bytes())?;
            if root.metadata(OsStr::new(name))?.is_none_or(|next| {
                next.identity != meta.identity || next.kind != meta.kind || next.mode != meta.mode
            }) {
                return Err(changed());
            }
            root.verify()?;
            Ok((
                State::Link {
                    identity: meta.identity,
                    text: text.clone(),
                    mode: Some(meta.mode),
                },
                Some(text.as_str().as_bytes().to_vec()),
            ))
        }
        _ => Err(Error::attention(
            "unsupported_capability_target",
            "事务目标不是普通文件或明确链接",
        )),
    }
}

pub fn cas(namespace: &Namespace, expected: &State) -> Result<()> {
    if observe(namespace, 64 * 1024 * 1024)?.0 != *expected {
        return Err(Error::attention(
            "capability_target_changed",
            "事务目标被外部修改，保留其内容",
        ));
    }
    Ok(())
}

pub struct Blobs {
    pub directory: Directory,
}
impl Blobs {
    pub fn create(root: &Directory) -> Result<Self> {
        rustix::fs::mkdirat(root.file(), "blobs", rustix::fs::Mode::RWXU).map_err(io)?;
        root.file().sync_all()?;
        let directory = root.child(OsStr::new("blobs"))?;
        Ok(Self { directory })
    }
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            directory: Directory::open(path)?,
        })
    }
    pub fn put(&self, bytes: &[u8]) -> Result<Blob> {
        let blob = Blob {
            sha256: hash(bytes),
            size: bytes.len() as u64,
        };
        if self.directory.metadata(OsStr::new(&blob.sha256))?.is_some() {
            if self.get(&blob)? != bytes {
                return Err(Error::new("capability_blob_conflict", "不可变备份内容冲突"));
            }
            return Ok(blob);
        }
        Store::atomic_bytes(self.directory.file(), &blob.sha256, bytes, true)?;
        Ok(blob)
    }
    pub fn get(&self, blob: &Blob) -> Result<Vec<u8>> {
        if !crate::capability::rules::capability_tx::blob_valid(blob) {
            return Err(Error::new("invalid_capability_blob", "备份摘要或大小无效"));
        }
        let (bytes, metadata) = self
            .directory
            .regular_config_snapshot(OsStr::new(&blob.sha256), blob.size)?;
        if bytes.len() as u64 != blob.size || hash(&bytes) != blob.sha256 || metadata.mode != 0o600
        {
            return Err(Error::new("invalid_capability_blob", "不可变备份校验失败"));
        }
        Ok(bytes)
    }
}

pub fn target_path(namespace: &Namespace) -> PathBuf {
    namespace
        .remaining
        .iter()
        .fold(namespace.ancestor.requested.clone(), |path, name| {
            path.join(name)
        })
}

pub fn stage(
    namespace: &Namespace,
    bytes: Option<&[u8]>,
    mode: u32,
) -> Result<crate::capability::rules::capability_tx::Candidate> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Write;
    let target = target_path(namespace);
    let Some(bytes) = bytes else {
        return Ok(crate::capability::rules::capability_tx::Candidate {
            target,
            staged: None,
            after: State::Missing,
        });
    };
    let root = parent(namespace)?
        .ok_or_else(|| Error::attention("capability_parent_missing", "事务父目录尚未准备"))?;
    let name = format!(".{}.cap.tmp", crate::operations::store::operation_id());
    let mut file = std::fs::File::from(
        rustix::fs::openat(
            root.file(),
            name.as_str(),
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io)?,
    );
    file.write_all(bytes)?;
    rustix::fs::fchmod(
        &file,
        Mode::from_raw_mode(mode.try_into().map_err(|_| changed())?),
    )
    .map_err(io)?;
    file.sync_all()?;
    root.file().sync_all()?;
    let staged = root.path.join(&name);
    let after = observe(&self::namespace(&staged)?, bytes.len() as u64)?.0;
    root.verify()?;
    Ok(crate::capability::rules::capability_tx::Candidate {
        target,
        staged: Some(staged),
        after,
    })
}

pub fn publish(
    namespace: &Namespace,
    before: &State,
    candidate: &crate::capability::rules::capability_tx::Candidate,
) -> Result<()> {
    publish_checked(namespace, before, candidate, || Ok(()))
}
pub fn publish_checked(
    namespace: &Namespace,
    before: &State,
    candidate: &crate::capability::rules::capability_tx::Candidate,
    check: impl FnOnce() -> Result<()>,
) -> Result<()> {
    crate::platform::fs::write_path(&candidate.target)?;
    if target_path(namespace) != candidate.target {
        return Err(changed());
    }
    let root = parent(namespace)?.ok_or_else(changed)?;
    let name = namespace.remaining.last().ok_or_else(changed)?;
    if let Some(staged) = &candidate.staged {
        let ns = self::namespace(staged)?;
        if staged.parent() != Some(root.path.as_path())
            || observe(&ns, 64 * 1024 * 1024)?.0 != candidate.after
        {
            return Err(changed());
        }
        let leaf = staged.file_name().ok_or_else(changed)?;
        cas(namespace, before)?;
        check()?;
        if *before == State::Missing {
            rustix::fs::renameat_with(
                root.file(),
                leaf,
                root.file(),
                name.as_str(),
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io)?;
        } else {
            rustix::fs::renameat(root.file(), leaf, root.file(), name.as_str()).map_err(io)?;
        }
    } else {
        if candidate.after != State::Missing {
            return Err(changed());
        }
        cas(namespace, before)?;
        check()?;
        if *before != State::Missing {
            rustix::fs::unlinkat(root.file(), name.as_str(), rustix::fs::AtFlags::empty())
                .map_err(io)?;
        }
    }
    root.file().sync_all()?;
    root.verify()?;
    Ok(())
}

pub fn prepare_parent(
    namespace: &Namespace,
) -> Result<Option<crate::capability::rules::capability_tx::ParentCandidate>> {
    if parent(namespace)?.is_some() {
        return Ok(None);
    }
    if namespace.remaining.len() != 2 {
        return Err(Error::attention(
            "unsupported_capability_parent",
            "只允许创建一个明确元数据父目录",
        ));
    }
    let ancestor = open(namespace)?;
    let name = format!(".{}.cap-dir", crate::operations::store::operation_id());
    rustix::fs::mkdirat(ancestor.file(), name.as_str(), rustix::fs::Mode::RWXU).map_err(io)?;
    ancestor.file().sync_all()?;
    let root = ancestor.child(OsStr::new(&name))?;
    let staged = root.path.clone();
    Ok(Some(
        crate::capability::rules::capability_tx::ParentCandidate {
            target: target_path(namespace).parent().ok_or_else(changed)?.into(),
            binding: binding(&root, &staged)?,
            staged,
        },
    ))
}

pub fn parent_published(
    candidate: &crate::capability::rules::capability_tx::ParentCandidate,
) -> Result<bool> {
    match Directory::open(&candidate.target) {
        Ok(root) if root.identity() == &candidate.binding.identity => {
            let actual = binding(&root, &candidate.target)?;
            if actual.mode != candidate.binding.mode
                || actual.uid != candidate.binding.uid
                || actual.lineage != candidate.binding.lineage
            {
                return Err(changed());
            }
            Ok(true)
        }
        Ok(_) => Err(changed()),
        Err(error) if error.code == "path_missing" => Ok(false),
        Err(error) => Err(error),
    }
}

pub fn publish_parent(
    candidate: &crate::capability::rules::capability_tx::ParentCandidate,
) -> Result<()> {
    crate::platform::fs::write_path(&candidate.target)?;
    if parent_published(candidate)? {
        return Ok(());
    }
    let ns = self::namespace(&candidate.staged.join(".probe"))?;
    if ns.ancestor != candidate.binding {
        return Err(changed());
    }
    open(&ns)?;
    let target = self::namespace(&candidate.target)?;
    cas(&target, &State::Missing)?;
    let parent = self::parent(&target)?.ok_or_else(changed)?;
    let source = candidate.staged.file_name().ok_or_else(changed)?;
    let leaf = candidate.target.file_name().ok_or_else(changed)?;
    rustix::fs::renameat_with(
        parent.file(),
        source,
        parent.file(),
        leaf,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io)?;
    parent.file().sync_all()?;
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/capability_store.rs"]
mod tests;

pub fn stage_link(
    namespace: &Namespace,
    text: &crate::capability::rules::delivery::LinkText,
) -> Result<crate::capability::rules::capability_tx::Candidate> {
    let root = parent(namespace)?.ok_or_else(changed)?;
    let name = format!(".{}.cap.tmp", crate::operations::store::operation_id());
    rustix::fs::symlinkat(text.as_str(), root.file(), name.as_str()).map_err(io)?;
    root.file().sync_all()?;
    let staged = root.path.join(&name);
    let after = observe(&self::namespace(&staged)?, 4096)?.0;
    root.verify()?;
    Ok(crate::capability::rules::capability_tx::Candidate {
        target: target_path(namespace),
        staged: Some(staged),
        after,
    })
}
pub fn publish_link(
    home: &Path,
    namespace: &Namespace,
    before: &State,
    candidate: &crate::capability::rules::capability_tx::Candidate,
) -> Result<()> {
    use crate::capability::rules::delivery::{LinkText, link_target_parts};
    let target = target_path(namespace);
    let (_, name) = link_target_parts(home, &target)?;
    let expected = LinkText::managed(&name)?;
    if candidate.target != target {
        return Err(changed());
    }
    let allowed = |state: &State| match state {
        State::Missing => true,
        State::Link {
            text,
            mode: Some(_),
            ..
        } => *text == expected,
        _ => false,
    };
    if !allowed(before) || !allowed(&candidate.after) {
        return Err(changed());
    }
    crate::platform::fs::write_path(target.parent().ok_or_else(changed)?)?;
    let root = parent(namespace)?.ok_or_else(changed)?;
    let leaf = target.file_name().ok_or_else(changed)?;
    if let Some(staged) = &candidate.staged {
        if staged.parent() != Some(root.path.as_path())
            || !matches!(candidate.after, State::Link { mode: Some(_), .. })
            || observe(&self::namespace(staged)?, 4096)?.0 != candidate.after
        {
            return Err(changed());
        }
        let source = staged.file_name().ok_or_else(changed)?;
        cas(namespace, before)?;
        if *before == State::Missing {
            rustix::fs::renameat_with(
                root.file(),
                source,
                root.file(),
                leaf,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(io)?;
        } else {
            rustix::fs::renameat(root.file(), source, root.file(), leaf).map_err(io)?;
        }
    } else {
        if candidate.after != State::Missing {
            return Err(changed());
        }
        cas(namespace, before)?;
        if *before != State::Missing {
            rustix::fs::unlinkat(root.file(), leaf, rustix::fs::AtFlags::empty()).map_err(io)?;
        }
    }
    root.file().sync_all()?;
    root.verify()?;
    Ok(())
}

pub fn remove_link_stage(namespace: &Namespace, expected: &State) -> Result<()> {
    let target = target_path(namespace);
    let name = target
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(changed)?;
    if !name
        .strip_prefix('.')
        .and_then(|n| n.strip_suffix(".cap.tmp"))
        .is_some_and(crate::policy::operation::state::valid_id)
        || !matches!(expected, State::Link { mode: Some(_), .. })
    {
        return Err(changed());
    }
    crate::platform::fs::write_path(target.parent().ok_or_else(changed)?)?;
    let parent = parent(namespace)?.ok_or_else(changed)?;
    cas(namespace, expected)?;
    rustix::fs::unlinkat(parent.file(), name, rustix::fs::AtFlags::empty()).map_err(io)?;
    parent.file().sync_all()?;
    Ok(())
}
