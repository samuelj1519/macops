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
fn shared_skill_adoption_shows_its_result_before_update_and_stops_on_incomplete() -> TestResult {
    use std::cell::Cell;

    for incomplete in [false, true] {
        let shown = Cell::new(false);
        let checked_update = Cell::new(false);
        let report = after_skill_adoption(
            json!({"skillAction":"adopt","incomplete":incomplete,"target":"owner/repo"}),
            |report| {
                assert_eq!(report["incomplete"], incomplete);
                shown.set(true);
            },
            || {
                assert!(shown.get());
                checked_update.set(true);
                Err(Error::attention("cancelled", "fixture cancel"))
            },
        )?;
        assert_eq!(checked_update.get(), !incomplete);
        assert_eq!(report["skillAction"], "adopt");
    }
    Ok(())
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

#[test]
fn public_skill_choice_requires_repository_matched_documented_command() {
    let proposal = |commands| {
        json!({
            "repository":"Owner/Repo",
            "manualInstall":[
                {"manager":"Other","commands":["npx skills add owner/repo@wrong-manager"]},
                {"manager":"Skills CLI","commands":commands}
            ]
        })
    };
    assert!(documented_skill_source(&json!({"repository":"../unknown"})).is_none());
    assert!(
        documented_skill_source(&proposal(vec![
            "npx skills add another/repo",
            "npx --yes skills add owner/repo"
        ]))
        .is_none()
    );
    assert_eq!(
        documented_skill_source(&proposal(vec![
            "npx skills add unrelated/tool",
            "npx skills add owner/repo@v2.0.0"
        ]))
        .as_deref(),
        Some("owner/repo@v2.0.0")
    );
}

#[test]
fn stale_recommendation_routes_fail_before_mutating_installation() -> TestResult {
    use clap::Parser as _;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let args = crate::cli::Cli::try_parse_from([
        "mo",
        "--home",
        home.path().to_str().ok_or("fixture home")?,
        "recommend",
        "owner/repo",
    ])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    for (action, expected) in [
        (RepositoryAction::Install, "skills_cli_unavailable"),
        (RepositoryAction::Adopt, "skill_not_found"),
        (RepositoryAction::Upgrade, "plan_stale"),
    ] {
        let result = public_skill_action(&ctx, "owner/repo", action);
        assert_eq!(
            result
                .err()
                .ok_or("stale source unexpectedly executed")?
                .code,
            expected
        );
        assert!(!ctx.state.exists());
    }
    Ok(())
}

#[test]
fn recommendation_offers_shared_install_only_with_supported_local_runner() -> TestResult {
    use clap::Parser as _;
    use std::fs;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let args = crate::cli::Cli::try_parse_from([
        "mo",
        "--home",
        home.path().to_str().ok_or("fixture home")?,
        "recommend",
        "owner/repo",
    ])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    let docs = json!({
        "query":"owner/repo","repository":"owner/repo",
        "manualInstall":[{"manager":"Skills CLI","commands":["npx skills add owner/repo@v2.0.0"]}]
    });
    let options = install_choices(&ctx, &docs, &[], Vec::new)?;
    assert_eq!(options.len(), 0);
    let node = ctx
        .home
        .join(".local/share/mise/installs/node/latest/bin/node");
    fs::create_dir_all(node.parent().ok_or("node folder")?)?;
    fs::copy("/usr/bin/true", &node)?;
    let package = ctx.home.join(".npm/_npx/test/node_modules/skills");
    fs::create_dir_all(package.join("bin"))?;
    fs::write(package.join("bin/cli.mjs"), "// fixture")?;
    fs::write(
        package.join("package.json"),
        br#"{"name":"skills","version":"1.9.0","bin":{"skills":"./bin/cli.mjs"}}"#,
    )?;
    let options = install_choices(&ctx, &docs, &[], Vec::new)?;
    assert!(
        matches!(options.as_slice(), [InstallChoice::PublicSkills(source, Ok(RepositoryAction::Install))]
        if source == "owner/repo@v2.0.0")
    );
    assert!(options[0].label().contains("用户共享目录"));
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn source_failure_guidance_is_specific_and_prefers_install_document_when_present() {
    for (reason, guidance) in [
        ("recommend_github_rate_limited", "等 GitHub 查询额度恢复"),
        (
            "recommend_github_not_found",
            "核对 GitHub 仓库名称和访问权限",
        ),
        ("recommend_source_unavailable", "核对来源地址和网络连接"),
    ] {
        let message = no_install_plan(&json!({"reason":reason}));
        assert!(
            message.contains("错误表现")
                && message.contains("错误原因")
                && message.contains(guidance)
        );
    }
    let repository = no_install_plan(&json!({"repository":"owner/repo"}));
    assert!(repository.contains("打开 https://github.com/owner/repo"));
    assert!(no_install_plan(&json!({})).contains("mo recommend OWNER/REPO"));
    let docs = no_install_plan(&json!({"documents":[{"path":"README.md"},{"path":"INSTALL.md"}]}));
    assert!(docs.contains("打开上面的 INSTALL.md"));
    assert!(
        no_install_plan(&json!({"documents":[{"path":"README.md"}]}))
            .contains("打开上面的 README.md")
    );
}

#[test]
fn recommendation_model_fallback_explains_each_recovery_action() {
    for (status, guidance) in [
        ("profile-unavailable", "检查设置"),
        ("transport-unavailable", "检查网络"),
        ("deadline", "可以重试"),
        ("http-status", "测试连接"),
        ("unverified-response", "按来源文档"),
        ("output-limit", "按来源文档"),
        ("suggested", "没有返回可引用"),
        ("keychain-authorization-required", "允许访问"),
        ("keychain-missing", "重新设置或绑定密钥"),
        ("keychain-unavailable", "检查已保存的密钥"),
    ] {
        let response = json!({"status":status,"candidates":[]});
        assert!(
            recommendation_model_hint(&response).is_some_and(|hint| hint.contains(guidance)),
            "{status}"
        );
    }
    assert!(
        recommendation_model_hint(&json!({"status":"suggested","candidates":[{"name":"widget"}]}))
            .is_none()
    );
    assert!(recommendation_model_hint(&json!({})).is_none());
}

#[test]
fn recommendation_confirms_a_concrete_plan_and_rechecks_installation_before_launch() -> TestResult {
    use crate::domain::{Catalog, CommandSpec, Provider};
    use clap::Parser as _;
    use std::{fs, os::unix::fs::PermissionsExt as _};
    let root = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "mo",
        "--home",
        root.path().to_str().ok_or("fixture home")?,
        "recommend",
        "widget",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let program = ctx.home.join("fixture-manager");
    let version = ctx.home.join("version");
    fs::write(&program,br#"#!/bin/sh
set -eu
case "$1" in
query)
  if [ -f "$2" ]; then printf '{"status":"present","version":"1.0.0"}\n'; else printf '{"status":"missing"}\n'; fi ;;
install) printf 'unexpected native write' > "$3" ;;
*) exit 9 ;;
esac
"#)?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;
    let launched = ctx.home.join("launched");
    let mut package = crate::test_support::package();
    package.name = "widget".into();
    package.provider = Provider::Command {
        query: CommandSpec {
            program: program.clone(),
            args: vec!["query".into(), version.display().to_string()],
        },
        available: None,
        install: Some(CommandSpec {
            program,
            args: vec![
                "install".into(),
                version.display().to_string(),
                launched.display().to_string(),
            ],
        }),
        upgrade: None,
        uninstall: None,
        synchronous: true,
    };
    fs::create_dir_all(ctx.catalog.parent().ok_or("catalog parent")?)?;
    fs::write(
        &ctx.catalog,
        serde_json::to_vec(&Catalog {
            schema_version: 1,
            packages: vec![package.clone()],
        })?,
    )?;
    let cancelled = install_selected_using(&ctx, &package, Some("2.0.0"), |prompt, explicit| {
        assert_eq!(prompt, "执行此安装计划");
        assert!(!explicit);
        Err(Error::attention("cancelled", "fixture cancelled"))
    });
    assert_eq!(
        cancelled.err().ok_or("cancelled plan accepted")?.code,
        "cancelled"
    );
    assert!(!version.exists() && !launched.exists());

    let mut confirmed = false;
    let changed = install_selected_using(&ctx, &package, Some("2.0.0"), |_, _| {
        confirmed = true;
        fs::write(&version, "1.0.0")?;
        Ok(())
    });
    assert!(confirmed && changed.is_err());
    assert_eq!(fs::read_to_string(version)?, "1.0.0");
    assert!(!launched.exists());
    Ok(())
}

#[test]
fn recommendation_evidence_requires_a_file_and_never_treats_a_directory_as_input() -> TestResult {
    let root_error = from_evidence("widget", Path::new("/"))
        .err()
        .ok_or("root accepted")?;
    assert_eq!(root_error.code, "recommend_evidence_invalid");
    let dir = tempfile::tempdir_in("/private/tmp")?;
    assert!(from_evidence("widget", dir.path()).is_err());
    let document = dir.path().join("evidence.json");
    std::fs::File::create(&document)?.set_len(EVIDENCE_LIMIT + 1)?;
    assert!(from_evidence("widget", &document).is_err());
    Ok(())
}
