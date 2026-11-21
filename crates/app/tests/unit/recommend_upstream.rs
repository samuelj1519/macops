use super::*;
use crate::test_support::TestResult;

#[test]
fn short_name_search_constrains_the_request_and_requires_exact_public_identity() {
    assert_eq!(
        search_url("Sample-skill").as_deref(),
        Some(
            "https://api.github.com/search/repositories?q=Sample-skill%20in%3Aname%20is%3Apublic%20archived%3Afalse&per_page=8"
        ),
    );
    for invalid in [
        "",
        "source/name",
        "@scope/name",
        "other repo",
        "name&token",
        "name%20",
    ] {
        assert!(search_url(invalid).is_none());
    }
    let found = search_matches(
        "sample-skill",
        &json!({"items":[
            {"full_name":"Example/Sample-skill","private":false,"archived":false,"fork":false},
            {"full_name":"example/sample-skill","private":false,"archived":false,"fork":false},
            {"full_name":"Second/sample-skill","private":false,"archived":false,"fork":true},
            {"full_name":"Private/sample-skill","private":true,"archived":false,"fork":false},
            {"full_name":"Archive/sample-skill","private":false,"archived":true,"fork":false},
            {"full_name":"Disabled/sample-skill","private":false,"archived":false,"disabled":true,"fork":false},
            {"full_name":"Other/sample-skill-plus","private":false,"archived":false,"fork":false},
            {"full_name":"a/b/sample-skill","private":false,"archived":false,"fork":false},
            {"full_name":"TooLate/sample-skill","private":false,"archived":false,"fork":false}
        ]}),
    );
    assert_eq!(
        found,
        [
            RepositoryMatch {
                name: "Example/Sample-skill".into(),
                fork: false
            },
            RepositoryMatch {
                name: "Second/sample-skill".into(),
                fork: true
            },
        ]
    );
    assert_eq!(
        search_matches("sample-skill", &json!({"items":"invalid"})),
        Vec::<RepositoryMatch>::new(),
    );
}

fn readme(excerpt: &[u8]) -> Value {
    json!({
        "html_url":"https://github.com/example/project/blob/main/README.md",
        "encoding":"base64",
        "content":base64::engine::general_purpose::STANDARD.encode(excerpt),
    })
}

#[test]
fn repository_documents_share_one_verified_commit() -> TestResult {
    let commit = "a".repeat(40);
    let mut requests = Vec::new();
    let result = repository_evidence("Owner/Example", |url| {
        requests.push(url.to_owned());
        if url.ends_with("/Owner/Example") {
            return Ok(json!({"full_name":"owner/example","private":false}));
        }
        if url.ends_with("/commits?per_page=1") {
            return Ok(json!([{"sha":commit}]));
        }
        assert!(url.ends_with(&format!("?ref={commit}")), "{url}");
        let path = if url.contains("/readme?") {
            "README.md"
        } else {
            "INSTALL.md"
        };
        Ok(json!({"path":path,"type":"file","encoding":"base64",
            "content":base64::engine::general_purpose::STANDARD.encode("官方安装说明")}))
    })?;
    assert_eq!(result.commit.as_str(), commit);
    assert_eq!(result.repository, "owner/example");
    assert_eq!(requests.len(), 4);
    assert_eq!(
        result
            .documents
            .iter()
            .map(|d| d.path.as_str())
            .collect::<Vec<_>>(),
        ["README.md", "INSTALL.md"]
    );
    assert_eq!(result.documents[1].content, "官方安装说明");
    assert!(
        document(
            &json!({"path":"another.md","type":"file","encoding":"base64","content":"dGVzdA=="}),
            Some("INSTALL.md")
        )
        .is_none()
    );
    assert!(
        document(
            &json!({"path":"../README.md","type":"file","encoding":"base64","content":"dGVzdA=="}),
            None
        )
        .is_none()
    );
    assert!(
        document(
            &json!({"path":"README.md","type":"symlink","encoding":"base64","content":"dGVzdA=="}),
            None
        )
        .is_none()
    );
    Ok(())
}

#[test]
fn repository_report_keeps_pinned_sources_when_the_model_profile_is_unavailable() -> TestResult {
    use clap::Parser;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let args = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("synthetic home")?,
        "recommend",
        "owner/repo",
    ])?;
    let ctx = crate::context::Context::from_cli(&args, &mut vec![])?;
    let commit = "b".repeat(40);
    let fetch = |url: &str| {
        if url.ends_with("/owner/repo") {
            Ok(json!({"full_name":"owner/repo","private":false}))
        } else if url.ends_with("/commits?per_page=1") {
            Ok(json!([{"sha":commit}]))
        } else if url.contains("/contents/INSTALL.md?") {
            Ok(json!({"path":"INSTALL.md","type":"file","encoding":"base64","content":"dGVzdA=="}))
        } else {
            Err(unavailable())
        }
    };
    let data = repository_review(&ctx, "owner/repo", false, fetch);
    assert_eq!(data["evidenceStatus"], "public-commit");
    assert_eq!(
        data["documents"][0]["url"],
        format!("https://github.com/owner/repo/blob/{commit}/INSTALL.md")
    );
    assert_eq!(data["canExecute"], false);
    assert_eq!(data["model"]["status"], "local-only");
    let config = crate::native_skills_files::private(&ctx.config)?;
    let profile = config.path.join("recommend-model.json");
    std::fs::write(&profile, b"broken")?;
    assert_eq!(
        repository_review(&ctx, "owner/repo", true, fetch)["model"]["status"],
        "profile-unavailable"
    );
    std::fs::write(
        &profile,
        serde_json::to_vec(&json!({
            "schemaVersion":1,"enabled":true,"endpoint":"https://models.example.test/chat/completions",
            "model":"example","account":format!("setup-{}", "a".repeat(64))
        }))?,
    )?;
    assert_eq!(
        repository_review(&ctx, "owner/repo", true, fetch)["model"]["status"],
        "local-only"
    );
    let absent = repository_review(&ctx, "owner/repo", true, |_| Err(unavailable()));
    assert_eq!(absent["evidenceStatus"], "source-unavailable");
    assert_eq!(absent["persistentWrites"], false);
    Ok(())
}

#[test]
fn repository_report_refuses_missing_revisions_and_uninspectable_documents() -> TestResult {
    use clap::Parser;
    let home = tempfile::tempdir_in("/private/tmp")?;
    let args = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("home")?,
        "recommend",
        "owner/repo",
    ])?;
    let ctx = Context::from_cli(&args, &mut vec![])?;
    for head in [json!([]), json!([{"sha":"not-a-git-commit"}])] {
        let result = repository_review(&ctx, "owner/repo", false, |url| {
            if url.ends_with("/owner/repo") {
                Ok(json!({"full_name":"owner/repo","private":false}))
            } else {
                Ok(head.clone())
            }
        });
        assert_eq!(result["evidenceStatus"], "source-unavailable");
    }
    let result = repository_review(&ctx, "owner/repo", false, |url| {
        if url.ends_with("/owner/repo") {
            Ok(json!({"full_name":"owner/repo","private":false}))
        } else if url.ends_with("/commits?per_page=1") {
            Ok(json!([{"sha":"c".repeat(40)}]))
        } else {
            Err(unavailable())
        }
    });
    assert_eq!(result["evidenceStatus"], "documents-unavailable");
    assert_eq!(result["documents"], json!([]));
    for payload in [
        json!({"path":".","type":"file","encoding":"base64","content":"dGVzdA=="}),
        json!({"path":"INSTALL.md","type":"file","encoding":"binary","content":"dGVzdA=="}),
        json!({"path":"INSTALL.md","type":"file","encoding":"base64","content":"?"}),
        json!({"path":"INSTALL.md","type":"file","encoding":"base64","content":"/w=="}),
        json!({"path":"INSTALL.md","type":"file","encoding":"base64",
            "content":base64::engine::general_purpose::STANDARD.encode(vec![b'a'; 64 * 1024 + 1])}),
    ] {
        assert!(document(&payload, None).is_none());
    }
    Ok(())
}

#[test]
fn published_readme_is_bound_to_the_selected_public_repository() -> TestResult {
    let mut urls = Vec::new();
    let answer = inspect("Project", "Example/Project", |url| {
        urls.push(url.to_owned());
        Ok(match url {
            "https://api.github.com/repos/Example/Project/readme" => readme(
                b"## Install

$ brew install project
npm install --global project

Description",
            ),
            "https://formulae.brew.sh/api/formula/project.json" => {
                json!({"name":"project","tap":"homebrew/core","urls":{"stable":{"url":"https://github.com/example/project/archive/refs/tags/v1.0.0.tar.gz"}}})
            }
            "https://registry.npmjs.org/project/latest" => {
                json!({"name":"project","version":"1.0.0","repository":{"url":"git+https://github.com/example/project.git"}})
            }
            _ => json!({"full_name":"example/project","private":false}),
        })
    })?;
    assert_eq!(
        urls,
        [
            "https://api.github.com/repos/Example/Project",
            "https://api.github.com/repos/Example/Project/readme",
            "https://formulae.brew.sh/api/formula/project.json",
            "https://registry.npmjs.org/project/latest",
        ]
    );
    assert_eq!(
        answer["analysis"]["findings"]
            .as_array()
            .ok_or("findings")?
            .len(),
        2
    );
    assert_eq!(answer["evidenceStatus"], "published-by-selected-repository");
    assert_eq!(answer["canExecute"], false);
    let matches = answer["publisherChecks"]
        .as_array()
        .ok_or("publication checks")?;
    assert_eq!(matches.len(), 2);
    assert!(
        matches
            .iter()
            .all(|item| item["status"] == "upstream-matched")
    );
    Ok(())
}

#[test]
fn mismatched_repositories_redirect_shape_and_bad_readme_are_rejected() {
    for repo in [
        "",
        "owner",
        "a/b/c",
        "../repo",
        "user/name#fragment",
        "owner/%2E",
        "ssh://host/path",
        &format!("user/{}", "x".repeat(101)),
    ] {
        assert!(repository_id(repo).is_err(), "{repo}");
    }
    for private in [true, false] {
        let foreign = if private {
            "example/project"
        } else {
            "stranger/project"
        };
        assert!(
            inspect("Project", "example/project", |_| Ok(
                json!({"full_name":foreign,"private":private})
            ))
            .is_err()
        );
    }
    for (case, replacement) in [
        (
            "wrong-html",
            json!("https://github.com/not-this/repo/blob/main/README.md"),
        ),
        ("wrong-encoding", json!("unknown")),
        ("wrong-base64", json!("?")),
        (
            "non-utf8",
            json!(base64::engine::general_purpose::STANDARD.encode([0xff])),
        ),
        ("missing-url", Value::Null),
        ("missing-content", Value::Null),
    ] {
        let mut value = readme(b"npm install project");
        let field = match case {
            "wrong-html" | "missing-url" => "html_url",
            "wrong-encoding" => "encoding",
            _ => "content",
        };
        value[field] = replacement;
        assert!(
            inspect("Project", "example/project", |url| {
                Ok(if url.ends_with("/readme") {
                    value.clone()
                } else {
                    json!({"full_name":"example/project","private":false})
                })
            })
            .is_err(),
            "{case}"
        );
    }
    assert!(inspect("Project", "example/project", |_| Err(unavailable())).is_err());
}

#[test]
fn website_excerpts_respect_line_and_total_budgets() {
    let text = format!(
        "npm install {}\n{}",
        "x".repeat(1200),
        "npm install ok\n".repeat(140)
    );
    assert_eq!(relevant_lines(&text).lines().count(), 128);
    let text = format!("npm install {}\n", "x".repeat(1000)).repeat(10);
    let output = relevant_lines(&text);
    assert!(output.len() <= 8192);
    assert_eq!(output.lines().count(), 8);
    assert_eq!(relevant_lines("disclaimer\n\n## Header"), "");
}

#[test]
fn registry_label_is_only_granted_when_metadata_binds_the_same_repository() -> TestResult {
    let recipe = Candidate::Npm {
        name: "@scope/demo".into(),
        version: None,
        scope: policy::Scope::Global,
        development: false,
    };
    assert_eq!(
        registry_url(&recipe).as_ref().map(|(url, _)| url.as_str()),
        Some("https://registry.npmjs.org/@scope%2Fdemo/latest")
    );
    let other_repo = json!({"name":"@scope/demo","version":"2.0.0","repository":{"url":"https://github.com/other/repo.git"}});
    assert!(!publishes_from(&recipe, "example/project", &other_repo));
    let same = json!({"name":"@scope/demo","version":"2.0.0","repository":"git://github.com/example/project"});
    assert!(publishes_from(&recipe, "example/project", &same));
    let brew = Candidate::Brew {
        name: "demo".into(),
        cask: false,
    };
    for address in [
        "https://github.com/other/project/archive/v1.2.tgz",
        "https://github.com/example/project",
        "https://github.com/example/project/archive/../v2.tgz",
        "https://github.com/example/project/archive/?redirect",
        "https://other.example.org/project/archive/v1.tar.gz",
    ] {
        let data = json!({"name":"demo","tap":"homebrew/core","urls":{"stable":{"url":address}}});
        assert!(
            !publishes_from(&brew, "example/project", &data),
            "{address}"
        );
    }
    let unavailable_checks = inspect("Project", "example/project", |url| {
        if url.ends_with("/readme") {
            Ok(readme(
                b"npm install project\nbrew install project\nnpx tsc",
            ))
        } else if url == "https://api.github.com/repos/example/project" {
            Ok(json!({"full_name":"example/project","private":false}))
        } else {
            Err(unavailable())
        }
    })?;
    let checks = unavailable_checks["publisherChecks"]
        .as_array()
        .ok_or("check rows")?;
    assert_eq!(checks.len(), 2);
    assert!(
        checks
            .iter()
            .all(|item| item["status"] == "source-unavailable")
    );
    assert_eq!(
        unavailable_checks["analysis"]["findings"]
            .as_array()
            .ok_or("findings")?
            .len(),
        3
    );
    Ok(())
}

#[test]
fn registry_check_keeps_requested_and_current_npm_versions_separate() -> TestResult {
    let mut inspected = Vec::new();
    let response = inspect("Project", "example/project", |url| {
        inspected.push(url.to_owned());
        Ok(match url {
            "https://api.github.com/repos/example/project" =>
                json!({"full_name":"example/project","private":false}),
            "https://api.github.com/repos/example/project/readme" =>
                readme(b"brew install homebrew/core/project\nnpm install -g project\nnpm install project\nnpm install -g project@1.0.0\nnpm install project@1.x"),
            "https://registry.npmjs.org/project/latest" | "https://registry.npmjs.org/project/1.0.0" =>
                json!({"name":"project","version":"2.0.0","repository":"https://github.com/example/project"}),
            "https://formulae.brew.sh/api/formula/project.json" =>
                json!({"name":"project","tap":"homebrew/core","homepage":"https://github.com/example/project"}),
            _ => return Err(unavailable()),
        })
    })?;
    assert_eq!(
        inspected
            .iter()
            .filter(|url| url.ends_with("/latest"))
            .count(),
        1
    );
    assert!(inspected.iter().any(|url| url.ends_with("/1.0.0")));
    assert!(inspected.iter().any(|url| url.ends_with("project.json")));
    assert!(!inspected.iter().any(|url| url.ends_with("/1.x")));
    let claims = response["publisherChecks"].as_array().ok_or("checks")?;
    assert!(
        claims
            .iter()
            .any(|row| row["evidenceScope"] == "requested-version"
                && row["status"] == "upstream-unverified")
    );
    assert!(claims.iter().any(
        |row| row["evidenceScope"] == "current-formula" && row["status"] == "upstream-matched"
    ));
    assert!(claims.iter().any(
        |row| row["evidenceScope"] == "current-release" && row["status"] == "upstream-matched"
    ));
    Ok(())
}

#[test]
fn publication_checks_bound_request_count_and_keep_version_evidence_specific() -> TestResult {
    let exact = Candidate::Npm {
        name: "project".into(),
        version: Some("1.2.3-beta.2".into()),
        scope: policy::Scope::Global,
        development: false,
    };
    let published = json!({"name":"project","version":"1.2.3-beta.2","repository":{"url":"git+ssh://git@github.com/example/project.git"}});
    assert!(publishes_from(&exact, "example/project", &published));
    assert!(
        registry_url(&Candidate::Brew {
            name: "custom/tap/project".into(),
            cask: false
        })
        .is_none()
    );
    assert!(
        registry_url(&Candidate::Brew {
            name: "project".into(),
            cask: true
        })
        .is_none()
    );
    let mut documented = String::new();
    for i in 1..15 {
        use std::fmt::Write;
        writeln!(documented, "npm install package{i}")?;
    }
    let mut registry_reads = 0;
    let result = inspect("Project", "example/project", |path| {
        if path.ends_with("/readme") {
            Ok(readme(documented.as_bytes()))
        } else if path == "https://api.github.com/repos/example/project" {
            Ok(json!({"full_name":"example/project","private":false}))
        } else {
            registry_reads += 1;
            Ok(json!({"name":"not-the-expected-package"}))
        }
    })?;
    assert_eq!(registry_reads, 6);
    assert_eq!(
        result["publisherChecks"].as_array().ok_or("checks")?.len(),
        6
    );
    assert_eq!(
        result["analysis"]["findings"]
            .as_array()
            .ok_or("findings")?
            .len(),
        14
    );
    Ok(())
}

fn codex_document(path: &str, content: &str) -> Value {
    json!({"path":path,"type":"file","encoding":"base64",
        "content":base64::engine::general_purpose::STANDARD.encode(content)})
}

fn documented_market(commit: &str) -> Result<RepositoryEvidence> {
    Ok(RepositoryEvidence {
        repository: "owner/repo".into(),
        commit: crate::policy::source_git::ObjectId::parse(commit)?,
        default_branch: Some("main".into()),
        documents: vec![Document {
            path: "INSTALL.md".into(),
            content: "codex plugin marketplace add owner/repo --ref main\ncodex plugin add fixture@market\n".into(),
        }],
    })
}

fn codex_catalog(url: &str, reference: &str) -> Value {
    json!({"name":"market","plugins":[{"name":"fixture",
        "source":{"source":"url","url":url,"ref":reference}}]})
}

#[test]
fn codex_market_observation_binds_each_manifest_to_its_own_revision() -> TestResult {
    let origin = "a".repeat(40);
    let evidence = documented_market(&origin)?;
    for commit in [origin.clone(), "b".repeat(40)] {
        let mut observed = vec![];
        let catalog = codex_catalog("https://github.com/owner/repo.git", "main").to_string();
        let plugin = r#"{"name":"fixture","version":"0.4.1"}"#;
        let report = codex_marketplace(&evidence, &mut |url| {
            observed.push(url.to_owned());
            if url.ends_with(&format!("{CODEX_MARKETPLACE}?ref={origin}")) {
                Ok(codex_document(CODEX_MARKETPLACE, &catalog))
            } else if url.ends_with("/commits/main") {
                Ok(json!({"sha":commit}))
            } else if url.ends_with(&format!("{CODEX_PLUGIN}?ref={commit}")) {
                Ok(codex_document(CODEX_PLUGIN, plugin))
            } else {
                Err(unavailable())
            }
        })
        .ok_or("expected Codex market evidence")?;
        assert_eq!(observed.len(), 3, "{observed:?}");
        assert_eq!(report["marketplaceCommit"], origin);
        assert_eq!(report["payloadCommit"], commit);
        assert_eq!(report["payloadRef"], "main");
        assert_eq!(report["declaredVersion"], "0.4.1");
        assert_eq!(report["pluginManifest"], CODEX_PLUGIN);
        assert_eq!(report["instructions"], "INSTALL.md");
        assert_eq!(
            report["status"],
            if origin == commit {
                "same-commit-observed"
            } else {
                "divergent-commits"
            }
        );
        assert_eq!(
            report["pluginManifestSha256"],
            format!("{:x}", Sha256::digest(plugin.as_bytes()))
        );
    }
    Ok(())
}

#[test]
fn codex_market_evidence_rejects_foreign_ambiguous_and_mutable_metadata_errors() -> TestResult {
    let revision = "a".repeat(40);
    let evidence = documented_market(&revision)?;
    let valid = codex_catalog("https://github.com/owner/repo", &revision);
    let mut duplicates = valid.clone();
    let detached = duplicates["plugins"][0].clone();
    duplicates["plugins"]
        .as_array_mut()
        .ok_or("plugins")?
        .push(detached);
    let mut member = valid.clone();
    member["plugins"][0]["source"]["path"] = json!("plugins/member");
    let mut different_sha = valid.clone();
    different_sha["plugins"][0]["source"]["sha"] = json!("b".repeat(40));
    for market in [
        json!({"name":"market","plugins":[]}),
        codex_catalog("https://github.com/other/project.git", "main"),
        codex_catalog("https://github.com/owner/repo.git", "../main"),
        duplicates,
        member,
        different_sha,
    ] {
        let mut calls = 0;
        assert!(
            codex_marketplace(&evidence, &mut |url| {
                calls += 1;
                assert!(url.contains(CODEX_MARKETPLACE));
                Ok(codex_document(CODEX_MARKETPLACE, &market.to_string()))
            })
            .is_none()
        );
        assert_eq!(calls, 1);
    }
    for (plugin, present) in [
        (r#"{"name":"fixture","version":"1.0.0"}"#, true),
        (r#"{"name":"foreign","version":"1.0.0"}"#, false),
        (r#"{"name":"fixture","version":"unexpected value"}"#, false),
    ] {
        let report = codex_marketplace(&evidence, &mut |url| {
            if url.ends_with(&format!("{CODEX_MARKETPLACE}?ref={revision}")) {
                Ok(codex_document(CODEX_MARKETPLACE, &valid.to_string()))
            } else {
                assert!(url.ends_with(&format!("{CODEX_PLUGIN}?ref={revision}")));
                Ok(codex_document(CODEX_PLUGIN, plugin))
            }
        });
        assert_eq!(report.is_some(), present);
    }
    Ok(())
}

#[test]
fn codex_market_evidence_binds_the_documented_market_ref_and_the_complete_route() -> TestResult {
    let head = "a".repeat(40);
    let mut evidence = documented_market(&head)?;
    evidence.documents.insert(
        0,
        Document {
            path: "README.md".into(),
            content: "codex plugin marketplace add owner/repo --ref main".into(),
        },
    );
    let catalog = codex_catalog("https://github.com/owner/repo.git", &head).to_string();
    let mut reads = 0;
    assert!(
        codex_marketplace(&evidence, &mut |url| {
            reads += 1;
            if url.contains(CODEX_MARKETPLACE) {
                Ok(codex_document(CODEX_MARKETPLACE, &catalog))
            } else {
                assert!(url.contains(CODEX_PLUGIN));
                Ok(codex_document(
                    CODEX_PLUGIN,
                    r#"{"name":"fixture","version":"1.0.0"}"#,
                ))
            }
        })
        .is_some_and(|offer| offer["instructions"] == "INSTALL.md")
    );
    assert_eq!(reads, 2);
    evidence.documents.remove(0);
    for selector in ["--ref release-old", "--ref main --unknown", "--ref"] {
        evidence.documents[0].content = format!(
            "codex plugin marketplace add owner/repo {selector}\ncodex plugin add fixture@market\n"
        );
        assert!(
            codex_marketplace(&evidence, &mut |url| {
                assert!(!url.contains(CODEX_MARKETPLACE), "{url}");
                Err(unavailable())
            })
            .is_none()
        );
    }
    Ok(())
}

#[test]
fn claude_offer_requires_same_commit_documents_and_exact_native_user_scope() -> TestResult {
    use crate::claude_install_source::{MARKET, PLUGIN};
    use crate::policy::source_git::ObjectId;
    let commit = ObjectId::parse(&"b".repeat(40))?;
    let docs = |install: &str| RepositoryEvidence {
        repository: "example/fixture".into(),
        commit: commit.clone(),
        default_branch: None,
        documents: vec![Document {
            path: "INSTALL.md".into(),
            content: format!("claude plugin marketplace add example/fixture\n{install}\n"),
        }],
    };
    for (line, expected) in [
        ("claude plugin install plugin@market", true),
        ("claude plugin install plugin@market --scope user", true),
        ("claude plugin install plugin@market --scope project", false),
        ("claude plugin install other@market", false),
        ("claude plugin install plugin@market && script", false),
    ] {
        let mut calls = vec![];
        let result = claude_marketplace(&docs(line), &mut |url| {
            calls.push(url.to_owned());
            assert!(url.ends_with(commit.as_str()), "{url}");
            let (path, bytes) = if url.contains(MARKET) {
                (
                    MARKET,
                    br#"{"name":"market","plugins":[{"name":"plugin","source":"./"}]}"#.as_slice(),
                )
            } else {
                assert!(url.contains(PLUGIN));
                (PLUGIN, br#"{"name":"plugin","version":"1.0.0"}"#.as_slice())
            };
            Ok(json!({"path":path,"type":"file","encoding":"base64",
                "content":base64::engine::general_purpose::STANDARD.encode(bytes)}))
        });
        assert_eq!(result.is_some(), expected);
        assert_eq!(calls.len(), 2);
    }
    let mut foreign = docs("claude plugin install plugin@market");
    foreign.documents[0].content = "claude plugin marketplace add foreign/fixture".into();
    assert!(claude_marketplace(&foreign, &mut |_| panic!("foreign source cannot fetch")).is_none());
    Ok(())
}

#[test]
fn repository_review_preserves_a_complete_claude_offer_and_drops_partial_evidence() -> TestResult {
    use crate::claude_install_source::{MARKET, PLUGIN};
    use clap::Parser as _;
    let root = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        root.path().to_str().ok_or("home")?,
        "recommend",
        "fixture",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let commit = "b".repeat(40);
    let readme =
        b"claude plugin marketplace add example/fixture\nclaude plugin install plugin@market\n";
    for missing in [None, Some(MARKET), Some(PLUGIN)] {
        let outcome = repository_review(&ctx, "example/fixture", false, |url| {
            if url.ends_with("/example/fixture") {
                return Ok(json!({"full_name":"example/fixture","private":false}));
            }
            if url.contains("/commits") {
                return Ok(json!([{"sha":commit}]));
            }
            if let Some(path) = missing
                && url.contains(path)
            {
                return Err(unavailable());
            }
            let (path, bytes) = if url.contains("/readme") {
                ("README.md", readme.as_slice())
            } else if url.contains("INSTALL.md") {
                ("INSTALL.md", readme.as_slice())
            } else if url.contains(MARKET) {
                (
                    MARKET,
                    br#"{"name":"market","plugins":[{"name":"plugin","source":"./"}]}"#.as_slice(),
                )
            } else {
                assert!(url.contains(PLUGIN), "{url}");
                (PLUGIN, br#"{"name":"plugin","version":"1.0.0"}"#.as_slice())
            };
            Ok(
                json!({"path":path,"type":"file","encoding":"base64","content":base64::engine::general_purpose::STANDARD.encode(bytes)}),
            )
        });
        assert_eq!(outcome["canExecute"], false);
        assert_eq!(outcome["persistentWrites"], false);
        assert_eq!(outcome["evidenceStatus"], "public-commit");
        assert_eq!(outcome["claudeMarketplace"].is_object(), missing.is_none());
        if missing.is_none() {
            assert_eq!(outcome["claudeMarketplace"]["commit"], commit);
        }
    }
    assert!(!ctx.state.exists());
    Ok(())
}
