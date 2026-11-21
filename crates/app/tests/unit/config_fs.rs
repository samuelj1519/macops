#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn supplementary_profile_lookup_uses_its_original_binding_after_a_child_changes() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-config-epoch-")
        .tempdir_in("/private/tmp")?;
    let base = temp.path().join("config");
    let outside = temp.path().join("outside");
    fs::create_dir_all(base.join("profile"))?;
    fs::create_dir_all(&outside)?;
    let path = base.join("profile/config.toml");
    fs::write(&path, "old")?;
    fs::write(outside.join("config.toml"), "outside")?;
    fs::set_permissions(
        outside.join("config.toml"),
        fs::Permissions::from_mode(0o777),
    )?;
    let mut bindings = Bindings::new(8);
    let root = RootBinding::capture(&base, &mut bindings);
    let known = vec![(path.clone(), bindings.target(&path))];
    fs::rename(base.join("profile"), base.join("old-profile"))?;
    symlink(&outside, base.join("profile"))?;
    let snapshot = filesystem(&base, root, known, &[])?;
    assert!(!snapshot.entries.contains_key(&path));
    assert_eq!(snapshot.coverage.state, CoverageState::Invalid);
    assert!(
        snapshot
            .coverage
            .issues
            .iter()
            .any(|s| s == "directory_changed")
    );
    Ok(())
}

#[test]
fn supplementary_project_lookup_never_reopens_an_exchanged_configuration_root() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-config-epoch-")
        .tempdir_in("/private/tmp")?;
    let base = temp.path().join("config");
    let outside = temp.path().join("outside");
    fs::create_dir_all(base.join("project"))?;
    fs::create_dir_all(outside.join("project"))?;
    let path = base.join("project/mise.toml");
    fs::write(&path, "old")?;
    fs::write(outside.join("project/mise.toml"), "outside")?;
    fs::set_permissions(
        outside.join("project/mise.toml"),
        fs::Permissions::from_mode(0o777),
    )?;
    let mut bindings = Bindings::new(8);
    let root = RootBinding::capture(&base, &mut bindings);
    let known = vec![(path.clone(), bindings.target(&path))];
    fs::rename(&base, temp.path().join("old-config"))?;
    symlink(&outside, &base)?;
    let snapshot = filesystem(&base, root, known, &[])?;
    assert!(snapshot.entries.is_empty());
    assert_eq!(snapshot.coverage.state, CoverageState::Invalid);
    assert_eq!(
        fs::read_to_string(outside.join("project/mise.toml"))?,
        "outside"
    );
    Ok(())
}

#[test]
fn inventory_keeps_other_known_files_when_the_absent_config_root_appears() -> TestResult {
    use clap::Parser;

    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let base = home.join(".config");
    fs::create_dir(&home)?;
    let project = base.join("project");
    let shell = home.join(".zshrc");
    fs::write(&shell, b"fixture shell settings")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("fixture HOME")?,
        "list",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let scan = || {
        let mut bindings = Bindings::new(64);
        let root = RootBinding::capture(&base, &mut bindings);
        let known = known_targets(&ctx, &[], Some(&project), &base, &root, &mut bindings);
        (root, known)
    };

    let (root, known) = scan();
    let absent = filesystem(&base, root, known, &[])?;
    assert_eq!(absent.coverage.state, CoverageState::Complete);
    assert_eq!(absent.entries.keys().collect::<Vec<_>>(), [&shell]);

    let (root, known) = scan();
    fs::create_dir_all(&project)?;
    let created = project.join("mise.toml");
    fs::write(&created, b"created during scan")?;
    let changed = filesystem(&base, root, known, &[])?;
    assert_eq!(changed.coverage.state, CoverageState::Invalid);
    assert!(
        changed
            .coverage
            .issues
            .iter()
            .any(|code| code == "directory_changed")
    );
    assert_eq!(changed.entries.keys().collect::<Vec<_>>(), [&shell]);
    assert_eq!(fs::read(created)?, b"created during scan");
    Ok(())
}

#[test]
fn cached_virtual_target_reports_missing_or_changed_bound_destination() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let destination = temp.path().join("destination");
    let target = destination.join("generated.conf");
    let mut mapping = config_inventory::Snapshot {
        roots: Some(config_inventory::Roots {
            config: None,
            configured_source: temp.path().join("source"),
            effective_source: temp.path().join("source"),
            destination: destination.clone(),
            provenance: "native-cached".into(),
        }),
        candidates: vec![],
        native_targets: vec![target.clone()],
        destination: Some(config_inventory::Destination::capture(&destination)?),
        coverage: Coverage {
            source: "chezmoi-native-cache".into(),
            state: CoverageState::Unsupported,
            count: 1,
            roots: vec![],
            issues: vec![],
        },
    };
    let observe = |mapping: &config_inventory::Snapshot| -> Result<(Resource, Coverage)> {
        let mut bindings = Bindings::new(64);
        let mut entries = BTreeMap::new();
        let mut coverage = mapping.coverage.clone();
        insert_virtual_targets(mapping, &mut bindings, &mut entries, &mut coverage, &[])?;
        let record = entries.remove(&target).ok_or_else(|| {
            crate::error::Error::new("fixture_missing", "expected a cached target")
        })?;
        Ok((record, coverage))
    };

    let (record, coverage) = observe(&mapping)?;
    assert_eq!(record.status, State::Missing);
    assert_eq!(record.management, Management::Native);
    assert_eq!(coverage.state, CoverageState::Unsupported);

    fs::create_dir(&destination)?;
    mapping.destination = Some(config_inventory::Destination::capture(&destination)?);
    fs::rename(&destination, temp.path().join("old-destination"))?;
    fs::create_dir(&destination)?;
    fs::write(&target, b"replacement destination")?;
    let (record, coverage) = observe(&mapping)?;
    assert_eq!(record.status, State::Unknown);
    assert_eq!(coverage.state, CoverageState::Invalid);
    assert_eq!(coverage.issues, ["native_virtual_target_unobserved"]);
    let Data::Config {
        owner,
        mode,
        source_candidates,
        ..
    } = record.data
    else {
        return Err("expected config resource".into());
    };
    assert_eq!(owner, "chezmoi-native-virtual");
    assert_eq!(mode, None);
    assert_eq!(source_candidates, [] as [SourceRef; 0]);
    assert_eq!(fs::read(target)?, b"replacement destination");
    Ok(())
}

#[test]
fn configuration_scan_reports_limits_and_skips_nonconfiguration_subtrees() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let base = temp.path().join("config");
    let depth = base.join("nested/one/two/three");
    let excluded = base.join("node_modules");
    fs::create_dir_all(&depth)?;
    fs::create_dir_all(&excluded)?;
    let deep = depth.join("generated.toml");
    let not_config = excluded.join("package.json");
    let top = base.join("settings.toml");
    for path in [&deep, &not_config, &top] {
        fs::write(path, b"fixture")?;
    }
    let root = RootBinding::capture(&base, &mut Bindings::new(64));
    let report = filesystem(&base, root, vec![], &[])?;
    assert_eq!(report.entries.keys().collect::<Vec<_>>(), [&top]);
    assert_eq!(report.coverage.state, CoverageState::Truncated);
    assert!(
        report
            .coverage
            .issues
            .iter()
            .any(|code| code == "config_scan_limit")
    );
    assert!(
        report
            .coverage
            .issues
            .iter()
            .any(|code| code == &format!("excluded_directory:{}", excluded.display()))
    );

    let singleton = temp.path().join("one-entry");
    fs::create_dir(&singleton)?;
    fs::write(singleton.join("config.toml"), b"first")?;
    let mut bounded = report.coverage;
    bounded.state = CoverageState::Complete;
    bounded.issues.clear();
    let directory = Directory::open(&singleton)?;
    let rows = fs_branch(
        &directory,
        &singleton,
        0,
        &mut Budget::new(1, 4),
        &mut bounded,
        &[],
    )?;
    assert_eq!(rows.len(), 1);
    assert_eq!(bounded.state, CoverageState::Truncated);
    assert_eq!(bounded.issues, ["config_scan_limit"]);
    Ok(())
}
