use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{ffi::OsStr, fs};

#[test]
fn a_prepared_generic_worker_uses_only_its_original_scope_program_and_actor_record() -> TestResult {
    let root = tempfile::tempdir_in("/private/tmp")?;
    let home = root.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let context = Context::from_cli(&cli, &mut vec![])?;
    let program = home.join("fixture-program");
    fs::write(&program, b"#!/bin/sh\nexit 97\n")?;
    let mut plan = crate::test_support::journal()?.plan;
    plan.command = Some(crate::domain::CommandSpec {
        program: program.clone(),
        args: vec!["install".into(), "fixture".into()],
    });
    let mut journal = new_journal(&context, plan)?;
    let path = context
        .state
        .join("operations")
        .join(format!("{}.json", journal.id));
    let pid: u32 = 41000;
    assert_eq!(
        prepare_worker(&journal, &path, pid, 41000)
            .err()
            .ok_or("missing phase rejection")?
            .code,
        "worker_identity_mismatch"
    );
    journal.phase = Phase::Running;
    journal.process_group = Some(pid);
    assert_eq!(
        prepare_worker(&journal, &path, pid, 41001)
            .err()
            .ok_or("missing actor rejection")?
            .code,
        "worker_identity_mismatch"
    );
    assert_eq!(
        prepare_worker(&journal, &path.with_extension("copy"), pid, 41000)
            .err()
            .ok_or("missing original path rejection")?
            .code,
        "journal_path_mismatch"
    );
    let command = prepare_worker(&journal, &path, pid, 41000)?;
    assert_eq!(command.get_program(), program.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        ["install", "fixture"]
    );
    assert_eq!(command.get_current_dir(), Some(home.as_path()));
    let value = command
        .get_envs()
        .find(|(name, _)| *name == OsStr::new("HOME"))
        .ok_or("HOME binding")?;
    assert_eq!(value.1, Some(home.as_os_str()));
    assert!(!context.state.exists());
    Ok(())
}

#[test]
fn an_npm_worker_rechecks_installed_package_evidence_before_uninstall() -> TestResult {
    use crate::domain::{CommandSpec, NpmWriteAuthority, Provider};

    let root = tempfile::tempdir_in("/private/tmp")?;
    let home = root.path().join("home");
    let prefix = home.join("npm");
    let name = "@scope/tool";
    let package_dir = prefix.join("lib/node_modules").join(name);
    fs::create_dir_all(&package_dir)?;
    fs::create_dir(prefix.join("bin"))?;
    let manifest = package_dir.join("package.json");
    fs::write(&manifest, br#"{"name":"@scope/tool","version":"1.0.0"}"#)?;
    let npmrc = home.join(".npmrc");
    fs::write(&npmrc, b"fund=false\n")?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let mut package = crate::test_support::package();
    let authority = NpmWriteAuthority {
        allow_remove: true,
        registry: None,
    };
    package.provider = Provider::Npm {
        package: name.into(),
        prefix: prefix.clone(),
        authority: Some(authority.clone()),
    };
    let query = providers::query_spec(&ctx, &package)?;
    let before = providers::observe_query(&package, &query, &ctx.scope, false)?;
    assert!(matches!(&before, Observation::Present { version, .. } if version == "1.0.0"));
    let program = home.join("fixture-npm");
    fs::write(&program, b"#!/bin/sh\nexit 97\n")?;
    let args = crate::policy::npm_local::expected_npm_write_args(
        &home,
        &prefix,
        name,
        Some(&authority),
        Action::Uninstall,
        None,
    )?;
    let mut journal = new_journal(
        &ctx,
        Plan {
            action: Action::Uninstall,
            recipe_sha256: fingerprint(&package)?,
            package,
            before,
            target: None,
            command: Some(CommandSpec {
                program: program.clone(),
                args: args.clone(),
            }),
            query: Some(query),
            ..crate::test_support::journal()?.plan
        },
    )?;
    journal.phase = Phase::Running;
    journal.process_group = Some(41000);
    let path = ctx
        .state
        .join("operations")
        .join(format!("{}.json", journal.id));
    let command = prepare_worker(&journal, &path, 41000, 41000)?;
    assert_eq!(command.get_program(), program.as_os_str());
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        args.iter().map(OsStr::new).collect::<Vec<_>>()
    );

    let drifted = br#"{"name":"@scope/tool","version":"2.0.0"}"#;
    fs::write(&manifest, drifted)?;
    assert_eq!(
        prepare_worker(&journal, &path, 41000, 41000)
            .err()
            .ok_or("missing stale package rejection")?
            .code,
        "plan_stale"
    );
    assert_eq!(fs::read(&manifest)?, drifted);
    assert_eq!(fs::read(&npmrc)?, b"fund=false\n");
    fs::write(&npmrc, b"fund=true\n")?;
    assert_eq!(
        prepare_worker(&journal, &path, 41000, 41000)
            .err()
            .ok_or("missing changed npm configuration rejection")?
            .code,
        "npm_config_changed"
    );
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn legacy_npm_observation_cannot_prove_uninstall_or_rebind_to_a_linked_prefix() -> TestResult {
    use crate::domain::{CommandSpec, Provider};
    use std::{os::unix::fs::symlink, path::Path};

    let temp = tempfile::tempdir_in("/private/tmp")?;
    let prefix = temp.path().join("npm");
    let package_path = prefix.join("lib/node_modules/demo");
    fs::create_dir_all(&package_path)?;
    fs::write(
        package_path.join("package.json"),
        br#"{"name":"demo","version":"1.0.0"}"#,
    )?;
    let scope = crate::domain::Scope {
        home: temp.path().to_path_buf(),
        ..crate::test_support::scope()
    };
    let observe_at = |prefix: &Path| {
        let mut package = crate::test_support::package();
        package.provider = Provider::Npm {
            package: "demo".into(),
            prefix: prefix.to_path_buf(),
            authority: None,
        };
        let query = QuerySpec::Command(CommandSpec {
            program: temp.path().join("missing-npm"),
            args: vec![
                "ls".into(),
                "--global".into(),
                "--prefix".into(),
                prefix.display().to_string(),
                "--depth=0".into(),
                "--json".into(),
                "demo".into(),
            ],
        });
        providers::observe_query(&package, &query, &scope, true)
    };
    assert_eq!(
        observe_at(&prefix)?,
        Observation::Present {
            version: "1.0.0".into(),
            evidence: None,
        }
    );
    let alias = temp.path().join("alias");
    symlink(&prefix, &alias)?;
    assert_eq!(
        observe_at(&alias)
            .err()
            .ok_or("missing legacy alias rejection")?
            .code,
        "legacy_npm_prefix_unbound"
    );
    fs::remove_dir_all(&package_path)?;
    assert_eq!(
        observe_at(&prefix)
            .err()
            .ok_or("missing unverified absence diagnostic")?
            .code,
        "legacy_npm_absence_unverified"
    );
    assert!(prefix.is_dir());
    assert!(alias.is_symlink());
    Ok(())
}
