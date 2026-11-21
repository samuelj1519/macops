#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn scope_values_distinguish_unset_empty_relative_and_absolute() {
    assert_eq!(scope_value("XDG_STATE_HOME", None), (None, None));
    assert_eq!(
        scope_value("XDG_CONFIG_HOME", Some(Path::new(""))),
        (None, None)
    );
    let (value, warning) = scope_value("XDG_CACHE_HOME", Some(Path::new("relative")));
    assert!(value.is_none());
    assert!(warning.is_some_and(|w| w.contains("XDG_CACHE_HOME")));
    assert_eq!(
        scope_value("XDG_DATA_HOME", Some(Path::new("/data"))),
        (Some(PathBuf::from("/data")), None)
    );
    assert_eq!(
        scope_value("UV_TOOL_DIR", Some(Path::new("relative"))),
        (Some(PathBuf::from("relative")), None)
    );
    assert_eq!(scope_value("CARGO_HOME", None), (None, None));
}

#[test]
fn roots_obey_precedence_and_validate_the_selected_absolute_path() -> TestResult {
    let home = Path::new("/fixture/home");
    assert_eq!(
        app_root(None, None, home, ".cache/macops")?,
        home.join(".cache/macops")
    );
    assert_eq!(
        app_root(None, Some(Path::new("/cache")), home, ".cache/macops")?,
        PathBuf::from("/cache/macops")
    );
    assert_eq!(
        app_root(
            Some(Path::new("/chosen/./state")),
            Some(Path::new("/cache")),
            home,
            ".cache/macops"
        )?,
        PathBuf::from("/chosen/state")
    );
    assert!(app_root(Some(Path::new("relative")), None, home, ".cache/macops").is_err());
    assert!(
        app_root(
            None,
            Some(Path::new("/Users/example/job")),
            home,
            ".cache/macops"
        )
        .is_err()
    );
    assert!(
        app_root(
            None,
            Some(Path::new("/cache/../other")),
            home,
            ".cache/macops"
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn default_state_switch_requires_explicit_migration_when_old_state_exists() {
    let old = Path::new("/old");
    let new = Path::new("/new");
    assert!(!state_migration_required(true, old, new, true));
    assert!(!state_migration_required(false, old, old, true));
    assert!(!state_migration_required(false, old, new, false));
    assert!(state_migration_required(false, old, new, true));
}
