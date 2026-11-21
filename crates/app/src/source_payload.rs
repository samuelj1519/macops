use crate::{
    capability_store,
    error::{Error, Result},
    paths::Directory,
    policy::{
        source_git::{Component, ObjectId, RelativePath, TreeKind},
        source_payload::{Entry, Payload, Rules},
        source_tree::{Slot, Tree},
    },
    source_git::Git,
};
use std::{ffi::OsStr, io::Write, os::unix::fs::MetadataExt};
fn invalid() -> Error {
    Error::attention(
        "source_candidate_unverified",
        "Git 子树或私有来源候选无法完整验证",
    )
}
fn io(e: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(e.raw_os_error()).into()
}
pub struct Prepared {
    pub repository: crate::policy::source_git::Repository,
    pub source_path: RelativePath,
    pub skill_tree: ObjectId,
    pub payload: Payload,
}
fn find(git: &Git, root: &ObjectId, path: &RelativePath) -> Result<(ObjectId, TreeKind)> {
    let mut current = root.clone();
    let mut kind = TreeKind::Directory;
    for part in path.parts() {
        if kind != TreeKind::Directory {
            return Err(invalid());
        }
        let entry = git
            .tree(&current)?
            .into_iter()
            .find(|e| e.name == *part)
            .ok_or_else(invalid)?;
        current = entry.object;
        kind = entry.kind;
    }
    Ok((current, kind))
}
fn body(git: &Git, object: &ObjectId, executable: bool, remaining: &mut u64) -> Result<Entry> {
    let bytes = git.blob(object)?;
    if bytes.len() as u64 > *remaining {
        return Err(invalid());
    }
    *remaining -= bytes.len() as u64;
    Ok(Entry::File {
        mode: if executable { 0o755 } else { 0o644 },
        bytes,
    })
}
pub fn directory_object(git: &Git, root: &ObjectId, path: &RelativePath) -> Result<ObjectId> {
    let (object, kind) = find(git, root, path)?;
    if kind != TreeKind::Directory {
        return Err(invalid());
    }
    Ok(object)
}
pub fn read_file(git: &Git, root: &ObjectId, path: &RelativePath) -> Result<Vec<u8>> {
    let (object, kind) = find(git, root, path)?;
    if !matches!(kind, TreeKind::File { .. }) {
        return Err(invalid());
    }
    let bytes = git.blob(&object)?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid());
    }
    Ok(bytes)
}
fn walk(
    git: &Git,
    tree: &ObjectId,
    prefix: &str,
    rules: &Rules,
    remaining: &mut u64,
    payload: &mut Payload,
) -> Result<()> {
    for entry in git.tree(tree)? {
        let path = if prefix.is_empty() {
            entry.name.as_str().to_owned()
        } else {
            format!("{prefix}/{}", entry.name.as_str())
        };
        RelativePath::parse(&path)?;
        if rules.excludes(&path) {
            continue;
        }
        if payload.entries.len() >= 10000 {
            return Err(invalid());
        }
        match entry.kind {
            TreeKind::Directory => {
                payload
                    .entries
                    .insert(path.clone(), Entry::Directory { mode: 0o755 });
                walk(git, &entry.object, &path, rules, remaining, payload)?;
            }
            TreeKind::File { executable } => {
                payload
                    .entries
                    .insert(path, body(git, &entry.object, executable, remaining)?);
            }
            TreeKind::Symlink | TreeKind::Submodule => return Err(invalid()),
        }
    }
    Ok(())
}
pub fn read(git: &Git, root: &ObjectId, rules: &Rules) -> Result<Prepared> {
    git.require_repository(&rules.repository)?;
    let skill_tree = directory_object(git, root, &rules.path)?;
    let (_, entrypoint) = find(git, &skill_tree, &RelativePath::parse("SKILL.md")?)?;
    if !matches!(entrypoint, TreeKind::File { .. }) {
        return Err(invalid());
    }
    let mut remaining = 64 * 1024 * 1024;
    let mut payload = Payload::default();
    walk(git, &skill_tree, "", rules, &mut remaining, &mut payload)?;
    let mut licenses = vec![];
    for path in &rules.licenses {
        let (object, kind) = find(git, root, path)?;
        let TreeKind::File { executable } = kind else {
            return Err(invalid());
        };
        let name = path
            .parts()
            .iter()
            .map(Component::as_str)
            .collect::<Vec<_>>()
            .join("/");
        licenses.push((name, body(git, &object, executable, &mut remaining)?));
    }
    payload.transform(rules, &licenses)?;
    Ok(Prepared {
        repository: rules.repository.clone(),
        source_path: rules.path.clone(),
        skill_tree,
        payload,
    })
}
fn child(root: &Directory, path: &str) -> Result<(Directory, String)> {
    let relative = RelativePath::parse(path)?;
    let (name, parents) = relative.parts().split_last().ok_or_else(invalid)?;
    let mut directory = root.clone();
    for part in parents {
        directory = directory.child(OsStr::new(part.as_str()))?;
    }
    Ok((directory, name.as_str().into()))
}
fn write_entry(root: &Directory, path: &str, entry: &Entry) -> Result<()> {
    use rustix::fs::{Mode, OFlags};
    root.verify()?;
    let (parent, name) = child(root, path)?;
    parent.verify()?;
    match entry {
        Entry::Directory { .. } => {
            rustix::fs::mkdirat(parent.file(), name.as_str(), Mode::RWXU).map_err(io)?;
        }
        Entry::File { mode, bytes } => {
            let mut file = std::fs::File::from(
                rustix::fs::openat(
                    parent.file(),
                    name.as_str(),
                    OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::WRONLY
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::RUSR | Mode::WUSR,
                )
                .map_err(io)?,
            );
            file.write_all(bytes)?;
            rustix::fs::fchmod(
                &file,
                Mode::from_raw_mode((*mode).try_into().map_err(|_| invalid())?),
            )
            .map_err(io)?;
            file.sync_all()?;
        }
    }
    parent.file().sync_all()?;
    parent.verify()?;
    root.verify()
}
pub fn materialize(
    parent: &Directory,
    name: &str,
    payload: &Payload,
    mode: u32,
) -> Result<(Slot, Tree)> {
    payload.validate()?;
    materialize_tree(parent, name, payload, mode)
}

pub(crate) fn materialize_tree(
    parent: &Directory,
    name: &str,
    payload: &Payload,
    mode: u32,
) -> Result<(Slot, Tree)> {
    let owner = nix::unistd::geteuid().as_raw();
    Component::parse(name)?;
    let expected = payload.shape(owner, mode);
    expected.validate(owner)?;
    let meta = parent.file().metadata()?;
    if meta.uid() != owner || meta.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    crate::paths::write_path(&parent.path.join(name))?;
    parent.verify()?;
    rustix::fs::mkdirat(parent.file(), name, rustix::fs::Mode::RWXU).map_err(io)?;
    parent.file().sync_all()?;
    let root = parent.child(OsStr::new(name))?;
    for (path, entry) in &payload.entries {
        write_entry(&root, path, entry)?;
    }
    for (path, entry) in payload.entries.iter().rev() {
        if let Entry::Directory { mode } = entry {
            let (parent, name) = child(&root, path)?;
            let dir = parent.child(OsStr::new(&name))?;
            rustix::fs::fchmod(
                dir.file(),
                rustix::fs::Mode::from_raw_mode((*mode).try_into().map_err(|_| invalid())?),
            )
            .map_err(io)?;
            dir.file().sync_all()?;
        }
    }
    rustix::fs::fchmod(
        root.file(),
        rustix::fs::Mode::from_raw_mode(mode.try_into().map_err(|_| invalid())?),
    )
    .map_err(io)?;
    root.file().sync_all()?;
    let slot = Slot {
        parent: capability_store::binding(parent, &parent.path)?,
        name: name.into(),
    };
    let tree = crate::source_tree::observe(&slot)?.ok_or_else(invalid)?;
    if tree.shape() != expected {
        return Err(invalid());
    }
    parent.verify()?;
    root.verify()?;
    Ok((slot, tree))
}
#[cfg(test)]
#[path = "../tests/unit/source_payload.rs"]
mod tests;
