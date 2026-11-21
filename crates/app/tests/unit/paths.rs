#![cfg(test)]
use super::*;
use crate::test_support::*;

#[test]
fn path_rules_cover_boundaries_and_link_normalization() -> TestResult {
    for path in [
        "relative",
        "/fixture/../other",
        "/Users/example/job",
        "/Users/example/job/anything",
        "/users/Example/JOB/document",
        "/Users/another-account/job",
    ] {
        assert!(lexical(Path::new(path)).is_err());
    }
    assert_eq!(
        lexical(Path::new("/fixture/./data"))?,
        PathBuf::from("/fixture/data")
    );
    lexical(Path::new("/Users/example/job2"))?;
    assert_eq!(
        normalize_link(Path::new("/fixture/a/../b"))?,
        PathBuf::from("/fixture/b")
    );
    assert_eq!(
        normalize_link(Path::new("/fixture/./b"))?,
        PathBuf::from("/fixture/b")
    );
    assert!(normalize_link(Path::new("/Users/example/job/../elsewhere")).is_err());
    assert!(normalize_link(Path::new("relative")).is_err());
    assert!(normalize_link(Path::new("./relative")).is_err());
    for (a, b) in [("/a", "/a"), ("/a", "/a/b"), ("/a/b", "/a")] {
        assert!(separate_roots(Path::new(a), Path::new(b)).is_err());
    }
    separate_roots(Path::new("/state"), Path::new("/cache"))?;
    assert_eq!(
        path_candidates("/bin/demo", std::ffi::OsStr::new("/other")),
        vec![PathBuf::from("/bin/demo")]
    );
    assert_eq!(
        path_candidates("demo", std::ffi::OsStr::new("relative:/bin:/other")),
        vec![PathBuf::from("/bin/demo"), PathBuf::from("/other/demo")]
    );
    Ok(())
}

#[test]
fn bun_absence_requires_exact_scope_diagnostic() {
    let mut s = scope();
    let mut out = output("");
    out.code = Some(1);
    out.stderr =
        "No package.json was found for directory \"/fixture/home/.bun/install/global\"".into();
    assert_eq!(
        empty_bun_root(&s, &out),
        Some(PathBuf::from("/fixture/home/.bun/install/global"))
    );
    out.uncertain = true;
    assert!(empty_bun_root(&s, &out).is_none());
    out.uncertain = false;
    out.code = Some(0);
    assert!(empty_bun_root(&s, &out).is_none());
    out.code = Some(1);
    out.stderr = "wrong error".into();
    assert!(empty_bun_root(&s, &out).is_none());
    s.paths
        .insert("BUN_INSTALL".into(), Some("/custom/bun".into()));
    out.stderr = "No package.json was found for directory \"/custom/bun/install/global\"".into();
    assert!(empty_bun_root(&s, &out).is_some());
    s.paths.insert(
        "BUN_INSTALL_GLOBAL_DIR".into(),
        Some("/other/global".into()),
    );
    out.stderr = "No package.json was found for directory \"/other/global\"".into();
    assert!(empty_bun_root(&s, &out).is_some());
}

#[test]
fn legacy_home_paths_expand_without_accepting_relative_or_excluded_targets() -> TestResult {
    assert_eq!(
        legacy_path("~/Applications/Demo.app", Path::new("/fixture/home"))?,
        PathBuf::from("/fixture/home/Applications/Demo.app")
    );
    assert_eq!(
        legacy_path("~//Applications/Demo.app", Path::new("/fixture/home"))?,
        PathBuf::from("/fixture/home/Applications/Demo.app")
    );
    assert_eq!(
        legacy_path("~///", Path::new("/fixture/home"))?,
        PathBuf::from("/fixture/home")
    );
    assert_eq!(
        legacy_path("/Applications/Demo.app", Path::new("/fixture/home"))?,
        PathBuf::from("/Applications/Demo.app")
    );
    for raw in ["relative.app", "~other/Demo.app", "~/../elsewhere"] {
        assert!(legacy_path(raw, Path::new("/fixture/home")).is_err());
    }
    assert!(legacy_path("~/job/private", Path::new("/Users/example")).is_err());
    assert!(legacy_path("~/Applications/Demo.app", Path::new("relative")).is_err());
    Ok(())
}

#[test]
fn non_app_artifacts_require_both_legacy_markers() -> TestResult {
    let home = Path::new("/fixture/home");
    assert!(legacy_app_path("not_applicable", Some("cask_non_app_artifact"), home)?.is_none());
    assert!(legacy_app_path("not_applicable", None, home).is_err());
    assert!(legacy_app_path("not_applicable", Some("system_top_level"), home).is_err());
    assert!(legacy_app_path("relative", Some("cask_non_app_artifact"), home).is_err());
    assert_eq!(
        legacy_app_path("~/Demo.app", None, home)?,
        Some(home.join("Demo.app"))
    );
    Ok(())
}
