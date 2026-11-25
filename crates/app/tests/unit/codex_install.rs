use super::*;
use crate::test_support::TestResult;

pub(super) fn offer() -> Result<Offer> {
    Offer::parse(&json!({
        "repository":"vendor/fixture","marketplace":"market","plugin":"item","declaredVersion":"1.0.0",
        "marketplaceCommit":"a".repeat(40),"payloadCommit":"a".repeat(40),"payloadRef":"main",
        "marketplaceSha256":"a".repeat(64),"pluginManifestSha256":"b".repeat(64),
    }))
}

fn prepared(base: &Path) -> Result<Prepared> {
    let root = base.join("codex-profile.Fixture");
    fs::create_dir_all(root.join("home/.codex"))?;
    for leaf in ["cwd", "tmp", "git/objects.git"] {
        fs::create_dir_all(root.join(leaf))?;
    }
    let manifest = br#"{"name":"item","version":"1.0.0"}"#;
    Ok(Prepared {
        target: None,
        initially_disabled: false,
        identity: same_directory(&root)?.identity().clone(),
        directories: allocation_directories(&root)?,
        root,
        home: base.join("home"),
        account_home: account_home()?,
        binary: "/fixture/bin/codex".into(),
        mirror: "mirror-fingerprint".into(),
        files: vec![(
            ".codex-plugin/plugin.json".into(),
            false,
            source::sha(manifest),
        )],
        offer: offer()?,
    })
}

#[test]
fn prepared_home_and_native_arguments_are_exactly_scoped() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let source = prepared(temp.path())?;
    let first = Frozen {
        prepared: source.clone(),
        market_operation: None,
    };
    let market = invocation(&first, 17)?;
    assert_eq!(market.command.program, Path::new("/usr/bin/sandbox-exec"));
    assert!(market.command.args[1].contains("(deny network*)"));
    assert!(market.command.args[1].contains("deny file-read-data"));
    assert_eq!(market.env["GIT_ALLOW_PROTOCOL"], "file");
    assert_eq!(market.env["HOME"], text(&source.root.join("home"))?);
    assert_eq!(market.env["CODEX_HOME"], text(&source.profile())?);
    assert!(reserved(&source.profile()));
    assert!(!reserved(&temp.path().join(".codex")));
    assert!(
        market
            .command
            .args
            .ends_with(&["--ref".into(), "a".repeat(40), "--json".into()])
    );
    let second = Frozen {
        prepared: source,
        market_operation: Some("OP-market".into()),
    };
    let plugin = invocation(&second, 17)?;
    assert!(
        plugin
            .command
            .args
            .ends_with(&["add".into(), "item@market".into(), "--json".into()])
    );
    Ok(())
}

#[test]
fn native_readback_rejects_an_unexpected_market_config_or_installed_file() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let p = prepared(temp.path())?;
    let fixture = br#"{"name":"item","version":"1.0.0"}"#;
    let config = p.profile().join("config.toml");
    let market = format!(
        "[marketplaces.market]\nsource_type='git'\nsource='{}'\nref='{}'\n",
        p.offer.url()?,
        p.offer.marketplace_commit
    );
    fs::create_dir_all(p.market().join(".codex-plugin"))?;
    fs::write(p.market().join(".codex-plugin/plugin.json"), fixture)?;
    fs::write(&config, &market)?;
    let registration = projected(&p, false)?;
    assert_ne!(registration, "");
    assert!(projected(&p, true).is_err());
    fs::write(
        &config,
        format!("{market}[plugins.'item@market']\nenabled = true\n"),
    )?;
    fs::create_dir_all(p.payload().join(".codex-plugin"))?;
    fs::write(p.payload().join(".codex-plugin/plugin.json"), fixture)?;
    assert_ne!(projected(&p, true)?, registration);
    assert!(projected(&p, false).is_err());
    fs::write(p.payload().join("extra"), "changed")?;
    assert!(projected(&p, true).is_err());
    fs::remove_file(p.payload().join("extra"))?;
    fs::write(&config, market.replace("vendor/fixture", "vendor/other"))?;
    assert!(projected(&p, false).is_err());
    Ok(())
}

const FIXTURE_MANIFEST: &[u8] = br#"{"name":"item","version":"1.0.0"}"#;

pub(super) fn fixture_context() -> Result<(tempfile::TempDir, Context)> {
    use clap::Parser as _;
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let home = temporary.path().join("home");
    fs::create_dir(&home)?;
    let cli = crate::cli::Cli::try_parse_from(["macops", "--home", &text(&home)?, "app"])
        .map_err(|_| stale())?;
    let context = Context::from_cli(&cli, &mut Vec::new())?;
    Ok((temporary, context))
}

pub(super) const FIXTURE: NativeIo = NativeIo {
    progress: |_| {},
    verify_binary: |reader, path| {
        if reader.binary(path)? != source::sha(b"fixture binary") {
            return Err(stale());
        }
        Ok(())
    },
    acquire: |_, path| {
        let repo = path.join("objects.git");
        fs::create_dir_all(&repo)?;
        fs::write(repo.join("HEAD"), "ref: refs/heads/main\n")?;
        Ok((
            repo,
            vec![(
                ".codex-plugin/plugin.json".into(),
                false,
                source::sha(FIXTURE_MANIFEST),
            )],
        ))
    },
    launch: fake_launch,
    outcome: fake_outcome,
};

fn clean_fixture() -> NativeOutcome {
    NativeOutcome::Exited {
        code: Some(0),
        signal: None,
    }
}

fn fake_outcome(journal: &Journal) -> Result<NativeOutcome> {
    let state = same_directory(&journal.state)?;
    let status: Option<String> = Store::read(state.file(), &name(journal, "fixture-exit"))?;
    let result = match status.as_deref() {
        Some("clean") => clean_fixture(),
        Some("running") => NativeOutcome::Running,
        Some("nonzero") => NativeOutcome::Exited {
            code: Some(1),
            signal: None,
        },
        _ => NativeOutcome::Unknown {
            reason: "fixture unobserved exit".into(),
        },
    };
    Ok(result)
}

fn fake_launch(
    store: &Store,
    journal: &mut Journal,
    argv: &FrozenInvocation,
) -> Result<NativeOutcome> {
    let state = same_directory(&journal.state)?;
    let input = frozen(journal, &state)?;
    guard_worker(journal, argv, &FIXTURE)?;
    if journal.home.join("no-gate").exists() {
        return Err(failed("fixture_no_gate"));
    }
    journal.phase = Phase::Running;
    journal.process_group = Some(42);
    store.save(journal)?;
    let p = &input.prepared;
    let installed = input.market_operation.is_some();
    let root = if installed { p.payload() } else { p.market() };
    fs::create_dir_all(root.join(".codex-plugin"))?;
    fs::write(root.join(".codex-plugin/plugin.json"), FIXTURE_MANIFEST)?;
    let config = format!(
        "[marketplaces.market]\nsource_type='git'\nsource='{}'\nref='{}'\n{}",
        p.offer.url()?,
        p.offer.marketplace_commit,
        if installed {
            "[plugins.'item@market']\nenabled = true\n"
        } else {
            ""
        }
    );
    fs::write(p.profile().join("config.toml"), config)?;
    let outcome = clean_fixture();
    Store::atomic_new(&store.root, &name(journal, "fixture-exit"), &"clean")?;
    if !journal.home.join("no-after").exists() {
        record_worker_after(journal, argv, &outcome, &FIXTURE)?;
    }
    Ok(outcome)
}

pub(super) fn fake_binary(ctx: &Context) -> Result<PathBuf> {
    let binary = ctx.home.join("codex");
    fs::write(&binary, b"fixture binary")?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    Ok(binary)
}

#[test]
fn installs_into_an_existing_codex_profile_with_original_preferences() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let profile = ctx.home.join(".codex");
    fs::create_dir(&profile)?;
    let settings = profile.join(CONFIG);
    fs::write(
        &settings,
        b"model = 'fixture-existing'\n[features]\nfast_mode = true\n",
    )?;
    let candidate = crate::agent::resource_agents::profile_roots(&ctx)?
        .into_iter()
        .find(|item| item.name == "codex:default")
        .ok_or("missing target")?;
    let target = crate::agent::resource_agents::InstallTarget::capture(&ctx, &candidate)?;
    crate::agent::resource_agents::enroll(&ctx, &target)?;
    let result = install_with_target(
        &ctx,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Disabled,
        Some(target),
    )?;
    assert_eq!(result["journal"]["phase"], "succeeded", "{result}");
    let updated = fs::read_to_string(&settings)?;
    assert!(updated.contains("fixture-existing"));
    assert!(updated.contains("fast_mode = true"));
    assert!(updated.contains("enabled = false"));
    assert!(
        profile
            .join("plugins/cache/market/item/1.0.0/.codex-plugin/plugin.json")
            .is_file()
    );
    assert!(
        profile
            .join(".tmp/marketplaces/market/.codex-plugin/plugin.json")
            .is_file()
    );
    assert_eq!(managed_profiles(&ctx)?, []);
    Ok(())
}

#[test]
fn existing_codex_target_continues_verified_market_and_never_replaces_an_already_installed_offer()
-> TestResult {
    for plugin_done in [false, true] {
        let (_temp, ctx) = fixture_context()?;
        let profile = ctx.home.join(".codex");
        fs::create_dir(&profile)?;
        fs::write(profile.join(CONFIG), "model = 'fixture-existing'\n")?;
        let candidate = crate::agent::resource_agents::profile_roots(&ctx)?
            .into_iter()
            .find(|item| item.name == "codex:default")
            .ok_or("missing target")?;
        let target = crate::agent::resource_agents::InstallTarget::capture(&ctx, &candidate)?;
        let store = Store::lock(&ctx)?;
        let offer = offer()?;
        let binary = fake_binary(&ctx)?;
        let mut prepared = prepare_source(
            &ctx,
            &store,
            offer.clone(),
            binary.clone(),
            &FIXTURE,
            InitialActivation::Enabled,
        )?;
        prepared.target = Some(target.clone());
        let checkout = prepared.market();
        let market = stage(
            &ctx,
            &store,
            &Frozen {
                prepared: prepared.clone(),
                market_operation: None,
            },
            &FIXTURE,
        )?;
        assert_eq!(market.phase, Phase::Succeeded);
        fs::create_dir(checkout.join(".git"))?;
        fs::write(checkout.join(".git/HEAD"), b"ref: refs/heads/main\n")?;
        assert!(crate::agent::plugin_target::recovery_matches(
            &market,
            Some(&target)
        )?);
        assert!(!crate::agent::plugin_target::recovery_matches(
            &market, None
        )?);
        if plugin_done {
            let installed = stage(
                &ctx,
                &store,
                &Frozen {
                    prepared,
                    market_operation: Some(market.id),
                },
                &FIXTURE,
            )?;
            assert_eq!(installed.phase, Phase::Succeeded);
        }
        drop(store);
        let run = |activation| {
            install_with_target(
                &ctx,
                offer.clone(),
                binary.clone(),
                &FIXTURE,
                activation,
                Some(target.clone()),
            )
        };
        assert!(run(InitialActivation::Disabled).is_err());
        assert_eq!(
            crate::operations::store::history(&ctx)?.len(),
            if plugin_done { 2 } else { 1 }
        );
        let first = run(InitialActivation::Enabled)?;
        assert_eq!(first["journal"]["phase"], "succeeded");
        assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);
        assert_eq!(
            fs::read(profile.join(".tmp/marketplaces/market/.git/HEAD"))?,
            b"ref: refs/heads/main\n"
        );
        assert_eq!(
            install_at(
                &ctx,
                offer.clone(),
                binary.clone(),
                InitialActivation::Enabled,
                target.clone(),
                |_| {},
            )?,
            first
        );
        assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);
        fs::write(
            profile.join("plugins/cache/market/item/1.0.0/.codex-plugin/plugin.json"),
            b"unexpected modification",
        )?;
        assert!(run(InitialActivation::Enabled).is_err());
        assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);
    }
    Ok(())
}

fn awaiting_publication(ctx: &Context, journal: &Journal) -> Result<bool> {
    let records = crate::operations::store::history(ctx)?;
    let (candidates, _) = awaiting_followup(ctx, &records)?;
    Ok(candidates.contains(&journal.id))
}

impl crate::test_support::PluginTargetFixture {
    pub(crate) fn unstarted_isolated_codex(
        activation: InitialActivation,
    ) -> Result<(tempfile::TempDir, Context, Offer)> {
        let (temporary, ctx) = fixture_context()?;
        let offer = offer()?;
        let store = Store::lock(&ctx)?;
        let prepared = prepare_source(
            &ctx,
            &store,
            offer.clone(),
            fake_binary(&ctx)?,
            &FIXTURE,
            activation,
        )?;
        let no_start = NativeIo {
            launch: |_, _, _| Err(stale()),
            ..FIXTURE
        };
        assert!(
            stage(
                &ctx,
                &store,
                &Frozen {
                    prepared,
                    market_operation: None
                },
                &no_start,
            )
            .is_err()
        );
        Ok((temporary, ctx, offer))
    }
}

#[test]
fn recovery_inventory_keeps_incomplete_codex_evidence_out_of_the_confirmable_list() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            market_operation: None,
        },
        &FIXTURE,
    )?;
    drop(store);
    let (pending, incomplete) = crate::operations::pending_recovery(&ctx)?;
    assert_eq!(
        pending.iter().map(|row| &row.id).collect::<Vec<_>>(),
        [&market.id]
    );
    assert!(!incomplete);
    fs::remove_file(ctx.state.join(name(&market, "plan")))?;
    let (pending, incomplete) = crate::operations::pending_recovery(&ctx)?;
    assert!(pending.is_empty() && incomplete);
    let (history, _) = crate::operations::recovery_history(&ctx)?;
    assert!(history.iter().any(|(entry, status)| {
        entry.id == market.id && *status == crate::operations::RecoveryState::Unverified
    }));
    let error = crate::operations::RecoveryPreview::read(&ctx, &market.id)
        .err()
        .ok_or("unverifiable stage must stay out of recovery")?;
    assert_eq!(error.code, "recovery_evidence_incomplete");
    Ok(())
}

#[test]
fn a_successor_completed_during_codex_resume_confirmation_is_never_replayed() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let offer = offer()?;
    let binary = fake_binary(&ctx)?;
    let prepared = prepare_source(
        &ctx,
        &store,
        offer.clone(),
        binary.clone(),
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            market_operation: None,
        },
        &FIXTURE,
    )?;
    drop(store);
    let chosen = resume_using(&ctx, &market, false, true, &FIXTURE, |_, _| {
        let result = install_using(&ctx, offer, binary, &FIXTURE, InitialActivation::Enabled)?;
        assert_eq!(result["journal"]["phase"], "succeeded");
        Ok(())
    });
    assert!(chosen.is_err());
    let history = crate::operations::store::history(&ctx)?;
    assert_eq!(history.len(), 3);
    assert_eq!(
        history
            .iter()
            .map(|entry| matches_prior(entry, &market.id))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|matches| *matches)
            .count(),
        1
    );
    Ok(())
}

#[test]
fn public_agent_resume_routes_to_codex_and_cannot_bypass_binary_verification() -> TestResult {
    use clap::Parser as _;
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            market_operation: None,
        },
        &FIXTURE,
    )?;
    drop(store);
    let cli =
        crate::cli::Cli::try_parse_from(["macops", "agent", "resume", &market.id, "--dry-run"])?;
    let crate::cli::Command::Agent {
        command: Some(command),
        ..
    } = &cli.command
    else {
        return Err("fixture did not parse".into());
    };
    let error = crate::run_agent_entry(&cli, &ctx, command)
        .err()
        .ok_or("fake native binary is not authorized")?;
    assert_eq!(error.code, "codex_install_binary_unverified");
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);
    Ok(())
}

#[test]
fn isolated_market_retry_resumes_the_frozen_install_at_most_once() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let offer = offer()?;
    let prepared = prepare_source(
        &ctx,
        &store,
        offer.clone(),
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            market_operation: None,
        },
        &FIXTURE,
    )?;
    drop(store);
    assert_eq!(
        matching_activation(&market, &offer, None)?,
        Some(InitialActivation::Enabled)
    );
    let store = Store::lock(&ctx)?;
    let mut checked = market.clone();
    let status = recover_using(&ctx, &store, &mut checked, false, false, &FIXTURE)?;
    assert_eq!(status["phase"], "succeeded");
    assert_eq!(
        status["continuationCommand"],
        format!("mo agent resume {}", market.id)
    );
    drop(store);

    let preview = resume_using(&ctx, &market, true, false, &FIXTURE, |_, _| {
        panic!("read-only preview")
    })?;
    assert_eq!(preview["persistentWrites"], false);
    assert_eq!(preview["enabled"], true);
    let cancelled = resume_using(&ctx, &market, false, false, &FIXTURE, |_, _| {
        Err(Error::attention("cancelled", "fixture"))
    });
    assert_eq!(cancelled.err().ok_or("refused")?.code, "cancelled");
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);

    let binary = fake_binary(&ctx)?;
    let run = |mode| install_using(&ctx, offer.clone(), binary.clone(), &FIXTURE, mode);
    assert_eq!(
        run(InitialActivation::Disabled)
            .err()
            .ok_or("original choice")?
            .code,
        "plugin_install_activation_changed"
    );
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);
    let result = run(InitialActivation::Enabled)?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    let receipt = result["journal"]["id"]
        .as_str()
        .ok_or("published receipt")?;
    assert!(receipt.ends_with("-receipt"));
    assert_ne!(receipt, format!("{}-receipt", market.id));
    assert!(result["codexHome"].is_string());
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);
    assert!(!awaiting_publication(&ctx, &market)?);
    assert!(resume_using(&ctx, &market, false, true, &FIXTURE, |_, _| Ok(())).is_err());
    let store = Store::lock(&ctx)?;
    let status = recover_using(&ctx, &store, &mut checked, false, false, &FIXTURE)?;
    assert_eq!(status["continuationRequired"], false);
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);

    Ok(())
}

#[test]
fn explicit_codex_continuation_publishes_the_pinned_offer() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Disabled,
    )?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            market_operation: None,
        },
        &FIXTURE,
    )?;
    drop(store);
    let result = resume_using(&ctx, &market, false, true, &FIXTURE, |summary, yes| {
        assert!(yes);
        assert!(summary.contains("保持停用"));
        Ok(())
    })?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    let native = crate::operations::store::history(&ctx)?
        .into_iter()
        .find(|journal| journal.id == market.id)
        .ok_or("original market")?;
    assert!(!awaiting_publication(&ctx, &native)?);
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);
    let mut another_scope = ctx.clone();
    another_scope.cache = ctx.home.join("another-cache");
    assert_eq!(
        resume_using(
            &another_scope,
            &market,
            false,
            true,
            &FIXTURE,
            |_, _| panic!("wrong scope")
        )
        .err()
        .ok_or("scope difference")?
        .code,
        "recovery_scope_changed"
    );
    let separate = install_using(
        &another_scope,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    assert_ne!(separate["codexHome"], result["codexHome"]);
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 6);

    Ok(())
}

#[test]
fn codex_resume_rechecks_the_frozen_destination_after_confirmation() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let target_path = ctx.home.join(".codex");
    fs::create_dir(&target_path)?;
    let row = crate::agent::resource_agents::profile_roots(&ctx)?
        .into_iter()
        .find(|row| row.name == "codex:default")
        .ok_or("existing config")?;
    let target = crate::agent::resource_agents::InstallTarget::capture(&ctx, &row)?;
    let store = Store::lock(&ctx)?;
    let mut prepared = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    prepared.target = Some(target);
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared,
            market_operation: None,
        },
        &FIXTURE,
    )?;
    drop(store);
    let result = resume_using(&ctx, &market, false, false, &FIXTURE, |message, _| {
        assert!(message.contains(&target_path.display().to_string()));
        fs::rename(&target_path, ctx.home.join("original-codex"))?;
        fs::create_dir(&target_path)?;
        Ok(())
    });
    assert_eq!(
        result.err().ok_or("rebound directory")?.code,
        "agent_install_target_changed"
    );
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);
    assert!(!target_path.join("plugins").exists());
    Ok(())
}

#[test]
fn fixture_first_install_publishes_one_discoverable_receipt_and_recovers_publication_windows()
-> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let installed = install_using(
        &ctx,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    assert_eq!(installed["journal"]["phase"], "succeeded");
    let id = installed["journal"]["id"].as_str().ok_or("adoption id")?;
    let native_id = id.strip_suffix("-receipt").ok_or("native id")?;
    let roots = managed_profiles(&ctx)?;
    assert_eq!(roots.len(), 1);
    assert_eq!(
        roots[0].1,
        Path::new(installed["codexHome"].as_str().ok_or("home")?)
    );
    let store = Store::lock(&ctx)?;
    let state = same_directory(&store.path)?;
    let terminal = read_operation(&state, id)?.ok_or("terminal adoption")?;
    let mut native = read_operation(&state, native_id)?.ok_or("install terminal")?;
    let plan = frozen(&native, &state)?;
    let market = read_operation(&state, plan.market_operation.as_deref().ok_or("market")?)?
        .ok_or("market journal")?;
    assert!(!awaiting_publication(&ctx, &market)?);
    assert!(!awaiting_publication(&ctx, &native)?);
    let initial_receipt = fs::read(ctx.state.join("receipts.json"))?;
    assert_eq!(
        recover_using(&ctx, &store, &mut native, false, false, &FIXTURE)?["journal"]["id"],
        id
    );
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, initial_receipt);
    assert!(require_ordinary_adoption(&terminal.plan.package).is_err());

    let mut foreign = terminal.clone();
    foreign.id = "OP-foreign".into();
    foreign.phase = Phase::Prepared;
    Store::atomic(&store.root, "active.json", &foreign)?;
    let before = fs::read(ctx.state.join("active.json"))?;
    let error = recover_using(&ctx, &store, &mut native, false, false, &FIXTURE)
        .err()
        .ok_or("foreign active operation")?;
    assert_eq!(error.code, "recovery_required");
    assert_eq!(fs::read(ctx.state.join("active.json"))?, before);
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, initial_receipt);

    let prepared: Journal =
        Store::read(&store.root, &name(&native, "adoption"))?.ok_or("frozen adoption")?;
    Store::atomic(&store.root, "active.json", &prepared)?;
    assert!(recover_publication_using(&ctx, &store, &prepared, true, &FIXTURE).is_err());
    let restored = recover_publication_using(&ctx, &store, &prepared, false, &FIXTURE)?;
    assert_eq!(restored["journal"]["id"], id);
    let active: Journal = Store::read(&store.root, "active.json")?.ok_or("active repaired")?;
    assert_eq!(active.phase, Phase::Succeeded);
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, initial_receipt);

    fs::remove_file(ctx.state.join("operations").join(format!("{id}.json")))?;
    fs::remove_file(ctx.state.join("receipts.json"))?;
    Store::atomic(&store.root, "active.json", &native)?;
    assert_eq!(managed_profiles(&ctx)?, []);
    assert!(awaiting_publication(&ctx, &native)?);
    let active_before = fs::read(ctx.state.join("active.json"))?;
    let (pending, incomplete) = crate::operations::pending_recovery(&ctx)?;
    assert!(!incomplete);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, native_id);
    assert_eq!(pending[0].phase, Phase::Succeeded);
    assert_eq!(fs::read(ctx.state.join("active.json"))?, active_before);
    assert!(!ctx.state.join("receipts.json").exists());
    let preview = crate::operations::RecoveryPreview::read(&ctx, native_id)?;
    assert_eq!(preview.journal.id, native_id);
    let restored = recover_using(&ctx, &store, &mut native, false, false, &FIXTURE)?;
    assert_eq!(restored["journal"]["started_at"], terminal.started_at);
    assert_eq!(restored["journal"]["id"], id);
    let mut altered = invocation(&plan, native.plan.timeout_secs)?;
    altered.command.args.push("--unapproved".into());
    assert!(guard_worker(&native, &altered, &FIXTURE).is_err());
    let rebuilt_receipt = fs::read(ctx.state.join("receipts.json"))?;
    drop(store);
    let error = preview.confirm(&ctx).err().ok_or("already published")?;
    assert_eq!(error.code, "operation_finished");
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, rebuilt_receipt);
    Ok(())
}

#[test]
fn missing_original_after_and_prepared_without_gate_never_dispatch_another_stage() -> TestResult {
    for (marker, expected) in [("no-after", Phase::Unknown), ("no-gate", Phase::Failed)] {
        let (_temp, ctx) = fixture_context()?;
        fs::write(ctx.home.join(marker), "")?;
        let result = install_using(
            &ctx,
            offer()?,
            fake_binary(&ctx)?,
            &FIXTURE,
            InitialActivation::Enabled,
        );
        assert_eq!(result.is_err(), marker == "no-gate");
        let store = Store::lock(&ctx)?;
        let mut journal: Journal =
            Store::read(&store.root, "active.json")?.ok_or("blocked phase")?;
        let recovered = recover_using(&ctx, &store, &mut journal, false, false, &FIXTURE)?;
        assert_eq!(recovered["phase"], serde_json::to_value(expected)?);
        assert_eq!(managed_profiles(&ctx)?, []);
        assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);
    }
    Ok(())
}

#[test]
fn frozen_private_scope_refuses_moved_directories_mirror_mutation_and_credentials() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let p = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    p.guard(&store.path, &FIXTURE)?;
    assert!(p.guard(&ctx.home, &FIXTURE).is_err());
    let mut malformed = p.clone();
    malformed.offer.payload_commit = "unverified".into();
    assert!(malformed.guard(&store.path, &FIXTURE).is_err());
    let git = p.repository();
    let moved_git = p.root.join("moved-mirror");
    fs::rename(&git, &moved_git)?;
    assert!(p.guard(&store.path, &FIXTURE).is_err());
    fs::rename(&moved_git, &git)?;

    let rewrite = p.root.join("gitconfig");
    fs::write(&rewrite, "unexpected rewrite")?;
    let error = p
        .guard(&store.path, &FIXTURE)
        .err()
        .ok_or("rewrite mismatch")?;
    assert_eq!(error.code, "codex_install_mirror_changed");
    fs::write(&rewrite, p.git_config()?)?;

    let auth = p.profile().join("auth.json");
    fs::write(&auth, b"synthetic")?;
    let error = p.guard(&store.path, &FIXTURE).err().ok_or("credentials")?;
    assert_eq!(error.code, "codex_install_profile_in_use");
    fs::remove_file(auth)?;

    let cwd = p.root.join("cwd");
    fs::rename(&cwd, p.root.join("moved-cwd"))?;
    fs::create_dir(&cwd)?;
    let error = p.guard(&store.path, &FIXTURE).err().ok_or("scope rebind")?;
    assert_eq!(error.code, "codex_install_private_root_changed");
    fs::remove_dir(&cwd)?;
    fs::rename(p.root.join("moved-cwd"), &cwd)?;

    fs::set_permissions(&p.root, fs::Permissions::from_mode(0o755))?;
    assert!(p.guard(&store.path, &FIXTURE).is_err());
    fs::set_permissions(&p.root, fs::Permissions::from_mode(0o700))?;
    fs::write(&p.binary, "changed binary")?;
    assert!(p.guard(&store.path, &FIXTURE).is_err());
    Ok(())
}

#[test]
fn running_and_unproven_exit_block_resolution_known_exit_can_be_closed() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    fs::write(ctx.home.join("no-after"), "")?;
    install_using(
        &ctx,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let store = Store::lock(&ctx)?;
    let mut journal: Journal = Store::read(&store.root, "active.json")?.ok_or("active")?;
    let exit = name(&journal, "fixture-exit");
    for (outcome, reason) in [
        ("running", "operation_in_flight"),
        ("missing", "codex_install_native_exit_unverified"),
    ] {
        Store::atomic(&store.root, &exit, &outcome)?;
        let err = recover_using(&ctx, &store, &mut journal, true, true, &FIXTURE)
            .err()
            .ok_or("must refuse unresolved exit")?;
        assert_eq!(err.code, reason);
        assert_eq!(journal.phase, Phase::Unknown);
    }
    Store::atomic(&store.root, &exit, &"nonzero")?;
    let closed = recover_using(&ctx, &store, &mut journal, true, true, &FIXTURE)?;
    assert_eq!(closed["phase"], "failed");
    assert_eq!(managed_profiles(&ctx)?, []);
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);
    Ok(())
}

#[test]
fn the_plugin_gate_requires_the_original_market_exit_and_matching_projection() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let prepared = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let state = same_directory(&store.path)?;
    let first = Frozen {
        prepared: prepared.clone(),
        market_operation: None,
    };
    let market = stage(&ctx, &store, &first, &FIXTURE)?;
    assert_eq!(market.phase, Phase::Succeeded);
    let second = Frozen {
        prepared: prepared.clone(),
        market_operation: Some(market.id.clone()),
    };
    stage_before(&second, &state, &market, &FIXTURE)?;

    let after = name(&market, "after");
    let original: After = Store::read(&store.root, &after)?.ok_or("market after evidence")?;
    let mut altered = After {
        worker: Some(777),
        ..original
    };
    Store::atomic(&store.root, &after, &altered)?;
    assert!(stage_before(&second, &state, &market, &FIXTURE).is_err());
    altered.worker = market.process_group;
    Store::atomic(&store.root, &after, &altered)?;

    Store::atomic(&store.root, &name(&market, "fixture-exit"), &"nonzero")?;
    assert!(stage_before(&second, &state, &market, &FIXTURE).is_err());
    Store::atomic(&store.root, &name(&market, "fixture-exit"), &"clean")?;

    let config = prepared.profile().join("config.toml");
    let bytes = fs::read(&config)?;
    fs::write(&config, [bytes.as_slice(), b"unapproved=true\n"].concat())?;
    assert!(stage_before(&second, &state, &market, &FIXTURE).is_err());
    fs::write(&config, bytes)?;

    fs::write(prepared.market().join("target-replacement"), "altered")?;
    assert!(stage_before(&second, &state, &market, &FIXTURE).is_err());
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 1);
    assert_eq!(managed_profiles(&ctx)?, []);
    Ok(())
}

#[test]
fn incompatible_originating_receipt_and_altered_scope_cannot_be_overwritten() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let installed = install_using(
        &ctx,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let id = installed["journal"]["id"].as_str().ok_or("adopt id")?;
    let native_id = id.strip_suffix("-receipt").ok_or("native id")?;
    let store = Store::lock(&ctx)?;
    let state = same_directory(&store.path)?;
    let mut native = read_operation(&state, native_id)?.ok_or("native journal")?;
    let mut other = ctx.clone();
    other.scope.home = ctx.home.join("different");
    let error = recover_using(&other, &store, &mut native, false, false, &FIXTURE)
        .err()
        .ok_or("forged scope")?;
    assert_eq!(error.code, "codex_install_evidence_changed");
    let mut receipts = store.receipts()?;
    let receipt = receipts
        .installations
        .values_mut()
        .next()
        .ok_or("receipt")?;
    receipt.operation_id = "OP-foreign".into();
    Store::atomic(&store.root, "receipts.json", &receipts)?;
    let bytes = fs::read(ctx.state.join("receipts.json"))?;
    let error = recover_using(&ctx, &store, &mut native, false, false, &FIXTURE)
        .err()
        .ok_or("foreign receipt conflict")?;
    assert_eq!(error.code, "commit_conflict");
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, bytes);
    assert_eq!(managed_profiles(&ctx)?, []);
    Ok(())
}

#[test]
fn malformed_native_configuration_and_extra_registrations_cannot_settle_installation() -> TestResult
{
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let p = prepared(temp.path())?;
    let config = p.profile().join("config.toml");
    assert!(projected(&p, false).is_err());
    let good = format!(
        "[marketplaces.market]\nsource_type='git'\nsource='{}'\nref='{}'\n",
        p.offer.url()?,
        p.offer.marketplace_commit
    );
    fs::write(&config, &good)?;
    assert!(projected(&p, false).is_err());
    fs::create_dir_all(p.market().join(".codex-plugin"))?;
    fs::write(
        p.market().join(".codex-plugin/plugin.json"),
        FIXTURE_MANIFEST,
    )?;
    fs::write(
        &config,
        format!("{good}[plugins.'item@market']\nenabled = true\n"),
    )?;
    assert!(projected(&p, true).is_err());
    fs::write(
        &config,
        format!("{good}[plugins.'item@market']\nenabled = 'maybe'\n"),
    )?;
    assert!(projected(&p, true).is_err());
    for malformed in [
        b"\xff".as_slice(),
        b"[[missing",
        b"unrecognized=true\n",
        b"marketplaces=[]",
        b"[marketplaces.foreign]\nsource_type='git'\n",
    ] {
        fs::write(&config, malformed)?;
        assert!(projected(&p, false).is_err());
    }
    for cache in [p.market(), p.payload()] {
        fs::create_dir_all(cache.join(".codex-plugin"))?;
        fs::write(cache.join(".codex-plugin/plugin.json"), FIXTURE_MANIFEST)?;
    }
    for (value, installed) in [
        (
            good.replace("source_type='git'", "source_type='local'"),
            false,
        ),
        (
            good.replace(&p.offer.marketplace_commit, &"b".repeat(40)),
            false,
        ),
        (format!("{good}unexpected=true\n"), false),
        (
            format!("{good}[marketplaces.foreign]\nsource_type='git'\n"),
            false,
        ),
        (
            format!("{good}[plugins.'foreign@market']\nenabled=true\n"),
            true,
        ),
        (
            format!("{good}[plugins.'item@market']\nenabled=false\n"),
            true,
        ),
        (
            format!("{good}[plugins.'item@market']\nenabled=true\nunexpected=true\n"),
            true,
        ),
        (
            format!(
                "{good}[plugins.'item@market']\nenabled=true\n[plugins.'other@market']\nenabled=false\n"
            ),
            true,
        ),
    ] {
        fs::write(&config, value)?;
        assert!(projected(&p, installed).is_err());
    }
    Ok(())
}

#[test]
fn malformed_publication_reservation_or_terminal_without_receipt_is_not_rebuilt() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let installed = install_using(
        &ctx,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let id = installed["journal"]["id"].as_str().ok_or("adopt id")?;
    let native_id = id.strip_suffix("-receipt").ok_or("native id")?;
    let store = Store::lock(&ctx)?;
    let state = same_directory(&store.path)?;
    let mut native = read_operation(&state, native_id)?.ok_or("native")?;
    let reserve = name(&native, "adoption");
    let saved: Journal = Store::read(&store.root, &reserve)?.ok_or("reserved adoption")?;
    let history = ctx.state.join("operations").join(format!("{id}.json"));
    let history_bytes = fs::read(&history)?;
    let receipt = ctx.state.join("receipts.json");
    let preserved = fs::read(&receipt)?;
    let failure = |journal: &mut Journal| {
        recover_using(&ctx, &store, journal, false, false, &FIXTURE)
            .err()
            .map(|err| err.code)
    };
    fs::remove_file(ctx.state.join(&reserve))?;
    assert_eq!(
        failure(&mut native).as_deref(),
        Some("codex_install_evidence_changed")
    );
    let mut bad = saved.clone();
    bad.id = "OP-other".into();
    Store::atomic_new(&store.root, &reserve, &bad)?;
    assert_eq!(
        failure(&mut native).as_deref(),
        Some("codex_install_evidence_changed")
    );
    Store::atomic(&store.root, &reserve, &saved)?;
    let mut wrong_history = read_operation(&state, id)?.ok_or("terminal history")?;
    wrong_history.started_at += 1;
    fs::write(&history, serde_json::to_vec(&wrong_history)?)?;
    assert_eq!(
        failure(&mut native).as_deref(),
        Some("codex_install_evidence_changed")
    );
    fs::write(&history, &history_bytes)?;
    assert_eq!(fs::read(&receipt)?, preserved);
    fs::remove_file(&receipt)?;
    assert_eq!(
        failure(&mut native).as_deref(),
        Some("codex_install_evidence_changed")
    );
    assert!(!receipt.exists());
    Ok(())
}

#[test]
fn uncertified_binary_and_acquisition_failure_cannot_leave_a_dispatchable_plan() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let executable = fake_binary(&ctx)?;
    let mut reader = Passive::new()?;
    let rejection = (NATIVE.verify_binary)(&mut reader, &executable)
        .err()
        .ok_or("uncertified")?;
    assert_eq!(rejection.code, "codex_install_binary_unverified");
    let blocked = NativeIo {
        acquire: |_, _| Err(failed("fixture_source_unavailable")),
        ..FIXTURE
    };
    let error = install_using(
        &ctx,
        offer()?,
        executable,
        &blocked,
        InitialActivation::Enabled,
    )
    .err()
    .ok_or("proof unavailable")?;
    assert_eq!(error.code, "fixture_source_unavailable");
    assert!(!ctx.state.join("active.json").exists());
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 0);
    assert_eq!(managed_profiles(&ctx)?, []);
    assert!(
        !fs::read_dir(&ctx.state)?
            .filter_map(std::result::Result::ok)
            .any(|entry| reserved_name(&entry.path()))
    );
    Ok(())
}

#[test]
fn native_lookup_never_approves_an_unverified_binary() -> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let candidate = ctx
        .home
        .join(".local/share/mise/installs/aqua-openai-codex/0.170.0/bin/codex");
    fs::create_dir_all(candidate.parent().ok_or("mise layout")?)?;
    fs::write(&candidate, b"not a certified executable")?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
    match binary(&ctx) {
        Ok(path) => {
            assert_ne!(path, candidate);
            assert_eq!(Passive::new()?.binary(&path)?, BINARY_SHA);
        }
        Err(error) => assert_eq!(error.code, "codex_install_binary_unverified"),
    }
    Ok(())
}

#[test]
fn unlinked_market_or_nonzero_worker_exit_cannot_create_authoritative_after_evidence() -> TestResult
{
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let p = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Enabled,
    )?;
    let state = same_directory(&store.path)?;
    let input = Frozen {
        prepared: p.clone(),
        market_operation: None,
    };
    let market = stage(&ctx, &store, &input, &FIXTURE)?;
    let argv = invocation(&input, market.plan.timeout_secs)?;
    let before = fs::read(ctx.state.join(name(&market, "after")))?;
    let mut wrong_argv = argv.clone();
    wrong_argv.command.args.push("--unexpected".into());
    assert!(record_worker_after(&market, &wrong_argv, &clean_fixture(), &FIXTURE).is_err());
    assert!(stage_before(&input, &state, &market, &FIXTURE).is_err());
    let rejected = record_worker_after(
        &market,
        &argv,
        &NativeOutcome::Exited {
            code: Some(1),
            signal: None,
        },
        &FIXTURE,
    );
    assert!(rejected.is_err());
    assert_eq!(fs::read(ctx.state.join(name(&market, "after")))?, before);
    let missing = Frozen {
        prepared: p.clone(),
        market_operation: Some("OP-missing".into()),
    };
    assert!(stage_before(&missing, &state, &market, &FIXTURE).is_err());
    let invalid = Frozen {
        prepared: p.clone(),
        market_operation: Some("../untrusted".into()),
    };
    assert!(stage_before(&invalid, &state, &market, &FIXTURE).is_err());
    let second = Frozen {
        prepared: p,
        market_operation: Some(market.id.clone()),
    };
    let mut changed_scope = second.clone();
    changed_scope.prepared.files.clear();
    assert!(stage_before(&changed_scope, &state, &market, &FIXTURE).is_err());
    let mut unconfirmed = market.clone();
    unconfirmed.phase = Phase::Unknown;
    let record = ctx
        .state
        .join("operations")
        .join(format!("{}.json", market.id));
    fs::write(&record, serde_json::to_vec(&unconfirmed)?)?;
    assert!(stage_before(&second, &state, &market, &FIXTURE).is_err());
    let mut unlinked = market.clone();
    unlinked.id = "OP-unlinked".into();
    assert!(guard_worker(&unlinked, &argv, &FIXTURE).is_err());
    assert!(recover_publication_using(&ctx, &store, &market, false, &FIXTURE).is_err());
    assert_eq!(managed_profiles(&ctx)?, []);
    Ok(())
}

#[test]
fn staged_activation_edits_only_the_exact_toml_value() -> TestResult {
    let input = b"# fixture\r\n[plugins.'item@market']\r\nenabled  =  true  # owner choice\r\n";
    let (enabled, disabled) = set_enabled(input, "item@market", false)?;
    assert!(enabled);
    assert_eq!(
        disabled,
        b"# fixture\r\n[plugins.'item@market']\r\nenabled  =  false  # owner choice\r\n"
    );
    let (enabled, restored) = set_enabled(&disabled, "item@market", true)?;
    assert!(!enabled);
    assert_eq!(restored, input);
    for invalid in [
        b"\xff".as_slice(),
        b"[[broken",
        b"[plugins.'item@market']\nenabled = 'true'\n",
        b"[plugins.'other@market']\nenabled = true\n",
    ] {
        assert!(set_enabled(invalid, "item@market", false).is_err());
    }
    Ok(())
}

#[test]
fn default_activation_keeps_the_previous_frozen_serialization() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let original = Frozen {
        prepared: prepared(temp.path())?,
        market_operation: None,
    };
    let bytes = serde_json::to_vec(&original)?;
    let data: Value = serde_json::from_slice(&bytes)?;
    assert!(data["prepared"].get("initially_disabled").is_none());
    let recovered: Frozen = serde_json::from_slice(&bytes)?;
    assert!(!recovered.prepared.initially_disabled);
    assert_eq!(serde_json::to_vec(&recovered)?, bytes);
    assert_eq!(fingerprint(&original)?, fingerprint(&recovered)?);
    Ok(())
}

#[test]
fn disabled_activation_recovers_exact_derived_image_without_repeat_or_post_publication_write()
-> TestResult {
    let (_temp, ctx) = fixture_context()?;
    let store = Store::lock(&ctx)?;
    let p = prepare_source(
        &ctx,
        &store,
        offer()?,
        fake_binary(&ctx)?,
        &FIXTURE,
        InitialActivation::Disabled,
    )?;
    let state = same_directory(&store.path)?;
    let market = stage(
        &ctx,
        &store,
        &Frozen {
            prepared: p.clone(),
            market_operation: None,
        },
        &FIXTURE,
    )?;
    let input = Frozen {
        prepared: p,
        market_operation: Some(market.id),
    };
    let mut native = stage(&ctx, &store, &input, &FIXTURE)?;
    assert_eq!(native.phase, Phase::Succeeded);
    let profile = same_directory(&input.prepared.profile())?;
    let leaf = OsStr::new(CONFIG);
    let config = profile.path.join(CONFIG);
    let (enabled_bytes, original) = profile.regular_config_snapshot(leaf, 64 * 1024)?;
    assert!(verify_activation(&native, &input, &state, &FIXTURE).is_err());
    let changed = [b"# Foreign edit\n".as_slice(), &enabled_bytes].concat();
    fs::write(&config, &changed)?;
    assert!(prepare_activation(&store, &state, &native, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&config)?, changed);
    fs::write(&config, &enabled_bytes)?;
    let proof_path = ctx.state.join(name(&native, "after"));
    let proof = fs::read(&proof_path)?;
    let mut forged: After = serde_json::from_slice(&proof)?;
    forged.worker = Some(777);
    fs::write(&proof_path, serde_json::to_vec(&forged)?)?;
    assert!(prepare_activation(&store, &state, &native, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&config)?, enabled_bytes);
    fs::write(&proof_path, proof)?;

    let reservation = name(&native, "adoption");
    Store::atomic_new(&store.root, &reservation, &native)?;
    assert!(prepare_activation(&store, &state, &native, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&config)?, enabled_bytes);
    fs::remove_file(ctx.state.join(reservation))?;

    let mut active = native.clone();
    active.id = format!("{}-receipt", native.id);
    active.phase = Phase::Prepared;
    Store::atomic(&store.root, "active.json", &active)?;
    assert!(prepare_activation(&store, &state, &native, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&config)?, enabled_bytes);
    Store::atomic(&store.root, "active.json", &native)?;

    prepare_activation(&store, &state, &native, &input, &FIXTURE)?;
    let (disabled, replaced) = profile.regular_config_snapshot(leaf, 64 * 1024)?;
    assert_ne!(original.identity, replaced.identity);
    assert_eq!(replaced.mode, original.mode);
    assert_eq!(
        disabled,
        set_enabled(&enabled_bytes, "item@market", false)?.1
    );
    assert_eq!(managed_profiles(&ctx)?, []);
    prepare_activation(&store, &state, &native, &input, &FIXTURE)?;
    assert_eq!(
        profile.regular_config_snapshot(leaf, 64 * 1024)?.1.identity,
        replaced.identity
    );

    let foreign = input.prepared.payload().join("unexpected-payload");
    fs::write(&foreign, b"modified")?;
    assert!(recover_using(&ctx, &store, &mut native, false, false, &FIXTURE).is_err());
    fs::remove_file(foreign)?;
    let recovered = recover_using(&ctx, &store, &mut native, false, false, &FIXTURE)?;
    assert_eq!(recovered["journal"]["phase"], "succeeded");
    assert_eq!(
        recovered["journal"]["plan"]["before"]["pack"]["configured_enabled"],
        false
    );
    assert_eq!(managed_profiles(&ctx)?.len(), 1);
    let receipts = fs::read(ctx.state.join("receipts.json"))?;
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 3);
    assert_eq!(
        profile.regular_config_snapshot(leaf, 64 * 1024)?.1.identity,
        replaced.identity
    );

    fs::write(&config, &enabled_bytes)?;
    assert!(recover_using(&ctx, &store, &mut native, false, false, &FIXTURE).is_err());
    fs::remove_file(ctx.state.join(name(&native, "adoption")))?;
    fs::remove_file(
        ctx.state
            .join("operations")
            .join(format!("{}-receipt.json", native.id)),
    )?;
    Store::atomic(&store.root, "active.json", &native)?;
    assert!(prepare_activation(&store, &state, &native, &input, &FIXTURE).is_err());
    assert_eq!(fs::read(&config)?, enabled_bytes);
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, receipts);
    Ok(())
}
