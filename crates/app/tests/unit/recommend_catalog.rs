use super::*;
use crate::test_support::TestResult;

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
    assert_eq!(choose(&ctx, advice.clone(), false)?, advice);
    let unavailable = json!({"query":"fixture","model":{"status":"keychain-unavailable"}});
    assert_eq!(choose(&ctx, unavailable.clone(), false)?, unavailable);
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn short_name_discovery_accepts_only_the_selected_public_repository() -> TestResult {
    use crate::recommend::upstream::RepositoryMatch;
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
    assert_eq!(choose(&ctx, data.clone(), false)?, data);
    assert!(!ctx.state.exists());
    assert!(!ctx.config.exists());
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
        false,
    )?;
    assert_eq!(result["model"]["status"], "keychain-unavailable");
    assert_eq!(result["documents"][0]["path"], "README.md");
    let mut suggested = result;
    suggested["model"] = json!({"status":"suggested","candidates":[
        {"name":"fixture","kind":"skill","adapter":"skills","scope":"global","version":"1.0.0","summary":"fixture evidence","document":0},
        {"name":"unlinked","kind":"plugin","adapter":"codex-plugin","scope":"global","version":null,"summary":"fixture","document":7},
        {"name":"unindexed","kind":"plugin","adapter":"claude-plugin","scope":"global","version":null,"summary":"fixture","document":null},
    ]});
    assert_eq!(choose(&ctx, suggested.clone(), true)?, suggested);
    assert!(!ctx.state.exists());
    assert!(dir.path().exists());
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
fn recommendation_explains_manual_installation_when_no_plan_can_run() {
    let data = json!({
        "query":"example/kit",
        "evidenceStatus":"public-commit",
        "documents":[{"path":"README.md"}, {"path":"INSTALL.md"}],
        "manualInstall":[{"manager":"Skills CLI","document":0,"commands":["npx skills add example/kit"]}],
    });
    let text = recommendation_sections(&data, &[]);
    assert!(text.starts_with("手动安装") && !text.contains("错误表现"));
    assert!(text.contains("Skills CLI · README.md") && text.contains("npx skills add example/kit"));
    let fallback =
        no_install_plan(&json!({"documents":[{"path":"README.md"}, {"path":"INSTALL.md"}]}));
    assert!(
        fallback.starts_with("错误表现")
            && fallback.contains("错误原因")
            && fallback.contains("正确用法")
    );
    assert!(fallback.contains("打开上面的 INSTALL.md"));
    let limited = no_install_plan(&json!({"reason":"recommend_github_rate_limited"}));
    assert!(
        limited.contains("GitHub 匿名 API 查询额度已用完")
            && limited.contains("等 GitHub 查询额度恢复后")
    );
    let no_docs = no_install_plan(
        &json!({"repository":"example/kit","evidenceStatus":"documents-unavailable"}),
    );
    assert!(no_docs.contains("打开 https://github.com/example/kit"));
}

#[test]
fn recommendations_group_verified_profiles_and_manual_agents_without_model_duplicates() -> TestResult
{
    let commit = crate::policy::source_git::ObjectId::parse(&"d".repeat(40))?;
    let offer = crate::agent::claude_install_source::Offer::identify(
        "example/kit",
        &commit,
        br#"{"name":"local","plugins":[{"name":"toolkit","source":"./"}]}"#,
        br#"{"name":"toolkit","version":"1.11.0"}"#,
    )?;
    let data = json!({
        "documents": [
            {"path":"README.md","url":"https://github.com/example/kit/blob/fixed/README.md"},
            {"path":"README.md","url":"https://github.com/example/kit/blob/fixed/README.md"},
            {"path":"INSTALL.md","url":"https://github.com/example/kit/blob/fixed/INSTALL.md"}
        ],
        "model":{"candidates":[{"summary":"repeated model explanation"},{"summary":"repeated model explanation"}]},
        "manualInstall":[
            {"manager":"Skills CLI","document":0,"commands":["npx skills add example/kit@v1.11.0"]},
            {"manager":"Claude Code","document":2,"commands":["claude plugin marketplace add example/kit#v1.11.0","claude plugin install toolkit@local"]}
        ]
    });
    let summary = recommendation_sections(
        &data,
        &[InstallChoice::Plugin(plugin_install::Offer::Claude(
            Box::new(offer),
            "/fixture/claude".into(),
        ))],
    );
    assert!(summary.starts_with("可直接安装\n  1 toolkit@local · Claude Code · 1.11.0"));
    assert_eq!(
        summary
            .matches("https://github.com/example/kit/blob/fixed/README.md")
            .count(),
        1
    );
    assert_eq!(
        summary
            .matches("npx skills add example/kit@v1.11.0")
            .count(),
        1
    );
    assert!(summary.contains("多选客户端"));
    assert!(!summary.contains("repeated model explanation"));
    assert!(!summary.contains("claude plugin install"));
    let fallback = recommendation_sections(&data, &[]);
    assert!(fallback.contains("claude plugin install toolkit@local"));
    assert!(!fallback.contains("可直接安装"));
    Ok(())
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

#[test]
fn verified_agent_offers_appear_as_recommendation_choices_in_order() -> TestResult {
    use clap::Parser as _;

    let home = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("home")?,
        "app",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let commit = "a".repeat(40);
    let codex = crate::agent::codex_install_source::Offer::parse(&json!({
        "repository":"example/fixture","marketplace":"market","plugin":"item","declaredVersion":"1.0.0",
        "marketplaceCommit":commit,"payloadCommit":commit,"payloadRef":"main",
        "marketplaceSha256":"a".repeat(64),"pluginManifestSha256":"b".repeat(64)
    }))?;
    let claude = crate::agent::claude_install_source::Offer::identify(
        "example/fixture",
        &crate::policy::source_git::ObjectId::parse(&commit)?,
        br#"{"name":"market","plugins":[{"name":"item","source":"./"}]}"#,
        br#"{"name":"item","version":"1.0.0"}"#,
    )?;
    let codex = || plugin_install::Offer::Codex(Box::new(codex.clone()), "/fixture/codex".into());
    let claude =
        || plugin_install::Offer::Claude(Box::new(claude.clone()), "/fixture/claude".into());
    let evidence = json!({});
    for (offers, expected) in [
        (
            vec![codex(), claude()],
            vec![
                "item@market · Codex · 1.0.0",
                "item@market · Claude Code · 1.0.0",
            ],
        ),
        (vec![claude()], vec!["item@market · Claude Code · 1.0.0"]),
        (vec![], vec![]),
    ] {
        let choices = install_choices(&ctx, &evidence, &[], || offers)?;
        let labels: Vec<_> = choices.iter().map(InstallChoice::label).collect();
        assert_eq!(labels, expected);
    }
    assert!(!ctx.state.exists());
    Ok(())
}
