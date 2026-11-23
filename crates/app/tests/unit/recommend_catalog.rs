use super::*;
use crate::test_support::TestResult;

#[test]
fn short_name_discovery_accepts_only_the_selected_public_repository() -> TestResult {
    use crate::recommend_upstream::RepositoryMatch;
    let no_search = |_: &str| panic!("no public lookup for this input");
    let no_choice = || panic!("no source may be selected");
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
            search_repository_using(&data, false, choices, || Ok(Some(1)))?.as_deref(),
            Some("second/example-skill"),
        );
        assert!(search_repository_using(&data, false, choices, || Ok(None))?.is_none());
        assert!(search_repository_using(&data, false, choices, || Ok(Some(9)))?.is_none());
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
    let warning = install_codex_using(
        &ctx,
        offer.clone(),
        binary.clone(),
        || Err(Error::attention("cancelled", "fixture declined")),
        |_, _, _, _| Err(Error::new("unexpected_dispatch", "unconfirmed operation")),
    )
    .err()
    .ok_or("must respect cancelled confirmation")?;
    assert_eq!(warning.code, "cancelled");
    let result = install_codex_using(
        &ctx,
        offer,
        binary,
        || Ok(crate::codex_install::InitialActivation::Disabled),
        |scoped, offer, path, activation| {
            assert_eq!(
                activation,
                crate::codex_install::InitialActivation::Disabled
            );
            assert_eq!(offer.payload_commit, "a".repeat(40));
            assert_eq!(offer.native_id(), "demo@market");
            assert_eq!(scoped.home, ctx.home);
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
    use crate::codex_install::InitialActivation;
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
    assert!(display.contains("独立 Claude Code 档案"));
    assert!(display.contains("fixture@example-market"));
    assert!(display.contains("2.1.1"));
    let refusal = install_claude_using(
        &ctx,
        offer.clone(),
        binary.clone(),
        || Err(Error::attention("cancelled", "fixture refusal")),
        |_, _, _, _| panic!("cancelled selection must not use the installer"),
    );
    assert!(refusal.is_err());
    assert!(!ctx.state.exists());
    let result = install_claude_using(
        &ctx,
        offer,
        binary.clone(),
        || Ok(InitialActivation::Disabled),
        |context, selected, selected_binary, enabled| {
            assert_eq!(context.state, ctx.state);
            assert_eq!(selected.commit, commit.as_str());
            assert_eq!(selected_binary, binary);
            assert!(!enabled);
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
