#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
#[test]
fn binary_capture_preserves_non_utf8_and_nul_instead_of_lossy_git_blobs() -> TestResult {
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf '\\000\\377A\\000'"])
        .process_group(0);
    let out = capture_bytes(&mut command, Duration::from_secs(1), 16)?;
    assert_eq!(out.stdout, vec![0, 255, b'A', 0]);
    assert_eq!(out.code, Some(0));
    assert!(!out.uncertain);
    assert_eq!(out.stderr.len(), 0);
    Ok(())
}
#[test]
fn binary_capture_flags_truncation_timeout_and_invalid_budgets() -> TestResult {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "printf '12345'"]).process_group(0);
    let out = capture_bytes(&mut command, Duration::from_secs(1), 4)?;
    assert_eq!(out.stdout, b"1234");
    assert!(out.uncertain);
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf started; /bin/sleep 2"])
        .process_group(0);
    let out = capture_bytes(&mut command, Duration::from_millis(50), 32)?;
    assert!(out.uncertain);
    assert_eq!(out.stdout, b"started");
    for limit in [0, 16 * 1024 * 1024 + 1] {
        assert!(
            capture_bytes(
                &mut Command::new("/bin/echo"),
                Duration::from_secs(1),
                limit
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn owned_capture_returns_spawn_pid_even_when_guard_fails_after_spawn() -> TestResult {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "printf '%s' $$"]).process_group(0);
    let captured =
        capture_bytes_guarded_process(&mut command, Duration::from_secs(1), 64, || Ok(()));
    let pid = captured.pid.ok_or("pid")?;
    let output = captured.result?;
    assert_eq!(String::from_utf8(output.stdout)?, pid.to_string());
    let mut absent =
        std::process::Command::new("/private/tmp/macops-definitely-missing-query-program");
    let captured =
        capture_bytes_guarded_process(&mut absent, Duration::from_secs(1), 64, || Ok(()));
    assert!(captured.pid.is_none());
    assert!(captured.result.is_err());
    let mut command = std::process::Command::new("/bin/sh");
    command.args(["-c", "/bin/sleep 2"]).process_group(0);
    let mut checks = 0;
    let captured = capture_bytes_guarded_process(&mut command, Duration::from_secs(1), 64, || {
        checks += 1;
        if checks > 1 {
            Err(crate::error::Error::new("guard_changed", "synthetic"))
        } else {
            Ok(())
        }
    });
    assert!(captured.pid.is_some());
    assert_eq!(captured.result.err().ok_or("guard")?.code, "guard_changed");
    Ok(())
}

#[test]
fn direct_query_exit_does_not_mean_its_owned_helper_group_has_stopped() -> TestResult {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "/bin/sleep 1 & printf done"])
        .process_group(0);
    let captured =
        capture_bytes_guarded_process(&mut command, Duration::from_secs(2), 64, || Ok(()));
    let pid = captured.pid.ok_or("pid")?;
    assert_eq!(captured.result?.code, Some(0));
    assert!(group_alive(pid));
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while group_alive(pid) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(!group_alive(pid));
    Ok(())
}

#[test]
fn owned_query_does_not_inherit_private_file_or_stdin_descriptors() -> TestResult {
    use std::{
        io::Write,
        os::{fd::AsRawFd, unix::process::CommandExt},
        process::Stdio,
    };
    let mut private = tempfile::tempfile()?;
    private.write_all(b"SYNTHETIC_PRIVATE_DESCRIPTOR")?;
    let fd = private.as_raw_fd();
    let script =
        format!("if [ -e /dev/fd/{fd} ];then /bin/cat /dev/fd/{fd} 2>/dev/null;fi; /bin/cat");
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", &script])
        .stdin(Stdio::null())
        .process_group(0);
    let captured =
        capture_bytes_guarded_process(&mut command, Duration::from_secs(1), 1024, || Ok(()));
    assert!(captured.pid.is_some());
    let output = captured.result?;
    assert_eq!(output.code, Some(0));
    assert!(
        !output
            .stdout
            .windows(b"SYNTHETIC_PRIVATE_DESCRIPTOR".len())
            .any(|v| v == b"SYNTHETIC_PRIVATE_DESCRIPTOR")
    );
    Ok(())
}

#[test]
fn bound_capture_keeps_raw_output_anonymous_and_refuses_before_spawn_when_namespace_changes()
-> TestResult {
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let root = crate::platform::fs::Directory::open(directory.path())?;
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "printf private-output; printf private-error >&2"])
        .process_group(0);
    let captured = capture_owned_group_in(
        &mut command,
        Duration::from_secs(1),
        1024,
        directory.path(),
        || root.verify(),
    );
    assert!(captured.pid.is_some());
    let output = captured.result?;
    assert_eq!(output.code, Some(0));
    assert_eq!(output.stdout, b"private-output");
    assert_eq!(output.stderr, b"private-error");
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "exit 0"]).process_group(0);
    let mut checks = 0;
    let refused = capture_owned_group_in(
        &mut command,
        Duration::from_secs(1),
        1024,
        directory.path(),
        || {
            checks += 1;
            if checks == 2 {
                Err(crate::error::Error::new("namespace_changed", "fixture"))
            } else {
                Ok(())
            }
        },
    );
    assert!(refused.pid.is_none());
    assert_eq!(
        refused.result.err().ok_or("guard")?.code,
        "namespace_changed"
    );
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}

#[test]
fn group_observation_distinguishes_an_empty_group_from_invalid_evidence() -> TestResult {
    assert_eq!(parse_group_members(Some(1), "")?, [] as [u32; 0]);
    assert_eq!(parse_group_members(Some(0), "101\n102\n")?, [101, 102]);
    for (code, sample) in [
        (None, ""),
        (Some(0), ""),
        (Some(1), "101\n"),
        (Some(2), ""),
        (Some(0), "garbage"),
        (Some(0), "0"),
        (Some(0), "101 102"),
        (Some(0), "\n"),
    ] {
        assert_eq!(
            parse_group_members(code, sample)
                .err()
                .ok_or("bad evidence")?
                .code,
            "native_query_group_unobserved",
            "{sample:?}"
        );
    }
    Ok(())
}

#[test]
fn query_budget_cancels_a_surviving_peer_without_signaling_an_unrelated_child() -> TestResult {
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let mut unrelated = Command::new("/bin/sleep")
        .arg("10")
        .process_group(0)
        .spawn()?;
    let ready = directory.path().join("ready");
    let delayed_effect = directory.path().join("delayed-effect");
    let mut query = Command::new("/bin/sh");
    query
        .args([
            "-c",
            r#"printf ready > "$1"; (/bin/sleep 3; printf bad > "$2") & exit 0"#,
            "fixture",
        ])
        .arg(&ready)
        .arg(&delayed_effect);
    let start = Instant::now();
    let result = capture_owned_group_in(
        &mut query,
        Duration::from_millis(350),
        4096,
        directory.path(),
        || Ok(()),
    );
    let elapsed = start.elapsed();
    let unaffected = unrelated.try_wait()?.is_none();
    unrelated.kill()?;
    unrelated.wait()?;
    assert!(ready.exists(), "the original child actually ran");
    assert!(result.result?.uncertain);
    assert!(
        elapsed >= Duration::from_millis(350) && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
    assert!(unaffected);
    std::thread::sleep(Duration::from_secs(3).saturating_sub(elapsed) + Duration::from_millis(250));
    assert!(!delayed_effect.exists());
    Ok(())
}

#[test]
fn an_exited_query_that_left_its_original_group_does_not_pin_the_lease() -> TestResult {
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let mut query = Command::new("/usr/bin/python3");
    query.args([
        "-I",
        "-S",
        "-c",
        "import os; os.setpgid(0, os.getpgid(os.getppid()))",
    ]);
    let start = Instant::now();
    let result = capture_owned_group_in(
        &mut query,
        Duration::from_secs(2),
        4096,
        directory.path(),
        || Ok(()),
    );
    assert!(
        result.pid.is_some(),
        "observer preflight must allow the test query to start"
    );
    let error = result.result.err().ok_or("group changes require unknown")?;
    assert_eq!(error.code, "native_query_group_unobserved");
    assert!(start.elapsed() < Duration::from_secs(2));
    Ok(())
}

#[test]
fn an_owned_query_does_not_spawn_when_its_observer_budget_is_exhausted() -> TestResult {
    verify_group_observer(Duration::from_secs(2))?;
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let marker = directory.path().join("must-not-start");
    let result = capture_owned_group_in(
        Command::new("/usr/bin/touch").arg(&marker),
        Duration::ZERO,
        4096,
        directory.path(),
        || Ok(()),
    );
    assert!(result.pid.is_none());
    assert_eq!(
        result.result.err().ok_or("expired preflight")?.code,
        "native_query_group_unobserved"
    );
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn an_owned_query_stops_if_its_running_audit_cannot_be_saved() -> TestResult {
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let marker = directory.path().join("must-not-continue");
    let result = capture_owned_group_with_audit(
        Command::new("/bin/sh")
            .args(["-c", r#"/bin/sleep 1; /usr/bin/touch "$1""#, "fixture"])
            .arg(&marker),
        Duration::from_secs(3),
        4096,
        directory.path(),
        || Ok(()),
        |_| {
            Err(crate::error::Error::attention(
                "fixture_audit_unavailable",
                "fixture audit refused",
            ))
        },
    );
    assert!(result.pid.is_some());
    assert_eq!(
        result.result.err().ok_or("audit refusal")?.code,
        "fixture_audit_unavailable"
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn a_reaped_process_leader_no_longer_authorizes_group_observation() -> TestResult {
    let mut child = Command::new("/usr/bin/true").process_group(0).spawn()?;
    assert!(child.wait()?.success());
    assert_eq!(
        exited_without_reaping(&child)
            .err()
            .ok_or("reaped leader")?
            .code,
        "native_query_group_unobserved"
    );
    Ok(())
}

#[test]
fn an_owned_query_stops_if_its_scope_changes_after_spawn() -> TestResult {
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let marker = directory.path().join("must-not-continue");
    let mut checks = 0;
    let result = capture_owned_group_in(
        Command::new("/bin/sh")
            .args(["-c", r#"/bin/sleep 1; /usr/bin/touch "$1""#, "fixture"])
            .arg(&marker),
        Duration::from_secs(3),
        4096,
        directory.path(),
        || {
            checks += 1;
            if checks < 3 {
                return Ok(());
            }
            Err(crate::error::Error::attention(
                "fixture_scope_changed",
                "fixture guard refused",
            ))
        },
    );
    assert!(result.pid.is_some());
    assert_eq!(
        result.result.err().ok_or("guard refusal")?.code,
        "fixture_scope_changed"
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!marker.exists());
    Ok(())
}
