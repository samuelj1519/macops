use super::*;
use crate::test_support::TestResult;
use std::fs;

#[test]
fn global_footprint_covers_layers_explicit_write_paths_and_physical_aliases() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path();
    let config = home.join("misescoped");
    let explicit = home.join("configured.toml");
    let local = config.join("config.local.toml");
    let fragments = config.join("conf.d");
    fs::create_dir_all(fragments.join("module"))?;
    fs::write(&local, b"[tools]\nnode='1.0.0'\n")?;
    let alias = home.join("same-file");
    fs::hard_link(&local, &alias)?;
    let linked = home.join("linked-local");
    std::os::unix::fs::symlink(&local, &linked)?;
    let plan = Footprint::capture(home, |key| match key {
        "MISE_CONFIG_DIR" => Some(config.clone()),
        "MISE_CONFIG_FILE" => Some(explicit.clone()),
        _ => None,
    })?;
    for target in [
        local.clone(),
        alias,
        linked,
        explicit.clone(),
        home.join(".tool-versions"),
        config.join("config.toml"),
        config.join("config.fixture.local.toml"),
        fragments.join("tool.toml"),
        fragments.join("module/mise.fixture.toml"),
    ] {
        assert!(plan.contains(&target)?, "{}", target.display());
    }
    for path in [
        config.join("config.override.toml"),
        config.join("tool.txt"),
        fragments.join(".hidden.toml"),
        fragments.join("module/nested/mise.toml"),
        home.join("foreign.toml"),
    ] {
        assert!(!plan.contains(&path)?, "{}", path.display());
    }
    assert!(plan.overlaps(home)?);
    assert!(plan.overlaps(&fragments)?);
    assert!(!plan.overlaps(&home.join("disjoint"))?);
    let explicit_only = Footprint::capture(home, |key| {
        (key == "MISE_GLOBAL_CONFIG_FILE").then(|| explicit.clone())
    })?;
    assert!(explicit_only.contains(&explicit)?);
    assert!(!explicit_only.contains(&local)?);
    assert!(explicit_only.overlaps(home)?);
    let composed = home.join("café.toml");
    let decomposed = home.join("cafe\u{301}.toml");
    let unicode = Footprint::capture(home, |key| {
        (key == "MISE_GLOBAL_CONFIG_FILE").then(|| composed.clone())
    })?;
    assert!(unicode.contains(&decomposed)?);
    assert!(plan.contains(&home.join("miseſcoped/config.toml"))?);
    let distinct = home.join("native-日本.toml");
    fs::write(&composed, b"one")?;
    fs::write(&distinct, b"two")?;
    assert!(!unicode.contains(&distinct)?);
    Ok(())
}

#[test]
fn emitted_worker_environment_selects_the_same_bound_default_and_rejects_missing_home() -> TestResult
{
    let root = tempfile::tempdir_in("/private/tmp")?;
    let command = Command::new("/usr/bin/true");
    assert!(command_footprint(&command).is_err());
    let mut command = Command::new("/usr/bin/true");
    command.env("HOME", root.path());
    command.env("XDG_CONFIG_HOME", root.path().join("xdg"));
    assert!(command_footprint(&command)?.contains(&root.path().join("xdg/mise/config.toml"))?);
    assert!(!command_footprint(&command)?.contains(&root.path().join(".config/mise/config.toml"))?);
    Ok(())
}

#[test]
fn bound_mise_namespace_refuses_changed_data_link_and_preserves_the_explicit_scope() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path();
    let a = root.join("first");
    let b = root.join("second");
    let link = root.join("data");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    std::os::unix::fs::symlink(&a, &link)?;
    let spec = CommandSpec {
        program: PathBuf::from("/usr/bin/true"),
        args: vec![],
    };
    let scope = Scope {
        home: root.to_owned(),
        paths: [("MISE_DATA_DIR".into(), Some(link.clone()))].into(),
        service_identity: None,
    };
    let captured = capture_query(&scope, spec)?;
    assert_eq!(captured.namespace.data, a);
    let command = query_command(&scope, &captured)?;
    assert_eq!(command_path(&command, "MISE_DATA_DIR"), Some(a));
    assert_eq!(
        command_path(&command, "MISE_CONFIG_DIR"),
        Some(root.join(".config/mise"))
    );
    fs::remove_file(&link)?;
    std::os::unix::fs::symlink(&b, &link)?;
    let err = query_command(&scope, &captured)
        .err()
        .ok_or("scope retarget refusal")?;
    assert_eq!(err.code, "mise_scope_changed");
    assert!(
        bind_mutation(
            &scope,
            &crate::test_support::journal()?.plan,
            &mut Command::new("/usr/bin/true")
        )
        .is_err()
    );
    Ok(())
}
