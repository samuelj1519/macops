use crate::{
    domain::CommandSpec,
    error::{Error, Result},
    platform::{
        fs as paths,
        process::{self, Output, capture},
    },
};
use std::{
    os::unix::{fs::FileTypeExt, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

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
    crate::operations::identity::validate(scope)?;
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
            .is_some_and(crate::software::rules::npm_local::sanitized_env_key)
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

pub fn probe(
    scope: &crate::domain::Scope,
    spec: &CommandSpec,
    timeout: Duration,
) -> Result<process::Probe> {
    process::probe(&mut command(spec, scope)?, timeout)
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
