#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;

fn context(home: &Path) -> Result<Context> {
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(changed)?,
        "config",
        "list",
    ])
    .map_err(|_| changed())?;
    Context::from_cli(&cli, &mut vec![])
}

#[test]
fn native_paths_and_query_errors_do_not_echo_secret_stdout() -> TestResult {
    assert_eq!(
        path_output(b"/fixture/home\n")?,
        PathBuf::from("/fixture/home")
    );
    for bytes in [
        b"relative SYNTHETIC_SECRET".as_slice(),
        b"/fixture\nSYNTHETIC_SECRET",
        b"/fixture\r",
        b"\xff",
    ] {
        let error = path_output(bytes).err().ok_or("expected error")?;
        assert!(!error.to_string().contains("SYNTHETIC_SECRET"));
    }
    for (code, uncertain, stdout) in [
        (Some(0), false, b"ok".to_vec()),
        (Some(1), false, b"SYNTHETIC_SECRET".to_vec()),
        (None, true, b"SYNTHETIC_SECRET".to_vec()),
        (Some(0), false, vec![0xff]),
    ] {
        let output = crate::process::BinaryOutput {
            code,
            uncertain,
            stdout,
            stderr: b"SYNTHETIC_SECRET".to_vec(),
        };
        if code == Some(0) && !uncertain && output.stdout == b"ok" {
            assert_eq!(success(output)?, b"ok");
        } else {
            assert!(
                !success(output)
                    .err()
                    .ok_or("error")?
                    .to_string()
                    .contains("SYNTHETIC_SECRET")
            );
        }
    }
    Ok(())
}

#[test]
fn source_stamps_handle_missing_parents_types_and_namespace_changes() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let missing = home.join("missing/leaf");
    assert!(stamp(&missing, true)?.identity.is_none());
    let file = home.join("data");
    std::fs::write(&file, "before")?;
    let before = stamp(&file, true)?;
    assert!(stamp(&file, false)?.identity.is_some());
    std::fs::write(&file, "after")?;
    assert_ne!(before, stamp(&file, true)?);
    let alias = home.join("alias");
    symlink(&file, &alias)?;
    assert!(stamp(&alias, true).is_err());
    assert!(stamp(&home, true).is_err());
    assert!(stamp(Path::new("/"), false).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
    stamp(&file, true)?;
    root(&home)?;
    let ctx = context(&home)?;
    let env = environment(&ctx);
    assert_eq!(env.get("HOME"), home.to_str().map(str::to_owned).as_ref());
    assert!(!env.contains_key("NODE_OPTIONS"));
    assert!(read::<serde_json::Value>(&ctx, "unknown.json")?.is_none());
    let store = Store::lock(&ctx)?;
    assert!(read::<serde_json::Value>(&ctx, "unknown.json")?.is_none());
    Store::atomic(
        &store.root,
        "invalid.json",
        &serde_json::json!({"present":true}),
    )?;
    assert_eq!(
        read::<serde_json::Value>(&ctx, "invalid.json")?,
        Some(serde_json::json!({"present":true}))
    );
    std::fs::write(ctx.state.join("duplicate.json"), b"{\"x\":1,\"x\":2}")?;
    assert!(read::<serde_json::Value>(&ctx, "duplicate.json").is_err());
    Ok(())
}

#[test]
fn native_runtime_metadata_owns_default_and_configured_state_selection() -> TestResult {
    let default=br#"{"configFile":"/fixture/custom/chezmoi.toml","cache":"/fixture/native-cache","configuredState":"","resolvedDefault":"/fixture/custom/chezmoistate.boltdb"}"#;
    assert_eq!(
        native_paths(default)?.2,
        PathBuf::from("/fixture/custom/chezmoistate.boltdb")
    );
    let configured=br#"{"configFile":"/fixture/custom/chezmoi.toml","cache":"/fixture/native-cache","configuredState":"/fixture/explicit-state.boltdb","resolvedDefault":"/fixture/custom/chezmoistate.boltdb"}"#;
    assert_eq!(
        native_paths(configured)?.2,
        PathBuf::from("/fixture/explicit-state.boltdb")
    );
    for invalid in [b"[1]".as_slice(), br#"{"cache":1,"cache":2}"#, b"{}"] {
        assert!(native_paths(invalid).is_err());
    }
    let mut bad: serde_json::Value = serde_json::from_slice(default)?;
    bad["cache"] = serde_json::json!("relative-secret-value");
    assert!(
        !native_paths(&serde_json::to_vec(&bad)?)
            .err()
            .ok_or("error")?
            .to_string()
            .contains("relative-secret-value")
    );
    Ok(())
}

#[test]
fn native_query_waits_for_early_exit_peer_and_never_calls_it_certain() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let ctx = context(&home)?;
    let store = Store::lock(&ctx)?;
    let marker = home.join("peer-finished");
    let script = format!(
        "(/bin/sleep 1; printf finished > '{}') >/dev/null 2>&1 & printf done",
        marker.display()
    );
    let output = query(
        &ctx.scope,
        &CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
        },
        &environment(&ctx),
        2,
        &ctx.state,
    )?;
    assert_eq!(output.code, Some(0));
    assert!(output.uncertain);
    assert!(marker.exists());
    store.verify_namespace()?;
    Ok(())
}

#[test]
fn native_query_does_not_signal_a_group_after_reaping_its_leader() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let ctx = context(&home)?;
    let store = Store::lock(&ctx)?;
    let marker = home.join("peer-finished");
    let script = format!(
        "(/bin/sleep 2; printf finished > '{}') >/dev/null 2>&1 & printf done",
        marker.display()
    );
    let output = query(
        &ctx.scope,
        &CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
        },
        &environment(&ctx),
        1,
        &ctx.state,
    )?;
    assert_eq!(output.code, Some(0));
    assert!(output.uncertain);
    assert_eq!(std::fs::read(&marker)?, b"finished");
    store.verify_namespace()?;
    Ok(())
}

#[test]
fn failed_query_audit_cannot_release_before_its_peer_finishes() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let ctx = context(&home)?;
    let _store = Store::lock(&ctx)?;
    let marker = home.join("peer-finished");
    let script = format!(
        "/bin/chmod 500 '{}'; (/bin/sleep 1; printf finished > '{}') >/dev/null 2>&1 & printf done",
        ctx.state.display(),
        marker.display()
    );
    let result = query(
        &ctx.scope,
        &CommandSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script],
        },
        &environment(&ctx),
        2,
        &ctx.state,
    );
    std::fs::set_permissions(&ctx.state, std::fs::Permissions::from_mode(0o700))?;
    assert!(result.is_err());
    assert!(marker.exists());
    Ok(())
}
