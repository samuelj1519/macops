use super::*;

#[test]
fn advisory_recommendation_with_documented_model_suggestions_does_not_choose_a_target()
-> crate::test_support::TestResult {
    use clap::Parser as _;
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let args = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        temporary.path().to_str().ok_or("fixture home")?,
        "recommend",
        "fixture",
    ])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    let advice = json!({
        "query":"fixture",
        "reason":"native approval missing",
        "documents":[{"path":"README.md","url":"https://example.org/fixture"}],
        "model":{"status":"ok","candidates":[{
            "name":"suggested-plugin", "kind":"plugin", "adapter":"claude",
            "scope":"user", "summary":"advisory suggestion", "document":0
        }]},
    });
    assert_eq!(choose(&ctx, advice.clone())?, advice);
    let unavailable = json!({"query":"fixture","model":{"status":"keychain-unavailable"}});
    assert_eq!(choose(&ctx, unavailable.clone())?, unavailable);
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn retrying_the_same_profile_reuses_the_frozen_pending_publication()
-> crate::test_support::TestResult {
    let commit = "a".repeat(40);
    let codex = crate::codex_install_source::Offer::parse(&json!({
        "repository":"example/fixture","marketplace":"fixture-market","plugin":"item","declaredVersion":"1.0",
        "marketplaceCommit":commit,"payloadCommit":commit,"payloadRef":"main",
        "marketplaceSha256":"a".repeat(64),"pluginManifestSha256":"b".repeat(64)
    }))?;
    let claude = crate::claude_install_source::Offer::identify(
        "example/fixture",
        &crate::policy::source_git::ObjectId::parse(&commit)?,
        br#"{"name":"fixture-market","plugins":[{"name":"item","source":"./"}]}"#,
        br#"{"name":"item","version":"1.0"}"#,
    )?;
    let offer = crate::domain::fingerprint(&claude)?;
    confirm(
        "claude",
        &offer,
        &InstallChoice::Claude(Box::new(claude), "/fixture".into()),
    )?;
    let offer = crate::domain::fingerprint(&codex)?;
    confirm(
        "codex",
        &offer,
        &InstallChoice::Codex(Box::new(codex), "/fixture".into()),
    )
}

fn confirm(name: &str, offer: &str, choice: &InstallChoice) -> crate::test_support::TestResult {
    for identical in [true, false] {
        let prior = if identical {
            offer.to_owned()
        } else {
            "another offer".into()
        };
        let (_temporary, ctx, target, id) =
            crate::test_support::PluginTargetFixture::pending_for_recommendation(name, prior)?;
        let select = |_: &str| Ok(vec![Destination::Existing(existing(&target, true))]);
        let cancelled = install_choice_using(
            &ctx,
            choice,
            select,
            || Err(Error::attention("cancelled", "fixture")),
            |_, _| panic!("cancel must not write"),
        );
        assert_eq!(cancelled.err().ok_or("must cancel")?.code, "cancelled");
        assert_eq!(
            crate::store::active(&ctx)?.ok_or("active journal")?.phase,
            crate::store::Phase::Prepared
        );
        let mut alternate = ctx.clone();
        alternate.cache = ctx.home.join("different-cache");
        let blocked = install_choice_using(
            &alternate,
            choice,
            select,
            || Ok(InitialActivation::Enabled),
            |_, _| panic!("scope mismatch must stop before installing"),
        )
        .err()
        .ok_or("the old scope must be checked first")?;
        assert_eq!(blocked.code, "recovery_required");
        assert_eq!(
            crate::store::active(&ctx)?.ok_or("active journal")?.phase,
            crate::store::Phase::Prepared
        );
        let result = install_choice_using(
            &ctx,
            choice,
            |kind| {
                assert_eq!(kind, name);
                select(kind)
            },
            || Ok(InitialActivation::Enabled),
            |activation, selected| {
                assert_eq!(selected, target);
                if identical {
                    return install_at_profile(&ctx, choice, activation, selected);
                }
                let store = crate::store::Store::lock(&ctx)?;
                store.check_pending()?;
                let previous = crate::plugin_target::previous(&ctx, &store, &selected, offer)?;
                assert!(previous.is_none());
                let mut result = installed_fixture(&selected);
                result["fixtureNewRequest"] = true.into();
                Ok(result)
            },
        )?;
        if identical {
            assert_eq!(result["journal"]["id"], id);
            assert_eq!(result["targetName"], target.name);
        } else {
            assert_eq!(result["fixtureNewRequest"], true);
        }
        let history = crate::store::history(&ctx)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].phase, crate::store::Phase::Succeeded);
    }
    Ok(())
}

#[test]
fn one_install_confirmation_enrolls_only_the_confirmed_profile() -> crate::test_support::TestResult
{
    use clap::Parser as _;
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let home = temporary.path();
    std::fs::create_dir(home.join(".claude-mi"))?;
    std::fs::create_dir(home.join(".codex"))?;
    let args =
        crate::cli::Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    let commit = "a".repeat(40);
    let codex = crate::codex_install_source::Offer::parse(&json!({
        "repository":"example/fixture","marketplace":"market","plugin":"item","declaredVersion":"1.0.0",
        "marketplaceCommit":commit,"payloadCommit":commit,"payloadRef":"main",
        "marketplaceSha256":"a".repeat(64),"pluginManifestSha256":"b".repeat(64)
    }))?;
    let claude = crate::claude_install_source::Offer::identify(
        "example/fixture",
        &crate::policy::source_git::ObjectId::parse(&commit)?,
        br#"{"name":"market","plugins":[{"name":"item","source":"./"}]}"#,
        br#"{"name":"item","version":"1.0.0"}"#,
    )?;
    let binary = home.join("unused");
    assert_compatible_plugin_offers(&ctx, &binary, &codex, &claude)?;
    let cancelled = || Error::attention("cancelled", "fixture");
    for (name, choice) in [
        (
            "claude:mi",
            InstallChoice::Claude(Box::new(claude), binary.clone()),
        ),
        (
            "codex:default",
            InstallChoice::Codex(Box::new(codex), binary),
        ),
    ] {
        let profile = crate::resource_agents::profile_roots(&ctx)?
            .into_iter()
            .find(|item| item.name == name)
            .ok_or("profile")?;
        let target = crate::resource_agents::InstallTarget::capture(&ctx, &profile)?;
        assert_install_route(&ctx, &choice, &target, name)?;
        assert_unverified_source_stops_before_registration(&ctx, &choice, &target)?;
        let stopped = install_profiles_using(
            &ctx,
            &choice,
            &[existing(&target, false)],
            || Err(cancelled()),
            |_, _| Err(Error::new("unexpected", "should not install")),
        )
        .err()
        .ok_or("cancel")?;
        assert_eq!(stopped.code, "cancelled");
        let stored = crate::resource_agents::profile_roots(&ctx)?
            .into_iter()
            .find(|item| item.id == profile.id)
            .ok_or("profile")?;
        assert_eq!(stored.management, crate::resource::Management::Untracked);
        let result = install_profiles_using(
            &ctx,
            &choice,
            &[existing(&target, false)],
            || Ok(InitialActivation::Disabled),
            |activation, selected| {
                assert_eq!(activation, InitialActivation::Disabled);
                assert_eq!(selected, target);
                Ok(installed_fixture(&selected))
            },
        )?;
        assert_eq!(result["targetName"], name);
        let stored = crate::resource_agents::profile_roots(&ctx)?
            .into_iter()
            .find(|item| item.id == profile.id)
            .ok_or("registered profile")?;
        assert_eq!(stored.management, crate::resource::Management::Adopted);
        assert!(!target.directory.canonical.join("plugins").exists());
    }
    Ok(())
}

fn assert_compatible_plugin_offers(
    ctx: &Context,
    binary: &std::path::Path,
    codex: &crate::codex_install_source::Offer,
    claude: &crate::claude_install_source::Offer,
) -> crate::test_support::TestResult {
    let report = json!({"codexMarketplace":codex,"claudeMarketplace":claude});
    let choices = install_choices_using(
        ctx,
        &report,
        &[],
        |_| Ok(binary.to_path_buf()),
        |_| Ok(binary.to_path_buf()),
    )?;
    assert_eq!(choices.len(), 2);
    assert!(choices[0].label().contains("Codex"));
    assert!(choices[1].label().contains("Claude Code"));
    let one = install_choices_using(
        ctx,
        &report,
        &[],
        |_| Err(Error::new("fixture_no_codex", "binary not certified")),
        |_| Ok(binary.to_path_buf()),
    )?;
    assert_eq!(one.len(), 1);
    assert!(one[0].label().contains("Claude Code"));

    Ok(())
}

fn assert_install_route(
    ctx: &Context,
    choice: &InstallChoice,
    target: &crate::resource_agents::InstallTarget,
    name: &str,
) -> crate::test_support::TestResult {
    let aborted = install_choice_using(
        ctx,
        choice,
        |_| Ok(Vec::new()),
        || panic!("no selection must not ask for confirmation"),
        |_, _| panic!("cancel must remain read-only"),
    )?;
    assert_eq!(aborted["mode"], "cancelled");
    let routed = install_choice_using(
        ctx,
        choice,
        |client| {
            assert_eq!(client, target.client);
            Ok(vec![Destination::Existing(existing(target, true))])
        },
        || Ok(InitialActivation::Enabled),
        |activation, selected| {
            assert_eq!(activation, InitialActivation::Enabled);
            assert_eq!(&selected, target);
            Ok(installed_fixture(&selected))
        },
    )?;
    assert_eq!(routed["targetName"], name);
    assert_eq!(routed["profile"], json!(target.directory.canonical));
    assert!(routed.get("results").is_none());

    Ok(())
}

fn assert_unverified_source_stops_before_registration(
    ctx: &Context,
    choice: &InstallChoice,
    target: &crate::resource_agents::InstallTarget,
) -> crate::test_support::TestResult {
    let changed = match choice {
        InstallChoice::Codex(offer, binary) => {
            let mut copy = offer.clone();
            copy.repository = "file:///unverified-source".into();
            InstallChoice::Codex(copy, binary.clone())
        }
        InstallChoice::Claude(offer, binary) => {
            let mut copy = offer.clone();
            copy.repository = "file:///unverified-source".into();
            InstallChoice::Claude(copy, binary.clone())
        }
        _ => return Err("expected a plugin".into()),
    };
    let result = install_profiles_using(
        ctx,
        &changed,
        &[existing(target, false)],
        || panic!("unverified source must not ask for confirmation"),
        |_, _| panic!("unverified source must not install"),
    );
    assert!(result.is_err());
    let selected = crate::resource_agents::profile_roots(ctx)?
        .into_iter()
        .find(|profile| profile.name == target.name)
        .ok_or("original profile")?;
    assert_eq!(selected.management, crate::resource::Management::Untracked);
    assert!(!target.directory.canonical.join("plugins").exists());
    Ok(())
}

#[test]
fn a_discovered_profile_outside_the_account_home_is_not_an_installation_target()
-> crate::test_support::TestResult {
    use clap::Parser as _;
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let home = temporary.path().join("home");
    let external = temporary.path().join("shared-claude-profile");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&external)?;
    let canary = external.join("settings.json");
    std::fs::write(&canary, b"fixture canary")?;
    let args =
        crate::cli::Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let mut ctx = Context::from_cli(&args, &mut vec![])?;
    ctx.harness.claude = Some(external);
    let profile = crate::resource_agents::profile_roots(&ctx)?
        .pop()
        .ok_or("discovered profile")?;
    assert!(crate::resource_agents::InstallTarget::capture(&ctx, &profile).is_err());
    assert!(!ctx.state.exists());
    assert_eq!(std::fs::read(canary)?, b"fixture canary");
    Ok(())
}

#[test]
fn enrolled_custom_profile_survives_without_its_discovery_environment()
-> crate::test_support::TestResult {
    use clap::Parser as _;
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let home = temporary.path();
    let path = home.join("my-claude");
    std::fs::create_dir(&path)?;
    let args =
        crate::cli::Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let mut ctx = Context::from_cli(&args, &mut vec![])?;
    ctx.harness.claude = Some(path);
    let selected = crate::resource_agents::profile_roots(&ctx)?
        .pop()
        .ok_or("profile")?;
    let target = crate::resource_agents::InstallTarget::capture(&ctx, &selected)?;
    crate::resource_agents::enroll(&ctx, &target)?;
    let registry = ctx.state.join("agent-profiles.json");
    let first = std::fs::read(&registry)?;
    crate::resource_agents::enroll(&ctx, &target)?;
    assert_eq!(std::fs::read(&registry)?, first);
    ctx.harness.claude = None;
    let rediscovered = crate::resource_agents::profile_roots(&ctx)?
        .pop()
        .ok_or("profile")?;
    assert_eq!(rediscovered.id, selected.id);
    assert_eq!(rediscovered.name, selected.name);
    assert_eq!(
        rediscovered.management,
        crate::resource::Management::Adopted
    );
    let directory = target.open()?.path;
    std::fs::rename(&directory, home.join("old-claude"))?;
    std::fs::create_dir(&directory)?;
    ctx.harness.claude = Some(directory);
    let replacement = crate::resource_agents::profile_roots(&ctx)?
        .pop()
        .ok_or("replacement")?;
    assert_eq!(
        replacement.management,
        crate::resource::Management::Untracked
    );
    let replacement = crate::resource_agents::InstallTarget::capture(&ctx, &replacement)?;
    assert!(crate::resource_agents::enroll(&ctx, &replacement).is_err());
    assert_eq!(std::fs::read(registry)?, first);
    Ok(())
}

#[test]
fn existing_profile_picker_keeps_unregistered_rows_read_only_until_confirmation()
-> crate::test_support::TestResult {
    use clap::Parser as _;
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let home = temporary.path();
    std::fs::create_dir(home.join(".claude-mi"))?;
    std::fs::create_dir(home.join(".claude-zen"))?;
    std::os::unix::fs::symlink(home.join(".claude-mi"), home.join(".claude-alias"))?;
    std::fs::create_dir(home.join(".codex"))?;
    let args =
        crate::cli::Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    let choose = |index: Option<usize>| {
        move |count| {
            assert_eq!(count, 3);
            Ok(index.into_iter().collect())
        }
    };
    assert!(choose_plugin_targets_using(&ctx, "claude", choose(None))?.is_empty());
    let Some(Destination::Existing(Selection {
        target,
        registered: false,
    })) = choose_plugin_targets_using(&ctx, "claude", choose(Some(1)))?.pop()
    else {
        return Err("expected second existing profile".into());
    };
    assert_eq!(target.name, "claude:zen");
    assert!(!ctx.state.exists());
    crate::resource_agents::enroll(&ctx, &target)?;
    let Some(Destination::Existing(Selection {
        target: first,
        registered: true,
    })) = choose_plugin_targets_using(&ctx, "claude", choose(Some(0)))?.pop()
    else {
        return Err("registered profile missing".into());
    };
    assert_eq!(first, target);
    assert!(matches!(
        choose_plugin_targets_using(&ctx, "claude", choose(Some(2)))?.pop(),
        Some(Destination::Independent)
    ));
    assert!(!home.join(".claude-zen/plugins").exists());
    Ok(())
}
use crate::test_support::TestResult;

#[test]
fn short_name_discovery_accepts_only_the_selected_public_repository() -> TestResult {
    use crate::recommend_upstream::RepositoryMatch;
    let no_search = |_: &str| panic!("no public lookup for this input");
    let no_choice = |_| panic!("no source may be selected");
    let base = |status| json!({"query":"example-skill","evidenceStatus":status});
    let choices = |query: &str| {
        assert_eq!(query, "example-skill");
        Some(Ok(vec![
            RepositoryMatch {
                name: "first/example-skill".into(),
                fork: true,
            },
            RepositoryMatch {
                name: "second/example-skill".into(),
                fork: false,
            },
        ]))
    };
    for status in ["declared-catalog", "embedded-public-snapshot"] {
        let data = base(status);
        assert_eq!(
            search_repository_using(&data, false, choices, |count| {
                assert_eq!(count, 2);
                Ok(Some(1))
            })?
            .as_deref(),
            Some("second/example-skill"),
        );
        assert!(search_repository_using(&data, false, choices, |_| Ok(None))?.is_none());
        assert!(search_repository_using(&data, false, choices, |_| Ok(Some(9)))?.is_none());
        assert!(search_repository_using(&data, false, |_| Some(Ok(vec![])), no_choice)?.is_none());
        assert!(search_repository_using(&data, false, |_| None, no_choice)?.is_none());
        assert!(
            search_repository_using(
                &data,
                false,
                |_| Some(Err(Error::attention("unavailable", "fixture unavailable"))),
                no_choice
            )?
            .is_none()
        );
    }
    assert!(
        search_repository_using(&base("declared-catalog"), true, no_search, no_choice)?.is_none()
    );
    for literal_source in ["public-commit", "source-unavailable", "provided-by-caller"] {
        assert!(
            search_repository_using(&base(literal_source), false, no_search, no_choice)?.is_none()
        );
    }
    Ok(())
}

#[test]
fn recommendation_source_labels_omit_embedded_and_query_credentials() {
    assert_eq!(
        source_label(concat!(
            "https://reader:",
            "fixture-private",
            "@",
            "github.com/owner/repo"
        )),
        "待核对"
    );
    assert_eq!(
        source_label("https://github.com/owner/repo?key=fixture-private#fragment"),
        "https://github.com/owner/repo"
    );
}

#[test]
fn public_brew_evidence_does_not_select_another_tap_with_the_same_formula_name() -> TestResult {
    let mut package = crate::test_support::package();
    let mut query = crate::test_support::brew::query()?;
    query.selection.name = "jq".into();
    query.key.full_name = "jq".into();
    let advised = Candidate::Brew {
        name: "jq".into(),
        cask: false,
    };
    package.provider = Provider::BrewFormula {
        query: Box::new(query.clone()),
        recipe_ref: None,
    };
    assert!(matches_public_channel(&package, &advised));
    assert_eq!(
        preview_resource(&package).ok_or("expected preview")?["entry"],
        "install"
    );
    query.selection.tap = "custom/tap".into();
    query.key.full_name = "custom/tap/jq".into();
    package.provider = Provider::BrewFormula {
        query: Box::new(query),
        recipe_ref: None,
    };
    assert!(!matches_public_channel(&package, &advised));
    assert!(matches_public_channel(
        &package,
        &Candidate::Brew {
            name: "custom/tap/jq".into(),
            cask: false
        }
    ));
    Ok(())
}

#[test]
fn isolated_codex_offer_uses_explicit_confirmation_and_the_selected_commit() -> TestResult {
    use clap::Parser as _;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("fixture home")?,
        "app",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let data = json!({
        "repository":"fixture/project","marketplace":"market","plugin":"demo","declaredVersion":"1.0.0",
        "marketplaceCommit":"a".repeat(40),"payloadCommit":"a".repeat(40),"payloadRef":"main",
        "marketplaceSha256":"a".repeat(64),"pluginManifestSha256":"b".repeat(64),"status":"divergent-commits",
    });
    show_codex_market(&data);
    let offer = crate::codex_install_source::Offer::parse(&data)?;
    let binary = std::path::PathBuf::from("/fixture/codex");
    let choice = InstallChoice::Codex(Box::new(offer), binary);
    let warning = install_isolated_using(
        &ctx,
        &choice,
        || Err(Error::attention("cancelled", "fixture declined")),
        |_| Err(Error::new("unexpected_dispatch", "unconfirmed operation")),
    )
    .err()
    .ok_or("must respect cancelled confirmation")?;
    assert_eq!(warning.code, "cancelled");
    let result = install_isolated_using(
        &ctx,
        &choice,
        || Ok(InitialActivation::Disabled),
        |activation| {
            let InstallChoice::Codex(offer, path) = &choice else {
                panic!("wrong client")
            };
            assert_eq!(activation, InitialActivation::Disabled);
            assert_eq!(offer.payload_commit, "a".repeat(40));
            assert_eq!(offer.native_id(), "demo@market");
            assert_eq!(path, std::path::Path::new("/fixture/codex"));
            Ok(
                json!({"journal":{"phase":"succeeded","id":"OP-example"},"codexHome":"/fixture/profile"}),
            )
        },
    )?;
    assert_eq!(result["codexHome"], "/fixture/profile");
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn advisory_repository_candidates_keep_their_citations_and_grant_no_execution() -> TestResult {
    use clap::Parser as _;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("home")?,
        "recommend",
        "example-skill",
    ])?;
    let ctx = Context::from_cli(&cli, &mut Vec::new())?;
    let data = json!({
        "query":"example-skill","repository":"example/example-skill","evidenceStatus":"public-commit",
        "documents":[{"path":"INSTALL.md","url":"https://github.com/example/example-skill/blob/pinned/INSTALL.md"}],
        "model":{"candidates":[
            {"name":"example-skill","kind":"skill","adapter":"skills-cli","scope":"global","version":"1.2.3",
             "summary":"Observed upstream documentation","document":0},
            {"name":"unverified","kind":"other","adapter":"unknown","scope":"local","summary":"No matching document",
             "document":99},
        ]},
        "reason":"fixture remains advisory",
        "canExecute":false,"persistentWrites":false,
    });
    assert_eq!(choose(&ctx, data.clone())?, data);
    assert!(!ctx.state.exists());
    assert!(!ctx.config.exists());
    Ok(())
}

#[test]
fn isolated_claude_preview_requires_confirmation_and_keeps_the_selected_mode() -> TestResult {
    use clap::Parser as _;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let args = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("home")?,
        "recommend",
        "fixture",
    ])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    let commit = crate::policy::source_git::ObjectId::parse(&"c".repeat(40))?;
    let market = br#"{"name":"example-market","plugins":[{"name":"fixture","source":"./"}]}"#;
    let plugin = br#"{"name":"fixture","version":"2.1.1"}"#;
    let offer =
        crate::claude_install_source::Offer::identify("example/fixture", &commit, market, plugin)?;
    let binary = ctx.home.join("fixed-claude");
    let display = InstallChoice::Claude(Box::new(offer.clone()), binary.clone()).label();
    assert!(display.contains("Claude Code"));
    assert!(!display.contains("独立"));
    assert!(display.contains("fixture@example-market"));
    assert!(display.contains("2.1.1"));
    let choice = InstallChoice::Claude(Box::new(offer), binary.clone());
    let refusal = install_isolated_using(
        &ctx,
        &choice,
        || Err(Error::attention("cancelled", "fixture refusal")),
        |_| panic!("cancelled selection must not use the installer"),
    );
    assert!(refusal.is_err());
    assert!(!ctx.state.exists());
    let result = install_isolated_using(
        &ctx,
        &choice,
        || Ok(InitialActivation::Disabled),
        |activation| {
            let InstallChoice::Claude(selected, selected_binary) = &choice else {
                panic!("wrong client")
            };
            assert_eq!(selected.commit, commit.as_str());
            assert_eq!(selected_binary, &binary);
            assert_eq!(activation, InitialActivation::Disabled);
            Ok(
                json!({"journal":{"phase":"succeeded","id":"OP-fixture","message":"fixture"},
                "claudeConfigDir":ctx.home.join("verified-profile")}),
            )
        },
    )?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn local_install_evidence_remains_available_when_silent_keychain_access_fails() -> TestResult {
    use clap::Parser as _;
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "mo",
        "--home",
        dir.path().to_str().ok_or("fixture home")?,
        "recommend",
        "owner/release",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let result = choose(
        &ctx,
        json!({"query":"owner/release","repository":"owner/release","evidenceStatus":"public-commit",
            "reason":"synthetic_partial_source","documents":[{"path":"README.md","url":"https://github.com/owner/release"}],
            "model":{"status":"keychain-unavailable","candidates":[]},"persistentWrites":false}),
    )?;
    assert_eq!(result["model"]["status"], "keychain-unavailable");
    assert_eq!(result["documents"][0]["path"], "README.md");
    let mut suggested = result;
    suggested["model"] = json!({"status":"suggested","candidates":[
        {"name":"fixture","kind":"skill","adapter":"skills","scope":"global","version":"1.0.0","summary":"fixture evidence","document":0},
        {"name":"unlinked","kind":"plugin","adapter":"codex-plugin","scope":"global","version":null,"summary":"fixture","document":7},
        {"name":"unindexed","kind":"plugin","adapter":"claude-plugin","scope":"global","version":null,"summary":"fixture","document":null},
    ]});
    assert_eq!(choose(&ctx, suggested.clone())?, suggested);
    assert!(!ctx.state.exists());
    assert!(dir.path().exists());
    Ok(())
}

#[test]
fn multi_selection_preserves_order_and_requires_new_profile_to_be_selected_alone() {
    assert_eq!(parse_target_numbers(" 5,3，4 5 ", 8), Some(vec![4, 2, 3]));
    assert_eq!(parse_target_numbers("8", 8), Some(vec![7]));
    for invalid in ["0 1", "1 9", "1 8", "abc,1", ","] {
        assert_eq!(parse_target_numbers(invalid, 8), None, "{invalid}");
    }
}

#[test]
fn profile_label_only_hides_the_suffix_of_the_matching_isolated_allocation() {
    let path = Path::new("/fixture/claude-profile.kTd2oW/home/.claude");
    assert_eq!(
        crate::resource_agents::profile_label("claude:mo-i-have-adhd-kTd2oW", path),
        "Claude Code · i-have-adhd"
    );
    for name in ["claude:mi", "claude:mo-i-have-adhd-other"] {
        assert_eq!(crate::resource_agents::profile_label(name, path), name);
    }
    let alias = "claude:mo-i-have-adhd-kTd2oW";
    assert_eq!(
        crate::resource_agents::profile_label(alias, Path::new("/fixture/.claude-mi")),
        alias
    );
}

fn batch_offer() -> Result<InstallChoice> {
    let claude = crate::claude_install_source::Offer::identify(
        "example/fixture",
        &crate::policy::source_git::ObjectId::parse(&"a".repeat(40))?,
        br#"{"name":"fixture-market","plugins":[{"name":"item","source":"./"}]}"#,
        br#"{"name":"item","version":"1.0"}"#,
    )?;
    Ok(InstallChoice::Claude(Box::new(claude), "/fixture".into()))
}

fn existing(target: &InstallTarget, registered: bool) -> Selection {
    Selection {
        target: Box::new(target.clone()),
        registered,
    }
}

fn installed_fixture(target: &InstallTarget) -> Value {
    json!({"journal":{"phase":"succeeded"},"targetName":target.name,"profile":target.directory.canonical})
}

fn add_batch_target(ctx: &Context, name: &str) -> Result<Selection> {
    let path = ctx.home.join(format!(".claude-{name}"));
    std::fs::create_dir(&path)?;
    let profile = crate::resource_agents::profile_roots(ctx)?
        .into_iter()
        .find(|profile| profile.name == format!("claude:{name}"))
        .ok_or_else(incompatible_profile_offer)?;
    let target = crate::resource_agents::InstallTarget::capture(ctx, &profile)?;
    Ok(existing(&target, false))
}

#[test]
fn multi_profile_install_preserves_a_pending_later_target_and_stops_after_incomplete_install()
-> TestResult {
    let (_temporary, ctx, pending, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let first = add_batch_target(&ctx, "first")?;
    let skipped = add_batch_target(&ctx, "skipped")?;
    let targets = [first, existing(&pending, true), skipped];
    let mut attempted = Vec::new();
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &targets,
        || Ok(InitialActivation::Disabled),
        |activation, target| {
            assert_eq!(activation, InitialActivation::Disabled);
            let journal = crate::store::active(&ctx)?.ok_or_else(incompatible_profile_offer)?;
            assert_eq!(journal.id, id);
            assert_eq!(journal.phase, crate::store::Phase::Succeeded);
            attempted.push(target.name.clone());
            if attempted.len() == 1 {
                return Ok(installed_fixture(&target));
            }
            Ok(json!({"journal":{"phase":"succeeded"},"continuationRequired":true}))
        },
    )?;
    assert_eq!(result["incomplete"], true);
    assert_eq!(
        attempted,
        [
            targets[0].target.name.clone(),
            targets[1].target.name.clone()
        ]
    );
    let profiles = crate::resource_agents::profile_roots(&ctx)?;
    assert!(
        profiles
            .iter()
            .any(|profile| profile.name == "claude:skipped"
                && profile.management == crate::resource::Management::Untracked)
    );
    Ok(())
}

#[test]
fn multi_profile_install_checks_approvals_and_never_recovers_an_unselected_profile() -> TestResult {
    let (_temporary, ctx, _, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let selections = [
        add_batch_target(&ctx, "one")?,
        add_batch_target(&ctx, "two")?,
    ];
    let registry = ctx.state.join("agent-profiles.json");
    let cancelled = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &selections,
        || Err(Error::attention("cancelled", "fixture")),
        |_, _| panic!("unexpected write"),
    );
    assert_eq!(cancelled.err().ok_or("must cancel")?.code, "cancelled");
    let blocked = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &selections,
        || Ok(InitialActivation::Enabled),
        |_, _| panic!("unexpected write"),
    );
    assert_eq!(blocked.err().ok_or("must block")?.code, "recovery_required");
    assert!(!registry.exists());
    let journal = crate::store::active(&ctx)?.ok_or("active")?;
    assert_eq!(journal.id, id);
    assert_eq!(journal.phase, crate::store::Phase::Prepared);
    Ok(())
}

#[test]
fn multi_profile_install_summarizes_success_and_leaves_later_targets_alone_on_error() -> TestResult
{
    for fail in [false, true] {
        let (_temporary, ctx, pending, _) =
            crate::test_support::PluginTargetFixture::pending_for_recommendation(
                "claude",
                "previous offer".into(),
            )?;
        let first = add_batch_target(&ctx, "first")?;
        let last = add_batch_target(&ctx, "last")?;
        let selections = [first, existing(&pending, true), last];
        let mut attempted = Vec::new();
        let result = install_profiles_using(
            &ctx,
            &batch_offer()?,
            &selections,
            || Ok(InitialActivation::Enabled),
            |activation, target| {
                assert_eq!(activation, InitialActivation::Enabled);
                attempted.push(target.name.clone());
                if attempted.len() == 2 && fail {
                    return Err(Error::attention("fixture_stop", "installer failed"));
                }
                Ok(installed_fixture(&target))
            },
        );
        let count = if fail {
            assert_eq!(
                result.err().ok_or("failure must stop the batch")?.code,
                "fixture_stop"
            );
            2
        } else {
            let value = result?;
            assert_ne!(value["incomplete"], true);
            assert_eq!(value["results"].as_array().ok_or("batch results")?.len(), 3);
            3
        };
        assert_eq!(
            attempted,
            selections[..count]
                .iter()
                .map(|selection| selection.target.name.clone())
                .collect::<Vec<_>>()
        );
        let last = crate::resource_agents::profile_roots(&ctx)?
            .into_iter()
            .find(|profile| profile.name == "claude:last")
            .ok_or("last profile")?;
        let registration = if fail {
            crate::resource::Management::Untracked
        } else {
            crate::resource::Management::Adopted
        };
        assert_eq!(last.management, registration);
    }
    Ok(())
}

#[test]
fn multi_profile_install_rechecks_every_directory_after_confirmation() -> TestResult {
    let (_temporary, ctx, _, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "previous offer".into(),
        )?;
    let first = add_batch_target(&ctx, "one")?;
    let second = add_batch_target(&ctx, "two")?;
    let path = second.target.directory.canonical.clone();
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &[first, second],
        || {
            std::fs::rename(&path, ctx.home.join("held-profile"))?;
            std::fs::create_dir(&path)?;
            Ok(InitialActivation::Enabled)
        },
        |_, _| panic!("changed directory must stop before installation"),
    );
    assert_eq!(
        result.err().ok_or("must reject the changed target")?.code,
        "agent_install_target_changed"
    );
    assert!(!ctx.state.join("agent-profiles.json").exists());
    let journal = crate::store::active(&ctx)?.ok_or("active")?;
    assert_eq!(journal.id, id);
    assert_eq!(journal.phase, crate::store::Phase::Prepared);
    Ok(())
}

#[test]
fn recommendation_keychain_hints_distinguish_authorization_and_replacement() {
    let authorize = recommendation_keychain_hint(Some("keychain-authorization-required"));
    assert!(authorize.is_some_and(|hint| hint.contains("选 t") && hint.contains("本地分析")));
    let missing = recommendation_keychain_hint(Some("keychain-missing"));
    assert!(
        missing.is_some_and(|hint| hint.contains("重新设置或绑定") && hint.contains("本地分析"))
    );
    assert!(recommendation_keychain_hint(Some("suggested")).is_none());
    assert!(recommendation_keychain_hint(None).is_none());
}

#[test]
fn multi_profile_command_rejects_mixed_new_profiles_and_stale_existing_paths() -> TestResult {
    let (_temporary, ctx, _, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "previous offer".into(),
        )?;
    let one = add_batch_target(&ctx, "one")?;
    let selection = vec![
        Destination::Existing(existing(&one.target, false)),
        Destination::Independent,
    ];
    let error = install_choice_using(
        &ctx,
        &batch_offer()?,
        |_| Ok(selection),
        || panic!("invalid selection must not ask for confirmation"),
        |_, _| panic!("invalid selection must not install"),
    )
    .err()
    .ok_or("must reject mixed selection")?;
    assert_eq!(error.code, "plugin_target_unsupported");

    let two = add_batch_target(&ctx, "two")?;
    let path = two.target.directory.canonical.clone();
    std::fs::rename(&path, ctx.home.join("held-profile"))?;
    std::fs::create_dir(&path)?;
    let selection = vec![Destination::Existing(one), Destination::Existing(two)];
    let error = install_choice_using(
        &ctx,
        &batch_offer()?,
        |_| Ok(selection),
        || panic!("invalid selection must not ask for confirmation"),
        |_, _| panic!("invalid selection must not install"),
    )
    .err()
    .ok_or("must reject stale selection")?;
    assert_eq!(error.code, "agent_install_target_changed");
    assert!(!ctx.state.join("agent-profiles.json").exists());
    let journal = crate::store::active(&ctx)?.ok_or("active")?;
    assert_eq!(journal.id, id);
    assert_eq!(journal.phase, crate::store::Phase::Prepared);
    Ok(())
}

#[test]
fn multi_profile_install_does_not_count_an_unknown_native_result_as_a_success() -> TestResult {
    let (_temporary, ctx, pending, _) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "previous offer".into(),
        )?;
    let second = add_batch_target(&ctx, "second")?;
    let targets = [existing(&pending, true), second];
    let mut calls = Vec::new();
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &targets,
        || Ok(InitialActivation::Enabled),
        |_, target| {
            calls.push(target.name);
            Ok(
                json!({"journal":{"phase":"unknown","message":"fixture could not verify publication"}}),
            )
        },
    )?;
    assert_eq!(calls, [targets[0].target.name.clone()]);
    assert_eq!(result["incomplete"], true);
    assert_eq!(result["results"][0]["journal"]["phase"], "unknown");
    assert!(!ctx.state.join("agent-profiles.json").exists());
    Ok(())
}

#[test]
fn multi_selection_input_reprompts_for_invalid_numbers_and_cancels_on_empty_line() -> TestResult {
    use std::{
        io::Write as _,
        process::{Command, Stdio},
    };
    const CHILD: &str = "MACOPS_TEST_PROFILE_SELECTION_INPUT";
    if std::env::var_os(CHILD).is_some() {
        assert_eq!(target_numbers(8)?, [2, 4]);
        assert_eq!(target_numbers(8)?, [] as [usize; 0]);
        return Ok(());
    }
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "recommend::tests::multi_selection_input_reprompts_for_invalid_numbers_and_cancels_on_empty_line",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    {
        let mut input = child.stdin.take().ok_or("profile input pipe")?;
        input.write_all(b"8,1\n0\n3 5\n\n")?;
    }
    let output = child.wait_with_output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert_eq!(stderr.matches("「新建单独配置」需要单独选择。").count(), 2);
    Ok(())
}

#[test]
fn a_single_profile_with_only_a_verified_stage_still_requires_attention() -> TestResult {
    let (_temporary, ctx, target, _) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let selection = [existing(&target, true)];
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &selection,
        || Ok(InitialActivation::Enabled),
        |_, target| {
            let mut stage_only = installed_fixture(&target);
            stage_only["continuationRequired"] = true.into();
            Ok(stage_only)
        },
    )?;
    assert_eq!(result["incomplete"], true);
    assert_eq!(result["continuationRequired"], true);
    assert_eq!(result["targetName"], target.name);
    assert!(result.get("results").is_none());
    Ok(())
}

#[test]
fn a_profile_receipt_must_identify_the_selected_configuration() -> TestResult {
    let (_temporary, ctx, target, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let targets = [existing(&target, true)];
    let wrong_directory = json!(ctx.home.join(".claude-unselected"));
    let mut wrong_name = installed_fixture(&target);
    wrong_name["targetName"] = "claude:unselected".into();
    let mut wrong_path = installed_fixture(&target);
    wrong_path["profile"] = wrong_directory;
    let mut no_path = installed_fixture(&target);
    no_path["profile"] = Value::Null;
    let mut unknown = installed_fixture(&target);
    unknown["journal"] = json!({"id":id,"phase":"unknown","message":"结果暂不可核对"});
    for receipt in [wrong_name, wrong_path, no_path, unknown] {
        let result = install_profiles_using(
            &ctx,
            &batch_offer()?,
            &targets,
            || Ok(InitialActivation::Enabled),
            |_, _| Ok(receipt.clone()),
        )?;
        assert_eq!(result["incomplete"], true);
        assert_eq!(result["journal"], receipt["journal"]);
    }
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &targets,
        || Ok(InitialActivation::Enabled),
        |_, _| Ok(json!("not a receipt")),
    )?;
    assert_eq!(result, json!({"incomplete":true,"result":"not a receipt"}));
    Ok(())
}

#[test]
fn a_batch_rechecks_each_directory_before_registering_it() -> TestResult {
    let (_temporary, ctx, first, _) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let changed = add_batch_target(&ctx, "changed")?;
    let untouched = add_batch_target(&ctx, "untouched")?;
    let path = changed.target.directory.canonical.clone();
    let mut attempted = Vec::new();
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &[existing(&first, true), changed, untouched],
        || Ok(InitialActivation::Enabled),
        |_, target| {
            assert_eq!(target, first);
            attempted.push(target.name.clone());
            std::fs::rename(&path, ctx.home.join("held-profile"))?;
            std::fs::create_dir(&path)?;
            Ok(installed_fixture(&target))
        },
    );
    assert_eq!(
        result
            .err()
            .ok_or("changed directory must stop the batch")?
            .code,
        "agent_install_target_changed"
    );
    assert_eq!(attempted, [first.name]);
    assert!(!ctx.state.join("agent-profiles.json").exists());
    assert!(!path.join("plugins").exists());
    Ok(())
}

#[test]
fn choosing_a_new_configuration_cannot_recover_into_an_existing_one() -> TestResult {
    let (_temporary, ctx, target, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let settings = target.directory.canonical.join("settings.json");
    let original = std::fs::read(&settings)?;
    let unconfirmed = install_choice_using(
        &ctx,
        &batch_offer()?,
        |_| Ok(vec![Destination::Independent]),
        || Err(Error::attention("cancelled", "fixture refused")),
        |_, _| panic!("cannot install into an existing configuration"),
    );
    assert_eq!(
        unconfirmed.err().ok_or("must honor cancellation")?.code,
        "cancelled"
    );
    let result = install_choice_using(
        &ctx,
        &batch_offer()?,
        |_| Ok(vec![Destination::Independent]),
        || Ok(InitialActivation::Enabled),
        |_, _| panic!("cannot install into an existing configuration"),
    );
    assert_eq!(
        result
            .err()
            .ok_or("cannot take over another operation")?
            .code,
        "recovery_required"
    );
    let pending = crate::store::active(&ctx)?.ok_or("original operation must remain active")?;
    assert_eq!(pending.id, id);
    assert_eq!(pending.phase, crate::store::Phase::Prepared);
    assert_eq!(std::fs::read(settings)?, original);
    assert!(
        !target
            .directory
            .canonical
            .join("plugins/cache/fixture-market/item/1.0/SKILL.md")
            .exists()
    );
    Ok(())
}

#[test]
fn selecting_a_new_offer_does_not_reuse_the_old_receipt() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;
    let (_temporary, ctx, target, _) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let InstallChoice::Claude(offer, _) = batch_offer()? else {
        return Err("expected a Claude plugin".into());
    };
    let binary = ctx.home.join("uncertified-claude");
    std::fs::write(&binary, b"uncertified fixture")?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    let choice = InstallChoice::Claude(offer, binary);
    let result = install_choice_using(
        &ctx,
        &choice,
        |_| Ok(vec![Destination::Existing(existing(&target, true))]),
        || Ok(InitialActivation::Enabled),
        |activation, target| install_at_profile(&ctx, &choice, activation, target),
    );
    assert_eq!(
        result
            .err()
            .ok_or("new install must verify its binary")?
            .code,
        "claude_install_binary_unverified"
    );
    let history = crate::store::history(&ctx)?;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].phase, crate::store::Phase::Succeeded);
    Ok(())
}

#[test]
fn missing_recovery_evidence_blocks_a_retry_before_any_install() -> TestResult {
    let (_temporary, ctx, target, id) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let active = ctx.state.join("active.json");
    let original = std::fs::read(&active)?;
    std::fs::remove_file(ctx.state.join(format!("plugin-target-{id}.json")))?;
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &[existing(&target, false)],
        || Ok(InitialActivation::Enabled),
        |_, _| panic!("uncertain prior state must stop the new installation"),
    );
    assert_eq!(
        result.err().ok_or("missing recovery evidence")?.code,
        "plugin_target_changed"
    );
    assert_eq!(std::fs::read(active)?, original);
    assert!(!ctx.state.join("agent-profiles.json").exists());
    Ok(())
}

#[test]
fn a_batch_stops_if_the_profile_registry_changes_to_unreadable_data() -> TestResult {
    let (_temporary, ctx, first, _) =
        crate::test_support::PluginTargetFixture::pending_for_recommendation(
            "claude",
            "old offer".into(),
        )?;
    let second = add_batch_target(&ctx, "second")?;
    let third = add_batch_target(&ctx, "third")?;
    let registry = ctx.state.join("agent-profiles.json");
    let mut installed = Vec::new();
    let result = install_profiles_using(
        &ctx,
        &batch_offer()?,
        &[existing(&first, true), second, third],
        || Ok(InitialActivation::Enabled),
        |_, target| {
            assert_eq!(target, first, "cannot install once enrollment fails");
            installed.push(target.name.clone());
            std::fs::write(&registry, b"malformed registry")?;
            Ok(installed_fixture(&target))
        },
    );
    assert!(result.is_err());
    assert_eq!(installed, [first.name]);
    assert_eq!(std::fs::read(registry)?, b"malformed registry");
    Ok(())
}

#[test]
fn independent_retry_requires_the_frozen_activation_before_settling_an_old_operation() -> TestResult
{
    use crate::test_support::PluginTargetFixture;
    for (original, changed, option) in [
        (
            InitialActivation::Enabled,
            InitialActivation::Disabled,
            "yes",
        ),
        (
            InitialActivation::Disabled,
            InitialActivation::Enabled,
            "disabled",
        ),
    ] {
        let (_temporary, ctx, offer) = PluginTargetFixture::unstarted_isolated_codex(original)?;
        let choice = InstallChoice::Codex(Box::new(offer), "/unused".into());
        let active_path = ctx.state.join("active.json");
        let before = std::fs::read(&active_path)?;
        let result = install_isolated_using(
            &ctx,
            &choice,
            || Ok(changed),
            |_| panic!("the changed request must not start"),
        );
        let error = result
            .err()
            .ok_or("must reject different initial activation")?;
        assert_eq!(error.code, "plugin_install_activation_changed");
        assert!(error.message.contains(option));
        assert_eq!(std::fs::read(&active_path)?, before);

        let result = install_isolated_using(
            &ctx,
            &choice,
            || Ok(original),
            |selected| {
                assert_eq!(selected, original);
                let journal = crate::store::active(&ctx)?.ok_or_else(|| {
                    Error::new("fixture_missing", "original operation must settle")
                })?;
                assert_eq!(journal.phase, crate::store::Phase::Failed);
                Ok(json!({"phase":"succeeded","codexHome":"/fixture/new-profile"}))
            },
        )?;
        assert_eq!(result["codexHome"], "/fixture/new-profile");
        assert_eq!(crate::store::history(&ctx)?.len(), 1);
    }
    Ok(())
}

#[test]
fn recommendation_explains_manual_installation_when_no_plan_can_run() {
    let data = json!({
        "query":"example/kit",
        "evidenceStatus":"public-commit",
        "documents":[{"path":"README.md"}, {"path":"INSTALL.md"}],
        "manualInstall":[{"manager":"Skills CLI","document":0,"commands":["npx skills add example/kit"]}],
    });
    let text = manual_install_text(&data);
    assert!(text.starts_with("错误表现") && text.contains("错误原因") && text.contains("正确用法"));
    assert!(text.contains("Skills CLI · README.md") && text.contains("npx skills add example/kit"));
    let fallback =
        manual_install_text(&json!({"documents":[{"path":"README.md"}, {"path":"INSTALL.md"}]}));
    assert!(fallback.contains("打开上面的 INSTALL.md"));
    let limited = manual_install_text(&json!({"reason":"recommend_github_rate_limited"}));
    assert!(
        limited.contains("GitHub 匿名 API 查询额度已用完")
            && limited.contains("等 GitHub 查询额度恢复后")
    );
    let no_docs = manual_install_text(
        &json!({"repository":"example/kit","evidenceStatus":"documents-unavailable"}),
    );
    assert!(no_docs.contains("打开 https://github.com/example/kit"));
}

#[test]
fn repository_model_failure_or_empty_response_has_a_next_step() {
    for (status, expected) in [
        ("profile-unavailable", "运行 mo recommend --setup"),
        ("transport-unavailable", "检查网络"),
        ("deadline", "请求超时"),
        ("http-status", "选 t 测试连接"),
        ("output-limit", "按来源文档继续"),
        ("unverified-response", "按来源文档继续"),
        ("suggested", "没有返回可引用的安装方式"),
    ] {
        assert!(
            recommendation_model_hint(&json!({"status":status,"candidates":[]}))
                .is_some_and(|hint| hint.contains(expected))
        );
    }
    assert!(recommendation_model_hint(&json!({"status":"local-only"})).is_none());
    assert!(recommendation_model_hint(&json!({"status":"suggested","candidates":[{}]})).is_none());
}
