use crate::{
    domain::CommandSpec,
    error::{Error, Result},
    paths,
};
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::os::unix::{
    fs::{FileExt, FileTypeExt, MetadataExt, OpenOptionsExt},
    process::CommandExt,
};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

pub use crate::domain::ProcessOutput as Output;

pub(crate) struct LiveProgress {
    terminal: std::fs::File,
    streams: [ProgressStream; 2],
    queued: Vec<u8>,
    visible: bool,
}

#[derive(Default)]
struct ProgressStream {
    offset: u64,
    remainder: Vec<u8>,
}

impl LiveProgress {
    pub(crate) fn open() -> Option<Self> {
        let stderr = std::io::stderr();
        if !stderr.is_terminal() || std::env::args_os().any(|arg| arg == "--json") {
            return None;
        }
        let path = nix::unistd::ttyname(&stderr).ok()?;
        let terminal = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOCTTY | nix::libc::O_NOFOLLOW)
            .open(path)
            .ok()?;
        let inherited = std::fs::File::from(rustix::io::dup(stderr).ok()?);
        if !terminal.is_terminal()
            || terminal.metadata().ok()?.rdev() != inherited.metadata().ok()?.rdev()
        {
            return None;
        }
        Some(Self {
            terminal,
            streams: Default::default(),
            queued: Vec::new(),
            visible: false,
        })
    }

    fn flush_pending(&mut self) {
        while !self.queued.is_empty() {
            match self.terminal.write(&self.queued) {
                Ok(n) if n > 0 => {
                    self.visible = true;
                    self.queued.drain(..n);
                }
                _ => break,
            }
        }
    }

    pub(crate) fn relay(&mut self, files: [&std::fs::File; 2], draining: bool) {
        const LIMIT: u64 = 2 * 1024 * 1024;
        let mut buffer = [0; 8192];
        for (stream, file) in self.streams.iter_mut().zip(files) {
            while stream.offset < LIMIT {
                let max = usize::try_from(LIMIT - stream.offset).unwrap_or(buffer.len());
                let size = buffer.len().min(max);
                let Ok(count) = file.read_at(&mut buffer[..size], stream.offset) else {
                    break;
                };
                if count == 0 {
                    break;
                }
                stream.offset += count as u64;
                stream.remainder.extend_from_slice(&buffer[..count]);
                let mut bytes = std::mem::take(&mut stream.remainder);
                if let Err(error) = std::str::from_utf8(&bytes)
                    && error.error_len().is_none()
                {
                    stream.remainder = bytes.split_off(error.valid_up_to());
                }
                let native = String::from_utf8_lossy(&bytes);
                let visible = native
                    .split('\r')
                    .map(|line| crate::output::sanitize(line, true))
                    .collect::<Vec<_>>()
                    .join("\r");
                if self.queued.len() + visible.len() <= 128 * 1024 {
                    self.queued.extend_from_slice(visible.as_bytes());
                }
                if !draining {
                    break;
                }
            }
        }
        self.flush_pending();
    }
}

impl Drop for LiveProgress {
    fn drop(&mut self) {
        if self.visible {
            let _ = self.terminal.write(b"\n");
        }
    }
}

pub fn executable(name: &str) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let candidates =
        crate::policy::paths::path_candidates(name, &std::env::var_os("PATH").unwrap_or_default());
    for p in candidates {
        let safe = paths::read_path(&p)?;
        if let Ok(m) = std::fs::metadata(&safe)
            && m.is_file()
            && m.permissions().mode() & 0o111 != 0
        {
            return Ok(p);
        }
    }
    Err(Error::attention(
        "manager_missing",
        format!("未找到可执行程序：{name}"),
    ))
}

pub fn command(spec: &CommandSpec, scope: &crate::domain::Scope) -> Result<Command> {
    crate::identity::validate(scope)?;
    paths::read_path(&spec.program)?;
    let mut c = Command::new(&spec.program);
    c.args(&spec.args).stdin(Stdio::null());
    c.env("HOME", paths::read_path(&scope.home)?)
        .current_dir(&scope.home);
    for (key, value) in &scope.paths {
        if let Some(value) = value {
            c.env(key, paths::read_path(value)?);
        } else {
            c.env_remove(key);
        }
    }
    for (raw_key, _) in std::env::vars_os() {
        if raw_key
            .to_str()
            .is_some_and(crate::policy::npm_local::sanitized_env_key)
        {
            c.env_remove(raw_key);
        }
    }
    c.env_remove("NODE_OPTIONS");
    if !std::fs::metadata("/dev/null")?.file_type().is_char_device() {
        return Err(Error::attention(
            "codegraph_cache_isolation_unavailable",
            "/dev/null 不是字符设备，拒绝启动可能访问缓存的子进程",
        ));
    }
    c.env(
        "CODEGRAPH_INSTALL_DIR",
        "/dev/null/macops-codegraph-disabled",
    );
    c.env("HOMEBREW_NO_AUTO_UPDATE", "1")
        .env("HOMEBREW_NO_BOOTSNAP", "1")
        .env("HOMEBREW_NO_INSTALL_CLEANUP", "1")
        .env("HOMEBREW_NO_AUTOREMOVE", "1")
        .env("MISE_AUTO_INSTALL", "0")
        .env("MISE_EXEC_AUTO_INSTALL", "0")
        .env("CODEGRAPH_NO_DOWNLOAD", "1")
        .env("CODEGRAPH_NO_UPDATE_CHECK", "1")
        .env("PYTHONDONTWRITEBYTECODE", "1");
    Ok(c)
}

pub fn query(scope: &crate::domain::Scope, spec: &CommandSpec) -> Result<Output> {
    let mut c = command(spec, scope)?;
    capture_query(&mut c)
}

pub(crate) fn capture_query(command: &mut Command) -> Result<Output> {
    command.process_group(0);
    capture(command, Duration::from_secs(60))
}

pub struct BinaryOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub uncertain: bool,
}
pub fn capture(command: &mut Command, timeout: Duration) -> Result<Output> {
    let raw = capture_bytes(command, timeout, 2 * 1024 * 1024)?;
    Ok(Output {
        code: raw.code,
        stdout: String::from_utf8_lossy(&raw.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&raw.stderr).into_owned(),
        uncertain: raw.uncertain,
    })
}
pub fn capture_bytes(command: &mut Command, timeout: Duration, limit: u64) -> Result<BinaryOutput> {
    capture_bytes_guarded(command, timeout, limit, || Ok(()))
}
pub fn capture_bytes_guarded(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    guard: impl FnMut() -> Result<()>,
) -> Result<BinaryOutput> {
    capture_bytes_guarded_process(command, timeout, limit, guard).result
}

pub struct CapturedProcess {
    pub pid: Option<u32>,
    pub result: Result<BinaryOutput>,
}

pub fn capture_bytes_guarded_process(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    guard: impl FnMut() -> Result<()>,
) -> CapturedProcess {
    capture_process(command, timeout, limit, None, false, guard, |_| Ok(()))
}

pub fn capture_owned_group_in(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    directory: &std::path::Path,
    guard: impl FnMut() -> Result<()>,
) -> CapturedProcess {
    capture_owned_group_with_audit(command, timeout, limit, directory, guard, |_| Ok(()))
}

pub(crate) fn capture_owned_group_with_audit(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    directory: &std::path::Path,
    guard: impl FnMut() -> Result<()>,
    started: impl FnOnce(u32) -> Result<()>,
) -> CapturedProcess {
    let startup = Instant::now();
    if let Err(error) = verify_group_observer(timeout) {
        return CapturedProcess {
            pid: None,
            result: Err(error),
        };
    }
    let remaining = timeout.saturating_sub(startup.elapsed());
    if remaining.is_zero() {
        return CapturedProcess {
            pid: None,
            result: Err(group_unobserved()),
        };
    }
    command.process_group(0);
    capture_process(
        command,
        remaining,
        limit,
        Some(directory),
        true,
        guard,
        started,
    )
}

fn capture_process(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    directory: Option<&std::path::Path>,
    owned_group: bool,
    mut guard: impl FnMut() -> Result<()>,
    started: impl FnOnce(u32) -> Result<()>,
) -> CapturedProcess {
    let mut pid = None;
    let result = (|| {
        if limit == 0 || limit > 16 * 1024 * 1024 {
            return Err(Error::new(
                "invalid_capture_limit",
                "二进制查询输出预算无效",
            ));
        }
        if directory.is_some() {
            guard()?;
        }
        let mut stdout = capture_file(directory)?;
        let mut stderr = capture_file(directory)?;
        command
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        guard()?;
        let start = Instant::now();
        let mut child = command.spawn()?;
        pid = Some(child.id());
        if let Err(error) = started(child.id()) {
            cancel_capture(&mut child, owned_group)?;
            return Err(error);
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        let (status, mut uncertain) = wait_for_capture(&mut child, remaining, owned_group, || {
            guard()?;
            Ok(stdout.metadata()?.len() > limit || stderr.metadata()?.len() > limit)
        })?;
        guard()?;
        stdout.seek(SeekFrom::Start(0))?;
        uncertain |= stdout.metadata()?.len() > limit || stderr.metadata()?.len() > limit;
        stderr.seek(SeekFrom::Start(0))?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        stdout.take(limit).read_to_end(&mut out)?;
        stderr.take(limit).read_to_end(&mut err)?;
        Ok(BinaryOutput {
            code: status.code(),
            stdout: out,
            stderr: err,
            uncertain,
        })
    })();
    CapturedProcess { pid, result }
}

fn wait_for_capture(
    child: &mut Child,
    timeout: Duration,
    owned_group: bool,
    mut limit_exceeded: impl FnMut() -> Result<bool>,
) -> Result<(ExitStatus, bool)> {
    let start = Instant::now();
    let mut saw_peer = false;
    loop {
        let exited = if owned_group {
            exited_without_reaping(child)?
        } else {
            false
        };
        let exceeded = match limit_exceeded() {
            Ok(exceeded) => exceeded,
            Err(error) => {
                cancel_capture(child, owned_group)?;
                return Err(error);
            }
        };
        let remaining = timeout.saturating_sub(start.elapsed());
        if exceeded || remaining.is_zero() {
            return Ok((cancel_capture(child, owned_group)?, true));
        }
        if owned_group {
            if exited {
                match live_peers(child.id(), remaining) {
                    Ok(true) => saw_peer = true,
                    Ok(false) => {
                        return Ok((child.wait()?, saw_peer || start.elapsed() >= timeout));
                    }
                    Err(error) => {
                        let expired = start.elapsed() >= timeout;
                        let status = cancel_capture(child, true)?;
                        return if expired {
                            Ok((status, true))
                        } else {
                            Err(error)
                        };
                    }
                }
            }
        } else if let Some(status) = child.try_wait()? {
            return Ok((status, false));
        }
        std::thread::sleep(
            timeout
                .saturating_sub(start.elapsed())
                .min(Duration::from_millis(15)),
        );
    }
}

fn exited_without_reaping(child: &Child) -> Result<bool> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let pid = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(group_unobserved)?;
    let flags = WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT;
    loop {
        match waitid(WaitId::Pid(pid), flags) {
            Ok(status) => return Ok(status.is_some()),
            Err(rustix::io::Errno::INTR) => {}
            // ECHILD and other failures provide no authority to signal a numeric group.
            Err(_) => return Err(group_unobserved()),
        }
    }
}

fn cancel_capture(child: &mut Child, owned_group: bool) -> Result<ExitStatus> {
    if !owned_group {
        kill_group(child.id());
        return child.wait().map_err(Into::into);
    }
    loop {
        let exited = exited_without_reaping(child)?;
        if exited
            && (!group_alive(child.id())
                || matches!(
                    live_group_members(child.id(), Duration::from_millis(300)),
                    Ok(false)
                ))
        {
            return child.wait().map_err(Into::into);
        }
        // Keep the leader and the caller's lease until the original group is observed quiescent.
        kill_group(child.id());
        if !exited {
            let _ = child.kill();
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn verify_group_observer(timeout: Duration) -> Result<()> {
    static VERIFIED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    if VERIFIED.get().is_some() {
        return Ok(());
    }
    let group = u32::try_from(nix::unistd::getpgrp().as_raw()).map_err(|_| group_unobserved())?;
    if !group_members(group, timeout)?.contains(&std::process::id()) {
        return Err(group_unobserved());
    }
    let _ = VERIFIED.set(());
    Ok(())
}

fn live_peers(leader: u32, remaining: Duration) -> Result<bool> {
    // waitid(WNOWAIT) still owns the child's PID. ESRCH here means the
    // original group is gone. EPERM alone cannot distinguish an empty group.
    if !group_alive(leader) {
        return Err(group_unobserved());
    }
    live_group_members(leader, remaining)
}

fn live_group_members(leader: u32, remaining: Duration) -> Result<bool> {
    let members = group_members(leader, remaining)?;
    if members.contains(&leader) {
        // On macOS pgrep omits zombies. Seeing the exited leader contradicts waitid.
        return Err(group_unobserved());
    }
    Ok(!members.is_empty())
}

fn group_members(group: u32, remaining: Duration) -> Result<Vec<u32>> {
    let mut inspect = Command::new("/usr/bin/pgrep");
    inspect
        .env_clear()
        .env("LC_ALL", "C")
        .args(["-a", "-g", &group.to_string(), ".*"])
        .stdin(Stdio::null())
        .process_group(0);
    let output =
        capture_bytes(&mut inspect, remaining, 1024 * 1024).map_err(|_| group_unobserved())?;
    if output.uncertain || !output.stderr.is_empty() {
        return Err(group_unobserved());
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| group_unobserved())?;
    parse_group_members(output.code, text)
}

fn parse_group_members(code: Option<i32>, text: &str) -> Result<Vec<u32>> {
    match (code, text.is_empty()) {
        (Some(1), true) => Ok(Vec::new()),
        (Some(0), false) => text
            .lines()
            .map(|line| {
                line.parse::<u32>()
                    .ok()
                    .filter(|id| *id > 0)
                    .ok_or_else(group_unobserved)
            })
            .collect(),
        _ => Err(group_unobserved()),
    }
}

fn group_unobserved() -> Error {
    Error::attention(
        "native_query_group_unobserved",
        "无法核对原生查询的辅助进程，查询结果未确认。",
    )
}

fn capture_file(directory: Option<&std::path::Path>) -> std::io::Result<std::fs::File> {
    directory.map_or_else(tempfile::tempfile, tempfile::tempfile_in)
}

pub fn group_alive(group: u32) -> bool {
    let Ok(group) = i32::try_from(group) else {
        return true;
    };
    if group == 0 {
        return true;
    }
    match nix::sys::signal::killpg(nix::unistd::Pid::from_raw(group), None) {
        Err(nix::errno::Errno::ESRCH) => false,
        Ok(()) | Err(_) => true,
    }
}

#[cfg(test)]
#[path = "../tests/unit/process.rs"]
mod tests;

pub fn kill_group(group: u32) {
    if let Ok(group) = i32::try_from(group)
        && group > 0
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(group),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

pub struct Probe {
    pub code: Option<i32>,
    pub uncertain: bool,
}

pub fn probe(scope: &crate::domain::Scope, spec: &CommandSpec, timeout: Duration) -> Result<Probe> {
    let mut command = command(spec, scope)?;
    command
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Probe {
                code: status.code(),
                uncertain: false,
            });
        }
        if start.elapsed() >= timeout {
            kill_group(child.id());
            return Ok(Probe {
                code: child.wait()?.code(),
                uncertain: true,
            });
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

pub fn query_at(
    scope: &crate::domain::Scope,
    spec: &CommandSpec,
    cwd: &std::path::Path,
    client_root: Option<(&str, &std::path::Path)>,
) -> Result<Output> {
    let mut c = command(spec, scope)?;
    let cwd = paths::read_path(cwd)?;
    c.current_dir(&cwd).env("PWD", &cwd);
    if let Some((key, root)) = client_root {
        c.env(key, paths::read_path(root)?);
    }
    c.env("DISABLE_UPDATES", "1")
        .env("CODEGRAPH_NO_UPDATE_CHECK", "1");
    c.process_group(0);
    capture(&mut c, Duration::from_secs(60))
}
