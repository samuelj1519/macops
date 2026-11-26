use crate::{
    domain::{CommandSpec, Journal, Phase, fingerprint},
    error::{Error, Result},
    operations::store::Store,
    platform::fs::Directory,
    policy::governance::DirectoryBinding,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::{fs::MetadataExt, process::CommandExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenInvocation {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub command: CommandSpec,
    pub env: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RunBinding {
    schema_version: u32,
    operation_id: String,
    plan_sha256: String,
    scope_sha256: String,
    invocation_sha256: String,
    nonce: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    state: DirectoryBinding,
    #[serde(deserialize_with = "crate::policy::json::object")]
    home: DirectoryBinding,
    controller_pid: u32,
    gate_name: String,
    completion_name: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub invocation: FrozenInvocation,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WorkerBinding {
    binding_sha256: String,
    worker_pid: u32,
    process_group: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RunGate {
    operation_id: String,
    plan_sha256: String,
    scope_sha256: String,
    nonce: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    worker: WorkerBinding,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Completion {
    binding_sha256: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    worker: WorkerBinding,
    child_pid: u32,
    code: Option<i32>,
    signal: Option<i32>,
    uncertain: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum NativeOutcome {
    Running,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Unknown {
        reason: String,
    },
}

fn invalid() -> Error {
    Error::attention(
        "native_runner_evidence_invalid",
        "原生执行证据缺失或身份不符，保留 Unknown",
    )
}
fn unknown(reason: &str) -> NativeOutcome {
    NativeOutcome::Unknown {
        reason: reason.into(),
    }
}
fn name(journal: &Journal, suffix: &str) -> String {
    format!("native-{}-{suffix}.json", journal.id)
}
fn private_state(path: &Path) -> Result<(Directory, DirectoryBinding)> {
    let directory = Directory::open(path)?;
    let metadata = directory.file().metadata()?;
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    let binding = DirectoryBinding {
        requested: path.into(),
        canonical: directory.path.clone(),
        identity: directory.identity().clone(),
        lineage: directory.lineage()?,
        uid: metadata.uid(),
        mode: metadata.mode() & 0o7777,
    };
    directory.verify()?;
    Ok((directory, binding))
}
fn opened(binding: &RunBinding) -> Result<Directory> {
    let (directory, current) = private_state(&binding.state.requested)?;
    if current != binding.state {
        return Err(invalid());
    }
    Ok(directory)
}
fn read<T: serde::de::DeserializeOwned>(directory: &Directory, leaf: &str) -> Result<T> {
    let (bytes, metadata) = directory.regular_config_snapshot(OsStr::new(leaf), 1024 * 1024)?;
    if metadata.mode != 0o600 {
        return Err(invalid());
    }
    if rustix::fs::statat(
        directory.file(),
        leaf,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?
    .st_uid
        != nix::unistd::geteuid().as_raw()
    {
        return Err(invalid());
    }
    let value = crate::policy::json::parse(&bytes)?;
    let crate::policy::json::Object(value) =
        serde_json::from_value::<crate::policy::json::Object<T>>(value)?;
    directory.verify()?;
    Ok(value)
}
fn gate(journal: &Journal, binding: &RunBinding, worker: &WorkerBinding) -> RunGate {
    RunGate {
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        scope_sha256: journal.scope_sha256.clone(),
        nonce: binding.nonce.clone(),
        worker: worker.clone(),
    }
}
fn home_keeper(binding: &RunBinding) -> Result<std::fs::File> {
    let stdin = std::io::stdin();
    let keeper = std::fs::File::from(
        rustix::io::fcntl_dupfd_cloexec(&stdin, 3).map_err(std::io::Error::from)?,
    );
    rustix::io::fcntl_setfd(&stdin, rustix::io::FdFlags::CLOEXEC).map_err(std::io::Error::from)?;
    let metadata = keeper.metadata()?;
    if !metadata.is_dir()
        || crate::domain::FileId::from_file(&keeper)? != binding.home.identity
        || metadata.uid() != binding.home.uid
        || metadata.mode() & 0o7777 != binding.home.mode
    {
        return Err(invalid());
    }
    let root = Directory::open(&binding.home.requested)?;
    if crate::capability::capability_store::binding(&root, &binding.home.requested)? != binding.home
    {
        return Err(invalid());
    }
    fs2::FileExt::try_lock_exclusive(&keeper).map_err(|_| invalid())?;
    Ok(keeper)
}
fn wait_gate(path: &Path, binding: &RunBinding) -> Result<(Journal, WorkerBinding)> {
    let started = Instant::now();
    loop {
        let directory = opened(binding)?;
        if directory
            .metadata(OsStr::new(&binding.gate_name))?
            .is_some()
        {
            let journal: Journal = crate::platform::fs::regular_json(path)?;
            validate(&journal, binding)?;
            crate::policy::operation::state::worker_identity(
                &journal,
                path,
                std::process::id(),
                nix::unistd::getpgrp().as_raw(),
            )?;
            let worker: WorkerBinding = read(&directory, &name(&journal, "worker"))?;
            if worker.binding_sha256 != fingerprint(binding)?
                || worker.worker_pid != std::process::id()
                || i32::try_from(worker.process_group).ok() != Some(nix::unistd::getpgrp().as_raw())
                || read::<RunGate>(&directory, &binding.gate_name)?
                    != gate(&journal, binding, &worker)
            {
                return Err(invalid());
            }
            return Ok((journal, worker));
        }
        if u32::try_from(nix::unistd::getppid().as_raw()).ok() != Some(binding.controller_pid)
            || started.elapsed() > Duration::from_secs(5)
        {
            return Err(Error::attention(
                "native_gate_missing",
                "原 RUN gate 尚未发送，未启动 Native",
            ));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn verify_group_observer(group: u32) -> Result<()> {
    let group = i32::try_from(group).map_err(|_| invalid())?;
    if group <= 0 {
        return Err(invalid());
    }
    nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None).map_err(|_| invalid())
}

fn drain_native_group(group: u32) {
    while crate::platform::process::group_alive(group) {
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn validate(journal: &Journal, binding: &RunBinding) -> Result<()> {
    crate::operations::store::validate_journal(journal)?;
    if binding.schema_version != 2
        || binding.operation_id != journal.id
        || binding.plan_sha256 != journal.plan_sha256
        || binding.scope_sha256 != journal.scope_sha256
        || binding.state.requested != journal.state
        || binding.home.requested != journal.home
        || binding.controller_pid == 0
        || binding.home.uid != nix::unistd::geteuid().as_raw()
        || binding.home.mode & 0o022 != 0
        || binding.gate_name != format!("native-{}-{}-run.json", journal.id, binding.nonce)
        || binding.invocation_sha256 != fingerprint(&binding.invocation)?
        || binding.nonce.len() != 32
        || !binding.nonce.bytes().all(|b| b.is_ascii_hexdigit())
        || binding.completion_name != format!("native-{}-{}-exit.json", journal.id, binding.nonce)
        || journal.plan.command.as_ref() != Some(&binding.invocation.command)
        || journal.plan.timeout_secs != binding.invocation.timeout_secs
    {
        return Err(invalid());
    }
    let invocation = &binding.invocation;
    if !(1..=3600).contains(&invocation.timeout_secs)
        || !invocation.command.program.is_absolute()
        || !invocation.cwd.is_absolute()
        || invocation.env.len() > 256
        || invocation.command.args.iter().any(|arg| arg.contains('\0'))
        || invocation
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        || serde_json::to_vec(invocation)?.len() > 512 * 1024
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn prepare(
    store: &Store,
    journal: &Journal,
    invocation: &FrozenInvocation,
) -> Result<RunBinding> {
    store.verify_namespace()?;
    if journal.phase != Phase::Prepared || journal.process_group.is_some() {
        return Err(invalid());
    }
    let (_, state) = private_state(&store.path)?;
    let (_, home) = store.native_home_keeper()?;
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| invalid())?;
    let nonce = crate::capability::rules::invocation::hash(&random)[..32].to_owned();
    let binding = RunBinding {
        schema_version: 2,
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        scope_sha256: journal.scope_sha256.clone(),
        invocation_sha256: fingerprint(invocation)?,
        completion_name: format!("native-{}-{nonce}-exit.json", journal.id),
        gate_name: format!("native-{}-{nonce}-run.json", journal.id),
        nonce,
        state,
        home,
        controller_pid: std::process::id(),
        invocation: invocation.clone(),
    };
    validate(journal, &binding)?;
    Store::atomic_new(&store.root, &name(journal, "binding"), &binding)?;
    store.verify_namespace()?;
    Ok(binding)
}

pub fn binding(store: &Store, journal: &Journal) -> Result<RunBinding> {
    store.verify_namespace()?;
    let (directory, _) = private_state(&store.path)?;
    let binding: RunBinding = read(&directory, &name(journal, "binding"))?;
    validate(journal, &binding)?;
    opened(&binding)?;
    Ok(binding)
}

pub fn launch(store: &Store, journal: &mut Journal, binding: &RunBinding) -> Result<NativeOutcome> {
    validate(journal, binding)?;
    if journal.phase != Phase::Prepared
        || journal.process_group.is_some()
        || binding.controller_pid != std::process::id()
        || self::binding(store, journal)? != *binding
    {
        return Err(invalid());
    }
    let (keeper, home) = store.native_home_keeper()?;
    if home != binding.home {
        return Err(invalid());
    }
    let verbose = std::env::args_os().any(|arg| arg == "--verbose");
    let mut progress = shows_progress(journal, verbose)
        .then(crate::terminal::progress::LiveProgress::open)
        .flatten();
    let captured = if progress.is_some() {
        Some([tempfile::tempfile()?, tempfile::tempfile()?])
    } else {
        None
    };
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("__native_worker")
        .arg(
            store
                .path
                .join("operations")
                .join(format!("{}.json", journal.id)),
        )
        .env_clear()
        .stdin(Stdio::from(keeper))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some([out, err]) = &captured {
        command
            .arg("--capture-progress")
            .stdout(out.try_clone()?)
            .stderr(err.try_clone()?);
    }
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command.spawn()?;
    let worker = WorkerBinding {
        binding_sha256: fingerprint(binding)?,
        worker_pid: child.id(),
        process_group: child.id(),
    };
    journal.phase = Phase::Running;
    journal.process_group = Some(child.id());
    let gated = (|| {
        Store::atomic_new(&store.root, &name(journal, "worker"), &worker)?;
        store.save(journal)?;
        Store::atomic_new(
            &store.root,
            &binding.gate_name,
            &gate(journal, binding, &worker),
        )?;
        Ok::<(), Error>(())
    })();
    // A failed gate publication may already be visible to the wrapper.
    // Only that wrapper can establish whether the Native process started.
    gated?;
    let start = Instant::now();
    loop {
        if child.try_wait()?.is_some() {
            if let (Some(progress), Some([out, err])) = (&mut progress, &captured) {
                progress.relay([out, err], true);
            }
            break;
        }
        if start.elapsed() > Duration::from_secs(binding.invocation.timeout_secs + 3) {
            return Ok(unknown("native_supervisor_timeout"));
        }
        if let (Some(progress), Some([out, err])) = (&mut progress, &captured) {
            progress.relay([out, err], false);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    observe(store, journal, binding)
}

fn shows_progress(journal: &Journal, verbose: bool) -> bool {
    progress_capture_allowed(journal)
        && (!(crate::agent::codex_install::is_install(journal)
            || crate::agent::claude_install::is_install(journal))
            || verbose)
}

fn progress_capture_allowed(journal: &Journal) -> bool {
    journal.plan.package.kind != crate::domain::Kind::Config
        && !crate::config::rules::chezmoi_native::is_native(&journal.plan)
}

fn capture_files(directory: &Directory, progress: bool) -> Result<(std::fs::File, std::fs::File)> {
    if !progress {
        return Ok((
            tempfile::tempfile_in(&directory.path)?,
            tempfile::tempfile_in(&directory.path)?,
        ));
    }
    let out =
        std::fs::File::from(rustix::io::dup(std::io::stdout()).map_err(std::io::Error::from)?);
    let err =
        std::fs::File::from(rustix::io::dup(std::io::stderr()).map_err(std::io::Error::from)?);
    let metadata = out.metadata()?;
    let other = err.metadata()?;
    if !metadata.is_file()
        || !other.is_file()
        || (metadata.dev(), metadata.ino()) == (other.dev(), other.ino())
    {
        return Err(invalid());
    }
    Ok((out, err))
}

pub fn observe(store: &Store, journal: &Journal, binding: &RunBinding) -> Result<NativeOutcome> {
    store.verify_namespace()?;
    let observed = self::binding(store, journal).and_then(|saved| {
        if saved != *binding {
            return Err(invalid());
        }
        inspect(journal, binding)
    });
    store.verify_namespace()?;
    Ok(observed.unwrap_or_else(|_| unknown("native_completion_unverified")))
}

pub(crate) fn original_outcome(journal: &Journal) -> Result<NativeOutcome> {
    let (directory, _) = private_state(&journal.state)?;
    let binding: RunBinding = read(&directory, &name(journal, "binding"))?;
    inspect(journal, &binding)
}

fn inspect(journal: &Journal, binding: &RunBinding) -> Result<NativeOutcome> {
    validate(journal, binding)?;
    let directory = opened(binding)?;
    if read::<RunBinding>(&directory, &name(journal, "binding"))? != *binding {
        return Err(invalid());
    }
    let worker: WorkerBinding = read(&directory, &name(journal, "worker"))?;
    if worker.binding_sha256 != fingerprint(binding)?
        || Some(worker.process_group) != journal.process_group
        || worker.worker_pid != worker.process_group
    {
        return Err(invalid());
    }
    let exists = directory
        .metadata(OsStr::new(&binding.completion_name))?
        .is_some();
    if !exists {
        return Ok(
            if crate::platform::process::group_alive(worker.process_group) {
                NativeOutcome::Running
            } else {
                unknown("native_completion_missing")
            },
        );
    }
    let completion: Completion = read(&directory, &binding.completion_name)?;
    if completion.binding_sha256 != fingerprint(binding)?
        || completion.worker != worker
        || completion.child_pid == 0
        || completion.code.is_some() == completion.signal.is_some()
    {
        return Err(invalid());
    }
    if crate::platform::process::group_alive(worker.process_group) {
        return Ok(NativeOutcome::Running);
    }
    if completion.uncertain {
        return Ok(unknown("native_result_uncertain"));
    }
    Ok(NativeOutcome::Exited {
        code: completion.code,
        signal: completion.signal,
    })
}

pub fn worker(
    path: &Path,
    capture_progress: bool,
    before_native: impl FnOnce(&Journal, &FrozenInvocation) -> Result<()>,
    after_native: impl FnOnce(&Journal, &FrozenInvocation, &NativeOutcome) -> Result<()>,
) -> Result<()> {
    let initial: Journal = crate::platform::fs::regular_json(path)?;
    crate::operations::store::validate_journal(&initial)?;
    let (directory, _) = private_state(&initial.state)?;
    let binding: RunBinding = read(&directory, &name(&initial, "binding"))?;
    validate(&initial, &binding)?;
    let _keeper = home_keeper(&binding)?;
    let (journal, worker) = wait_gate(path, &binding)?;
    let directory = opened(&binding)?;
    if directory
        .metadata(OsStr::new(&binding.completion_name))?
        .is_some()
    {
        return Err(invalid());
    }
    verify_group_observer(worker.process_group)?;
    let budget = Instant::now();
    before_native(&journal, &binding.invocation)?;
    if budget.elapsed() >= Duration::from_secs(binding.invocation.timeout_secs) {
        return Err(Error::attention(
            "native_not_started_timeout",
            "前置核验超时；未启动所选 Native mutator",
        ));
    }
    opened(&binding)?;
    let (stdout, stderr) = capture_files(
        &directory,
        capture_progress && progress_capture_allowed(&journal),
    )?;
    opened(&binding)?;
    Directory::open(&binding.invocation.cwd)?;
    let invocation = &binding.invocation;
    let mut command = Command::new(&invocation.command.program);
    command
        .args(&invocation.command.args)
        .env_clear()
        .envs(&invocation.env)
        .current_dir(&invocation.cwd)
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .process_group(0);
    opened(&binding)?;
    let mut child = command.spawn()?;
    let child_pid = child.id();
    let executed = wait_native(
        &mut child,
        &stdout,
        &stderr,
        budget,
        invocation.timeout_secs,
    );
    if executed.is_err() {
        let _ = child.wait();
    }
    // The child PID is the Native group ID. Once reaped, only observe that group.
    drain_native_group(child_pid);
    let (status, uncertain) = executed?;
    let uncertain = uncertain || budget.elapsed() >= Duration::from_secs(invocation.timeout_secs);
    opened(&binding)?;
    let completion = Completion {
        binding_sha256: fingerprint(&binding)?,
        worker,
        child_pid,
        code: status.code(),
        signal: status.signal(),
        uncertain,
    };
    Store::atomic_new(directory.file(), &binding.completion_name, &completion)?;
    opened(&binding)?;
    let outcome = if uncertain {
        unknown("native_result_uncertain")
    } else {
        NativeOutcome::Exited {
            code: completion.code,
            signal: completion.signal,
        }
    };
    after_native(&journal, &binding.invocation, &outcome)?;
    opened(&binding)?;
    if uncertain || completion.code != Some(0) {
        return Err(Error::attention(
            "native_process_unsuccessful",
            "所选 Native 进程非零、被信号结束或结果不确定；原退出证据已保留",
        ));
    }
    Ok(())
}

fn wait_native(
    child: &mut std::process::Child,
    stdout: &std::fs::File,
    stderr: &std::fs::File,
    budget: Instant,
    timeout_secs: u64,
) -> Result<(std::process::ExitStatus, bool)> {
    let mut uncertain = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if budget.elapsed() >= Duration::from_secs(timeout_secs)
            || stdout.metadata()?.len() + stderr.metadata()?.len() > 4 * 1024 * 1024
        {
            uncertain = true;
            // The leader has not been reaped, so its ID cannot yet be reused.
            crate::platform::process::kill_group(child.id());
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(15));
    };
    uncertain |= stdout.metadata()?.len() + stderr.metadata()?.len() > 4 * 1024 * 1024;
    Ok((status, uncertain))
}

#[cfg(test)]
#[path = "../../tests/unit/native_runner.rs"]
mod tests;
