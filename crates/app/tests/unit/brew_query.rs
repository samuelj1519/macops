use super::{query::*, *};
use crate::{
    cli::Cli,
    test_support::{
        TestResult,
        brew::{query, report},
    },
};
use clap::Parser;
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt};

fn absent_slot(
    path: &std::path::Path,
) -> std::result::Result<crate::domain::BrewConfigStamp, Box<dyn std::error::Error>> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Err("expected-absent loader slot is present; no content read".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let physical = crate::paths::read_path(path)?;
    let mut parent = physical.parent().ok_or("slot parent")?;
    while !parent.exists() {
        parent = parent.parent().ok_or("slot ancestor")?;
    }
    let anchor = parent.canonicalize()?;
    Ok(crate::domain::BrewConfigStamp::Absent {
        path: path.into(),
        anchor: anchor.clone(),
        parent: node(&fs::metadata(anchor)?),
    })
}
fn fixture()
-> std::result::Result<(tempfile::TempDir, Context, BrewQuerySpec), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let mut q = query()?;
    q.binding.prefix = home.join("prefix");
    q.key.canonical_prefix = q.binding.prefix.clone();
    q.binding.context_id = policy::context_id(&q.binding.prefix)?;
    q.binding.cellar = home.join("cellar");
    q.binding.metadata_scope.home = home.clone();
    q.binding.metadata_scope.actual_user_config_home = home.join(".homebrew");
    fs::create_dir(&q.binding.prefix)?;
    fs::create_dir(&q.binding.cellar)?;
    q.binding.prefix_id = node(&fs::metadata(&q.binding.prefix)?).identity;
    q.binding.cellar_id = node(&fs::metadata(&q.binding.cellar)?).identity;
    q.script.absolute_path = home.join("query.rb");
    q.script.canonical_path = q.script.absolute_path.clone();
    fs::write(&q.script.absolute_path, policy::QUERY_SCRIPT)?;
    fs::set_permissions(&q.script.absolute_path, fs::Permissions::from_mode(0o400))?;
    q.script.file_id = node(&fs::metadata(&q.script.absolute_path)?).identity;
    fs::create_dir_all(&q.binding.metadata_scope.actual_user_config_home)?;
    q.binding.metadata_scope.config_files = [
        std::path::PathBuf::from("/etc/homebrew/brew.env"),
        q.binding.prefix.join("etc/homebrew/brew.env"),
        q.binding
            .metadata_scope
            .actual_user_config_home
            .join("brew.env"),
    ]
    .iter()
    .map(|p| absent_slot(p))
    .collect::<std::result::Result<Vec<_>, _>>()?;
    let response = home.join("response.json");
    let mut r = report(&q, &[]);
    r.rack.cellar_before.identity = q.binding.cellar_id.clone();
    r.rack.cellar_after = r.rack.cellar_before.clone();
    fs::write(&response, serde_json::to_vec(&r)?)?;
    let program = home.join("brew");
    fs::write(
        &program,
        format!(
            "#!/bin/sh\n/usr/bin/sed \"s/QUERY-1/$6/g\" '{}'\n",
            response.display()
        ),
    )?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    q.binding.invocation = program.clone();
    q.binding.canonical_program = program.clone();
    q.binding.program_stamp = file_stamp(&program)?;
    Ok((temp, ctx, q))
}
#[test]
fn frozen_environment_query_executes_one_scoped_native_process_and_returns_typed_missing()
-> TestResult {
    let (_temp, ctx, mut q) = fixture()?;
    let store = Store::lock(&ctx)?;
    let record = super::register_context(&ctx, &store, q.binding.clone(), BTreeMap::new())?;
    let env = ConfirmedEnvironment::from_values(
        &ctx,
        &record,
        [
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (
                OsString::from("https_proxy"),
                OsString::from("synthetic-private-proxy"),
            ),
        ],
    )?;
    assert!(!format!("{env:?}").contains("synthetic-private-proxy"));
    q.env_capture_id = env.capture_id().into();
    assert_eq!(
        env.values().get("https_proxy").map(String::as_str),
        Some("synthetic-private-proxy")
    );
    let observed = observe(&store, &q, &env, 2)?;
    assert!(matches!(observed, Observation::BrewMissing { .. }));
    super::record_observation(&ctx, &store, &q, &observed)?;
    assert!(super::read_registry(&store)?.contexts[&q.binding.context_id].observation_valid);
    assert!(!ctx.state.join("native.stdout").exists());
    Ok(())
}

#[test]
fn config_change_during_native_query_cannot_become_fresh_missing() -> TestResult {
    let (_temp, ctx, mut q) = fixture()?;
    let config = q
        .binding
        .metadata_scope
        .actual_user_config_home
        .join("brew.env");
    let program = q.binding.invocation.clone();
    let report = fs::read_to_string(ctx.home.join("response.json"))?;
    let (before, after) = report.split_once("QUERY-1").ok_or("query identifier")?;
    let staged = config.with_extension("staged");
    fs::write(&staged, b"synthetic")?;
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    fs::write(
        &program,
        format!(
            "#!/bin/sh\nprintf '%s%s%s' {} \"$6\" {}\nexec /bin/mv {} {}\n",
            quote(before),
            quote(after),
            quote(staged.to_str().ok_or("staged config path")?),
            quote(config.to_str().ok_or("config path")?),
        ),
    )?;
    q.binding.program_stamp = file_stamp(&program)?;
    let store = Store::lock(&ctx)?;
    let record = super::register_context(&ctx, &store, q.binding.clone(), BTreeMap::new())?;
    let env = ConfirmedEnvironment::from_values(&ctx, &record, [])?;
    q.env_capture_id = env.capture_id().into();
    let observed = observe(&store, &q, &env, 2)?;
    assert_eq!(fs::read(config)?, b"synthetic");
    assert!(
        matches!(&observed, Observation::Unknown { reason } if reason == "brew_loader_slot_mismatch"),
        "{observed:?}"
    );
    assert!(!super::read_registry(&store)?.contexts[&q.binding.context_id].observation_valid);
    Ok(())
}

fn frozen_journal(
    ctx: &Context,
    q: &BrewQuerySpec,
    store: &Store,
) -> std::result::Result<(crate::domain::Journal, BrewRunState), Box<dyn std::error::Error>> {
    use crate::{domain::Action, test_support::brew};
    let package = brew::package(q)?;
    let mut report = brew::report(q, &[("homebrew/core", "stable", "1.0.0")]);
    report.rack.cellar_before.identity = q.binding.cellar_id.clone();
    report.rack.cellar_after = report.rack.cellar_before.clone();
    let before = policy::observation(q, brew::stamp(q)?, report)?;
    let catalog = crate::domain::Catalog {
        schema_version: 1,
        packages: vec![],
    };
    let owner =
        policy::owner_snapshot(&q.key, &catalog, &store.receipts()?, &read_registry(store)?)?;
    let plan = prepared_plan(
        ctx,
        &package,
        q.clone(),
        before,
        owner,
        Action::Upgrade,
        crate::domain::BrewConsent::Yes,
    )?;
    let journal = crate::operations::new_journal(ctx, plan)?;
    let directory = crate::paths::Directory::open(&store.path)?;
    let run = BrewRunState {
        schema_version: 2,
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        state: crate::capability_store::binding(&directory, &store.path)?,
        catalog: ctx.catalog.clone(),
        expected_loader_paths: expected_loader_paths(
            &q.binding,
            std::path::Path::new("/etc/homebrew/brew.env"),
        ),
    };
    Ok((journal, run))
}
#[test]
fn worker_context_epoch_and_frozen_invocation_cannot_be_replaced() -> TestResult {
    let (_temp, ctx, q) = fixture()?;
    let store = Store::lock(&ctx)?;
    register_context(&ctx, &store, q.binding.clone(), BTreeMap::new())?;
    let (journal, mut run) = frozen_journal(&ctx, &q, &store)?;
    let name = run_name(&journal, "input");
    Store::atomic_new(&store.root, &name, &run)?;
    assert!(run_state(&journal).is_ok());
    assert_eq!(
        execution_owner(&journal, &run)
            .err()
            .ok_or("writer permission")?
            .code,
        "brew_writer_scope_unconfirmed"
    );
    let mut authorized = read_registry(&store)?;
    authorized
        .contexts
        .get_mut(&q.binding.context_id)
        .ok_or("context")?
        .writer_scope_confirmed = true;
    Store::atomic(&store.root, REGISTRY, &authorized)?;
    execution_owner(&journal, &run)?;
    let projection =
        policy::invocation_projection(&policy::checked_plan(&journal.plan)?.ok_or("plan")?)?;
    let invocation = crate::native_runner::FrozenInvocation {
        command: projection.command,
        env: BTreeMap::from([("HOME".into(), ctx.home.to_str().ok_or("home")?.into())]),
        cwd: projection.cwd,
        timeout_secs: projection.timeout_secs,
    };
    invocation_guard(&journal, &invocation)?;
    for variant in 0..4 {
        let mut wrong = invocation.clone();
        match variant {
            0 => wrong.command.args.push("--force".into()),
            1 => wrong.cwd = ctx.state.clone(),
            2 => wrong.timeout_secs += 1,
            _ => {
                wrong.env.remove("HOME");
            }
        }
        assert!(invocation_guard(&journal, &wrong).is_err());
    }
    for variant in 0..5 {
        let mut value = serde_json::to_value(&run)?;
        match variant {
            0 => value["schema_version"] = serde_json::json!(3),
            1 => value["operation_id"] = serde_json::json!("OP-foreign"),
            2 => value["plan_sha256"] = serde_json::json!("a".repeat(64)),
            3 => value["catalog"] = serde_json::json!("relative"),
            _ => value["state"]["requested"] = serde_json::json!(ctx.home),
        }
        Store::atomic(&store.root, &name, &value)?;
        assert!(run_state(&journal).is_err());
    }
    Store::atomic(&store.root, &name, &run)?;
    let mut registry = read_registry(&store)?;
    registry
        .contexts
        .get_mut(&q.binding.context_id)
        .ok_or("context")?
        .epoch += 1;
    Store::atomic(&store.root, REGISTRY, &registry)?;
    assert!(execution_owner(&journal, &run).is_err());
    run.state.identity.inode += 1;
    assert!(original_state(&run.state).is_err());
    assert!(
        report_of(&Observation::Unknown {
            reason: "missing-proof".into()
        })
        .is_err()
    );
    assert!(
        same_native_observation(
            &journal.plan.before,
            &Observation::Unknown {
                reason: "missing-proof".into()
            }
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn recorded_missing_files_do_not_survive_rack_creation_or_root_metadata_change() -> TestResult {
    let (_temp, _ctx, q) = fixture()?;
    let mut report = report(&q, &[]);
    report.rack.cellar_before = node(&fs::symlink_metadata(&q.binding.cellar)?);
    report.rack.cellar_after = report.rack.cellar_before.clone();
    report.rack.rack_before = None;
    report.rack.rack_after = None;
    let before = policy::observation(&q, crate::test_support::brew::stamp(&q)?, report.clone())?;
    verify_report_files(&before)?;
    let mut changed = report.clone();
    changed.query_id = "QUERY-OTHER".into();
    let same = policy::observation(
        &q,
        {
            let mut s = crate::test_support::brew::stamp(&q)?;
            s.query_id = changed.query_id.clone();
            s
        },
        changed,
    )?;
    same_native_observation(&before, &same)?;
    fs::create_dir(&report.rack.path)?;
    assert!(verify_report_files(&before).is_err());
    report.rack.cellar_before = node(&fs::symlink_metadata(&q.binding.cellar)?);
    report.rack.cellar_after = report.rack.cellar_before.clone();
    report.rack.rack_before = Some(node(&fs::symlink_metadata(&report.rack.path)?));
    report.rack.rack_after = report.rack.rack_before.clone();
    let empty = policy::observation(&q, crate::test_support::brew::stamp(&q)?, report.clone())?;
    verify_report_files(&empty)?;
    let keg = report.rack.path.join("1.0.0");
    fs::create_dir(&keg)?;
    let receipt = keg.join("INSTALL_RECEIPT.json");
    fs::write(&receipt, b"first")?;
    assert!(verify_report_files(&empty).is_err());
    let mut installed =
        crate::test_support::brew::report(&q, &[("homebrew/core", "stable", "1.0.0")]);
    installed.rack.cellar_before = node(&fs::symlink_metadata(&q.binding.cellar)?);
    installed.rack.cellar_after = installed.rack.cellar_before.clone();
    installed.rack.rack_before = Some(node(&fs::symlink_metadata(&report.rack.path)?));
    installed.rack.rack_after = installed.rack.rack_before.clone();
    installed.rack.kegs[0].receipt = file_stamp(&receipt)?;
    let observed = policy::observation(&q, crate::test_support::brew::stamp(&q)?, installed)?;
    verify_report_files(&observed)?;
    fs::write(&receipt, b"external-change")?;
    assert!(verify_report_files(&observed).is_err());
    assert!(
        verify_report_files(&Observation::Unknown {
            reason: "no-report".into()
        })
        .is_err()
    );
    Ok(())
}

#[test]
fn frozen_environment_refuses_unencodable_data_and_cross_context_reuse() -> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let (_temp, ctx, mut q) = fixture()?;
    let store = Store::lock(&ctx)?;
    let record = register_context(&ctx, &store, q.binding.clone(), BTreeMap::new())?;
    let bad = OsString::from_vec(vec![0xff]);
    assert!(
        ConfirmedEnvironment::from_values(&ctx, &record, [(bad.clone(), OsString::from("value"))])
            .is_err()
    );
    assert!(
        ConfirmedEnvironment::from_values(&ctx, &record, [(OsString::from("KEY"), bad.clone())])
            .is_err()
    );
    let mut foreign = ctx.clone();
    foreign.home = ctx.home.join("other");
    assert!(ConfirmedEnvironment::from_values(&foreign, &record, []).is_err());
    let mut scope = ctx.clone();
    scope.scope.paths.insert(
        "XDG_CONFIG_HOME".into(),
        Some(std::path::PathBuf::from(bad)),
    );
    assert!(ConfirmedEnvironment::from_values(&scope, &record, []).is_err());
    let env = ConfirmedEnvironment::from_values(&ctx, &record, [])?;
    q.env_capture_id = env.capture_id().into();
    env.matches(&q)?;
    for variant in 0..4 {
        let mut wrong = q.clone();
        match variant {
            0 => wrong.env_capture_id.push('x'),
            1 => wrong.binding.context_id.push('x'),
            2 => wrong.context_epoch += 1,
            _ => wrong.binding.invocation = ctx.home.join("other-brew"),
        }
        assert!(env.matches(&wrong).is_err());
    }
    assert!(!ctx.state.join("native.stdout").exists());
    Ok(())
}
#[test]
fn frozen_query_files_refuse_nonregular_oversize_and_changed_roots_or_config() -> TestResult {
    let (_temp, ctx, mut q) = fixture()?;
    verify(&q)?;
    assert!(file_stamp(&ctx.home).is_err());
    let huge = ctx.home.join("oversize");
    let file = fs::File::create(&huge)?;
    file.set_len(256 * 1024 * 1024 + 1)?;
    assert!(file_stamp(&huge).is_err());
    for cellar in [false, true] {
        let mut wrong = q.clone();
        if cellar {
            wrong.binding.cellar_id.inode += 1;
        } else {
            wrong.binding.prefix_id.inode += 1;
        }
        assert!(verify(&wrong).is_err());
    }
    let config = q
        .binding
        .metadata_scope
        .actual_user_config_home
        .join("brew.env");
    fs::write(&config, b"one")?;
    q.binding.metadata_scope.config_files[2] = crate::domain::BrewConfigStamp::Present {
        path: config.clone(),
        stamp: file_stamp(&config)?,
    };
    verify(&q)?;
    fs::write(&config, b"two")?;
    assert!(verify(&q).is_err());
    q.binding.metadata_scope.config_files[2] = crate::domain::BrewConfigStamp::Present {
        path: config.clone(),
        stamp: file_stamp(&config)?,
    };
    verify(&q)?;
    fs::set_permissions(&q.script.absolute_path, fs::Permissions::from_mode(0o600))?;
    assert!(verify(&q).is_err());
    let mut altered = policy::QUERY_SCRIPT.to_vec();
    let last = altered.last_mut().ok_or("helper")?;
    *last = b' ';
    fs::write(&q.script.absolute_path, altered)?;
    fs::set_permissions(&q.script.absolute_path, fs::Permissions::from_mode(0o400))?;
    assert!(verify(&q).is_err());
    Ok(())
}

#[test]
fn already_current_upgrade_is_a_verified_noop_without_native_command() -> TestResult {
    let (_temp, ctx, q) = fixture()?;
    let store = Store::lock(&ctx)?;
    register_context(&ctx, &store, q.binding.clone(), BTreeMap::new())?;
    let (journal, _) = frozen_journal(&ctx, &q, &store)?;
    let mut current =
        crate::test_support::brew::report(&q, &[("homebrew/core", "stable", "2.0.0")]);
    current.formula.as_mut().ok_or("formula")?.outdated = false;
    current.rack.cellar_before.identity = q.binding.cellar_id.clone();
    current.rack.cellar_after = current.rack.cellar_before.clone();
    let observed = policy::observation(&q, crate::test_support::brew::stamp(&q)?, current)?;
    let plan = prepared_plan(
        &ctx,
        &journal.plan.package,
        q,
        observed,
        journal.plan.brew.ok_or("brew")?.owner,
        crate::domain::Action::Upgrade,
        crate::domain::BrewConsent::Yes,
    )?;
    assert_eq!(
        plan.brew.ok_or("brew")?.execution,
        crate::domain::BrewExecution::VerifiedNoop
    );
    assert!(plan.command.is_none());
    Ok(())
}

#[test]
fn caller_chosen_files_do_not_cover_the_fixed_loader_slots() -> TestResult {
    let (_temp, ctx, mut q) = fixture()?;
    let parent = node(&fs::metadata(&ctx.home)?);
    q.binding.metadata_scope.config_files = (0..3)
        .map(|i| crate::domain::BrewConfigStamp::Absent {
            path: ctx.home.join(format!("unrelated-{i}")),
            anchor: ctx.home.clone(),
            parent: parent.clone(),
        })
        .collect();
    assert!(verify(&q).is_err());
    Ok(())
}

#[test]
fn private_loader_scope_preserves_slots_aliases_and_checks_all_metadata_first() -> TestResult {
    use std::os::unix::fs::symlink;
    let (_temp, ctx, mut q) = fixture()?;
    let dir = ctx.home.join("system");
    fs::create_dir(&dir)?;
    let system = dir.join("brew.env");
    q.binding.metadata_scope.config_files[0] = absent_slot(&system)?;
    verify_scoped(&q, &system)?;
    let alias = ctx.home.join("system-alias");
    symlink(&dir, &alias)?;
    if let crate::domain::BrewConfigStamp::Absent { path, .. } =
        &mut q.binding.metadata_scope.config_files[0]
    {
        *path = alias.join("brew.env");
    }
    verify_scoped(&q, &system)?;
    assert_eq!(expected_loader_paths(&q.binding, &system)[0], system);
    let mut wrong = q.clone();
    wrong.binding.metadata_scope.config_files.swap(0, 1);
    assert!(verify_scoped(&wrong, &system).is_err());
    wrong = q.clone();
    wrong.binding.metadata_scope.config_files[1] =
        wrong.binding.metadata_scope.config_files[0].clone();
    assert!(verify_scoped(&wrong, &system).is_err());
    wrong = q.clone();
    if let crate::domain::BrewConfigStamp::Absent { anchor, parent, .. } =
        &mut wrong.binding.metadata_scope.config_files[0]
    {
        *anchor = ctx.home.clone();
        *parent = node(&fs::metadata(&ctx.home)?);
    }
    assert!(verify_scoped(&wrong, &system).is_err());
    fs::write(&system, b"SYNTHETIC_ENV=one")?;
    let mut stamp = file_stamp(&system)?;
    fs::set_permissions(&system, fs::Permissions::from_mode(0o000))?;
    stamp.node = node(&fs::metadata(&system)?);
    q.binding.metadata_scope.config_files[0] = crate::domain::BrewConfigStamp::Present {
        path: system.clone(),
        stamp,
    };
    q.binding.metadata_scope.config_files[2] = absent_slot(&ctx.home.join("unrelated-user-env"))?;
    assert_eq!(
        verify_scoped(&q, &system)
            .err()
            .ok_or("invalid last slot")?
            .code,
        "brew_loader_slot_mismatch"
    );
    fs::set_permissions(&system, fs::Permissions::from_mode(0o600))?;
    Ok(())
}
