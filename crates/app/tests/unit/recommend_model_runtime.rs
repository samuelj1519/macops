use super::*;
use crate::test_support::TestResult;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

const FIXTURE_TOKEN: &str = "synthetic-token-do-not-log";

fn script(base: &Path, name: &str, body: &str) -> Result<PathBuf, io::Error> {
    let path = base.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n"))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

fn brief() -> crate::error::Result<PublicAnalysis> {
    crate::policy::recommend::public_analysis("jq")?
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
        security: &security,
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
            security: &security,
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
        security: &security,
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
            security: &security,
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
        assert!(!crate::process::group_alive(pid), "{waiting}");
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
    while crate::process::group_alive(pgid) && Instant::now() < limit {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!crate::process::group_alive(pgid));
    Ok(())
}

#[test]
fn public_http_reader_requires_its_selected_service_and_an_explicit_success_status() -> TestResult {
    let f = tempfile::tempdir_in("/private/tmp")?;
    let public = "https://api.github.com/repos/example/project";
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
  last="$field"
done
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
