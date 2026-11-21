use super::*;
use crate::{
    claude_install_source::{MARKET, PLUGIN},
    policy::source_git::ObjectId,
    test_support::TestResult,
};
use std::{fs, io::Write as _};

const MARKET_DOC: &[u8] =
    br#"{"name":"fixture-market","plugins":[{"name":"fixture-plugin","source":"./"}]}"#;
const PLUGIN_DOC: &[u8] = br#"{"name":"fixture-plugin","version":"1.0.0"}"#;
const SKILL: &[u8] = b"---\nname: fixture\n---\nTest\n";
const FILES: &[crate::codex_install_source::VerifiedFile<'static>] = &[
    (MARKET, false, MARKET_DOC),
    (PLUGIN, false, PLUGIN_DOC),
    ("skills/fixture/SKILL.md", false, SKILL),
];

fn fixture()
-> std::result::Result<(tempfile::TempDir, Context, Offer, PathBuf), Box<dyn std::error::Error>> {
    use clap::Parser as _;
    let run = tempfile::tempdir_in("/private/tmp")?;
    let home = run.path().join("home");
    fs::create_dir_all(home.join(".claude-glm"))?;
    fs::write(
        home.join(".claude-glm/settings.json"),
        br#"{"model":"fixture-sibling"}"#,
    )?;
    let cli = crate::cli::Cli::try_parse_from(["macops", "--home", text(&home)?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let offer = Offer::identify(
        "example/fixture-plugin",
        &ObjectId::parse(&"a".repeat(40))?,
        MARKET_DOC,
        PLUGIN_DOC,
    )?;
    let binary = home.join("claude");
    fs::write(&binary, b"fixture Claude binary")?;
    fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    Ok((run, ctx, offer, binary))
}

fn json_file(path: &Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}

fn mock_native(input: &Frozen) -> Result<()> {
    let p = &input.prepared;
    let profile = p.profile();
    let plugins = profile.join("plugins");
    let id = p.offer.native_id();
    let market = &p.offer.marketplace;
    let source = json!({"source":"directory","path":p.checkout()});
    let mut settings = json!({"extraKnownMarketplaces":{market:{"source":source}}});
    match input.step {
        Step::Market => {
            fs::create_dir_all(&plugins)?;
            json_file(
                &plugins.join("known_marketplaces.json"),
                &json!({market:{"source":source,"installLocation":p.checkout(),"lastUpdated":"fixture"}}),
            )?;
        }
        Step::Plugin { .. } => {
            let payload = p.payload();
            for (relative, _, bytes) in FILES {
                let path = payload.join(relative);
                fs::create_dir_all(path.parent().ok_or_else(invalid)?)?;
                fs::write(path, bytes)?;
            }
            let enabled = !p.host_home.join("default-disabled").exists();
            settings["enabledPlugins"] = json!({id.as_str():enabled});
            json_file(
                &plugins.join("installed_plugins.json"),
                &json!({"version":2,"plugins":{id:[{
                    "scope":"user","installPath":payload,"version":p.offer.version,
                    "installedAt":"fixture","lastUpdated":"fixture"
                }]}}),
            )?;
        }
        Step::Activation { .. } => {
            settings["enabledPlugins"] = json!({id:p.enabled});
        }
    }
    json_file(&profile.join("settings.json"), &settings)
}

const FIXTURE: NativeIo = NativeIo {
    binary: |reader, path| {
        if reader.binary(path)? != crate::codex_install_source::sha(b"fixture Claude binary") {
            return Err(invalid());
        }
        Ok(())
    },
    checkout: |offer, root| source::materialize(offer, root, FILES),
    launch: |store, journal, argv| {
        let state = physical(&store.path)?;
        let input = frozen(journal, &state)?;
        before(&input, journal, &state, &FIXTURE)?;
        journal.phase = Phase::Running;
        journal.process_group = Some(4242);
        store.save(journal)?;
        let host = &input.prepared.host_home;
        let mut calls = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(host.join("native-calls"))?;
        let step = match input.step {
            Step::Market => "market",
            Step::Plugin { .. } => "plugin",
            Step::Activation { .. } => "activation",
        };
        writeln!(calls, "{step}")?;
        let status = if host.join("unknown-exit").exists()
            || host.join(format!("unknown-{step}")).exists()
        {
            "unknown"
        } else if host.join("failed-exit").exists() {
            "failed"
        } else {
            "clean"
        };
        Store::atomic_new(&store.root, &sidecar(journal, "fixture-exit"), &status)?;
        let outcome = (FIXTURE.outcome)(journal)?;
        if clean(&outcome) {
            mock_native(&input)?;
            if !host.join("missing-after").exists() {
                record_after(journal, argv, &outcome, &FIXTURE)?;
            }
        }
        Ok(outcome)
    },
    outcome: |journal| {
        let state = physical(&journal.state)?;
        let status: String =
            Store::read(state.file(), &sidecar(journal, "fixture-exit"))?.ok_or_else(invalid)?;
        Ok(match status.as_str() {
            "clean" => NativeOutcome::Exited {
                code: Some(0),
                signal: None,
            },
            "failed" => NativeOutcome::Exited {
                code: Some(7),
                signal: None,
            },
            "running" => NativeOutcome::Running,
            _ => NativeOutcome::Unknown {
                reason: "fixture_unknown".into(),
            },
        })
    },
};

#[test]
fn fixture_first_install_proves_default_and_explicit_initial_mode_before_receipt() -> TestResult {
    for (initial, requested, calls) in [
        (true, true, "market\nplugin\n"),
        (true, false, "market\nplugin\nactivation\n"),
        (false, true, "market\nplugin\nactivation\n"),
        (false, false, "market\nplugin\n"),
    ] {
        let (_temp, ctx, offer, binary) = fixture()?;
        let sibling = ctx.home.join(".claude-glm/settings.json");
        let unchanged = fs::read(&sibling)?;
        if !initial {
            fs::write(ctx.home.join("default-disabled"), b"")?;
        }
        let value = install_using(&ctx, offer, binary, requested, &FIXTURE)?;
        assert_eq!(value["journal"]["phase"], "succeeded", "{value}");
        assert_eq!(
            value["journal"]["plan"]["before"]["pack"]["configured_enabled"],
            requested
        );
        assert_eq!(fs::read_to_string(ctx.home.join("native-calls"))?, calls);
        assert_eq!(fs::read(sibling)?, unchanged);
        let profiles = managed_profiles(&ctx)?;
        assert_eq!(profiles.len(), 1);
        assert!(profiles[0].0.starts_with(".claude-mo-fixture-plugin-"));
        assert_eq!(
            profiles[0].1,
            Path::new(value["claudeConfigDir"].as_str().ok_or("profile")?)
        );
        let receipt_id = value["journal"]["id"].as_str().ok_or("receipt identity")?;
        let store = Store::lock(&ctx)?;
        let directory = physical(&store.path)?;
        let publication = original(&directory, receipt_id)?.ok_or("publication")?;
        assert!(is_publication(&publication));
        assert!(require_ordinary_adoption(&publication.plan.package).is_err());
        let native_id = receipt_id
            .strip_suffix("-receipt")
            .ok_or("native identity")?;
        let native = original(&directory, native_id)?.ok_or("native stage")?;
        let input = frozen(&native, &directory)?;
        assert!(is_install(&native));
        assert_eq!(
            state::observe(&input.prepared, &input.step)?.enabled,
            Some(requested)
        );
        let args = invocation(&input, 17)?;
        assert_eq!(
            args.env.get("CLAUDE_CONFIG_DIR"),
            Some(&text(&profiles[0].1)?.into())
        );
        assert_eq!(args.command.program, Path::new("/usr/bin/sandbox-exec"));
        assert!(
            args.command
                .args
                .iter()
                .any(|arg| arg.contains("(deny network*)"))
        );
        assert!(
            args.command
                .args
                .iter()
                .any(|arg| arg.contains("com.apple.securityd"))
        );
    }
    Ok(())
}

#[test]
fn original_failure_can_be_closed_but_ambiguous_evidence_stays_pending() -> TestResult {
    for (marker, expected) in [
        ("unknown-exit", Phase::Unknown),
        ("failed-exit", Phase::Failed),
        ("missing-after", Phase::Unknown),
    ] {
        let (_temp, ctx, offer, binary) = fixture()?;
        fs::write(ctx.home.join(marker), b"")?;
        let attempt = install_using(&ctx, offer, binary, true, &FIXTURE)?;
        assert_eq!(attempt["journal"]["phase"], "unknown");
        let store = Store::lock(&ctx)?;
        let mut active: Journal = Store::read(&store.root, "active.json")?.ok_or("active stage")?;
        let original = active.id.clone();
        let reported = recover_using(&ctx, &store, &mut active, true, true, &FIXTURE);
        assert_eq!(reported.is_ok(), expected == Phase::Failed);
        assert_eq!(active.phase, expected);
        if expected == Phase::Unknown {
            recover_using(&ctx, &store, &mut active, false, false, &FIXTURE)?;
            assert_eq!(active.phase, Phase::Unknown);
        }
        assert_eq!(active.id, original);
        assert_eq!(
            fs::read_to_string(ctx.home.join("native-calls"))?,
            "market\n"
        );
        assert!(store.receipts()?.installations.is_empty());
    }
    Ok(())
}

#[test]
fn publication_recovery_keeps_exact_existing_receipt_and_refuses_foreign_pending() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let installed = install_using(&ctx, offer, binary, false, &FIXTURE)?;
    let adopted_id = installed["journal"]["id"]
        .as_str()
        .ok_or("receipt identity")?;
    let source_id = adopted_id
        .strip_suffix("-receipt")
        .ok_or("native identity")?;
    let store = Store::lock(&ctx)?;
    let directory = physical(&store.path)?;
    let native = original(&directory, source_id)?.ok_or("native stage")?;
    let reservation: Journal =
        Store::read(&store.root, &sidecar(&native, "adoption"))?.ok_or("original reservation")?;
    let receipt = ctx.state.join("receipts.json");
    let preserved = fs::read(&receipt)?;
    fs::remove_file(
        ctx.state
            .join("operations")
            .join(format!("{adopted_id}.json")),
    )?;
    Store::atomic(&store.root, "active.json", &reservation)?;
    assert_eq!(managed_profiles(&ctx)?, []);
    let mut unrelated = reservation.clone();
    unrelated.id = "OP-unrelated".into();
    Store::atomic(&store.root, "active.json", &unrelated)?;
    assert!(recover_publication_using(&ctx, &store, &reservation, false, &FIXTURE).is_err());
    assert_eq!(fs::read(&receipt)?, preserved);
    Store::atomic(&store.root, "active.json", &reservation)?;
    let restored = recover_publication_using(&ctx, &store, &reservation, false, &FIXTURE)?;
    assert_eq!(restored["journal"]["id"], adopted_id);
    assert_eq!(restored["journal"]["phase"], "succeeded");
    assert_eq!(fs::read(receipt)?, preserved);
    assert_eq!(managed_profiles(&ctx)?.len(), 1);
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\nplugin\nactivation\n"
    );
    Ok(())
}

#[test]
fn an_error_before_dispatch_closes_only_the_proven_unstarted_stage() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let no_start = NativeIo {
        launch: |store, journal, invocation| {
            Store::atomic_new(
                &store.root,
                &format!("native-{}-binding.json", journal.id),
                &json!({}),
            )?;
            (NATIVE.launch)(store, journal, invocation)
        },
        ..FIXTURE
    };
    let interrupted = install_using(&ctx, offer, binary, false, &no_start)?;
    assert_eq!(interrupted["journal"]["phase"], "prepared");
    let store = Store::lock(&ctx)?;
    let mut active: Journal =
        Store::read(&store.root, "active.json")?.ok_or("original staged journal")?;
    assert_eq!(active.process_group, None);
    let settled = recover(&ctx, &store, &mut active, false, false)?;
    assert_eq!(settled["journal"]["phase"], "failed");
    assert!(!ctx.home.join("native-calls").exists());
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn continuation_requires_fresh_confirmation_and_starts_only_the_unattempted_native_stage()
-> TestResult {
    for from_plugin in [false, true] {
        let (_temp, ctx, offer, binary) = fixture()?;
        let store = Store::lock(&ctx)?;
        let prepared = prepare(&ctx, &store, offer, binary, false, &FIXTURE)?;
        let market = stage(
            &ctx,
            &store,
            &Frozen {
                prepared: prepared.clone(),
                step: Step::Market,
            },
            &FIXTURE,
        )?;
        assert_eq!(market.phase, Phase::Succeeded);
        let mut interrupted = if from_plugin {
            stage(
                &ctx,
                &store,
                &Frozen {
                    prepared,
                    step: Step::Plugin { market: market.id },
                },
                &FIXTURE,
            )?
        } else {
            market
        };
        assert_eq!(interrupted.phase, Phase::Succeeded);
        let before = fs::read(ctx.home.join("native-calls"))?;
        let verification = recover_using(&ctx, &store, &mut interrupted, false, false, &FIXTURE)?;
        assert_eq!(verification["continuationRequired"], true);
        assert_eq!(
            verification["continuationCommand"],
            format!("mo resume {}", interrupted.id)
        );
        assert_eq!(fs::read(ctx.home.join("native-calls"))?, before);
        drop(store);
        let preview = resume_using(&ctx, &interrupted.id, true, false, &FIXTURE, |_, _| {
            panic!("preview cannot ask for action")
        })?;
        assert_eq!(preview["enabled"], false);
        assert_eq!(preview["operation"], interrupted.id);
        assert_eq!(preview["persistentWrites"], false);
        let declined = resume_using(&ctx, &interrupted.id, false, false, &FIXTURE, |_, _| {
            Err(Error::attention("cancelled", "fixture cancellation"))
        });
        assert!(declined.is_err());
        assert_eq!(fs::read(ctx.home.join("native-calls"))?, before);
        let result = resume_using(
            &ctx,
            &interrupted.id,
            false,
            false,
            &FIXTURE,
            |prompt, yes| {
                assert!(
                    !yes && prompt.contains(&interrupted.id) && prompt.contains("初始设置 停用")
                );
                Ok(())
            },
        )?;
        assert_eq!(result["journal"]["phase"], "succeeded");
        assert_eq!(
            fs::read_to_string(ctx.home.join("native-calls"))?,
            "market\nplugin\nactivation\n"
        );
        assert!(
            resume_using(
                &ctx,
                &interrupted.id,
                false,
                false,
                &FIXTURE,
                |_, _| panic!("subsequent lifecycle already recorded")
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn stage_proof_refuses_drift_in_source_native_registry_settings_or_original_exit() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, false, &FIXTURE)?;
    let copy = prepared.checkout().join("changed-file");
    fs::write(&copy, b"foreign checkout content")?;
    assert!(prepared.guard(&store.path, &FIXTURE).is_err());
    fs::remove_file(copy)?;

    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared: prepared.clone(),
            step: Step::Market,
        },
        &FIXTURE,
    )?;
    let install = Frozen {
        prepared,
        step: Step::Plugin {
            market: market.id.clone(),
        },
    };
    let directory = physical(&store.path)?;
    let candidate = crate::operations::new_journal(&ctx, market.plan.clone())?;
    before(&install, &candidate, &directory, &FIXTURE)?;
    let registry = install
        .prepared
        .profile()
        .join("plugins/known_marketplaces.json");
    let before_market = fs::read(&registry)?;
    let mut altered = crate::policy::json::parse(&before_market)?;
    for key in ["installLocation", "source"] {
        altered["fixture-market"][key] = json!("foreign");
        json_file(&registry, &altered)?;
        assert!(before(&install, &candidate, &directory, &FIXTURE).is_err());
        altered = crate::policy::json::parse(&before_market)?;
    }
    fs::write(&registry, &before_market)?;
    Store::atomic(&store.root, &sidecar(&market, "fixture-exit"), &"failed")?;
    assert!(before(&install, &candidate, &directory, &FIXTURE).is_err());
    Store::atomic(&store.root, &sidecar(&market, "fixture-exit"), &"clean")?;
    let mut receipt: After =
        Store::read(&store.root, &sidecar(&market, "after"))?.ok_or("worker evidence")?;
    receipt.worker = Some(1515);
    Store::atomic(&store.root, &sidecar(&market, "after"), &receipt)?;
    assert!(before(&install, &candidate, &directory, &FIXTURE).is_err());
    receipt.worker = market.process_group;
    Store::atomic(&store.root, &sidecar(&market, "after"), &receipt)?;

    let plugin = stage(&ctx, &store, &install, &FIXTURE)?;
    assert_eq!(plugin.phase, Phase::Succeeded);
    let settings = install.prepared.profile().join("settings.json");
    let current = fs::read(&settings)?;
    let mut unexpected = crate::policy::json::parse(&current)?;
    unexpected["model"] = json!("foreign change");
    json_file(&settings, &unexpected)?;
    assert!(after(&install, &plugin, &directory, &FIXTURE).is_err());
    fs::write(&settings, current)?;
    let wrong = install.prepared.payload().join("foreign-file");
    fs::write(&wrong, b"unexpected cache mutation")?;
    assert!(after(&install, &plugin, &directory, &FIXTURE).is_err());
    assert!(store.receipts()?.installations.is_empty());
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\nplugin\n"
    );
    Ok(())
}

#[test]
fn repeated_receipt_finalization_keeps_the_original_bytes_and_refuses_conflicting_owner()
-> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let result = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let receipt_id = result["journal"]["id"].as_str().ok_or("receipt identity")?;
    let native_id = receipt_id
        .strip_suffix("-receipt")
        .ok_or("native identity")?;
    let store = Store::lock(&ctx)?;
    let directory = physical(&store.path)?;
    let terminal = original(&directory, receipt_id)?.ok_or("original receipt")?;
    let mut native = original(&directory, native_id)?.ok_or("original native")?;
    let pack = ctx.state.join("receipts.json");
    let bytes = fs::read(&pack)?;
    let repeated = recover_using(&ctx, &store, &mut native, false, false, &FIXTURE)?;
    assert_eq!(repeated["journal"]["id"], receipt_id);
    assert_eq!(fs::read(&pack)?, bytes);
    assert!(recover_publication_using(&ctx, &store, &terminal, true, &FIXTURE).is_err());

    let mut ledger = store.receipts()?;
    ledger
        .installations
        .values_mut()
        .next()
        .ok_or("receipt")?
        .operation_id = "OP-foreign".into();
    json_file(&pack, &json!(ledger))?;
    let preserved = fs::read(&pack)?;
    assert!(recover_publication_using(&ctx, &store, &terminal, false, &FIXTURE).is_err());
    assert_eq!(fs::read(&pack)?, preserved);
    fs::remove_file(&pack)?;
    assert!(recover_publication_using(&ctx, &store, &terminal, false, &FIXTURE).is_err());
    assert!(!pack.exists());
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\nplugin\n"
    );
    Ok(())
}

#[test]
fn still_running_and_foreign_context_hold_the_original_stage() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    fs::write(ctx.home.join("unknown-exit"), b"")?;
    install_using(&ctx, offer, binary, false, &FIXTURE)?;
    let store = Store::lock(&ctx)?;
    let mut journal: Journal = Store::read(&store.root, "active.json")?.ok_or("original stage")?;
    let old = fs::read(ctx.state.join("active.json"))?;
    Store::atomic(&store.root, &sidecar(&journal, "fixture-exit"), &"running")?;
    let error = recover_using(&ctx, &store, &mut journal, false, false, &FIXTURE)
        .err()
        .ok_or("expected original process protection")?;
    assert_eq!(error.code, "operation_in_flight");
    let mut foreign = ctx.clone();
    foreign.settings = ctx.home.join("unrelated-settings");
    assert!(recover_using(&foreign, &store, &mut journal, true, true, &FIXTURE).is_err());
    assert_eq!(fs::read(ctx.state.join("active.json"))?, old);
    assert!(store.receipts()?.installations.is_empty());
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    Ok(())
}

#[test]
fn continuation_refuses_replaced_evidence_and_existing_next_stage_even_without_effects()
-> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, true, &FIXTURE)?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared: prepared.clone(),
            step: Step::Market,
        },
        &FIXTURE,
    )?;
    drop(store);
    let path = ctx
        .state
        .join("operations")
        .join(format!("{}.json", market.id));
    let original = fs::read(&path)?;
    let ordinary = |_: &str, _: bool| panic!("an already attempted stage must not prompt");
    let altered = resume_using(&ctx, &market.id, false, false, &FIXTURE, |_, _| {
        let mut changed = crate::policy::json::parse(&original)?;
        changed["message"] = json!("replacement occurred during consent");
        json_file(&path, &changed)
    });
    assert!(altered.is_err());
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    fs::write(&path, original)?;
    let store = Store::lock(&ctx)?;
    let no_start = NativeIo {
        launch: |_, _, _| Err(invalid()),
        ..FIXTURE
    };
    let mut following = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            step: Step::Plugin {
                market: market.id.clone(),
            },
        },
        &no_start,
    )?;
    assert_eq!(following.phase, Phase::Prepared);
    recover_using(&ctx, &store, &mut following, false, false, &FIXTURE)?;
    assert_eq!(following.phase, Phase::Failed);
    drop(store);
    let error = resume_using(&ctx, &market.id, false, true, &FIXTURE, ordinary)
        .err()
        .ok_or("the next stage already has a journal")?;
    assert_eq!(error.code, "claude_install_next_stage_exists");
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    Ok(())
}

#[test]
fn uncertified_public_entrypoints_leave_the_native_gate_closed() -> TestResult {
    let (_temp, ctx, offer, binary_path) = fixture()?;
    let installed_location = ctx
        .home
        .join(".local/share/mise/installs/aqua-anthropics-claude-code/2.1.328");
    fs::create_dir_all(&installed_location)?;
    fs::write(installed_location.join("claude"), b"uncertified executable")?;
    assert!(binary(&ctx).is_err());
    assert!(install(&ctx, offer.clone(), binary_path.clone(), true).is_err());
    assert!(store::history(&ctx)?.is_empty());

    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary_path, true, &FIXTURE)?;
    let market = Frozen {
        prepared,
        step: Step::Market,
    };
    let original = stage(&ctx, &store, &market, &FIXTURE)?;
    let args = invocation(&market, original.plan.timeout_secs)?;
    let mut wrong = args.clone();
    wrong
        .env
        .insert("CLAUDE_CONFIG_DIR".into(), ctx.home.display().to_string());
    assert!(worker_guard(&original, &wrong).is_err());
    assert!(worker_guard(&original, &args).is_err());
    assert!(
        worker_after(
            &original,
            &args,
            &NativeOutcome::Exited {
                code: Some(7),
                signal: None
            }
        )
        .is_err()
    );
    assert!(
        worker_after(
            &original,
            &args,
            &NativeOutcome::Exited {
                code: Some(0),
                signal: None
            }
        )
        .is_err()
    );
    assert!(resume(&ctx, &original.id, true, false).is_err());
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn matching_terminal_history_repairs_only_its_pending_active_copy() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let installed = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let id = installed["journal"]["id"]
        .as_str()
        .ok_or("receipt identity")?;
    let native_id = id.strip_suffix("-receipt").ok_or("native identity")?;
    let store = Store::lock(&ctx)?;
    let directory = physical(&store.path)?;
    let origin = original(&directory, native_id)?.ok_or("native job")?;
    let mut reservation: Journal =
        Store::read(&store.root, &sidecar(&origin, "adoption"))?.ok_or("original reservation")?;
    let bytes = fs::read(ctx.state.join("receipts.json"))?;
    Store::atomic(&store.root, "active.json", &reservation)?;
    assert!(recover_publication(&ctx, &store, &reservation, false).is_err());
    let sent = recover_publication_using(&ctx, &store, &reservation, false, &FIXTURE)?;
    assert_eq!(sent["journal"]["phase"], "succeeded");
    let active: Journal = Store::read(&store.root, "active.json")?.ok_or("completed active")?;
    assert_eq!(active.phase, Phase::Succeeded);
    assert_eq!(active.id, id);
    reservation.plan.warnings = vec![format!("{PUBLICATION}OP-foreign")];
    assert!(recover_publication_using(&ctx, &store, &reservation, false, &FIXTURE).is_err());
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, bytes);
    Ok(())
}

#[test]
fn native_metadata_readback_rejects_nonmatching_scope_version_and_payload_location() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let outcome = install_using(&ctx, offer, binary, false, &FIXTURE)?;
    let id = outcome["journal"]["id"]
        .as_str()
        .ok_or("receipt")?
        .strip_suffix("-receipt")
        .ok_or("install stage")?;
    let dir = physical(&ctx.state)?;
    let native = original(&dir, id)?.ok_or("original stage")?;
    let plan = frozen(&native, &dir)?;
    let path = plan
        .prepared
        .profile()
        .join("plugins/installed_plugins.json");
    let good = fs::read(&path)?;
    let native_id = plan.prepared.offer.native_id();
    for (key, value) in [
        ("installPath", json!(ctx.home.join("foreign"))),
        ("scope", json!("local")),
        ("version", json!("0.0.0")),
        ("installedAt", json!("")),
        ("lastUpdated", json!(null)),
        ("foreignExtra", json!(true)),
    ] {
        let mut data = crate::policy::json::parse(&good)?;
        data["plugins"][&native_id][0][key] = value;
        json_file(&path, &data)?;
        assert!(state::observe(&plan.prepared, &plan.step).is_err());
    }
    fs::write(&path, &good)?;
    let mut invalid = crate::policy::json::parse(&good)?;
    invalid["version"] = json!(3);
    json_file(&path, &invalid)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    fs::write(&path, &good)?;
    verify_installation(&plan, &native, &dir, &FIXTURE)?;
    Ok(())
}

#[test]
fn saved_plugin_phase_checks_predecessor_shape_and_frozen_argv_before_any_call() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, true, &FIXTURE)?;
    let market_plan = Frozen {
        prepared: prepared.clone(),
        step: Step::Market,
    };
    let market = stage(&ctx, &store, &market_plan, &FIXTURE)?;
    let directory = physical(&store.path)?;
    assert!(original(&directory, "../other").is_err());
    let mut wrong_argv = market.clone();
    if let Some(command) = &mut wrong_argv.plan.command {
        command.program = "/usr/bin/true".into();
    }
    wrong_argv.plan_sha256 = fingerprint(&wrong_argv.plan)?;
    assert!(frozen(&wrong_argv, &directory).is_err());
    let next = crate::operations::new_journal(&ctx, market.plan.clone())?;
    let bad = Frozen {
        prepared: prepared.clone(),
        step: Step::Activation {
            plugin: market.id.clone(),
        },
    };
    assert!(before(&bad, &next, &directory, &FIXTURE).is_err());
    let wrong_root = Frozen {
        prepared,
        step: Step::Plugin {
            market: "OP-missing".into(),
        },
    };
    assert!(before(&wrong_root, &next, &directory, &FIXTURE).is_err());
    assert!(
        before(
            &Frozen {
                prepared: wrong_root.prepared,
                step: Step::Market
            },
            &next,
            &directory,
            &FIXTURE
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    Ok(())
}

#[test]
fn private_profile_discovery_requires_the_exact_terminal_adoption_and_reservation() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let called = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let receipt_id = called["journal"]["id"].as_str().ok_or("receipt")?;
    let native_id = receipt_id.strip_suffix("-receipt").ok_or("original")?;
    let directory = physical(&ctx.state)?;
    let native = original(&directory, native_id)?.ok_or("native")?;
    let terminal = original(&directory, receipt_id)?.ok_or("adopted")?;
    let file = ctx.state.join(sidecar(&native, "adoption"));
    let bytes = fs::read(&file)?;
    let mut wrong = crate::policy::json::parse(&bytes)?;
    wrong["phase"] = json!("succeeded");
    json_file(&file, &wrong)?;
    assert_eq!(managed_profiles(&ctx)?, []);
    fs::write(&file, &bytes)?;

    let store = Store::lock(&ctx)?;
    let receipts = ctx.state.join("receipts.json");
    let mut saved = store.receipts()?;
    saved
        .installations
        .values_mut()
        .next()
        .ok_or("receipt")?
        .operation_id = native_id.into();
    Store::atomic(&store.root, "receipts.json", &saved)?;
    assert_eq!(managed_profiles(&ctx)?, []);
    assert!(require_ordinary_adoption(&terminal.plan.package).is_err());
    assert!(receipts.is_file());
    Ok(())
}

#[test]
fn account_policy_rejects_ambiguous_path_text_and_absent_native_metadata() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let mut prepared = prepare(&ctx, &store, offer, binary, true, &FIXTURE)?;
    let before = prepared.protected.clone();
    prepared.protected.push(ctx.home.join("control\npath"));
    assert!(
        invocation(
            &Frozen {
                prepared: prepared.clone(),
                step: Step::Market
            },
            30
        )
        .is_err()
    );
    prepared.protected = before;
    let plan = Frozen {
        prepared,
        step: Step::Market,
    };
    let market = stage(&ctx, &store, &plan, &FIXTURE)?;
    let registry = plan
        .prepared
        .profile()
        .join("plugins/known_marketplaces.json");
    let bytes = fs::read(&registry)?;
    fs::remove_file(&registry)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    std::os::unix::fs::symlink(ctx.home.join(".claude-glm/settings.json"), &registry)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    fs::remove_file(&registry)?;
    fs::write(&registry, &bytes)?;
    let mut changed = crate::policy::json::parse(&bytes)?;
    changed["fixture-market"]["lastUpdated"] = json!(null);
    json_file(&registry, &changed)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    assert_eq!(market.phase, Phase::Succeeded);
    assert_eq!(
        fs::read(ctx.home.join(".claude-glm/settings.json"))?,
        br#"{"model":"fixture-sibling"}"#
    );
    Ok(())
}

#[test]
fn allocation_revalidation_fences_rebound_directories_checkout_and_credentials() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, false, &FIXTURE)?;
    let mut foreign = prepared.clone();
    foreign.account_home = ctx.home.join("fabricated-account");
    assert!(foreign.guard(&store.path, &FIXTURE).is_err());
    foreign = prepared.clone();
    foreign.checkout_slot.parent.identity.inode += 1;
    assert!(foreign.guard(&store.path, &FIXTURE).is_err());

    let temporary = prepared.root.join("tmp");
    let original = prepared.root.join("saved-tmp");
    fs::rename(&temporary, &original)?;
    std::os::unix::fs::symlink(&ctx.home, &temporary)?;
    assert!(prepared.guard(&store.path, &FIXTURE).is_err());
    fs::remove_file(&temporary)?;
    fs::rename(original, &temporary)?;

    let credentials = prepared.profile().join(".credentials.json");
    fs::write(&credentials, b"synthetic shared configuration marker")?;
    assert!(prepared.guard(&store.path, &FIXTURE).is_err());
    fs::remove_file(credentials)?;

    fs::set_permissions(&prepared.root, std::fs::Permissions::from_mode(0o755))?;
    assert!(prepared.guard(&store.path, &FIXTURE).is_err());
    fs::set_permissions(&prepared.root, std::fs::Permissions::from_mode(0o700))?;
    prepared.guard(&store.path, &FIXTURE)?;
    assert!(store::history(&ctx)?.is_empty());
    Ok(())
}

#[test]
fn failed_checkout_preparation_leaves_no_allocations_or_native_starts() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let unverified_source = NativeIo {
        checkout: |_, _| Err(invalid()),
        ..FIXTURE
    };
    assert!(install_using(&ctx, offer, binary, true, &unverified_source).is_err());
    assert_eq!(
        fs::read_dir(&ctx.state)?
            .filter_map(std::result::Result::ok)
            .filter(|entry| label(&entry.path()).is_some())
            .count(),
        0
    );
    assert!(store::history(&ctx)?.is_empty());
    assert!(!ctx.home.join("native-calls").exists());
    Ok(())
}

#[test]
fn native_after_record_cannot_be_replaced_or_created_against_altered_arguments() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, true, &FIXTURE)?;
    let input = Frozen {
        prepared,
        step: Step::Market,
    };
    let original = stage(&ctx, &store, &input, &FIXTURE)?;
    let checkpoint = ctx.state.join(sidecar(&original, "after"));
    let before = fs::read(&checkpoint)?;
    let done = NativeOutcome::Exited {
        code: Some(0),
        signal: None,
    };
    let mut argv = invocation(&input, original.plan.timeout_secs)?;
    argv.cwd = ctx.home.clone();
    assert!(record_after(&original, &argv, &done, &FIXTURE).is_err());
    let argv = invocation(&input, original.plan.timeout_secs)?;
    assert!(record_after(&original, &argv, &done, &FIXTURE).is_err());
    assert_eq!(fs::read(&checkpoint)?, before);

    fs::remove_file(ctx.state.join(sidecar(&original, "plan")))?;
    assert!(record_after(&original, &argv, &done, &FIXTURE).is_err());
    assert_eq!(fs::read(&checkpoint)?, before);
    Ok(())
}

#[test]
fn missing_or_changed_adoption_reservation_preserves_existing_receipt() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let value = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let id = value["journal"]["id"].as_str().ok_or("receipt")?;
    let original_id = id.strip_suffix("-receipt").ok_or("native id")?;
    let store = Store::lock(&ctx)?;
    let directory = physical(&store.path)?;
    let original = original(&directory, original_id)?.ok_or("original stage")?;
    let input = frozen(&original, &directory)?;
    let reservation = ctx.state.join(sidecar(&original, "adoption"));
    let bytes = fs::read(&reservation)?;
    let receipt = ctx.state.join("receipts.json");
    let recorded = fs::read(&receipt)?;
    let mut tampered = crate::policy::json::parse(&bytes)?;
    tampered["started_at"] = json!(
        tampered["started_at"]
            .as_u64()
            .ok_or("original timestamp")?
            + 1
    );
    json_file(&reservation, &tampered)?;
    assert!(publish(&ctx, &store, &original, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&receipt)?, recorded);
    fs::remove_file(&reservation)?;
    assert!(publish(&ctx, &store, &original, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&receipt)?, recorded);
    assert!(!reservation.exists());
    Ok(())
}

#[test]
fn resume_does_not_take_over_a_foreign_pending_operation_after_confirmation() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, false, &FIXTURE)?;
    let stage = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            step: Step::Market,
        },
        &FIXTURE,
    )?;
    drop(store);
    let active_path = ctx.state.join("active.json");
    let mut other = stage.clone();
    other.id = "OP-other-pending".into();
    other.phase = Phase::Prepared;
    other.process_group = None;
    let bytes = serde_json::to_vec(&other)?;
    let declined = resume_using(&ctx, &stage.id, false, false, &FIXTURE, |_, _| {
        fs::write(&active_path, &bytes).map_err(Into::into)
    });
    assert_eq!(
        declined.err().ok_or("write fence")?.code,
        "recovery_required"
    );
    assert_eq!(fs::read(active_path)?, bytes);
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    Ok(())
}

#[test]
fn receipt_recovery_refuses_a_later_native_plugin_setting_change_in_both_install_shapes()
-> TestResult {
    for selected in [false, true] {
        let (_temp, ctx, offer, binary) = fixture()?;
        let published = install_using(&ctx, offer, binary, selected, &FIXTURE)?;
        let receipt_id = published["journal"]["id"]
            .as_str()
            .ok_or("receipt identity")?;
        let store = Store::lock(&ctx)?;
        let directory = physical(&store.path)?;
        let origin = original(&directory, receipt_id)?.ok_or("terminal journal")?;
        let setting = Path::new(published["claudeConfigDir"].as_str().ok_or("profile")?)
            .join("settings.json");
        let mut updated = crate::policy::json::parse(&fs::read(&setting)?)?;
        updated["enabledPlugins"]["fixture-plugin@fixture-market"] = json!(!selected);
        json_file(&setting, &updated)?;
        let retained = fs::read(&setting)?;
        let receipt = ctx.state.join("receipts.json");
        let receipt_bytes = fs::read(&receipt)?;
        assert!(recover_publication_using(&ctx, &store, &origin, false, &FIXTURE).is_err());
        assert_eq!(fs::read(setting)?, retained);
        assert_eq!(fs::read(receipt)?, receipt_bytes);
    }
    Ok(())
}

#[test]
fn native_market_proof_excludes_an_early_installation_and_extra_registry_rows() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, true, &FIXTURE)?;
    let market = Frozen {
        prepared,
        step: Step::Market,
    };
    let original = stage(&ctx, &store, &market, &FIXTURE)?;
    let profile = market.prepared.profile();
    let plugins = profile.join("plugins");
    let installed = plugins.join("installed_plugins.json");
    json_file(&installed, &json!({"version":2,"plugins":{}}))?;
    assert!(state::observe(&market.prepared, &market.step).is_err());
    fs::remove_file(&installed)?;
    let premature_cache = plugins.join("cache");
    fs::create_dir(&premature_cache)?;
    assert!(state::observe(&market.prepared, &market.step).is_err());
    fs::remove_dir(premature_cache)?;

    let history = plugins.join("known_marketplaces.json");
    let good = fs::read(&history)?;
    let mut wrong = crate::policy::json::parse(&good)?;
    wrong["foreign"] = json!({"source":{"source":"directory","path":ctx.home}});
    json_file(&history, &wrong)?;
    assert!(state::observe(&market.prepared, &market.step).is_err());
    fs::write(history, &good)?;
    assert_eq!(
        proof(&original, &physical(&ctx.state)?, &FIXTURE)?.state,
        state::observe(&market.prepared, &market.step)?
    );
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    Ok(())
}

#[test]
fn later_native_phases_verify_unselected_registry_evidence_from_the_original_stage() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, false, &FIXTURE)?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared: prepared.clone(),
            step: Step::Market,
        },
        &FIXTURE,
    )?;
    let install = Frozen {
        prepared: prepared.clone(),
        step: Step::Plugin { market: market.id },
    };
    let plugin = stage(&ctx, &store, &install, &FIXTURE)?;
    let directory = physical(&ctx.state)?;
    let market_file = prepared.profile().join("plugins/known_marketplaces.json");
    let before = fs::read(&market_file)?;
    let mut market = crate::policy::json::parse(&before)?;
    market["fixture-market"]["lastUpdated"] = json!("later-success-looking-unknown");
    json_file(&market_file, &market)?;
    assert!(state::observe(&prepared, &install.step).is_ok());
    assert!(after(&install, &plugin, &directory, &FIXTURE).is_err());
    fs::write(market_file, before)?;

    let activation = Frozen {
        prepared,
        step: Step::Activation { plugin: plugin.id },
    };
    let switched = stage(&ctx, &store, &activation, &FIXTURE)?;
    let file = activation
        .prepared
        .profile()
        .join("plugins/installed_plugins.json");
    let before = fs::read(&file)?;
    let mut records = crate::policy::json::parse(&before)?;
    records["plugins"]["fixture-plugin@fixture-market"][0]["lastUpdated"] =
        json!("foreign-later-mutation");
    json_file(&file, &records)?;
    assert!(state::observe(&activation.prepared, &activation.step).is_ok());
    assert!(after(&activation, &switched, &directory, &FIXTURE).is_err());
    fs::write(file, before)?;
    verify_installation(&activation, &switched, &directory, &FIXTURE)?;
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn already_prepared_adoption_can_commit_original_receipt_after_an_interruption() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let confirmed = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let id = confirmed["journal"]["id"].as_str().ok_or("receipt")?;
    let native_id = id.strip_suffix("-receipt").ok_or("original stage")?;
    let store = Store::lock(&ctx)?;
    let directory = physical(&ctx.state)?;
    let origin = original(&directory, native_id)?.ok_or("original phase")?;
    let reservation: Journal =
        Store::read(&store.root, &sidecar(&origin, "adoption"))?.ok_or("original reservation")?;
    fs::remove_file(ctx.state.join("receipts.json"))?;
    store.save(&reservation)?;
    assert_eq!(managed_profiles(&ctx)?, []);
    let outcome = recover_publication_using(&ctx, &store, &reservation, false, &FIXTURE)?;
    assert_eq!(outcome["journal"]["id"], id);
    assert_eq!(
        outcome["journal"]["started_at"],
        confirmed["journal"]["started_at"]
    );
    assert_eq!(outcome["journal"]["phase"], "succeeded");
    assert_eq!(managed_profiles(&ctx)?.len(), 1);
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\nplugin\n"
    );
    Ok(())
}

#[test]
fn certified_binary_selection_prefers_available_compatible_tool_and_honors_bound_mise_data()
-> TestResult {
    let (temp, mut ctx, _offer, preferred) = fixture()?;
    let install_root = ctx.home.join("known-mise-root");
    let fallback = install_root.join("installs/aqua-anthropics-claude-code/2.1.328/claude");
    fs::create_dir_all(fallback.parent().ok_or("bound install dir")?)?;
    fs::write(&fallback, b"fallback native client")?;
    ctx.scope
        .paths
        .insert("MISE_DATA_DIR".into(), Some(install_root));
    assert_eq!(
        select_binary(&ctx, Some(preferred.clone()), |path| {
            assert_eq!(path, preferred);
            Ok(())
        })?,
        preferred
    );
    let mut seen = vec![];
    assert_eq!(
        select_binary(&ctx, Some(preferred.clone()), |path| {
            seen.push(path.to_owned());
            if path == fallback {
                Ok(())
            } else {
                Err(invalid())
            }
        })?,
        fallback
    );
    assert_eq!(seen, vec![preferred, fallback.clone()]);
    let foreign = temp.path().join("host-only-client");
    fs::write(&foreign, b"outside logical home")?;
    assert_eq!(
        select_binary(&ctx, Some(foreign), |path| {
            assert_eq!(path, fallback);
            Ok(())
        })?,
        fallback
    );
    Ok(())
}

#[test]
fn installed_managed_claude_profile_has_one_adopted_inventory_identity() -> TestResult {
    let (_temp, mut ctx, offer, binary) = fixture()?;
    let result = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let profile = PathBuf::from(
        result["claudeConfigDir"]
            .as_str()
            .ok_or("managed profile")?,
    );
    ctx.harness.claude = Some(profile.clone());
    let (observed, _) = crate::resource_agents::collect(&ctx, None, &[])?;
    let profiles:Vec<_>=observed.iter().filter(|entry| {
        matches!(&entry.data, crate::resource::Data::Agent{config_dir,..} if config_dir==&profile)
    }).collect();
    assert_eq!(profiles.len(), 1);
    assert!(profiles[0].name.starts_with("claude:mo-fixture-plugin-"));
    assert_eq!(profiles[0].management, crate::resource::Management::Adopted);
    assert!(observed.iter().any(|entry| entry.name == "claude:glm"
        && entry.management == crate::resource::Management::Untracked));
    Ok(())
}

#[test]
fn mirrored_ordinary_claude_profile_needs_the_exact_managed_id() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let (_, native_path) = managed_profiles(&ctx)?.pop().ok_or("published profile")?;
    let managed = crate::resource::id("agent", &("claude", &native_path))?;
    let resolve = |selector: &str| crate::resource_agents::resolve_session_profile(&ctx, selector);
    let label = resolve(&managed)?.name;
    let name = label.strip_prefix("claude:").ok_or("Claude name")?;
    let mirror = ctx.home.join(format!(".claude-{name}"));
    fs::create_dir(&mirror)?;
    let ordinary = crate::resource::id("agent", &("claude", &mirror))?;
    assert_eq!(
        resolve(&label).err().ok_or("ambiguous")?.code,
        "session_profile_ambiguous"
    );
    assert!(resolve(&ordinary).is_err());
    assert_eq!(resolve(&managed)?.name, label);
    session_target_using(&ctx, &native_path, FIXTURE.binary)?;
    assert_eq!(fs::read_dir(mirror)?.count(), 0);
    Ok(())
}

#[test]
fn published_session_admits_user_model_but_rejects_conflicting_installation_evidence() -> TestResult
{
    let (_temp, ctx, offer, binary) = fixture()?;
    let plugin = offer.native_id();
    let marketplace = offer.marketplace.clone();
    let result = install_using(&ctx, offer, binary, false, &FIXTURE)?;
    let path = Path::new(
        result["claudeConfigDir"]
            .as_str()
            .ok_or("managed profile")?,
    );
    let settings = path.join("settings.json");
    let records = path.join("plugins/installed_plugins.json");
    let registry = path.join("plugins/known_marketplaces.json");
    let original_receipt = fs::read(ctx.state.join("receipts.json"))?;
    let native = |path: &Path| session_target_using(&ctx, path, FIXTURE.binary);
    let (_, _, initial) = native(path)?;

    let mut changed = crate::policy::json::parse(&fs::read(&settings)?)?;
    changed["model"] = json!("fixture-new-model");
    changed["env"] = json!({"ANTHROPIC_API_KEY":"synthetic-specific-value"});
    changed["enabledPlugins"]["unrelated"] = json!(true);
    json_file(&settings, &changed)?;
    fs::write(path.join(".credentials.json"), b"synthetic-claude-login")?;
    let (_, _, current) = native(path)?;
    assert_eq!(current.model.as_deref(), Some("fixture-new-model"));
    assert_eq!(current.authority, initial.authority);
    assert_ne!(current.evidence, initial.evidence);
    assert!(initial.verify(&ctx).is_err());
    current.verify(&ctx)?;
    assert!(
        session_target(&ctx, path).is_err(),
        "binary attestation is required"
    );

    let mut installed = crate::policy::json::parse(&fs::read(&records)?)?;
    let mut conflicting = installed["plugins"][&plugin][0].clone();
    conflicting["scope"] = json!("project");
    installed["plugins"]["unrelated"] = json!([conflicting.clone()]);
    json_file(&records, &installed)?;
    native(path)?;
    let restore = fs::read(&records)?;
    installed["plugins"][&plugin]
        .as_array_mut()
        .ok_or("fixture registration")?
        .push(conflicting);
    json_file(&records, &installed)?;
    assert!(native(path).is_err());
    fs::write(&records, restore)?;

    let mut observed_market = crate::policy::json::parse(&fs::read(&registry)?)?;
    observed_market[&marketplace]["installLocation"] = json!(ctx.home.join("unrelated"));
    let restore = fs::read(&registry)?;
    json_file(&registry, &observed_market)?;
    assert!(native(path).is_err());
    fs::write(&registry, restore)?;

    changed["extraKnownMarketplaces"][&marketplace]["source"]["path"] = json!(ctx.home);
    let restore = fs::read(&settings)?;
    json_file(&settings, &changed)?;
    assert!(native(path).is_err());
    fs::write(&settings, restore)?;

    let (_, _, still_installed) = native(path)?;
    let payload = still_installed.prepared.payload();
    fs::write(payload.join("skills/fixture/SKILL.md"), b"changed fixture")?;
    assert!(still_installed.verify(&ctx).is_err());
    assert!(native(path).is_err());
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, original_receipt);
    Ok(())
}

#[test]
fn an_unknown_later_stage_keeps_its_own_operation_and_never_publishes() -> TestResult {
    for (step, calls) in [
        ("plugin", "market\nplugin\n"),
        ("activation", "market\nplugin\nactivation\n"),
    ] {
        let (_temp, ctx, offer, binary) = fixture()?;
        fs::write(ctx.home.join(format!("unknown-{step}")), b"")?;
        let result = install_using(&ctx, offer, binary, false, &FIXTURE)?;
        assert_eq!(result["stage"], step);
        assert_eq!(result["journal"]["phase"], "unknown");
        let original_id = result["journal"]["id"].as_str().ok_or("original id")?;
        let store = Store::lock(&ctx)?;
        let mut unconfirmed: Journal = Store::read(&store.root, "active.json")?.ok_or("active")?;
        assert_eq!(unconfirmed.id, original_id);
        let verified = recover_using(&ctx, &store, &mut unconfirmed, false, false, &FIXTURE)?;
        assert_eq!(verified["journal"]["phase"], "unknown");
        assert_eq!(verified["journal"]["id"], original_id);
        assert!(store.receipts()?.installations.is_empty());
        assert_eq!(managed_profiles(&ctx)?, []);
        assert_eq!(fs::read_to_string(ctx.home.join("native-calls"))?, calls);
    }
    Ok(())
}

#[test]
fn failure_after_worker_identity_exists_preserves_unknown_origin_evidence() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let interrupted = NativeIo {
        launch: |store, journal, _| {
            journal.phase = Phase::Running;
            journal.process_group = Some(4242);
            store.save(journal)?;
            Store::atomic_new(&store.root, &sidecar(journal, "fixture-exit"), &"unknown")?;
            Err(invalid())
        },
        ..FIXTURE
    };
    let attempt = install_using(&ctx, offer, binary, true, &interrupted)?;
    assert_eq!(attempt["journal"]["phase"], "unknown");
    let store = Store::lock(&ctx)?;
    let mut active: Journal = Store::read(&store.root, "active.json")?.ok_or("active")?;
    assert_eq!(active.process_group, Some(4242));
    assert!(recover_using(&ctx, &store, &mut active, true, true, &FIXTURE).is_err());
    assert_eq!(active.phase, Phase::Unknown);
    assert_eq!(active.id, attempt["journal"]["id"].as_str().ok_or("id")?);
    assert!(store.check_pending().is_err());
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn late_payload_drift_at_receipt_commit_preserves_the_pending_original_adoption() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, true, &FIXTURE)?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared: prepared.clone(),
            step: Step::Market,
        },
        &FIXTURE,
    )?;
    let install = Frozen {
        prepared,
        step: Step::Plugin { market: market.id },
    };
    let installed = stage(&ctx, &store, &install, &FIXTURE)?;
    fs::write(ctx.home.join("commit-check"), b"first")?;
    let late = NativeIo {
        binary: |reader, path| {
            (FIXTURE.binary)(reader, path)?;
            let home = path.parent().ok_or_else(invalid)?;
            let counter = home.join("commit-check");
            if fs::read(&counter)? == b"first" {
                fs::write(counter, b"callback")?;
                return Ok(());
            }
            let allocation = fs::read_dir(home.join(".local/state/macops"))?
                .filter_map(std::result::Result::ok)
                .map(|item| item.path())
                .find(|path| label(path).is_some())
                .ok_or_else(invalid)?;
            fs::write(
                allocation.join(
                    "home/.claude/plugins/cache/fixture-market/fixture-plugin/1.0.0/unexpected",
                ),
                b"foreign change",
            )?;
            Ok(())
        },
        ..FIXTURE
    };
    assert!(publish(&ctx, &store, &installed, &install, &late).is_err());
    let pending: Journal = Store::read(&store.root, "active.json")?.ok_or("pending receipt")?;
    assert_eq!(pending.id, format!("{}-receipt", installed.id));
    assert_eq!(pending.phase, Phase::Prepared);
    assert!(store.receipts()?.installations.is_empty());
    assert!(install.prepared.payload().join("unexpected").is_file());
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\nplugin\n"
    );
    Ok(())
}

#[test]
fn original_market_history_repairs_an_unknown_active_copy_without_dispatch() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare(&ctx, &store, offer, binary, false, &FIXTURE)?;
    let original = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            step: Step::Market,
        },
        &FIXTURE,
    )?;
    let mut lagging = original.clone();
    lagging.phase = Phase::Unknown;
    Store::atomic(&store.root, "active.json", &lagging)?;
    let repaired = recover_using(&ctx, &store, &mut lagging, false, false, &FIXTURE)?;
    assert_eq!(repaired["journal"]["phase"], "succeeded");
    assert_eq!(repaired["journal"]["id"], original.id);
    assert_eq!(repaired["continuationRequired"], true);
    assert_eq!(
        fs::read_to_string(ctx.home.join("native-calls"))?,
        "market\n"
    );
    Ok(())
}

#[test]
fn missing_or_malformed_installation_metadata_cannot_become_a_successful_readback() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let result = install_using(&ctx, offer, binary, true, &FIXTURE)?;
    let op = result["journal"]["id"].as_str().ok_or("receipt")?;
    let id = op.strip_suffix("-receipt").ok_or("native operation")?;
    let directory = physical(&ctx.state)?;
    let native = original(&directory, id)?.ok_or("native stage")?;
    let plan = frozen(&native, &directory)?;
    let profile = plan.prepared.profile();
    let settings = profile.join("settings.json");
    let registry = profile.join("plugins/installed_plugins.json");
    for path in [&settings, &registry] {
        let bytes = fs::read(path)?;
        fs::remove_file(path)?;
        assert!(state::observe(&plan.prepared, &plan.step).is_err());
        fs::write(path, b"{not json")?;
        assert!(state::observe(&plan.prepared, &plan.step).is_err());
        fs::write(path, bytes)?;
    }
    let bytes = fs::read(&settings)?;
    let mut bad = crate::policy::json::parse(&bytes)?;
    bad["enabledPlugins"]["fixture-plugin@fixture-market"] = json!("true");
    json_file(&settings, &bad)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    fs::write(settings, bytes)?;

    let registry_bytes = fs::read(&registry)?;
    let mut bad = crate::policy::json::parse(&registry_bytes)?;
    bad["plugins"]["fixture-plugin@fixture-market"] = json!({});
    json_file(&registry, &bad)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    bad["plugins"]["fixture-plugin@fixture-market"] = json!([]);
    json_file(&registry, &bad)?;
    assert!(state::observe(&plan.prepared, &plan.step).is_err());
    fs::write(registry, registry_bytes)?;
    verify_installation(&plan, &native, &directory, &FIXTURE)?;
    Ok(())
}

fn assert_claude_launch(
    command: &std::process::Command,
    path: &Path,
    plugin: &str,
    enabled: bool,
) -> Result<()> {
    let args = command.get_args().collect::<Vec<_>>();
    assert_eq!(args.first().copied(), Some(OsStr::new("--settings")));
    assert_eq!(args.len(), 2);
    let settings: Value = serde_json::from_str(args[1].to_str().ok_or_else(invalid)?)?;
    assert_eq!(settings, json!({"enabledPlugins":{plugin:enabled}}));
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == "CLAUDE_CONFIG_DIR" && value == Some(path.as_os_str()))
    );
    assert!(!command.get_envs().any(|(key, _)| key == "CODEX_HOME"));
    Ok(())
}

#[test]
fn claude_session_uses_the_same_confirmation_flow_and_preserves_native_profile() -> TestResult {
    use crate::codex_install::session::{
        LaunchOptions, PREFERENCE, SessionIo, configure_using, details_using, launch_using,
    };

    let (_temp, ctx, offer, binary) = fixture()?;
    let published = install_using(&ctx, offer, binary, false, &FIXTURE)?;
    let path = PathBuf::from(
        published["claudeConfigDir"]
            .as_str()
            .ok_or("Claude profile")?,
    );
    let client =
        SessionIo::with_claude(|ctx, path| session_target_using(ctx, path, FIXTURE.binary));
    let id = crate::resource::id("agent", &("claude", &path))?;
    let config = path.join("settings.json");
    let saved = fs::read(&config)?;
    let mut native_settings = crate::policy::json::parse(&saved)?;
    native_settings["model"] = json!("fixture-claude-model");
    let saved = serde_json::to_vec(&native_settings)?;
    fs::write(&config, &saved)?;
    let sibling = ctx.home.join(".claude-glm/settings.json");
    let sibling_bytes = fs::read(&sibling)?;
    let current = details_using(&ctx, &id, &client).ok_or("managed details")?;
    assert_eq!(
        current.observed_model.as_deref(),
        Some("fixture-claude-model")
    );
    assert_eq!(current.preference, "按原生设置");
    let preview = LaunchOptions {
        name: None,
        dry: true,
        yes: false,
        interactive: false,
    };
    let plan = launch_using(
        &ctx,
        &id,
        preview,
        &client,
        |_, _| Err(invalid()),
        |_| Err(invalid()),
    )?;
    assert_eq!(plan["argv"], json!([]));
    assert_eq!(
        launch_using(
            &ctx,
            &id,
            LaunchOptions {
                name: Some("codex-only"),
                ..preview
            },
            &client,
            |_, _| Err(invalid()),
            |_| Err(invalid()),
        )
        .err()
        .ok_or("unsupported Claude option")?
        .code,
        "native_profile_codex_only"
    );
    let preference = path
        .ancestors()
        .nth(2)
        .ok_or("allocation")?
        .join(PREFERENCE);
    assert!(configure_using(&ctx, &id, &client, &b"1\n"[..], |_, _| Err(invalid())).is_err());
    assert!(!preference.exists());
    for (answer, enabled) in [(&b"1\n"[..], true), (&b"2\n"[..], false)] {
        configure_using(&ctx, &id, &client, answer, |prompt, yes| {
            assert!(prompt.contains("fixture-plugin"));
            assert!(!yes);
            Ok(())
        })?;
        let running = LaunchOptions {
            dry: false,
            interactive: true,
            ..preview
        };
        launch_using(
            &ctx,
            &id,
            running,
            &client,
            |_, _| Ok(()),
            |command| {
                assert_claude_launch(&command, &path, &current.plugin, enabled)?;
                Ok(json!("synthetic launch"))
            },
        )?;
        assert_eq!(fs::read(&config)?, saved);
        assert_eq!(fs::read(&sibling)?, sibling_bytes);
    }
    let prior = fs::read(&preference)?;
    let result = configure_using(&ctx, &id, &client, &b"3\n"[..], |_, _| {
        native_settings["model"] = json!("changed-during-confirmation");
        fs::write(&config, serde_json::to_vec(&native_settings)?)?;
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(fs::read(&preference)?, prior);
    Ok(())
}

#[test]
fn claude_session_requires_the_selected_config_and_terminal_publication() -> TestResult {
    let (_temp, ctx, offer, binary) = fixture()?;
    let key = offer.native_id();
    let installed = install_using(&ctx, offer, binary, false, &FIXTURE)?;
    let profile = Path::new(installed["claudeConfigDir"].as_str().ok_or("profile")?);
    let path = profile.join("settings.json");
    let settings = fs::read(&path)?;
    fs::remove_file(&path)?;
    assert!(session_target_using(&ctx, profile, FIXTURE.binary).is_err());
    let mut invalid_settings = crate::policy::json::parse(&settings)?;
    invalid_settings["enabledPlugins"][&key] = json!("true");
    json_file(&path, &invalid_settings)?;
    assert!(session_target_using(&ctx, profile, FIXTURE.binary).is_err());
    fs::write(path, &settings)?;
    let id = installed["journal"]["id"]
        .as_str()
        .ok_or("terminal publication")?;
    let recorded = ctx.state.join("operations").join(format!("{id}.json"));
    let bytes = fs::read(&recorded)?;
    fs::remove_file(&recorded)?;
    assert!(session_target_using(&ctx, profile, FIXTURE.binary).is_err());
    fs::write(recorded, bytes)?;
    session_target_using(&ctx, profile, FIXTURE.binary)?;
    Ok(())
}
