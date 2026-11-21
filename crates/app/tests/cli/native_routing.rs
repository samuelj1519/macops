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
        vec!["ai", "list"],
        vec!["skills", "list"],
        vec!["skills", "list", "demo", "--details", "--only", "demo"],
        vec!["skills", "check", "--only", "demo"],
        vec!["skills", "status", "demo", "--only", "demo"],
        vec!["skills", "outdated", "--only", "demo"],
        vec!["skills", "policy", "check"],
        vec!["skills", "policy", "sync", "--change-id", "CASE-1"],
        vec!["skills", "policy", "sync"],
        vec!["skills", "sync", "--only", "demo"],
        vec!["skills", "policy", "recover", "--change-id", "CASE-2"],
        vec!["skills", "update", "demo"],
        vec!["skills", "upgrade", "demo"],
        vec!["config", "list"],
        vec!["config", "where", "query"],
        vec!["config", "diff"],
        vec!["config", "reconcile", "codex"],
        vec!["ai", "reconcile"],
        vec!["ai", "reconcile", "codex"],
        vec!["ai", "upgrade", "dsh", "1.0.0-rc.1", "--force-live"],
        vec!["app", "upgrade", "codex", "1.0.0"],
        vec!["credential", "list"],
        vec!["credential", "check", "mi"],
        vec!["credential", "add", "mi", "alias"],
        vec!["credential", "activate", "mi", "alias"],
        vec!["doctor"],
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
        vec!["skills", "upgrade"],
        vec!["skills", "upgrade", "demo", "--all"],
        vec!["skills", "upgrade", "--all", "--only", "demo"],
        vec!["skills", "sync", "demo", "--revision", "short"],
        vec![
            "skills",
            "sync",
            "demo",
            "--revision",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        ],
        vec!["skills", "sync", "demo", "--change-id", ""],
        vec!["skills", "sync", "demo", "--change-id", "../escape"],
        vec!["skills", "update", "--revision", "bad"],
        vec!["skills", "policy", "sync", "--change-id", "bad/unsafe"],
        vec!["skills", "policy", "recover", "--change-id", "bad/unsafe"],
        vec!["skills", "policy", "sync", "--change-id", "../outside"],
        vec![
            "skills",
            "update",
            "--only",
            "source",
            "--change-id",
            "CASE-1",
        ],
        vec!["skills", "update", "--change-id", "../escape"],
        vec!["ai", "upgrade", "dsh"],
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
    let (output, _) = f.run(&["skills", "policy", "sync", "--dry-run"])?;
    assert_code(&output, 0);
    for _ in 0..2 {
        let (output, value) = f.run(&["skills", "policy", "sync", "--yes"])?;
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
