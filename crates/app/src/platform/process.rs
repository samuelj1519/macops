use crate::error::{Error, Result};
use std::{
    io::{Read, Seek, SeekFrom},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

pub use crate::domain::ProcessOutput as Output;

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
#[path = "../../tests/unit/process.rs"]
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

pub fn probe(command: &mut Command, timeout: Duration) -> Result<Probe> {
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
