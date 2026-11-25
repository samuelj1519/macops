#![cfg(test)]
use super::*;
use crate::{
    domain::resource::{Coverage, CoverageState},
    test_support::TestResult,
};
use std::fs;

fn report() -> Coverage {
    Coverage {
        source: "npm".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    }
}

#[test]
fn namespace_scan_counts_hidden_entries_and_stops_at_global_budget() -> TestResult {
    let prefix = tempfile::tempdir_in("/private/tmp")?;
    let node = prefix.path().join("lib/node_modules");
    fs::create_dir_all(&node)?;
    for index in 0..12 {
        fs::write(node.join(format!(".hidden-{index}")), "opaque")?;
    }
    let mut budget = ScanBudget {
        entries: 4,
        bytes: 1024,
    };
    let mut coverage = report();
    let mut namespace = NamespaceScan {
        prefix: prefix.path().to_path_buf(),
        sources: ["catalog"].into(),
        expected: vec![],
    };
    let rows = scan_namespace(&mut namespace, &mut budget, &mut coverage)?;
    assert!(rows.is_empty());
    assert_eq!(budget.entries, 0);
    assert_eq!(coverage.state, CoverageState::Truncated);
    assert!(coverage.issues.contains(&"npm_inventory_limit".into()));
    Ok(())
}

#[test]
fn manifest_budget_and_special_files_report_unknown_without_following_them() -> TestResult {
    let prefix = tempfile::tempdir_in("/private/tmp")?;
    let node = prefix.path().join("lib/node_modules");
    let dir = node.join("demo");
    fs::create_dir_all(&dir)?;
    let package = dir.join("package.json");
    fs::write(&package, br#"{"name":"demo","version":"1.0.0"}"#)?;
    let query = capture(prefix.path(), "demo", NpmContract::DeclaredV1, false)?;
    assert_eq!(
        observe(&query, false, &mut 16).err().ok_or("limit")?.code,
        "npm_manifest_limit"
    );
    fs::rename(&package, dir.join("outside.json"))?;
    std::os::unix::fs::symlink(dir.join("outside.json"), &package)?;
    assert_eq!(
        observe(&query, false, &mut 1024).err().ok_or("link")?.code,
        "unsupported_npm_manifest"
    );
    Ok(())
}

#[test]
fn plain_namespace_write_checks_and_npmrc_bindings_fail_closed_on_symlinks_and_changes()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let prefix = temp.path().join("prefix");
    let pkg_dir = prefix.join("lib/node_modules/@scope/demo");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&pkg_dir)?;
    fs::write(
        pkg_dir.join("package.json"),
        br#"{"name":"@scope/demo","version":"1.0.0"}"#,
    )?;
    fs::write(
        home.join(".npmrc"),
        "//registry.npmjs.org/:_authToken=SYNTHETIC_SECRET_TOKEN\n",
    )?;
    let q = capture_with_configs(
        Some(&home),
        &prefix,
        "@scope/demo",
        NpmContract::DeclaredV1,
        false,
    )?;
    assert!(!serde_json::to_string(&q)?.contains("SYNTHETIC_SECRET_TOKEN"));
    verify_plain_namespace_for_write(&q, Action::Adopt)?;
    verify_plain_namespace_for_write(&q, Action::Uninstall)?;
    let bindings = q.config_bindings.as_ref().ok_or("bindings")?;
    verify_config_bindings(&home, &prefix, bindings)?;

    fs::write(home.join(".npmrc"), "changed=1\n")?;
    assert_eq!(
        verify_config_bindings(&home, &prefix, bindings)
            .err()
            .ok_or("changed")?
            .code,
        "npm_config_changed"
    );
    fs::remove_file(home.join(".npmrc"))?;
    std::os::unix::fs::symlink("/etc/hosts", home.join(".npmrc"))?;
    assert_eq!(
        capture_config_bindings(&home, &prefix)
            .err()
            .ok_or("symlink npmrc")?
            .code,
        "unsupported_npm_config"
    );

    fs::remove_dir_all(&pkg_dir)?;
    assert_eq!(
        verify_plain_namespace_for_write(&q, Action::Uninstall)
            .err()
            .ok_or("missing pkg")?
            .code,
        "npm_namespace_changed"
    );
    std::os::unix::fs::symlink(temp.path(), &pkg_dir)?;
    assert_eq!(
        verify_plain_namespace_for_write(&q, Action::Install)
            .err()
            .ok_or("symlink pkg")?
            .code,
        "npm_namespace_link_unsafe"
    );
    Ok(())
}
