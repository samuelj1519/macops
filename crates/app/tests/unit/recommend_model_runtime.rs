use super::*;
use crate::test_support::TestResult;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

const FIXTURE_TOKEN: &str = "synthetic-token-do-not-log";

#[test]
fn repository_proposals_require_bound_documents_and_a_closed_shape() -> TestResult {
    let candidate = json!({
        "kind":"plugin","adapter":"codex-plugin","name":"example",
        "scope":"global","document":0,"version":null,"summary":"上游插件市场"
    });
    let response = |candidate: Value, finish: &str| {
        let content = json!({"candidates":[candidate]}).to_string();
        format!(
            "{}\n200",
            json!({"choices":[{"finish_reason":finish,"message":{"role":"assistant","content":content}}]})
        )
    };
    let result = parse_proposals(
        response(candidate.clone(), "stop").as_bytes(),
        FIXTURE_TOKEN,
        1,
    )
    .map_err(Failure::label)?;
    assert_eq!(result.len(), 1);
    for channel in [
        "npx",
        "bun",
        "cargo",
        "uv",
        "claude-plugin",
        "cursor-plugin",
    ] {
        let mut proposed = candidate.clone();
        proposed["adapter"] = json!(channel);
        let parsed = parse_proposals(response(proposed, "stop").as_bytes(), FIXTURE_TOKEN, 1)
            .map_err(Failure::label)?;
        assert_eq!(serde_json::to_value(&parsed[0])?["adapter"], channel);
    }
    for (key, value) in [
        ("document", json!(9)),
        ("adapter", json!("arbitrary")),
        ("summary", json!(FIXTURE_TOKEN)),
        ("scope", json!("unrestricted")),
        ("command", json!("fixture")),
        ("version", json!("bad\nline")),
    ] {
        let mut bad = candidate.clone();
        bad[key] = value;
        assert!(parse_proposals(response(bad, "stop").as_bytes(), FIXTURE_TOKEN, 1).is_err());
    }
    assert!(parse_proposals(response(candidate, "length").as_bytes(), FIXTURE_TOKEN, 1).is_err());
    Ok(())
}

#[test]
fn repository_model_sends_only_public_evidence_with_an_endpoint_bound_key() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let profile = serde_json::from_value::<crate::recommend::setup::Profile>(json!({
        "schemaVersion":1,"enabled":true,"endpoint":"https://models.example.test/v1/chat/completions",
        "model":"explicit-model","account":"setup-test"
    }))?;
    let security = script(
        temp.path(),
        "key",
        &format!(
            "[ \"$3\" = '{}' ] && [ \"$5\" = setup-test ]\nprintf '%s\\n' '{}'",
            profile.service(temp.path()),
            FIXTURE_TOKEN
        ),
    )?;
    let http = script(
        temp.path(),
        "http",
        r#"
IFS= read -r header
[ "$header" = 'Authorization: Bearer synthetic-token-do-not-log' ]
found=
last=
for arg; do
  case "$arg" in *synthetic-token-do-not-log*|*/private/tmp/*) exit 7 ;; esac
  case "$arg" in *public-evidence-fixture*explicit-model*|*explicit-model*public-evidence-fixture*) found=yes ;; esac
  last=$arg
done
[ "$found" = yes ] && [ "$last" = 'https://models.example.test/v1/chat/completions' ]
printf '%s\n200' '{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"{\"candidates\":[{\"kind\":\"skill\",\"adapter\":\"skills\",\"name\":\"fixture\",\"scope\":\"global\",\"document\":0,\"summary\":\"上游安装说明\"}]}"}}]}'
"#,
    )?;
    let result = repository_proposals(
        &Clients {
            credentials: &|service, account, deadline| {
                credentials(&security, service, account, deadline)
            },
            curl: &http,
        },
        &profile,
        temp.path(),
        "owner/fixture",
        &[("INSTALL.md", "public-evidence-fixture")],
        Instant::now() + Duration::from_secs(4),
    )
    .map_err(Failure::label)?;
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "fixture");
    Ok(())
}

#[test]
fn connection_check_reuses_the_confirmed_endpoint_and_accepts_only_a_chat_response() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let response = script(
        temp.path(),
        "check",
        r#"
IFS= read -r header
[ "$header" = 'Authorization: Bearer synthetic-token-do-not-log' ]
for arg; do last=$arg; done
[ "$last" = 'https://models.example.test/chat/completions' ] || [ "$last" = 'https://api.deepseek.com/chat/completions' ]
printf '%s\n200' '{"choices":[{"message":{"content":"OK"}}]}'
"#,
    )?;
    let bad = script(temp.path(), "bad", "printf '%s\\n200' '{}'")?;
    let unauthorized = script(temp.path(), "unauthorized", "printf '%s\\n401' '{}'")?;
    for endpoint in [
        policy::ENDPOINT,
        "https://models.example.test/chat/completions",
    ] {
        let profile = serde_json::from_value::<crate::recommend::setup::Profile>(json!({
            "schemaVersion":1,"enabled":true,"endpoint":endpoint,"model":"fixture","account":"setup-test"
        }))?;
        let deadline = || Instant::now() + Duration::from_secs(4);
        test_connection(&response, &profile, FIXTURE_TOKEN, deadline()).map_err(Failure::label)?;
        assert_eq!(
            test_connection(&bad, &profile, FIXTURE_TOKEN, deadline()),
            Err(Failure::Unverified)
        );
        assert_eq!(
            test_connection(&unauthorized, &profile, FIXTURE_TOKEN, deadline()),
            Err(Failure::HttpStatus)
        );
    }
    Ok(())
}

fn script(base: &Path, name: &str, body: &str) -> Result<PathBuf, io::Error> {
    let path = base.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn brief() -> crate::error::Result<PublicAnalysis> {
    crate::recommend::candidate::public_analysis("jq")?
        .ok_or_else(|| crate::error::Error::new("fixture", "expected public input"))
}

#[test]
fn model_runtime_passes_a_fixture_header_by_pipe_and_validates_the_only_answer() -> TestResult {
    let f = tempfile::tempdir_in("/private/tmp")?;
    let security = script(
        f.path(),
        "key",
        r#"[ "$1" = find-generic-password ] && [ "$3" = org.macops.recommend.deepseek ] &&
[ "$5" = separate-profile ] && [ "$6" = -w ]
printf 'synthetic-token-do-not-log\n'"#,
    )?;
    let curl = script(
        f.path(),
        "http",
        r#"
[ "$1" = --disable ]
IFS= read -r auth
[ "$auth" = 'Authorization: Bearer synthetic-token-do-not-log' ]
last_arg=""
for argument; do
  [ "$argument" != --location ] && [ "$argument" != synthetic-token-do-not-log ]
  last_arg="$argument"
done
[ "$last_arg" = 'https://api.deepseek.com/chat/completions' ]
printf '%s\n200' '{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"{\"notes\":[{\"candidate\":0,\"summary\":\"Homebrew 适用于全局安装\"}]}"}}]}'
"#,
    )?;
    let clients = Clients {
        credentials: &|service, account, deadline| {
            credentials(&security, service, account, deadline)
        },
        curl: &curl,
    };
    let started = Instant::now();
    let result = compare(
        &clients,
        brief()?.facts(),
        "separate-profile",
        started + Duration::from_secs(4),
    )
    .map_err(Failure::label)?;
    assert_eq!(result.notes[0].summary, "Homebrew 适用于全局安装");
    assert_eq!(result.notes[0].candidate, 0);
    assert!(!serde_json::to_string(&result)?.contains(FIXTURE_TOKEN));
    Ok(())
}

#[test]
fn model_runtime_recovers_from_missing_or_excessive_subprocess_data() -> TestResult {
    let f = tempfile::tempdir_in("/private/tmp")?;
    let curl = script(f.path(), "curl-unused", "exit 109")?;
    let account = "not-use-real-keychain";
    for (case, body, expected) in [
        ("missing", "exit 44", Failure::Keychain),
        ("invalid", "printf 'key-with-spaces fix'", Failure::Keychain),
        (
            "too-long",
            "/usr/bin/head -c 4096 /dev/zero",
            Failure::OutputLimit,
        ),
    ] {
        let security = script(f.path(), case, body)?;
        let clients = Clients {
            credentials: &|service, account, deadline| {
                credentials(&security, service, account, deadline)
            },
            curl: &curl,
        };
        let end = Instant::now() + Duration::from_secs(3);
        let error = compare(&clients, brief()?.facts(), account, end)
            .err()
            .ok_or("accepted invalid fixture key")?;
        assert_eq!(error, expected, "{case}");
    }
    let security = script(f.path(), "key", "printf 'synthetic-token-do-not-log\\n'")?;
    let curl = script(f.path(), "long-output", "/usr/bin/head -c 200000 /dev/zero")?;
    let clients = Clients {
        credentials: &|service, account, deadline| {
            credentials(&security, service, account, deadline)
        },
        curl: &curl,
    };
    let end = Instant::now() + Duration::from_secs(3);
    assert_eq!(
        compare(&clients, brief()?.facts(), account, end).err(),
        Some(Failure::OutputLimit)
    );
    Ok(())
}

#[test]
fn model_runtime_enforces_one_deadline_and_reaps_its_own_processes() -> TestResult {
    for waiting in ["keychain", "http"] {
        let f = tempfile::tempdir_in("/private/tmp")?;
        let pidfile = f.path().join("fake-pid");
        let wait = format!(
            "printf '%s' $$ > '{}'\nexec /bin/sleep 5",
            pidfile.display()
        );
        let key = "printf 'synthetic-token-do-not-log\\n'";
        let security = script(
            f.path(),
            "key",
            if waiting == "keychain" { &wait } else { key },
        )?;
        let curl = script(
            f.path(),
            "http",
            if waiting == "http" { &wait } else { "exit 9" },
        )?;
        let clients = Clients {
            credentials: &|service, account, deadline| {
                credentials(&security, service, account, deadline)
            },
            curl: &curl,
        };
        let started = Instant::now();
        let result = compare(
            &clients,
            brief()?.facts(),
            "synthetic",
            started + Duration::from_secs(2),
        );
        assert_eq!(result.err(), Some(Failure::Deadline));
        assert!(started.elapsed() < Duration::from_secs(8));
        let pid = fs::read_to_string(pidfile)
            .map_err(|e| format!("{waiting} fixture PID unavailable: {e}"))?
            .parse()?;
        assert!(!crate::platform::process::group_alive(pid), "{waiting}");
    }
    Ok(())
}

#[test]
fn model_runtime_rejects_bad_status_or_a_reply_with_fixture_credentials() {
    let response = |summary: &str| {
        json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":
            json!({"notes":[{"candidate":0,"summary":summary}]}).to_string()
        }}]})
    };
    for suffix in ["", "\n301", "\n429", "\n0", "\n2na", "\n20x"] {
        let output = format!("{}{suffix}", response("本地使用"));
        assert!(parse_annotation(output.as_bytes(), FIXTURE_TOKEN, 1).is_err());
    }
    let output = format!("{}\n200", response(FIXTURE_TOKEN));
    assert_eq!(
        parse_annotation(output.as_bytes(), FIXTURE_TOKEN, 1).err(),
        Some(Failure::Unverified)
    );
    let body = format!("{}\n200", response(FIXTURE_TOKEN));
    let body = body.replace("synthetic-token", r"synthetic\u002dtoken");
    assert!(!body.contains(FIXTURE_TOKEN));
    assert_eq!(
        parse_annotation(body.as_bytes(), FIXTURE_TOKEN, 1).err(),
        Some(Failure::Unverified)
    );
}

#[test]
fn model_runtime_reaps_the_leader_after_stopping_closed_output_group_members() -> TestResult {
    let f = tempfile::tempdir_in("/private/tmp")?;
    let group = f.path().join("group");
    let script = script(
        f.path(),
        "group-leader",
        &format!(
            "/bin/sleep 5 >/dev/null 2>&1 &\nprintf '%s' $$ > '{}'\nprintf 'public result'",
            group.display()
        ),
    )?;
    let mut native = Command::new(script);
    let output =
        run(&mut native, Instant::now() + Duration::from_secs(3), 64).map_err(Failure::label)?;
    assert_eq!(output, b"public result");
    let pgid = fs::read_to_string(group)?.parse()?;
    let limit = Instant::now() + Duration::from_secs(2);
    while crate::platform::process::group_alive(pgid) && Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!crate::platform::process::group_alive(pgid));
    Ok(())
}

#[test]
fn public_http_reader_requires_its_selected_service_and_an_explicit_success_status() -> TestResult {
    let f = tempfile::tempdir_in("/private/tmp")?;
    for public in [
        "https://api.github.com/repos/example/project",
        "https://api.github.com/search/repositories?q=example",
    ] {
        for (status, body, valid) in [
            (
                "200",
                r#"{"full_name":"example/project","private":false}"#,
                true,
            ),
            ("301", r#"{"full_name":"example/project"}"#, false),
            ("200", "bad json", false),
        ] {
            let runner = script(
                f.path(),
                "public-http",
                &format!(
                    r#"[ "$1" = --disable ]
last=""
for field; do
  [ "$field" != --location ] && [ "$field" != --data-binary ]
  case "$field" in Authorization*) exit 51;; esac
  if [ "$field" = 'Accept: application/vnd.github+json' ]; then accept=1; fi
  last="$field"
done
[ "$accept" = 1 ]
[ "$last" = '{public}' ]
printf '%s\n%s' '{body}' '{status}'"#
                ),
            )?;
            let deadline = Instant::now() + Duration::from_secs(3);
            let response = fetch_public_with(&runner, public, deadline);
            assert_eq!(response.is_ok(), valid);
            if let Ok(value) = response {
                assert_eq!(value["full_name"], "example/project");
            }
            assert!(fetch_public_with(&runner, "https://other.example.org", deadline).is_err());
        }
    }
    Ok(())
}

#[test]
fn recommendation_fallback_exposes_only_known_local_status_labels() {
    for (error, label) in [
        (Failure::Keychain, "keychain-unavailable"),
        (Failure::Transport, "transport-unavailable"),
        (Failure::Deadline, "deadline"),
        (Failure::OutputLimit, "output-limit"),
        (Failure::HttpStatus, "http-status"),
        (Failure::Unverified, "unverified-response"),
    ] {
        assert_eq!(error.label(), label);
    }
}

#[test]
fn registry_and_formula_lookups_use_public_json_without_model_headers() -> TestResult {
    let f = tempfile::tempdir_in("/private/tmp")?;
    for url in [
        "https://registry.npmjs.org/example/latest",
        "https://formulae.brew.sh/api/formula/example.json",
    ] {
        let runner = script(
            f.path(),
            "registry",
            &format!(
                r#"found=0
last=""
for field; do
  if [ "$field" = 'Accept: application/json' ]; then found=1;fi
  case "$field" in Authorization*) exit 55;; esac
  last="$field"
done
[ "$found" = '1' ]
[ "$last" = '{url}' ]
printf '{{"name":"example"}}\n200'"#
            ),
        )?;
        let body = fetch_public_with(&runner, url, Instant::now() + Duration::from_secs(2))?;
        assert_eq!(body["name"], "example");
    }
    Ok(())
}

fn credentials(
    path: &Path,
    service: &str,
    account: &str,
    deadline: Instant,
) -> Result<Vec<u8>, Failure> {
    let mut command = Command::new(path);
    command
        .args(["find-generic-password", "-s", service, "-a", account, "-w"])
        .stdin(Stdio::null());
    run(&mut command, deadline, 513).map_err(|error| {
        if error == Failure::Transport {
            Failure::Keychain
        } else {
            error
        }
    })
}

#[test]
fn late_native_keychain_result_cannot_start_an_http_request() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let marker = dir.path().join("http-started");
    let curl = script(
        dir.path(),
        "must-not-start",
        &format!("touch '{}'", marker.display()),
    )?;
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let permit = std::cell::RefCell::new(Some((release_rx, done_tx)));
    let clients = Clients {
        credentials: &|_, _, deadline| {
            let (receive, done) = permit.borrow_mut().take().ok_or(Failure::Keychain)?;
            keychain::bounded(deadline, move || {
                receive.recv().map_err(|_| keychain::unavailable())?;
                done.send(()).map_err(|_| keychain::unavailable())?;
                Ok(FIXTURE_TOKEN.as_bytes().to_vec())
            })
            .map_err(keychain_failure)
        },
        curl: &curl,
    };
    let response = compare(
        &clients,
        brief()?.facts(),
        "synthetic",
        Instant::now() + Duration::from_millis(300),
    );
    assert_eq!(response.err(), Some(Failure::Deadline));
    release_tx.send(())?;
    done_rx.recv_timeout(Duration::from_secs(2))?;
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn public_model_status_reports_only_redacted_outcomes_without_host_authentication() -> TestResult {
    use clap::Parser as _;
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let profile = serde_json::from_value::<crate::recommend::setup::Profile>(json!({
        "schemaVersion": 1, "enabled": true, "endpoint": "https://example.test/chat/completions",
        "model": "fixture", "account": "fixture-account"
    }))?;
    assert_eq!(
        annotate(&ctx, &brief()?, Some("synthetic"))["status"],
        "keychain-unavailable"
    );
    assert_eq!(
        suggest(
            &ctx,
            &profile,
            "example/fixture",
            &[("README.md", "fixture")]
        )["status"],
        "local-only"
    );
    assert_eq!(
        annotation(Ok(Notes { notes: vec![] }), policy::MODEL)["status"],
        "annotated"
    );
    assert_eq!(
        annotation(Err(Failure::Deadline), policy::MODEL)["status"],
        "deadline"
    );
    assert_eq!(suggestion(Ok(vec![]))["status"], "suggested");
    assert_eq!(
        suggestion(Err(Failure::Keychain))["status"],
        "keychain-unavailable"
    );
    let error = test_profile(&profile, FIXTURE_TOKEN, Instant::now())
        .err()
        .ok_or("expected timeout")?;
    assert_eq!(error.code, "model_connection_test_failed");
    assert!(!error.to_string().contains(FIXTURE_TOKEN));
    Ok(())
}

#[test]
fn expired_legacy_authentication_is_not_dispatched_to_the_keychain() -> TestResult {
    let expired = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .ok_or("fixture time")?;
    assert_eq!(
        native_legacy_credentials("synthetic-service", "synthetic", expired).err(),
        Some(Failure::Deadline)
    );
    Ok(())
}

#[test]
fn exhausted_public_model_budget_exits_before_any_native_or_remote_read() -> TestResult {
    use clap::Parser as _;
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let mut ctx = Context::from_cli(&cli, &mut vec![])?;
    // Simulate an already exhausted runtime budget, independent of argument parsing.
    ctx.isolated = false;
    ctx.timeout_secs = 0;
    let profile = serde_json::from_value::<crate::recommend::setup::Profile>(json!({
        "schemaVersion": 1, "enabled": true, "endpoint": "https://example.test/chat/completions",
        "model": "fixture", "account": "fixture-account"
    }))?;
    assert_eq!(
        annotate(&ctx, &brief()?, Some("synthetic"))["status"],
        "deadline"
    );
    assert_eq!(
        suggest(&ctx, &profile, "fixture/repo", &[])["status"],
        "local-only"
    );
    assert_eq!(
        suggest(
            &ctx,
            &profile,
            "fixture/repo",
            &[("README.md", "public fixture")]
        )["status"],
        "deadline"
    );
    Ok(())
}

#[test]
fn explicit_model_request_uses_the_same_fixed_key_and_configured_endpoint() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let profile = serde_json::from_value::<crate::recommend::setup::Profile>(json!({
        "schemaVersion":2,"enabled":true,"endpoint":"https://models.example.test/v1/chat/completions",
        "model":"selected-model","account":"setup-test"
    }))?;
    let curl = script(
        temp.path(),
        "annotation",
        r#"
IFS= read -r header
[ "$header" = 'Authorization: Bearer synthetic-token-do-not-log' ]
found=
last=
for arg; do
  case "$arg" in *'"model":"selected-model"'*) found=yes ;; esac
  case "$arg" in *synthetic-token-do-not-log*|*'"thinking"'*) exit 7 ;; esac
  last=$arg
done
[ "$found" = yes ] && [ "$last" = 'https://models.example.test/v1/chat/completions' ]
printf '%s\n200' '{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"{\"notes\":[{\"candidate\":0,\"summary\":\"fixture\"}]}"}}]}'
"#,
    )?;
    let clients = Clients {
        credentials: &|service, account, _| {
            assert_eq!(service, "org.macops.recommend.api-key");
            assert_eq!(account, "default");
            Ok(FIXTURE_TOKEN.as_bytes().to_vec())
        },
        curl: &curl,
    };
    let result = compare_configured(
        &clients,
        &profile,
        temp.path(),
        brief()?.facts(),
        Instant::now() + Duration::from_secs(4),
    )
    .map_err(Failure::label)?;
    assert_eq!(result.notes[0].summary, "fixture");
    Ok(())
}

#[test]
fn default_explicit_model_respects_paused_or_unreadable_settings() -> TestResult {
    use clap::Parser as _;

    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = crate::cli::Cli::try_parse_from([
        "mo",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let mut ctx = Context::from_cli(&cli, &mut vec![])?;
    ctx.isolated = false;
    ctx.timeout_secs = 0;
    let path = ctx.config.join("recommend-model.json");
    let analysis = brief()?;
    let mut configured = json!({"schemaVersion":2, "enabled":false,
        "endpoint":"https://example.test/chat/completions","model":"fixture-model",
        "account":format!("setup-{}", "a".repeat(64))});
    crate::skills::native_skills_files::write_cas(&path, None, &serde_json::to_vec(&configured)?)?;
    assert_eq!(annotate(&ctx, &analysis, None)["status"], "local-only");
    let before = fs::read(&path)?;
    configured["enabled"] = json!(true);
    crate::skills::native_skills_files::write_cas(
        &path,
        Some(&before),
        &serde_json::to_vec(&configured)?,
    )?;
    let annotation = annotate(&ctx, &analysis, None);
    assert_eq!(annotation["model"], "fixture-model");
    assert_eq!(annotation["status"], "deadline");
    let before = fs::read(&path)?;
    crate::skills::native_skills_files::write_cas(&path, Some(&before), b"{}")?;
    assert_eq!(
        annotate(&ctx, &analysis, None)["status"],
        "profile-unavailable"
    );
    fs::remove_file(path)?;
    let fallback = annotate(&ctx, &analysis, None);
    assert_eq!(fallback["status"], "deadline");
    assert_eq!(fallback["model"], policy::MODEL);
    Ok(())
}

#[test]
fn invalid_legacy_account_exits_before_loading_any_credential() -> TestResult {
    let analysis = brief()?;
    let clients = Clients {
        credentials: &|_, _, _| panic!("invalid account attempted Keychain access"),
        curl: Path::new("/unreachable"),
    };
    assert_eq!(
        compare(
            &clients,
            analysis.facts(),
            "invalid account",
            Instant::now()
        )
        .err(),
        Some(Failure::Keychain)
    );
    Ok(())
}

#[test]
fn configured_annotation_stops_if_silent_credentials_are_denied_or_corrupt() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let profile = serde_json::from_value::<crate::recommend::setup::Profile>(json!({
        "schemaVersion":2,"enabled":true,"endpoint":"https://models.example.test/chat/completions",
        "model":"current-model","account":"setup-test"
    }))?;
    let analysis = brief()?;
    let marker = temp.path().join("http-called");
    let never_send = script(
        temp.path(),
        "no-http",
        &format!("printf ran > '{}'; exit 7", marker.display()),
    )?;
    for scenario in [Err(Failure::Keychain), Ok(b"corrupt".to_vec())] {
        let clients = Clients {
            credentials: &|_, _, _| scenario.clone(),
            curl: &never_send,
        };
        let result = compare_configured(
            &clients,
            &profile,
            temp.path(),
            analysis.facts(),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(result.err(), Some(Failure::Keychain));
    }
    assert!(!marker.exists());
    Ok(())
}

#[test]
fn credential_read_failures_reach_the_recommendation_result() {
    for (error, status) in [
        (ReadFailure::Missing, "keychain-missing"),
        (
            ReadFailure::Authorization,
            "keychain-authorization-required",
        ),
        (ReadFailure::Keychain, "keychain-unavailable"),
        (ReadFailure::Deadline, "deadline"),
    ] {
        let output = suggestion(Err(keychain_failure(error)));
        assert_eq!(output["status"], status);
        assert_eq!(output["candidates"], json!([]));
    }
}

#[test]
fn github_rate_limit_reports_the_public_source_failure_without_exposing_the_body() {
    let url = "https://api.github.com/repos/fixture/public";
    let limited = public_source_error(
        url,
        b"{\"message\":\"API rate limit exceeded for fixture\"}\n403",
    );
    assert_eq!(limited.code, "recommend_github_rate_limited");
    assert!(!limited.message.contains("fixture"));
    let missing = public_source_error(url, b"{}\n404");
    assert_eq!(missing.code, "recommend_github_not_found");
    let denied = public_source_error(url, b"{\"message\":\"Forbidden\"}\n403");
    assert_eq!(denied.code, "recommend_source_unavailable");
}
