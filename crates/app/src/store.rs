use crate::{
    context::Context,
    domain::{MiseQuery, Observation, QuerySpec, Receipts},
    error::{Error, Result},
    paths,
};
use fs2::FileExt;
use rustix::fs::{Mode, OFlags};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    ffi::OsStr,
    fs::File,
    io::Write,
    os::unix::fs::MetadataExt,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

pub use crate::domain::{Journal, Phase};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
pub fn operation_id() -> String {
    format!(
        "OP-{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
        std::process::id()
    )
}

fn namespace_changed() -> Error {
    Error::attention(
        "state_namespace_changed",
        "HOME、状态目录、日志目录或写锁身份改变，保留现场",
    )
}
#[derive(Debug)]
struct DirectoryGuard {
    directory: paths::Directory,
    lineage: Vec<crate::domain::FileId>,
    uid: u32,
    mode: u32,
}
impl DirectoryGuard {
    fn capture(directory: paths::Directory) -> Result<Self> {
        let meta = directory.file().metadata()?;
        let guard = Self {
            lineage: directory.lineage()?,
            uid: meta.uid(),
            mode: meta.mode() & 0o7777,
            directory,
        };
        guard.verify()?;
        Ok(guard)
    }
    fn verify(&self) -> Result<()> {
        self.directory.verify().map_err(|_| namespace_changed())?;
        let meta = self.directory.file().metadata()?;
        if self.directory.lineage()? != self.lineage
            || meta.uid() != self.uid
            || meta.mode() & 0o7777 != self.mode
        {
            return Err(namespace_changed());
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct Store {
    pub root: File,
    pub operations: File,
    lock: File,
    home_guard: DirectoryGuard,
    state_guard: DirectoryGuard,
    operation_guard: DirectoryGuard,
    lock_uid: u32,
    lock_mode: u32,
    pub path: PathBuf,
}
fn exclusive(file: &File) -> Result<()> {
    file.try_lock_exclusive().map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Error::attention("operation_busy", "另一个 macops 写操作仍在运行")
        } else {
            error.into()
        }
    })
}

fn io_error(e: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(e.raw_os_error()).into()
}

#[derive(Debug)]
pub enum CommitFailure {
    Check(Error),
    Outcome(Error),
}

impl CommitFailure {
    fn into_error(self) -> Error {
        match self {
            Self::Check(error) | Self::Outcome(error) => error,
        }
    }
}

impl Store {
    pub fn native_home_keeper(
        &self,
    ) -> Result<(File, crate::policy::governance::DirectoryBinding)> {
        self.verify_namespace()?;
        let file = self.home_guard.directory.file().try_clone()?;
        let binding = crate::capability_store::binding(
            &self.home_guard.directory,
            &self.home_guard.directory.path,
        )?;
        self.verify_namespace()?;
        Ok((file, binding))
    }
    pub fn lock(ctx: &Context) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let home = paths::Directory::open(&paths::write_path(&ctx.home)?)?;
        exclusive(home.file())?;
        let home_guard = DirectoryGuard::capture(home)?;
        let safe = paths::write_path(&ctx.state)?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&safe)?;
        let state = paths::Directory::open(&safe)?;
        if state.identity() != home_guard.directory.identity() {
            exclusive(state.file())?;
        }
        let state_guard = DirectoryGuard::capture(state)?;
        let root = state_guard.directory.file().try_clone()?;
        if state_guard
            .directory
            .metadata(std::ffi::OsStr::new("write.lock"))?
            .is_some_and(|m| m.kind != paths::EntryKind::File)
        {
            return Err(Error::attention("invalid_state_lock", "写锁必须是普通文件"));
        }
        let lock = File::from(
            rustix::fs::openat(
                &root,
                "write.lock",
                OFlags::CREATE
                    | OFlags::RDWR
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io_error)?,
        );
        let meta = lock.metadata()?;
        if !meta.is_file() {
            return Err(Error::attention("invalid_state_lock", "写锁必须是普通文件"));
        }
        exclusive(&lock)?;
        match rustix::fs::mkdirat(&root, "operations", Mode::RWXU) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => (),
            Err(error) => return Err(io_error(error)),
        }
        let directory = state_guard
            .directory
            .child(std::ffi::OsStr::new("operations"))?;
        let operations = directory.file().try_clone()?;
        let operation_guard = DirectoryGuard::capture(directory)?;
        let store = Self {
            root,
            operations,
            lock,
            home_guard,
            state_guard,
            operation_guard,
            lock_uid: meta.uid(),
            lock_mode: meta.mode() & 0o7777,
            path: safe,
        };
        store.verify_namespace()?;
        Ok(store)
    }
    pub fn verify_namespace(&self) -> Result<()> {
        for guard in [&self.home_guard, &self.state_guard, &self.operation_guard] {
            guard.verify()?;
        }
        let current = self
            .state_guard
            .directory
            .metadata(std::ffi::OsStr::new("write.lock"))?
            .ok_or_else(namespace_changed)?;
        let held = self.lock.metadata()?;
        if current.kind != paths::EntryKind::File
            || current.identity != crate::domain::FileId::from_file(&self.lock)?
            || current.mode != self.lock_mode
            || held.uid() != self.lock_uid
            || held.mode() & 0o7777 != self.lock_mode
        {
            return Err(namespace_changed());
        }
        Ok(())
    }
    pub fn atomic<T: Serialize>(directory: &File, name: &str, value: &T) -> Result<()> {
        Self::atomic_mode(directory, name, value, false)
    }

    pub fn atomic_new<T: Serialize>(directory: &File, name: &str, value: &T) -> Result<()> {
        Self::atomic_mode(directory, name, value, true)
    }

    fn atomic_mode<T: Serialize>(
        directory: &File,
        name: &str,
        value: &T,
        new_only: bool,
    ) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        Self::atomic_bytes(directory, name, &bytes, new_only)
    }

    pub fn atomic_bytes(directory: &File, name: &str, bytes: &[u8], new_only: bool) -> Result<()> {
        Self::atomic_bytes_with_check(directory, name, bytes, new_only, || Ok(()))
    }

    pub(crate) fn atomic_bytes_with_check(
        directory: &File,
        name: &str,
        bytes: &[u8],
        new_only: bool,
        check: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let temporary = format!(".{}.tmp", operation_id());
        let mut file = File::from(
            rustix::fs::openat(
                directory,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(io_error)?,
        );
        let result = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            check()?;
            if new_only {
                rustix::fs::renameat_with(
                    directory,
                    temporary.as_str(),
                    directory,
                    name,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
                .map_err(|e| {
                    if e == rustix::io::Errno::EXIST {
                        Error::attention("already_exists", name)
                    } else {
                        io_error(e)
                    }
                })?;
            } else {
                rustix::fs::renameat(directory, temporary.as_str(), directory, name)
                    .map_err(io_error)?;
            }
            directory.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ =
                rustix::fs::unlinkat(directory, temporary.as_str(), rustix::fs::AtFlags::empty());
        }
        result
    }

    pub fn read<T: DeserializeOwned>(directory: &File, name: &str) -> Result<Option<T>> {
        let fd = match rustix::fs::openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(v) => v,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io_error(e)),
        };
        let file = File::from(fd);
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() > 16 * 1024 * 1024 {
            return Err(Error::new("invalid_state_file", name));
        }
        Ok(Some(serde_json::from_reader(file)?))
    }

    fn save_part(&self, directory: &File, name: &str, journal: &Journal) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(journal)?;
        bytes.push(b'\n');
        Self::atomic_bytes_with_check(directory, name, &bytes, false, || self.verify_namespace())?;
        self.verify_namespace()
    }
    fn save_pair(&self, journal: &Journal, between: impl FnOnce() -> Result<()>) -> Result<()> {
        self.verify_namespace()?;
        let history = format!("{}.json", journal.id);
        if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
            self.save_part(&self.operations, &history, journal)?;
            between()?;
            self.save_part(&self.root, "active.json", journal)
        } else {
            self.save_part(&self.root, "active.json", journal)?;
            between()?;
            self.save_part(&self.operations, &history, journal)
        }
    }
    pub fn save(&self, journal: &Journal) -> Result<()> {
        self.save_pair(journal, || Ok(()))
    }

    pub(crate) fn publish_existing_terminal(&self, active: &Journal) -> Result<Option<Journal>> {
        const LIMIT: u64 = 16 * 1024 * 1024;
        let conflict =
            || Error::attention("operation_conflict", "原操作的终态记录或活动记录已改变");
        self.verify_namespace()?;
        let operations = &self.operation_guard.directory;
        let state = &self.state_guard.directory;
        let history_name = format!("{}.json", active.id);
        let history_name = OsStr::new(&history_name);
        let active_name = OsStr::new("active.json");
        if operations.metadata(history_name)?.is_none() {
            return Ok(None);
        }
        let history = operations.regular_config_snapshot(history_name, LIMIT)?;
        let decode = |bytes: &[u8]| -> Result<Journal> {
            let crate::policy::json::Object(journal) =
                serde_json::from_value(crate::policy::json::parse(bytes)?)?;
            validate_journal(&journal)?;
            Ok(journal)
        };
        let completed = decode(&history.0)?;
        if crate::policy::state::pending(completed.phase) {
            return Ok(None);
        }
        let mut expected = active.clone();
        expected.phase = completed.phase;
        expected.message.clone_from(&completed.message);
        let observed = state.regular_config_snapshot(active_name, LIMIT)?;
        if !crate::policy::state::pending(active.phase)
            || crate::domain::fingerprint(&completed)? != crate::domain::fingerprint(&expected)?
            || crate::domain::fingerprint(&decode(&observed.0)?)?
                != crate::domain::fingerprint(active)?
        {
            return Err(conflict());
        }
        let unchanged = |directory: &paths::Directory,
                         name: &OsStr,
                         previous: &(Vec<u8>, paths::Metadata)|
         -> Result<bool> {
            let current = directory.regular_config_snapshot(name, LIMIT)?;
            Ok(current.0 == previous.0
                && current.1.identity == previous.1.identity
                && current.1.mode == previous.1.mode)
        };
        Self::atomic_bytes_with_check(&self.root, "active.json", &history.0, false, || {
            self.verify_namespace()?;
            if !unchanged(state, active_name, &observed)?
                || !unchanged(operations, history_name, &history)?
            {
                return Err(conflict());
            }
            self.verify_namespace()
        })?;
        self.verify_namespace()?;
        Ok(Some(completed))
    }

    pub fn receipts(&self) -> Result<Receipts> {
        self.verify_namespace()?;
        let value = Self::read::<Receipts>(&self.root, "receipts.json")?.unwrap_or(Receipts {
            schema_version: 1,
            installations: std::collections::BTreeMap::default(),
        });
        crate::policy::state::receipt_schema(&value)?;
        self.verify_namespace()?;
        Ok(value)
    }

    pub fn commit(&self, journal: &Journal, observation: Observation) -> Result<()> {
        self.commit_checked(journal, observation, || Ok(()))
            .map_err(CommitFailure::into_error)
    }

    pub fn commit_checked(
        &self,
        journal: &Journal,
        observation: Observation,
        check: impl FnOnce() -> Result<()>,
    ) -> std::result::Result<(), CommitFailure> {
        self.commit_with_query(journal, observation, None, check)
    }

    pub(crate) fn commit_mise(
        &self,
        journal: &Journal,
        observation: Observation,
        query: MiseQuery,
    ) -> Result<()> {
        crate::mise_owner::retirement::freeze_absence(self, journal, &query)?;
        self.commit_with_query(journal, observation, Some(&QuerySpec::Mise(query)), || {
            Ok(())
        })
        .map_err(CommitFailure::into_error)
    }

    fn commit_with_query(
        &self,
        journal: &Journal,
        observation: Observation,
        verified_query: Option<&QuerySpec>,
        check: impl FnOnce() -> Result<()>,
    ) -> std::result::Result<(), CommitFailure> {
        self.verify_namespace().map_err(CommitFailure::Check)?;
        let bound = verified_query.or(journal.plan.query.as_ref());
        let check = || {
            check()?;
            if let Some(QuerySpec::Mise(query)) = bound {
                crate::mise_owner::query_command(&journal.scope, query)?;
                if let Some(retirement) = &query.retirement {
                    crate::mise_owner::retirement::verify_absence(retirement)?;
                    crate::mise_owner::retirement::verify_receipt(
                        &self.receipts()?,
                        &journal.plan.package.id,
                        retirement,
                        true,
                    )?;
                }
            }
            self.verify_namespace()
        };
        if crate::policy::service::is_control(journal.plan.action) {
            return check().map_err(CommitFailure::Check);
        }
        let mut receipts = self.receipts().map_err(CommitFailure::Outcome)?;
        let changed = match verified_query {
            Some(query) => crate::policy::state::commit_verified_receipt(
                &mut receipts,
                journal,
                observation,
                now(),
                Some(query.clone()),
            ),
            None => {
                crate::policy::state::commit_receipt(&mut receipts, journal, observation, now())
            }
        }
        .map_err(CommitFailure::Outcome)?;
        if !changed {
            return check().map_err(CommitFailure::Check);
        }
        let mut bytes = serde_json::to_vec_pretty(&receipts)
            .map_err(|error| CommitFailure::Outcome(error.into()))?;
        bytes.push(b'\n');
        let mut rejected = false;
        let result =
            Self::atomic_bytes_with_check(&self.root, "receipts.json", &bytes, false, || {
                let result = check();
                rejected = result.is_err();
                result
            });
        result.map_err(|error| {
            if rejected {
                CommitFailure::Check(error)
            } else {
                CommitFailure::Outcome(error)
            }
        })?;
        self.verify_namespace().map_err(CommitFailure::Outcome)
    }

    pub fn check_pending(&self) -> Result<()> {
        self.verify_namespace()?;
        self.check_migration_pending()?;
        if let Some(journal) = Self::read::<Journal>(&self.root, "active.json")? {
            validate_journal(&journal)?;
            if crate::policy::state::pending(journal.phase) {
                return Err(Error::attention(
                    "recovery_required",
                    format!("操作 {} 未收口；先执行 recover，不会重跑安装器", journal.id),
                ));
            }
        }
        self.verify_namespace()
    }

    pub fn check_migration_pending(&self) -> Result<()> {
        for name in ["migration-active.json", "migration.pending.json"] {
            match rustix::fs::statat(&self.root, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Ok(_) => {
                    return Err(Error::attention(
                        "migration_recovery_required",
                        "一次性迁移尚未收口；先执行 migrate recover",
                    ));
                }
                Err(rustix::io::Errno::NOENT) => (),
                Err(error) => return Err(io_error(error)),
            }
        }
        Ok(())
    }
}

pub(crate) fn active(ctx: &Context) -> Result<Option<Journal>> {
    let path = ctx.state.join("active.json");
    if !paths::read_path(&path)?.exists() {
        return Ok(None);
    }
    let journal = paths::regular_json(&path)?;
    validate_journal(&journal)?;
    Ok(Some(journal))
}

pub fn history(ctx: &Context) -> Result<Vec<Journal>> {
    let root = paths::read_path(&ctx.state.join("operations"))?;
    if !root.exists() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.extension().is_some_and(|v| v == "json") {
            let journal: Journal = paths::regular_json(&path)?;
            validate_journal(&journal)?;
            result.push(journal);
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

pub use crate::policy::state::{valid_id, validate_journal};
#[cfg(test)]
#[path = "../tests/unit/store.rs"]
mod tests;
