use crate::{
    capability::{capability_guard, capability_store, rules::capability_tx::Namespace},
    error::{Error, Result},
    platform::fs::{Directory, EntryKind},
    policy::{
        source_git::Component,
        source_tree::{self, Change, Content, Entry, Position, Slot, Tree},
    },
};
use std::{collections::BTreeMap, ffi::OsStr, os::unix::fs::MetadataExt};

fn changed() -> Error {
    Error::attention(
        "source_tree_changed",
        "来源目录已改变或不能证明属于冻结前态/自身候选",
    )
}
fn parent(slot: &Slot) -> Result<Directory> {
    slot.validate(nix::unistd::geteuid().as_raw())?;
    capability_store::open(&Namespace {
        ancestor: slot.parent.clone(),
        remaining: vec![slot.name.clone()],
    })
}
fn uid(dir: &Directory, name: &str) -> Result<u32> {
    let stat = rustix::fs::statat(dir.file(), name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
    Ok(stat.st_uid)
}
pub fn observe(slot: &Slot) -> Result<Option<Tree>> {
    observe_tree(slot, &mut None)
}

type Links = BTreeMap<String, (crate::domain::FileId, u32, Vec<u8>)>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TreeStamp {
    pub digest: String,
    pub entries: usize,
    pub bytes: u64,
}

pub(crate) fn observe_protected(slot: &Slot) -> Result<Option<TreeStamp>> {
    let mut links = Some(Links::new());
    let Some(tree) = observe_tree(slot, &mut links)? else {
        return Ok(None);
    };
    let links = links.ok_or_else(changed)?;
    let bytes = tree
        .entries
        .values()
        .filter_map(|entry| match entry.content {
            Content::File { size, .. } => Some(size),
            Content::Directory => None,
        })
        .sum();
    Ok(Some(TreeStamp {
        digest: crate::domain::fingerprint(&(&tree, &links))?,
        entries: tree.entries.len() + links.len(),
        bytes,
    }))
}

fn observe_tree(slot: &Slot, links: &mut Option<Links>) -> Result<Option<Tree>> {
    let parent = parent(slot)?;
    let name = OsStr::new(&slot.name);
    let Some(before) = parent.metadata(name)? else {
        parent.verify()?;
        return Ok(None);
    };
    if before.kind != EntryKind::Directory {
        return Err(changed());
    }
    let root = parent.child(name)?;
    parent.verify_child(name, &root)?;
    let meta = root.file().metadata()?;
    let mut tree = Tree {
        identity: root.identity().clone(),
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
        entries: BTreeMap::new(),
    };
    if tree.uid != nix::unistd::geteuid().as_raw() || tree.mode > 0o777 || tree.mode & 0o022 != 0 {
        return Err(changed());
    }
    let mut remaining = 64 * 1024 * 1024;
    walk(
        &root,
        "",
        0,
        tree.identity.device,
        &mut remaining,
        &mut tree.entries,
        links,
    )?;
    parent.verify_child(name, &root)?;
    root.verify()?;
    parent.verify()?;
    let after = root.file().metadata()?;
    if after.uid() != tree.uid || after.mode() & 0o7777 != tree.mode {
        return Err(changed());
    }
    tree.validate(nix::unistd::geteuid().as_raw())?;
    Ok(Some(tree))
}
fn walk(
    dir: &Directory,
    prefix: &str,
    depth: usize,
    device: u64,
    remaining: &mut u64,
    entries: &mut BTreeMap<String, Entry>,
    links: &mut Option<Links>,
) -> Result<()> {
    if depth > 32 {
        return Err(changed());
    }
    let listing = capability_guard::listing(dir)?;
    for (name, before) in &listing {
        Component::parse(name)?;
        if entries.len() + links.as_ref().map_or(0, Links::len) >= 10000
            || before.identity.device != device
        {
            return Err(changed());
        }
        let owner = uid(dir, name)?;
        if owner != nix::unistd::geteuid().as_raw()
            || before.mode > 0o777
            || (before.kind == "directory" && before.mode & 0o022 != 0)
        {
            return Err(changed());
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let content = match before.kind.as_str() {
            "directory" => {
                let child = dir.child(OsStr::new(name))?;
                entries.insert(
                    relative.clone(),
                    Entry {
                        identity: before.identity.clone(),
                        uid: owner,
                        mode: before.mode,
                        content: Content::Directory,
                    },
                );
                walk(
                    &child,
                    &relative,
                    depth + 1,
                    device,
                    remaining,
                    entries,
                    links,
                )?;
                dir.verify_child(OsStr::new(name), &child)?;
                if uid(dir, name)? != owner {
                    return Err(changed());
                }
                continue;
            }
            "file" => {
                let (bytes, verified) = dir.regular_config_snapshot(
                    OsStr::new(name),
                    (*remaining).min(16 * 1024 * 1024),
                )?;
                if verified.identity != before.identity
                    || verified.mode != before.mode
                    || uid(dir, name)? != owner
                {
                    return Err(changed());
                }
                *remaining = remaining
                    .checked_sub(bytes.len() as u64)
                    .ok_or_else(changed)?;
                Content::File {
                    sha256: capability_store::hash(&bytes),
                    size: bytes.len() as u64,
                }
            }
            "symlink" if let Some(links) = links => {
                let target = rustix::fs::readlinkat(dir.file(), OsStr::new(name), Vec::new())
                    .map_err(std::io::Error::from)?;
                links.insert(
                    relative,
                    (before.identity.clone(), before.mode, target.into_bytes()),
                );
                continue;
            }
            _ => return Err(changed()),
        };
        entries.insert(
            relative,
            Entry {
                identity: before.identity.clone(),
                uid: owner,
                mode: before.mode,
                content,
            },
        );
    }
    if capability_guard::listing(dir)? != listing {
        return Err(changed());
    }
    dir.verify()?;
    Ok(())
}
pub fn position(change: &Change) -> Result<Position> {
    source_tree::validate_changes(
        std::slice::from_ref(change),
        nix::unistd::geteuid().as_raw(),
    )?;
    let live = observe(&change.live)?;
    let retained = observe(&change.retained)?;
    Ok(source_tree::classify(
        change,
        live.as_ref(),
        retained.as_ref(),
    ))
}
#[derive(Debug)]
pub struct ExchangeOutcome {
    pub exchanged: bool,
    pub syscall_error: Option<i32>,
}
pub fn publish_checked(
    change: &Change,
    guard: impl FnOnce() -> Result<()>,
) -> Result<ExchangeOutcome> {
    move_towards(change, Position::OwnAfter, guard)
}
pub fn restore_checked(
    change: &Change,
    guard: impl FnOnce() -> Result<()>,
) -> Result<ExchangeOutcome> {
    move_towards(change, Position::Before, guard)
}
fn move_towards(
    change: &Change,
    wanted: Position,
    guard: impl FnOnce() -> Result<()>,
) -> Result<ExchangeOutcome> {
    let current = position(change)?;
    let live = parent(&change.live)?;
    let retained = parent(&change.retained)?;
    if current == Position::Unknown {
        return Err(changed());
    }
    if current == wanted {
        live.file().sync_all()?;
        retained.file().sync_all()?;
        return Ok(ExchangeOutcome {
            exchanged: false,
            syscall_error: None,
        });
    }
    live.verify()?;
    retained.verify()?;
    guard()?;
    let result = rustix::fs::renameat_with(
        live.file(),
        &change.live.name,
        retained.file(),
        &change.retained.name,
        rustix::fs::RenameFlags::EXCHANGE,
    );
    let error = result.err().map(rustix::io::Errno::raw_os_error);
    live.file().sync_all()?;
    retained.file().sync_all()?;
    if position(change)? != wanted {
        return Err(changed());
    }
    Ok(ExchangeOutcome {
        exchanged: true,
        syscall_error: error,
    })
}

#[cfg(test)]
#[path = "../../tests/unit/source_tree.rs"]
mod tests;
