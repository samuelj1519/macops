use super::common::{Fixture, TestResult, assert_code};
use std::fs;

fn legacy_canary(fixture: &Fixture) -> TestResult {
    let bin = fixture.home.join(".local/bin");
    fs::create_dir_all(&bin)?;
    let script = fixture.script(
        "legacy-mo",
        "base=$(/usr/bin/dirname \"$0\"); printf invoked > \"$base/last-args\"; exit 93",
    )?;
    fs::copy(script, bin.join("mo"))?;
    fs::write(bin.join("last-args"), "untouched")?;
    Ok(())
}

#[test]
fn native_commands_never_dispatch_to_the_retired_backend() -> TestResult {
    let f = Fixture::new()?;
    legacy_canary(&f)?;
    for args in [
        vec!["agent", "tools"],
        vec!["skill"],
        vec!["skill", "demo", "--details", "--only", "demo"],
        vec!["skill", "doctor", "--only", "demo"],
        vec!["skill", "doctor", "demo", "--only", "demo"],
        vec!["skill", "outdated", "--only", "demo"],
        vec!["skill", "policy", "check"],
        vec!["skill", "policy", "sync", "--change-id", "CASE-1"],
        vec!["skill", "policy", "sync"],
        vec!["skill", "sync", "--only", "demo"],
        vec!["skill", "policy", "recover", "--change-id", "CASE-2"],
        vec!["skill", "refresh", "demo"],
        vec!["skill", "upgrade", "demo"],
        vec!["config", "list"],
        vec!["config", "where", "query"],
        vec!["config", "diff"],
        vec!["config", "reconcile", "codex"],
        vec!["agent", "upgrade"],
        vec!["agent", "upgrade", "codex"],
        vec!["agent", "install", "dsh", "1.0.0-rc.1", "--force-live"],
        vec!["agent", "install", "codex", "1.0.0"],
        vec!["agent", "credential", "list"],
        vec!["agent", "credential", "check", "mi"],
        vec!["agent", "credential", "add", "mi", "alias"],
        vec!["agent", "credential", "activate", "mi", "alias"],
        vec!["app", "doctor"],
    ] {
        let mut preview = vec!["--dry-run"];
        preview.extend(args);
        let (output, value) = f.run(&preview)?;
        assert_ne!(output.status.code(), Some(93), "{preview:?}: {value}");
        assert_ne!(value["data"]["backend"], "legacy-mo", "{preview:?}");
        assert_eq!(
            fs::read_to_string(f.home.join(".local/bin/last-args"))?,
            "untouched"
        );
        assert!(!f.file("state").exists(), "{preview:?}: {value}");
        assert!(!f.file("cache").exists(), "{preview:?}: {value}");
    }
    Ok(())
}

#[test]
fn native_selection_and_change_id_validation_replace_legacy_route_validation() -> TestResult {
    let f = Fixture::new()?;
    legacy_canary(&f)?;
    for args in [
        vec!["skill", "upgrade"],
        vec!["skill", "upgrade", "demo", "--all"],
        vec!["skill", "upgrade", "--all", "--only", "demo"],
        vec!["skill", "sync", "demo", "--revision", "short"],
        vec![
            "skill",
            "sync",
            "demo",
            "--revision",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ],
        vec!["skill", "sync", "demo", "--change-id", ""],
        vec!["skill", "sync", "demo", "--change-id", "../escape"],
        vec!["skill", "refresh", "--revision", "bad"],
        vec!["skill", "policy", "sync", "--change-id", "bad/unsafe"],
        vec!["skill", "policy", "recover", "--change-id", "bad/unsafe"],
        vec!["skill", "policy", "sync", "--change-id", "../outside"],
        vec![
            "skill",
            "refresh",
            "--only",
            "source",
            "--change-id",
            "CASE-1",
        ],
        vec!["skill", "refresh", "--change-id", "../escape"],
        vec!["agent", "install", "dsh"],
    ] {
        let (output, value) = f.run(&args)?;
        assert!(
            matches!(output.status.code(), Some(1..=3)),
            "{args:?}: {value}"
        );
        assert!(value["error"]["code"].is_string(), "{args:?}: {value}");
        assert_eq!(
            fs::read_to_string(f.home.join(".local/bin/last-args"))?,
            "untouched",
            "{args:?}: {value}"
        );
        assert!(!f.file("state").exists());
        assert!(!f.file("cache").exists());
    }
    Ok(())
}

#[test]
fn empty_native_policy_sync_preserves_zero_write_preview_and_noop() -> TestResult {
    let f = Fixture::new()?;
    legacy_canary(&f)?;
    let (output, _) = f.run(&["skill", "policy", "sync", "--dry-run"])?;
    assert_code(&output, 0);
    for _ in 0..2 {
        let (output, value) = f.run(&["skill", "policy", "sync", "--yes"])?;
        assert_code(&output, 0);
        assert_eq!(value["data"]["backend"], "rust");
        assert_eq!(value["data"]["status"], "same");
        assert_eq!(value["data"]["persistentWrites"], false);
    }
    assert_eq!(
        fs::read_to_string(f.home.join(".local/bin/last-args"))?,
        "untouched"
    );
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}
