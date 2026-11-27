use super::*;
use crate::{
    native_plugins_test_fixture::{Check, Fixture, error, write_json},
    test_support::TestResult,
};
use std::{
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
};

fn authorized_book(fixture: &Fixture) -> Value {
    let mut book = fixture.book();
    book["instances"]["codex"]["binary_sha256"] = json!(CODEX_ARM64_0169);
    book
}

#[test]
fn inventory_reinstall_requires_a_unique_recipe_for_the_selected_scope() -> TestResult {
    let fixture = Fixture::new()?;
    let ctx = fixture.context()?;
    let mut book = authorized_book(&fixture);
    fixture.write_book(&book)?;
    let recipe = |directory, id| {
        crate::agent::native_plugins::inventory_reinstall_recipe(&ctx, directory, id)
    };
    assert_eq!(
        recipe(&fixture.config, "example@market").as_deref(),
        Some("example")
    );
    assert!(recipe(&fixture.home, "example@market").is_none());
    assert!(recipe(&fixture.config, "other@market").is_none());
    book["recipes"]["second"] = book["recipes"]["example"].clone();
    fixture.write_book(&book)?;
    assert!(recipe(&fixture.config, "example@market").is_none());
    Ok(())
}

#[test]
fn installed_argv_binds_only_declared_environment_and_keeps_paths_with_spaces() -> TestResult {
    let fixture = Fixture::new()?;
    let (mut instance, recipe) = fixture.binding()?;
    let call = invocation(&instance, &recipe.native_id(), &fixture.state, 19)?;
    assert_eq!(call.command.program, fixture.binary);
    assert_eq!(
        call.command.args,
        ["plugin", "add", "example@market", "--json"]
    );
    assert_eq!(call.cwd, fixture.cwd);
    assert_eq!(call.timeout_secs, 19);
    let expected = BTreeMap::from([
        ("HOME".into(), fixture.home.to_str().ok_or("home")?.into()),
        (
            "CODEX_HOME".into(),
            fixture.config.to_str().ok_or("config")?.into(),
        ),
        (
            "TMPDIR".into(),
            fixture.state.to_str().ok_or("state")?.into(),
        ),
    ]);
    assert_eq!(call.env, expected);
    instance.cwd = fixture.cwd.join("nested");
    assert_ne!(
        invocation(&instance, &recipe.native_id(), &fixture.state, 19)?,
        call
    );
    instance.home = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
    error(
        invocation(&instance, &recipe.native_id(), &fixture.state, 19),
        "native_plugin_input_changed",
    );
    Ok(())
}

#[test]
fn untrusted_claim_cannot_use_pinned_digest_to_execute_a_different_binary() -> TestResult {
    let fixture = Fixture::new()?;
    let ctx = fixture.context()?;
    let (instance, _) = fixture.binding()?;
    assert!(!certified(&instance));
    let mut book = authorized_book(&fixture);
    fixture.write_book(&book)?;
    fixture.installed()?;
    error(
        upgrade(&ctx, "example", false, true),
        "native_plugin_binary_changed",
    );
    error(
        upgrade(&ctx, "example", true, true),
        "native_plugin_binary_changed",
    );
    let command = crate::cli::NativePlugins::Upgrade {
        recipe: "example".into(),
    };
    error(
        crate::agent::native_plugins::run(&ctx, &command, true, false),
        "native_plugin_binary_changed",
    );
    assert!(!ctx.state.exists());

    book["instances"]["codex"]["version"] = json!("0.168.0");
    fixture.write_book(&book)?;
    error(
        upgrade(&ctx, "example", false, true),
        "native_contract_required",
    );
    book = authorized_book(&fixture);
    book["recipes"]["example"]["instances"] = json!(["codex", "cursor"]);
    fixture.write_book(&book)?;
    error(
        upgrade(&ctx, "example", false, true),
        "native_contract_required",
    );
    book["recipes"]["example"]["instances"] = json!(["codex"]);
    book["recipes"]["example"]["source"] = json!({"type":"native_git","repo":"fiction/source"});
    fixture.write_book(&book)?;
    error(
        upgrade(&ctx, "example", false, true),
        "native_contract_required",
    );
    assert!(!ctx.state.exists());
    error(
        upgrade(&ctx, "unregistered", false, true),
        "native_plugin_input_changed",
    );
    Ok(())
}

#[test]
fn unverified_native_origin_enabled_status_or_version_never_reaches_home_lock() -> TestResult {
    let f = Fixture::new()?;
    let ctx = f.context()?;
    let check = || upgrade(&ctx, "example", false, true);
    f.write_book(&authorized_book(&f))?;
    error(check(), "native_plugin_registration_unknown");
    f.installed()?;
    let enabled = "[plugins.'example@market']\nenabled=true\n";
    let disabled = "[plugins.'example@market']\nenabled=false\n";
    fs::write(f.codex_file(), Fixture::market_entry(&f.source, disabled))?;
    error(check(), "codex_readd_would_enable");
    fs::write(
        f.codex_file(),
        Fixture::market_entry(&f.root().join("alternate"), enabled),
    )?;
    error(check(), "marketplace_source_change_required");
    fs::write(f.codex_file(), Fixture::market_entry(&f.source, ""))?;
    error(check(), "native_plugin_registration_unknown");
    let mut wrong = authorized_book(&f);
    wrong["recipes"]["example"]["source"]["sha256"] = json!("0".repeat(64));
    fs::write(f.codex_file(), Fixture::market_entry(&f.source, enabled))?;
    f.write_book(&wrong)?;
    error(check(), "native_plugin_source_differs");
    f.write_book(&authorized_book(&f))?;
    write_json(
        &f.source_member().join(".codex-plugin/plugin.json"),
        &json!({"name":"example","version":"1.2.4"}),
    )?;
    error(check(), "native_plugin_same_version_contract_required");
    f.installed()?;
    let manifest = f.source.join(".agents/plugins/marketplace.json");
    let fallback = f.source.join(".claude-plugin/marketplace.json");
    fs::create_dir_all(fallback.parent().ok_or("manifest parent")?)?;
    fs::rename(manifest, fallback)?;
    error(check(), "native_plugin_same_version_contract_required");
    assert!(!f.state.exists());
    Ok(())
}

#[test]
fn working_directory_and_storage_fences_refuse_project_auth_and_aliases() -> TestResult {
    let f = Fixture::new()?;
    let ctx = f.context()?;
    let check = || upgrade(&ctx, "example", false, true);
    let mut book = authorized_book(&f);
    f.installed()?;
    f.write_book(&book)?;
    fs::write(
        f.cwd.join(".git"),
        "synthetic pointer that must never be loaded",
    )?;
    error(check(), "native_plugin_project_scope_unproven");
    fs::remove_file(f.cwd.join(".git"))?;
    let project = f.cwd.join(".codex");
    fs::create_dir(&project)?;
    error(check(), "native_plugin_project_scope_unproven");
    fs::remove_dir(project)?;
    symlink(f.root().join("not-accessible"), f.config.join("auth.json"))?;
    error(check(), "native_plugin_auth_scope_unproven");
    fs::remove_file(f.config.join("auth.json"))?;
    for other in [&f.source, &f.config, &f.state] {
        book["instances"]["codex"]["cwd"] = json!(other);
        f.write_book(&book)?;
        error(check(), "native_plugin_overlapping_paths");
    }
    book["instances"]["codex"]["cwd"] = json!(f.cwd);
    book["instances"]["claude:mi"] = f.instance_json();
    book["instances"]["claude:mi"]["version"] = json!("2.1.323");
    f.write_book(&book)?;
    error(check(), "shared_native_storage_unsupported");
    let alias = f.root().join("alias");
    symlink(&f.config, &alias)?;
    book["instances"]["claude:mi"]["config_root"] = json!(alias);
    f.write_book(&book)?;
    error(check(), "shared_native_storage_unsupported");
    book["instances"]["claude:mi"]["config_root"] = json!(f.root().join("distinct-profile"));
    f.write_book(&book)?;
    error(check(), "native_plugin_binary_changed");
    assert!(!ctx.state.exists());
    Ok(())
}

fn make_snapshot(f: &Fixture) -> Check<Snapshot> {
    let root = f.config.join("plugins/cache");
    let mut passive = Passive::new()?;
    let (instance, recipe) = f.binding()?;
    let evidence = files::codex(&mut passive, &instance, &recipe)?;
    let Payload::Copied {
        source_sha256,
        installed_sha256,
    } = evidence.payload
    else {
        return Err("fixture selected payload".into());
    };
    Ok(Snapshot {
        scope: fingerprint(&bound_directory(&mut passive, &f.cwd)?)?,
        source: source_sha256,
        selected: installed_sha256,
        unrelated: passive.projected_tree(&root, Some(Path::new("market/example")))?,
        config: config_fingerprint(&mut passive, &f.codex_file())?,
        version: evidence.version.ok_or("fixture selected version")?,
    })
}

#[test]
fn genuine_disk_projection_accepts_exact_convergence_and_detects_unowned_drift() -> TestResult {
    let f = Fixture::new()?;
    f.installed()?;
    let source = f.source_member().join("README");
    let installed = f.cache().join("1.2.3/README");
    let unrelated = f.config.join("plugins/cache/market/control/README");
    fs::create_dir_all(unrelated.parent().ok_or("unrelated parent")?)?;
    fs::write(&unrelated, "unrelated original")?;
    fs::write(&source, "approved selected")?;
    fs::write(&installed, "old selected")?;
    let before = make_snapshot(&f)?;
    assert!(!before.accepts(&before));
    fs::copy(&source, &installed)?;
    let settled = make_snapshot(&f)?;
    assert!(before.accepts(&settled));
    fs::write(&unrelated, "external unrelated edit")?;
    assert!(!before.accepts(&make_snapshot(&f)?));
    fs::write(&unrelated, "unrelated original")?;
    fs::set_permissions(f.codex_file(), fs::Permissions::from_mode(0o600))?;
    assert!(!before.accepts(&make_snapshot(&f)?));
    let snapshot = Snapshot {
        version: "2.0".into(),
        ..settled
    };
    assert!(!before.accepts(&snapshot));
    error(
        config_fingerprint(&mut Passive::new()?, &f.root().join("nonexistent.toml")),
        "native_plugin_input_changed",
    );
    Ok(())
}

struct Operation {
    journal: Journal,
    snapshot: Frozen,
    call: FrozenInvocation,
}

impl Operation {
    fn new(f: &Fixture, ctx: &Context, store: &Store) -> Check<Self> {
        f.installed()?;
        f.write_book(&authorized_book(f))?;
        let snapshot = Frozen {
            key: "example".into(),
            registry: f.declaration(),
            before: make_snapshot(f)?,
            state: crate::capability::capability_store::binding(
                &Directory::open(&f.state)?,
                &f.state,
            )?,
        };
        let (instance, recipe) = f.binding()?;
        let call = invocation(&instance, &recipe.native_id(), &f.state, ctx.timeout_secs)?;
        let mut plan = crate::test_support::journal()?.plan;
        plan.package.provider = Provider::Macops;
        plan.recipe_sha256 = fingerprint(&plan.package)?;
        plan.action = Action::Upgrade;
        plan.command = Some(call.command.clone());
        plan.query = None;
        plan.timeout_secs = ctx.timeout_secs;
        plan.delegated = true;
        plan.warnings = vec![format!("{PROTOCOL}{}", fingerprint(&snapshot)?)];
        let mut journal = crate::operations::new_journal(ctx, plan)?;
        Store::atomic_new(&store.root, &plan_name(&journal), &snapshot)?;
        journal.phase = Phase::Unknown;
        journal.process_group = Some(1_234_567);
        store.save(&journal)?;
        Ok(Self {
            journal,
            snapshot,
            call,
        })
    }

    fn write_after(
        &self,
        store: &Store,
        op: &str,
        worker: Option<u32>,
        observed: Snapshot,
    ) -> TestResult {
        Store::atomic(
            &store.root,
            &after_name(&self.journal),
            &After {
                operation: op.into(),
                plan: self.journal.plan_sha256.clone(),
                worker,
                observed,
            },
        )?;
        Ok(())
    }
}

#[test]
fn trusted_preview_returns_the_exact_command_without_entering_a_write_stage() -> TestResult {
    let f = Fixture::new()?;
    f.installed()?;
    f.write_book(&f.book())?;
    let ctx = f.context()?;
    let (instance, recipe) = f.binding()?;
    let native_id = recipe.native_id();
    let call = invocation(&instance, &native_id, &f.state, ctx.timeout_secs)?;
    let mut capture = Some(Captured {
        before: make_snapshot(&f)?,
        invocation: call.clone(),
        native_id,
    });
    let mut proceeded = false;
    let result = upgrade_with_capture(
        &ctx,
        "example",
        true,
        false,
        |path| {
            assert_eq!(path, f.declaration());
            assert!(!f.state.exists());
            capture
                .take()
                .ok_or_else(|| refusal("unexpected_second_capture"))
        },
        |_, _, _| {
            proceeded = true;
            Err(refusal("unexpected_write_stage"))
        },
    )?;
    assert!(capture.is_none());
    assert!(!proceeded);
    assert_eq!(result["mode"], "dry-run");
    assert_eq!(result["recipe"], "example");
    assert_eq!(result["native_id"], "example@market");
    assert_eq!(result["command"], serde_json::to_value(&call.command)?);
    assert_eq!(result["persistentWrites"], false);
    assert!(!f.state.exists());
    Ok(())
}

#[test]
fn confirmed_upgrade_refuses_a_changed_scope_under_lock_before_preparation() -> TestResult {
    let f = Fixture::new()?;
    f.installed()?;
    f.write_book(&f.book())?;
    let ctx = f.context()?;
    let (instance, recipe) = f.binding()?;
    let native_id = recipe.native_id();
    let call = invocation(&instance, &native_id, &f.state, ctx.timeout_secs)?;
    let before = make_snapshot(&f)?;
    let mut changed_scope = make_snapshot(&f)?;
    changed_scope.scope = fingerprint(&bound_directory(&mut Passive::new()?, &f.config)?)?;
    assert_ne!(before.scope, changed_scope.scope);
    let mut observations = [before, changed_scope].into_iter();
    let files = [
        f.declaration(),
        f.codex_file(),
        f.source_member().join(".codex-plugin/plugin.json"),
        f.cache().join("1.2.3/.codex-plugin/plugin.json"),
    ];
    let before_files = files
        .iter()
        .map(|path| Ok((fs::read(path)?, fs::metadata(path)?.permissions().mode())))
        .collect::<std::io::Result<Vec<_>>>()?;
    let mut captures = 0;
    let mut proceeded = false;

    error(
        upgrade_with_capture(
            &ctx,
            "example",
            false,
            true,
            |path| {
                assert_eq!(path, f.declaration());
                captures += 1;
                if captures == 1 {
                    assert!(!f.state.exists());
                } else {
                    assert_eq!(
                        Store::lock(&ctx)
                            .err()
                            .map(|failure| failure.code)
                            .as_deref(),
                        Some("operation_busy")
                    );
                }
                let before = observations
                    .next()
                    .ok_or_else(|| refusal("unexpected_capture"))?;
                Ok(Captured {
                    before,
                    invocation: call.clone(),
                    native_id: native_id.clone(),
                })
            },
            |_, _, _| {
                proceeded = true;
                Err(refusal("unexpected_preparation"))
            },
        ),
        "native_plugin_input_changed",
    );
    assert_eq!(captures, 2);
    assert!(!proceeded);
    let mut leaves = fs::read_dir(&f.state)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    leaves.sort_unstable();
    assert_eq!(
        leaves,
        ["operations", "write.lock"].map(std::ffi::OsString::from)
    );
    assert_eq!(fs::read_dir(f.state.join("operations"))?.count(), 0);
    for (path, (bytes, mode)) in files.iter().zip(before_files) {
        assert_eq!(fs::read(path)?, bytes);
        assert_eq!(fs::metadata(path)?.permissions().mode(), mode);
    }
    Ok(())
}

#[test]
fn preparation_persists_only_the_frozen_plan_journal_and_launch_intent() -> TestResult {
    let f = Fixture::new()?;
    f.installed()?;
    f.write_book(&f.book())?;
    let selected = f.cache().join("1.2.3/README");
    fs::write(&selected, b"previous installed contents")?;
    fs::write(f.source_member().join("README"), b"declared replacement")?;
    let ctx = f.context()?;
    let store = Store::lock(&ctx)?;
    let (instance, recipe) = f.binding()?;
    let native_id = recipe.native_id();
    let call = invocation(&instance, &native_id, &f.state, ctx.timeout_secs)?;
    let captured = Captured {
        before: make_snapshot(&f)?,
        invocation: call.clone(),
        native_id,
    };
    let config = fs::read(f.codex_file())?;
    let registry = fs::read(f.declaration())?;

    let (journal, binding) = prepare_upgrade(&ctx, &store, "example", f.declaration(), captured)?;

    assert_eq!(journal.phase, Phase::Prepared);
    assert!(journal.process_group.is_none());
    crate::operations::store::validate_journal(&journal)?;
    assert!(is_plugin(&journal));
    assert_eq!(
        journal.plan.package.id,
        "native-plugin:codex:example@market"
    );
    assert_eq!(journal.plan.action, Action::Upgrade);
    assert_eq!(journal.plan.target.as_deref(), Some("1.2.3"));
    assert!(matches!(
        &journal.plan.before,
        Observation::Present { version, evidence: None } if version == "1.2.3"
    ));
    assert_eq!(journal.plan.command.as_ref(), Some(&call.command));
    assert_eq!(binding.invocation, call);
    assert_eq!(native_runner::binding(&store, &journal)?, binding);
    error(store.check_pending(), "recovery_required");

    let (state, saved) = frozen(&journal)?;
    assert_eq!(saved.key, "example");
    assert_eq!(saved.registry, f.declaration());
    assert_eq!(saved.state.identity, *state.identity());
    assert_eq!(saved.before, make_snapshot(&f)?);
    assert_ne!(saved.before.source, saved.before.selected);

    let active = f.state.join("active.json");
    let history = f.state.join(format!("operations/{}.json", journal.id));
    assert_eq!(fs::read(&active)?, fs::read(&history)?);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&active)?)?,
        serde_json::to_value(&journal)?
    );
    for path in [
        active,
        history,
        f.state.join(plan_name(&journal)),
        f.state.join(format!("native-{}-binding.json", journal.id)),
    ] {
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
    }
    let binding_json = serde_json::to_value(&binding)?;
    for key in ["gate_name", "completion_name"] {
        let leaf = binding_json[key].as_str().ok_or("run evidence name")?;
        assert!(!f.state.join(leaf).exists());
    }
    assert!(
        !f.state
            .join(format!("native-{}-worker.json", journal.id))
            .exists()
    );
    assert!(!f.state.join(after_name(&journal)).exists());
    assert!(!f.state.join("receipts.json").exists());
    assert_eq!(fs::read(&selected)?, b"previous installed contents");
    assert_eq!(fs::read(f.codex_file())?, config);
    assert_eq!(fs::read(f.declaration())?, registry);
    Ok(())
}

#[test]
fn private_plan_refuses_foreign_adapter_scope_and_malformed_bytes() -> TestResult {
    let f = Fixture::new()?;
    let ctx = f.context()?;
    let store = Store::lock(&ctx)?;
    let mut operation = Operation::new(&f, &ctx, &store)?;
    let journal = &mut operation.journal;
    assert!(is_plugin(journal));
    journal.plan.delegated = false;
    assert!(!is_plugin(journal));
    error(frozen(journal), "native_plugin_input_changed");
    journal.plan.delegated = true;
    journal.plan.action = Action::Uninstall;
    error(frozen(journal), "native_plugin_input_changed");
    journal.plan.action = Action::Upgrade;
    assert_eq!(frozen(journal)?.1.registry, f.declaration());
    let path = f.state.join(plan_name(journal));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    error(frozen(journal), "native_plugin_input_changed");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    fs::write(&path, br#"{"key":"one","key":"two"}"#)?;
    assert!(frozen(journal).is_err());
    Store::atomic(&store.root, &plan_name(journal), &operation.snapshot)?;
    let prior_warning = journal.plan.warnings[0].clone();
    journal.plan.warnings[0] = "native-plugin-v1:foreign-fingerprint".into();
    error(frozen(journal), "native_plugin_input_changed");
    journal.plan.warnings[0] = prior_warning;
    let displaced = f.root().join("displaced-state");
    let plan_leaf = plan_name(journal);
    fs::rename(&f.state, &displaced)?;
    fs::create_dir(&f.state)?;
    fs::set_permissions(&f.state, fs::Permissions::from_mode(0o700))?;
    fs::copy(displaced.join(&plan_leaf), f.state.join(plan_leaf))?;
    error(frozen(journal), "native_plugin_input_changed");
    Ok(())
}

#[test]
fn recovery_requires_original_completion_worker_match_and_present_projection() -> TestResult {
    let f = Fixture::new()?;
    let mut ctx = f.context()?;
    let store = Store::lock(&ctx)?;
    let mut operation = Operation::new(&f, &ctx, &store)?;
    let exit = NativeOutcome::Exited {
        code: Some(0),
        signal: None,
    };
    error(
        worker_after(
            &operation.journal,
            &operation.call,
            &NativeOutcome::Exited {
                code: Some(4),
                signal: None,
            },
        ),
        "native_plugin_completion_unknown",
    );
    error(
        worker_guard(&operation.journal, &operation.call),
        "native_plugin_binary_changed",
    );
    error(
        worker_after(&operation.journal, &operation.call, &exit),
        "native_plugin_binary_changed",
    );

    let observed = make_snapshot(&f)?;
    operation.write_after(
        &store,
        "OP-foreign",
        operation.journal.process_group,
        observed,
    )?;
    let result = finish(&ctx, &store, &mut operation.journal, &operation.call, &exit)?;
    assert_eq!(result["journal"]["phase"], "unknown");
    assert_eq!(result["reason"], "native_plugin_after_unverified");

    operation.write_after(&store, &operation.journal.id, Some(77), make_snapshot(&f)?)?;
    let result = finish(&ctx, &store, &mut operation.journal, &operation.call, &exit)?;
    assert_eq!(result["reason"], "native_plugin_after_unverified");
    operation.write_after(
        &store,
        &operation.journal.id,
        operation.journal.process_group,
        make_snapshot(&f)?,
    )?;
    let result = finish(&ctx, &store, &mut operation.journal, &operation.call, &exit)?;
    assert_eq!(result["reason"], "native_plugin_binary_changed");
    assert_eq!(result["journal"]["phase"], "unknown");

    let original_home = ctx.home.clone();
    ctx.home = f.root().join("foreign-home");
    error(
        recover(&ctx, &store, &mut operation.journal, false, true),
        "native_plugin_input_changed",
    );
    ctx.home = original_home;
    assert!(recover(&ctx, &store, &mut operation.journal, false, true).is_err());
    operation.journal.phase = Phase::Failed;
    assert_eq!(
        recover(&ctx, &store, &mut operation.journal, false, true)?["phase"],
        "failed"
    );
    Ok(())
}

#[test]
fn prepared_binding_without_child_evidence_cannot_be_resolved_as_failed_or_reexecuted() -> TestResult
{
    let f = Fixture::new()?;
    let ctx = f.context()?;
    let store = Store::lock(&ctx)?;
    let mut operation = Operation::new(&f, &ctx, &store)?;
    operation.journal.phase = Phase::Prepared;
    operation.journal.process_group = None;
    native_runner::prepare(&store, &operation.journal, &operation.call)?;
    let before = fs::read(f.state.join("active.json"))?;
    error(
        recover(&ctx, &store, &mut operation.journal, true, true),
        "native_plugin_completion_unknown",
    );
    assert_eq!(fs::read(f.state.join("active.json"))?, before);
    let recovered = recover(&ctx, &store, &mut operation.journal, false, true)?;
    assert_eq!(recovered["journal"]["phase"], "unknown");
    assert_eq!(recovered["native_outcome"]["status"], "unknown");
    assert_eq!(recovered["reason"], "native_plugin_completion_unknown");
    assert!(
        !fs::read_dir(&f.state)?
            .filter_map(std::result::Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with("-exit.json"))
    );
    Ok(())
}

#[test]
fn observed_cache_alone_cannot_replace_exit_and_after_evidence_at_finalization() -> TestResult {
    let f = Fixture::new()?;
    let ctx = f.context()?;
    let store = Store::lock(&ctx)?;
    let mut operation = Operation::new(&f, &ctx, &store)?;
    let path = f.state.join(after_name(&operation.journal));
    let config = fs::read(f.codex_file())?;
    let matched = make_snapshot(&f)?;
    assert!(operation.snapshot.before.accepts(&matched));
    let value = finish(
        &ctx,
        &store,
        &mut operation.journal,
        &operation.call,
        &NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        },
    )?;
    assert_eq!(value["journal"]["phase"], "unknown");
    assert_eq!(value["reason"], "path_missing");
    assert!(value["result"].is_null());
    assert!(!path.exists());

    operation.write_after(
        &store,
        &operation.journal.id,
        operation.journal.process_group,
        matched,
    )?;
    let after = fs::read(&path)?;
    for outcome in [
        NativeOutcome::Exited {
            code: Some(2),
            signal: None,
        },
        NativeOutcome::Exited {
            code: None,
            signal: Some(15),
        },
        NativeOutcome::Unknown {
            reason: "native_completion_missing".into(),
        },
    ] {
        let value = finish(
            &ctx,
            &store,
            &mut operation.journal,
            &operation.call,
            &outcome,
        )?;
        assert_eq!(value["journal"]["phase"], "unknown");
        assert_eq!(value["reason"], "native_plugin_completion_unknown");
        assert_eq!(value["native_outcome"], serde_json::to_value(&outcome)?);
        assert_eq!(value["loaded"], "unknown");
        assert!(value["result"].is_null());
        assert_eq!(fs::read(&path)?, after);
    }
    assert_eq!(fs::read(f.codex_file())?, config);
    assert_eq!(
        fs::read(f.state.join("active.json"))?,
        fs::read(
            f.state
                .join(format!("operations/{}.json", operation.journal.id))
        )?
    );
    assert!(!f.state.join("receipts.json").exists());
    Ok(())
}

#[test]
fn settled_plugin_recovery_reads_original_result_after_recipe_retirement() -> TestResult {
    for phase in [Phase::Succeeded, Phase::Failed] {
        let f = Fixture::new()?;
        let ctx = f.context()?;
        let store = Store::lock(&ctx)?;
        let mut operation = Operation::new(&f, &ctx, &store)?;
        let journal = &mut operation.journal;
        journal.phase = phase;
        journal.message = "fixture terminal result".into();
        store.save(journal)?;
        fs::remove_file(f.declaration())?;
        let active = f.state.join("active.json");
        let history = f.state.join(format!("operations/{}.json", journal.id));
        let before = fs::read(&active)?;
        let active_inode = fs::metadata(&active)?.ino();
        let history_inode = fs::metadata(&history)?.ino();
        for resolve_failed in [false, true] {
            let value = recover(&ctx, &store, journal, resolve_failed, false)?;
            assert_eq!(value["phase"], serde_json::to_value(phase)?);
            assert_eq!(value["message"], "fixture terminal result");
            assert_eq!(fs::read(&active)?, before);
            assert_eq!(fs::read(&history)?, before);
            assert_eq!(fs::metadata(&active)?.ino(), active_inode);
            assert_eq!(fs::metadata(&history)?.ino(), history_inode);
        }
        assert!(!f.declaration().exists());
        assert!(!f.state.join(after_name(journal)).exists());
    }
    Ok(())
}
