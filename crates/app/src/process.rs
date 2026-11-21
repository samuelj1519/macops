use crate::{
    domain::CommandSpec,
    error::{Error, Result},
    paths,
};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::{fs::FileTypeExt, process::CommandExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub use crate::domain::ProcessOutput as Output;

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
    capture_process(command, timeout, limit, None, guard)
}

pub fn capture_bytes_guarded_process_in(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    directory: &std::path::Path,
    guard: impl FnMut() -> Result<()>,
) -> CapturedProcess {
    capture_process(command, timeout, limit, Some(directory), guard)
}

fn capture_process(
    command: &mut Command,
    timeout: Duration,
    limit: u64,
    directory: Option<&std::path::Path>,
    mut guard: impl FnMut() -> Result<()>,
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
        let mut child = command.spawn()?;
        pid = Some(child.id());
        let start = Instant::now();
        let mut uncertain = false;
        let status = loop {
            if let Err(error) = guard() {
                kill_group(child.id());
                child.wait()?;
                return Err(error);
            }
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if start.elapsed() >= timeout
                || stdout.metadata()?.len() > limit
                || stderr.metadata()?.len() > limit
            {
                uncertain = true;
                kill_group(child.id());
                break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(15));
        };
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
