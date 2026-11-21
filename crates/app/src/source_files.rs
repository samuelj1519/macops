use crate::{
    capability_store as files,
    error::{Error, Result},
    paths::{Directory, EntryKind},
    policy::{
        capability_tx::{Namespace, State, Witness},
        source_files::{self as policy, Object, Pair},
        source_tree::{Position, Slot},
    },
};
use std::{ffi::OsStr, io::Write, os::unix::fs::MetadataExt};
fn invalid() -> Error {
    policy::invalid()
}
fn io(error: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}
fn namespace(slot: &Slot) -> Namespace {
    Namespace {
        ancestor: slot.parent.clone(),
        remaining: vec![slot.name.clone()],
    }
}
fn stat(parent: &Directory, name: &str) -> Result<Option<rustix::fs::Stat>> {
    match rustix::fs::statat(parent.file(), name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(io(error)),
    }
}
pub fn observe(slot: &Slot) -> Result<Option<Object>> {
    let owner = nix::unistd::geteuid().as_raw();
    slot.validate(owner)?;
    let namespace = namespace(slot);
    let parent = files::open(&namespace)?;
    let before = parent.metadata(OsStr::new(&slot.name))?;
    let first = stat(&parent, &slot.name)?;
    let (state, raw) = files::observe(&namespace, 16 * 1024 * 1024)?;
    let after = parent.metadata(OsStr::new(&slot.name))?;
    let last = stat(&parent, &slot.name)?;
    if before
        .as_ref()
        .map(|meta| (&meta.identity, meta.mode, meta.kind.as_str()))
        != after
            .as_ref()
            .map(|meta| (&meta.identity, meta.mode, meta.kind.as_str()))
    {
        return Err(invalid());
    }
    match (state, raw, after, first, last) {
        (State::Missing, None, None, None, None) => Ok(None),
        (
            State::File {
                identity,
                sha256,
                mode,
            },
            Some(raw),
            Some(meta),
            Some(first),
            Some(last),
        ) if meta.kind == EntryKind::File
            && meta.identity == identity
            && meta.mode == mode
            && first.st_uid == owner
            && last.st_uid == owner
            && first.st_uid == last.st_uid
            && first.st_ino == last.st_ino
            && first.st_dev == last.st_dev
            && first.st_mode == last.st_mode
            && u32::from(last.st_mode) & 0o7777 == mode
            && u64::try_from(last.st_dev).ok() == Some(identity.device)
            && last.st_ino == identity.inode
            && first.st_size == last.st_size
            && u64::try_from(last.st_size).ok() == Some(raw.len() as u64) =>
        {
            parent.verify()?;
            Ok(Some(Object {
                identity,
                uid: owner,
                mode,
                size: raw.len() as u64,
                sha256,
            }))
        }
        _ => Err(invalid()),
    }
}
pub fn position(pair: &Pair) -> Result<Position> {
    pair.validate(nix::unistd::geteuid().as_raw())?;
    Ok(policy::classify(
        pair,
        observe(&pair.live)?.as_ref(),
        observe(&pair.retained)?.as_ref(),
    ))
}
pub fn stage(witness: &Witness, raw: &[u8], mode: u32) -> Result<Pair> {
    if raw.len() > 16 * 1024 * 1024 || mode > 0o777 || witness.namespace.remaining.len() != 1 {
        return Err(invalid());
    }
    let parent = files::open(&witness.namespace)?;
    let live = Slot {
        parent: witness.namespace.ancestor.clone(),
        name: witness.namespace.remaining[0].clone(),
    };
    let before = observe(&live)?;
    if live.path() != witness.path
        || before.as_ref().map_or(State::Missing, Object::state) != witness.state
    {
        return Err(invalid());
    }
    let temporary = tempfile::Builder::new()
        .prefix(".macops-source-ledger-")
        .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .tempdir_in(&parent.path)?;
    let private = Directory::open(&temporary.keep())?;
    parent.file().sync_all()?;
    parent.verify()?;
    let retained = Slot {
        parent: files::binding(&private, &private.path)?,
        name: "ledger".into(),
    };
    let after = write(&private, &retained.name, raw, mode)?;
    let pair = Pair {
        live,
        retained,
        before,
        after,
    };
    pair.validate(nix::unistd::geteuid().as_raw())?;
    if position(&pair)? != Position::Before {
        return Err(invalid());
    }
    Ok(pair)
}
fn write(parent: &Directory, name: &str, raw: &[u8], mode: u32) -> Result<Object> {
    use rustix::fs::{Mode, OFlags};
    parent.verify()?;
    let mut file = std::fs::File::from(
        rustix::fs::openat(
            parent.file(),
            name,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io)?,
    );
    file.write_all(raw)?;
    rustix::fs::fchmod(
        &file,
        Mode::from_raw_mode(mode.try_into().map_err(|_| invalid())?),
    )
    .map_err(io)?;
    file.sync_all()?;
    let meta = file.metadata()?;
    let after = Object {
        identity: crate::domain::FileId::from_file(&file)?,
        uid: meta.uid(),
        mode: meta.mode() & 0o7777,
        size: meta.len(),
        sha256: files::hash(raw),
    };
    parent.file().sync_all()?;
    parent.verify()?;
    Ok(after)
}
pub fn publish_checked(
    pair: &Pair,
    guard: impl FnOnce() -> Result<()>,
) -> Result<crate::source_tree::ExchangeOutcome> {
    move_towards(pair, Position::OwnAfter, guard, || Ok(()))
}
pub fn restore_checked(
    pair: &Pair,
    guard: impl FnOnce() -> Result<()>,
) -> Result<crate::source_tree::ExchangeOutcome> {
    move_towards(pair, Position::Before, guard, || Ok(()))
}
fn move_towards(
    pair: &Pair,
    wanted: Position,
    guard: impl FnOnce() -> Result<()>,
    before_syscall: impl FnOnce() -> Result<()>,
) -> Result<crate::source_tree::ExchangeOutcome> {
    if position(pair)? == Position::Unknown {
        return Err(invalid());
    }
    let live = files::open(&namespace(&pair.live))?;
    let retained = files::open(&namespace(&pair.retained))?;
    guard()?;
    let current = position(pair)?;
    if current == Position::Unknown {
        return Err(invalid());
    }
    if current == wanted {
        live.file().sync_all()?;
        retained.file().sync_all()?;
        if position(pair)? != wanted {
            return Err(invalid());
        }
        return Ok(crate::source_tree::ExchangeOutcome {
            exchanged: false,
            syscall_error: None,
        });
    }
    live.verify()?;
    retained.verify()?;
    before_syscall()?;
    let result = if pair.before.is_some() {
        rustix::fs::renameat_with(
            live.file(),
            &pair.live.name,
            retained.file(),
            &pair.retained.name,
            rustix::fs::RenameFlags::EXCHANGE,
        )
    } else if wanted == Position::OwnAfter {
        rustix::fs::renameat_with(
            retained.file(),
            &pair.retained.name,
            live.file(),
            &pair.live.name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
    } else {
        rustix::fs::renameat_with(
            live.file(),
            &pair.live.name,
            retained.file(),
            &pair.retained.name,
            rustix::fs::RenameFlags::NOREPLACE,
        )
    };
    let syscall_error = result.err().map(rustix::io::Errno::raw_os_error);
    live.file().sync_all()?;
    retained.file().sync_all()?;
    if position(pair)? != wanted {
        return Err(invalid());
    }
    Ok(crate::source_tree::ExchangeOutcome {
        exchanged: true,
        syscall_error,
    })
}

#[cfg(test)]
#[path = "../tests/unit/source_files.rs"]
mod tests;
