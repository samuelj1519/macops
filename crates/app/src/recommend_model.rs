use crate::{
    context::Context,
    policy::{
        recommend::{Analysis, PublicAnalysis},
        recommend_model::{self as policy, Notes},
    },
};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Keychain,
    Transport,
    Deadline,
    OutputLimit,
    HttpStatus,
    Unverified,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::Keychain => "keychain-unavailable",
            Self::Transport => "transport-unavailable",
            Self::Deadline => "deadline",
            Self::OutputLimit => "output-limit",
            Self::HttpStatus => "http-status",
            Self::Unverified => "unverified-response",
        }
    }
}

pub fn annotate(ctx: &Context, analysis: &PublicAnalysis, account: &str) -> Value {
    if !cfg!(target_os = "macos") || ctx.isolated {
        return json!({"status":"keychain-unavailable"});
    }
    let clients = Clients {
        security: Path::new("/usr/bin/security"),
        curl: Path::new("/usr/bin/curl"),
    };
    let deadline = Instant::now() + Duration::from_secs(ctx.timeout_secs.min(15));
    match compare(&clients, analysis.facts(), account, deadline) {
        Ok(result) => json!({"status":"annotated","model":policy::MODEL,"result":result}),
        Err(error) => json!({"status":error.label(),"model":policy::MODEL}),
    }
}

struct Clients<'a> {
    security: &'a Path,
    curl: &'a Path,
}

fn compare(
    clients: &Clients<'_>,
    analysis: &Analysis,
    account: &str,
    deadline: Instant,
) -> Result<Notes, Failure> {
    let account = policy::account(account).map_err(|_| Failure::Keychain)?;
    let bytes = credentials(clients.security, account, deadline)?;
    let token = policy::api_token(&bytes).map_err(|_| Failure::Keychain)?;
    let body = policy::request(analysis);
    let output = send(clients.curl, token, &body, deadline)?;
    parse_annotation(&output, token, analysis.findings.len())
}

fn credentials(path: &Path, account: &str, deadline: Instant) -> Result<Vec<u8>, Failure> {
    let mut command = Command::new(path);
    command
        .args([
            "find-generic-password",
            "-s",
            policy::KEYCHAIN_SERVICE,
            "-a",
            account,
            "-w",
        ])
        .stdin(Stdio::null());
    run(&mut command, deadline, 513).map_err(|error| {
        if error == Failure::Transport {
            Failure::Keychain
        } else {
            error
        }
    })
}

fn send(path: &Path, token: &str, body: &str, deadline: Instant) -> Result<Vec<u8>, Failure> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining < Duration::from_millis(1) {
        return Err(Failure::Deadline);
    }
    let (reader, mut writer) = io::pipe().map_err(|_| Failure::Transport)?;
    writer
        .write_all(b"Authorization: Bearer ")
        .map_err(|_| Failure::Transport)?;
    writer
        .write_all(token.as_bytes())
        .map_err(|_| Failure::Transport)?;
    writer.write_all(b"\n").map_err(|_| Failure::Transport)?;
    drop(writer);

    let mut command = Command::new(path);
    command
        .args([
            "--disable",
            "--silent",
            "--proto",
            "=https",
            "--noproxy",
            "*",
            "--max-redirs",
            "0",
            "--connect-timeout",
            "4",
            "--max-time",
            &format!("{:.3}", remaining.as_secs_f64()),
            "--header",
            "@-",
            "--header",
            "Content-Type: application/json",
            "--data-binary",
            body,
            "--write-out",
            "\n%{http_code}",
            "--",
            policy::ENDPOINT,
        ])
        .stdin(Stdio::from(reader));
    run(&mut command, deadline, policy::RESPONSE_LIMIT + 4)
}

fn parse_annotation(output: &[u8], token: &str, candidates: usize) -> Result<Notes, Failure> {
    let body = http_body(output)?;
    if body
        .windows(token.len())
        .any(|piece| piece == token.as_bytes())
    {
        return Err(Failure::Unverified);
    }
    let notes = policy::response(body, candidates).map_err(|_| Failure::Unverified)?;
    if notes.notes.iter().any(|item| item.summary.contains(token)) {
        return Err(Failure::Unverified);
    }
    Ok(notes)
}

fn http_body(output: &[u8]) -> Result<&[u8], Failure> {
    let border = output
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or(Failure::Unverified)?;
    let status = &output[border + 1..];
    if !matches!(status, [b'2', a, b] if a.is_ascii_digit() && b.is_ascii_digit()) {
        return Err(Failure::HttpStatus);
    }
    Ok(&output[..border])
}

pub(super) fn fetch_public(url: &str, deadline: Instant) -> crate::error::Result<Value> {
    fetch_public_with(Path::new("/usr/bin/curl"), url, deadline)
}

fn fetch_public_with(path: &Path, url: &str, deadline: Instant) -> crate::error::Result<Value> {
    let error =
        || crate::error::Error::attention("recommend_source_unavailable", "未能核验远端公开来源");
    let accept = if url.starts_with("https://api.github.com/repos/") {
        "Accept: application/vnd.github+json"
    } else if url.starts_with("https://registry.npmjs.org/")
        || url.starts_with("https://formulae.brew.sh/api/formula/")
    {
        "Accept: application/json"
    } else {
        return Err(error());
    };
    let mut command = Command::new(path);
    command
        .args([
            "--disable",
            "--silent",
            "--proto",
            "=https",
            "--noproxy",
            "*",
            "--max-redirs",
            "0",
            "--connect-timeout",
            "4",
            "--max-time",
            "15",
            "--user-agent",
            "macops",
            "--header",
            accept,
            "--write-out",
            "\n%{http_code}",
            "--",
            url,
        ])
        .stdin(Stdio::null());
    let data = run(&mut command, deadline, policy::RESPONSE_LIMIT + 4).map_err(|_| error())?;
    let body = http_body(&data).map_err(|_| error())?;
    crate::policy::json::parse(body).map_err(|_| error())
}

fn run(command: &mut Command, deadline: Instant, limit: usize) -> Result<Vec<u8>, Failure> {
    if Instant::now() >= deadline {
        return Err(Failure::Deadline);
    }
    command
        .env_clear()
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().map_err(|_| Failure::Transport)?;
    let mut reaped = false;
    let result = collect(&mut child, deadline, limit, &mut reaped);
    if result.is_err() && !reaped {
        crate::process::kill_group(child.id());
        let _ = child.wait();
    }
    result
}

fn collect(
    child: &mut Child,
    deadline: Instant,
    limit: usize,
    reaped: &mut bool,
) -> Result<Vec<u8>, Failure> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    let mut stdout = child.stdout.take().ok_or(Failure::Transport)?;
    let flags = fcntl_getfl(&stdout).map_err(|_| Failure::Transport)?;
    fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|_| Failure::Transport)?;
    let mut data = Vec::new();
    let mut buffer = [0; 4096];
    let mut eof = false;
    loop {
        if Instant::now() >= deadline {
            return Err(Failure::Deadline);
        }
        if !eof {
            match stdout.read(&mut buffer) {
                Ok(0) => eof = true,
                Ok(length) => {
                    if length > limit - data.len() {
                        return Err(Failure::OutputLimit);
                    }
                    data.extend_from_slice(&buffer[..length]);
                    continue;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(Failure::Transport),
            }
        }
        if eof && exited_without_reaping(child)? {
            // The leader still owns its PID. End any remaining group members before reaping it.
            crate::process::kill_group(child.id());
            let status = child.wait().map_err(|_| Failure::Transport)?;
            *reaped = true;
            return if status.success() {
                Ok(data)
            } else {
                Err(Failure::Transport)
            };
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn exited_without_reaping(child: &Child) -> Result<bool, Failure> {
    use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
    let raw = i32::try_from(child.id()).map_err(|_| Failure::Transport)?;
    let pid = Pid::from_raw(raw).ok_or(Failure::Transport)?;
    waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|result| result.is_some())
    .map_err(|_| Failure::Transport)
}

#[cfg(test)]
#[path = "../tests/unit/recommend_model_runtime.rs"]
mod tests;
