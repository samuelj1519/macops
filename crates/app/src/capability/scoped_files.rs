use crate::{
    capability::{capability_store as files, rules::capability_tx::State},
    error::{Error, Result},
    platform::fs::{self, Directory},
};
use std::{
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::Path,
};

pub(crate) fn scope_changed() -> Error {
    Error::attention(
        "native_skills_scope_changed",
        "Native Skills 范围、载荷或未选 owner 证据不匹配",
    )
}
pub(crate) fn regular(path: &Path) -> Result<Option<Vec<u8>>> {
    let (state, raw) = files::observe(&files::namespace(path)?, 16 * 1024 * 1024)?;
    if matches!(state, State::Link { .. }) {
        return Err(scope_changed());
    }
    Ok(raw)
}
pub(crate) fn private(path: &Path) -> Result<Directory> {
    fs::write_path(path)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let directory = Directory::open(path)?;
    let metadata = directory.file().metadata()?;
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700 {
        return Err(scope_changed());
    }
    directory.verify()?;
    Ok(directory)
}
pub(crate) fn write_cas_checked(
    path: &Path,
    before: Option<&[u8]>,
    after: &[u8],
    guard: impl Fn() -> Result<()>,
) -> Result<()> {
    guard()?;
    private(path.parent().ok_or_else(scope_changed)?)?;
    let namespace = files::namespace(path)?;
    let (state, current) = files::observe(&namespace, 16 * 1024 * 1024)?;
    if current.as_deref() != before || matches!(state, State::Link { .. }) {
        return Err(scope_changed());
    }
    let candidate = files::stage(&namespace, Some(after), 0o600)?;
    files::publish_checked(&namespace, &state, &candidate, guard)
}
