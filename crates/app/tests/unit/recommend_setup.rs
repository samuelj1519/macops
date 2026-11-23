use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::cell::{Cell, RefCell};

#[derive(Default)]
struct FixtureKeys {
    added: RefCell<Vec<String>>,
    removed: RefCell<Vec<String>>,
    retire_requests: RefCell<Vec<String>>,
    reads: Cell<usize>,
    fail_load: Cell<bool>,
    fail_add: Cell<bool>,
    last_deadline: Cell<Option<Instant>>,
    fail_retire: Cell<bool>,
}

impl KeyStore for FixtureKeys {
    fn add(&self, profile: &Profile, source: Secret<'_>) -> Result<()> {
        if self.fail_add.get() {
            return Err(Error::attention(
                "fixture_add_failed",
                "synthetic keychain error",
            ));
        }
        if let Secret::New(token) = source {
            assert_eq!(token, "fixture-token-private");
        }
        self.added.borrow_mut().push(profile.account.clone());
        Ok(())
    }
    fn remove(&self, profile: &Profile, require_tag: bool) -> Result<()> {
        if require_tag {
            self.retire_requests
                .borrow_mut()
                .push(profile.account.clone());
            if self.fail_retire.get() {
                return Err(Error::new(
                    "fixture_retire_failed",
                    "temporary keychain failure",
                ));
            }
        }
        self.removed.borrow_mut().push(profile.account.clone());
        Ok(())
    }
    fn contains(&self, profile: &Profile) -> Result<bool> {
        Ok(self.added.borrow().contains(&profile.account))
    }
    fn load(&self, _: &Profile, deadline: Instant) -> Result<Vec<u8>> {
        self.last_deadline.set(Some(deadline));
        self.reads.set(self.reads.get() + 1);
        if self.fail_load.get() {
            return Err(keychain_error());
        }
        Ok(b"fixture-token-private".to_vec())
    }
    fn external(&self, _: &ExternalItem, deadline: Instant) -> Result<Vec<u8>> {
        self.load(&profile("https://example.test", "model")?, deadline)
    }
}

#[test]
fn endpoint_has_one_https_destination_and_binds_the_credential_namespace() -> TestResult {
    assert_eq!(
        endpoint("https://API.example.test:8443/v1/")?,
        "https://api.example.test:8443/v1/chat/completions"
    );
    assert_eq!(
        endpoint("https://api.example.test/chat/completions")?,
        "https://api.example.test/chat/completions"
    );
    for bad in [
        "http://example.test",
        concat!("https://user", "@", "example.test"),
        "https://a.test:0",
        "https://a.test:+4",
        "https://a.test:65536",
        "https://a..test",
        "https://-a.test",
        "https://a-.test",
        "https://_a.test",
        "https://a.test/v1//a",
        "https://a.test/../x",
        "https://a.test/v1?key=example",
        "https://a.test/v1#x",
        "https://a.test/%2f",
        "https://a.test/v1\n/a",
    ] {
        assert!(endpoint(bad).is_err(), "{bad}");
    }
    let mut p = profile("https://api.example.test/v1", "provider/model")?;
    let service = p.service(Path::new("/fixture/config"));
    assert_ne!(p.service(Path::new("/fixture/other-config")), service);
    p.endpoint = endpoint("https://other.example.test/v1")?;
    assert_ne!(p.service(Path::new("/fixture/config")), service);
    let isolation = NativeKeychain(None);
    assert!(
        isolation
            .add(&p, Secret::New("fixture-token-private"))
            .is_err()
    );
    assert!(isolation.remove(&p, true).is_err());
    assert!(isolation.contains(&p).is_err());
    Ok(())
}

#[test]
fn malformed_stored_profiles_cannot_redirect_an_existing_key() -> TestResult {
    let expected = profile("https://model.example.test/v1", "fixture-model")?;
    for (name, value) in [
        ("schemaVersion", json!(2)),
        ("endpoint", json!("https://model.example.test/v1")),
        ("account", json!("setup-invalid")),
        ("account", json!("legacy-default")),
        ("model", json!("a".repeat(129))),
        ("model", json!("bad model")),
    ] {
        let mut data = serde_json::to_value(&expected)?;
        data[name] = value;
        assert!(decode(Some(&serde_json::to_vec(&data)?)).is_err(), "{name}");
    }
    assert!(endpoint(&format!("https://{}.example.test", "x".repeat(254))).is_err());
    assert!(endpoint(&format!("https://model.example.test/{}", "x".repeat(240))).is_err());
    assert!(profile("https://model.example.test", &"x".repeat(129)).is_err());
    assert!(decode(Some(b"[]")).is_err());
    Ok(())
}

#[test]
fn setup_recovers_staged_credentials_without_removing_the_active_key() -> TestResult {
    let home = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys {
        added: RefCell::new(vec![]),
        removed: RefCell::new(vec![]),
        ..FixtureKeys::default()
    };
    let first = profile("https://api.example.test/v1", "example-model")?;
    assert!(read(&ctx)?.is_none());
    publish(
        &ctx,
        None,
        &first,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )?;
    assert!(read(&ctx)?.is_some_and(|p| p == first));
    let raw = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
    assert!(!String::from_utf8_lossy(&raw).contains("fixture-token-private"));
    let transition = ctx.config.join(PENDING);
    write_cas(
        &transition,
        Some(b"null"),
        &serde_json::to_vec(&Pending {
            profile: first.clone(),
            added: true,
            previous: None,
            tagged: false,
        })?,
    )?;
    recover_pending(&ctx.config, Some(&first), &keys)?;
    assert!(keys.removed.borrow().is_empty());
    let orphan = profile("https://second.example.test", "new-model")?;
    write_cas(
        &transition,
        Some(b"null"),
        &serde_json::to_vec(&Pending {
            profile: orphan.clone(),
            added: true,
            previous: None,
            tagged: false,
        })?,
    )?;
    let mut paused = first.clone();
    paused.enabled = false;
    publish(&ctx, Some(&raw), &paused, None, &keys)?;
    assert_eq!(keys.removed.borrow().as_slice(), [orphan.account()]);
    assert_eq!(keys.added.borrow().as_slice(), [first.account()]);
    assert_eq!(status(read(&ctx)?.as_ref())["mode"], "local");
    assert!(
        publish(
            &ctx,
            Some(&raw),
            &first,
            Some(Secret::New("fixture-token-private")),
            &keys
        )
        .is_err()
    );
    let uncertain = profile("https://uncertain.example.test", "third-model")?;
    let not_added = serde_json::to_vec(&Pending {
        profile: uncertain.clone(),
        added: false,
        previous: None,
        tagged: false,
    })?;
    write_cas(&transition, Some(b"null"), &not_added)?;
    recover_pending(&ctx.config, Some(&paused), &keys)?;
    assert_eq!(regular(&transition)?.as_deref(), Some(b"null".as_slice()));
    keys.add(&uncertain, Secret::New("fixture-token-private"))?;
    let pending = serde_json::to_vec(&Pending {
        profile: uncertain,
        added: false,
        previous: None,
        tagged: false,
    })?;
    write_cas(&transition, Some(b"null"), &pending)?;
    let error = recover_pending(&ctx.config, Some(&paused), &keys)
        .err()
        .ok_or("pending must be retained")?;
    assert_eq!(error.code, "model_keychain_confirmation_pending");
    assert_eq!(regular(&transition)?.as_deref(), Some(pending.as_slice()));

    Ok(())
}

#[test]
fn wizard_verifies_the_selected_destination_before_committing_an_opt_in() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys {
        added: RefCell::new(vec![]),
        removed: RefCell::new(vec![]),
        ..FixtureKeys::default()
    };
    let run = |input: &[&str], token: &str, fail: bool| {
        let before = regular(&ctx.config.join(PROFILE))?;
        let current = decode(before.as_deref())?;
        let mut replies = input.iter().copied();
        wizard(
            &ctx,
            before.as_deref(),
            current,
            &keys,
            |_| {
                replies
                    .next()
                    .map(str::to_owned)
                    .ok_or_else(|| Error::new("fixture", "unexpected prompt"))
            },
            || Ok(token.to_owned()),
            |selected, key, _| {
                assert_eq!(key, "fixture-token-private");
                assert!(selected.endpoint.ends_with("/chat/completions"));
                if fail {
                    Err(Error::attention(
                        "fixture_denied",
                        "fixture response denied",
                    ))
                } else {
                    Ok(())
                }
            },
        )
    };
    let mock = "fixture-token-private";
    assert_eq!(run(&["1"], "", false)?["mode"], "cancelled");
    assert_eq!(run(&["1", "n"], mock, false)?["mode"], "cancelled");
    assert_eq!(run(&["1", "y", "n"], mock, false)?["mode"], "cancelled");
    assert!(run(&["1", "y"], mock, true).is_err());
    assert!(run(&["2", "http://api.example.test", "model"], mock, false).is_err());
    assert!(run(&["2", "https://api.example.test", "bad model"], mock, false).is_err());
    assert!(run(&["1"], "short", false).is_err());
    assert!(run(&["9"], mock, false).is_err());
    assert_eq!(run(&["3"], mock, false)?["mode"], "local");
    assert!(!ctx.config.exists());
    let data = run(
        &[
            "2",
            "https://api.example.test/v1",
            "explicit-model",
            "y",
            "y",
        ],
        mock,
        false,
    )?;
    assert_eq!(data["mode"], "remote");
    assert_eq!(
        data["endpoint"],
        "https://api.example.test/v1/chat/completions"
    );
    assert_eq!(data["model"], "explicit-model");
    assert_eq!(run(&["3"], mock, false)?["mode"], "local");
    assert_eq!(keys.added.borrow().len(), 1);
    assert!(!serde_json::to_string(&data)?.contains(mock));
    let pending = profile("https://second.example.test/v1", "uncertain-model")?;
    keys.add(&pending, Secret::New(mock))?;
    let raw_pending = serde_json::to_vec(&Pending {
        profile: pending,
        added: false,
        previous: None,
        tagged: false,
    })?;
    write_cas(&ctx.config.join(PENDING), Some(b"null"), &raw_pending)?;
    assert_eq!(run(&["3"], mock, false)?["mode"], "local");
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(raw_pending.as_slice())
    );
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn saved_key_test_resume_and_model_edit_share_the_existing_keychain_entry() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys::default();
    let mut first = profile("https://example.test", "first-model")?;
    first.enabled = false;
    publish(
        &ctx,
        None,
        &first,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )?;
    let run = |input: &[&str]| {
        let before = regular(&ctx.config.join(PROFILE))?;
        let mut answers = input.iter();
        wizard(
            &ctx,
            before.as_deref(),
            decode(before.as_deref())?,
            &keys,
            |_| {
                answers
                    .next()
                    .map(|value| (*value).into())
                    .ok_or_else(invalid)
            },
            || {
                Err(Error::new(
                    "fixture",
                    "saved-key action requested secret input",
                ))
            },
            |profile, token, until| {
                assert_eq!(keys.last_deadline.get(), Some(until));
                assert_eq!(profile.endpoint, first.endpoint);
                assert_eq!(token, "fixture-token-private");
                Ok(())
            },
        )
    };
    assert_eq!(run(&["t", "n"])?["mode"], "cancelled");
    assert_eq!(keys.reads.get(), 0);
    assert_eq!(run(&["t", "y"])?["connection"], "verified");
    assert!(read(&ctx)?.is_some_and(|p| p == first));
    assert_eq!(run(&["m", "next-model", "y", "y"])?["mode"], "local");
    assert_eq!(run(&["r", "y", "y"])?["mode"], "remote");
    let last = read(&ctx)?.ok_or("saved profile")?;
    assert_eq!(last.account(), first.account());
    assert_eq!(last.model, "next-model");
    assert_eq!(keys.added.borrow().len(), 1);
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn rotation_commits_new_profile_before_cleanup_and_can_retry_cleanup() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys::default();
    let previous = profile("https://example.test", "first-model")?;
    let next = profile("https://example.test", "second-model")?;
    publish(
        &ctx,
        None,
        &previous,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )?;
    let old = regular(&ctx.config.join(PROFILE))?.ok_or("old profile")?;
    keys.fail_retire.set(true);
    let error = publish(
        &ctx,
        Some(&old),
        &next,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )
    .err()
    .ok_or("cleanup failure should be reported")?;
    assert_eq!(error.code, "model_keychain_cleanup_pending");
    assert!(read(&ctx)?.is_some_and(|p| p == next));
    assert_eq!(
        keys.retire_requests.borrow().as_slice(),
        [previous.account()]
    );
    assert!(keys.removed.borrow().is_empty());
    let raw = regular(&ctx.config.join(PROFILE))?.ok_or("new profile")?;
    assert!(optional_record::<Pending>(regular(&ctx.config.join(PENDING))?.as_deref())?.is_some());
    keys.fail_retire.set(false);
    keys.fail_load.set(true);
    let failure = recover_setup(&ctx, Some(&raw), &keys)
        .err()
        .ok_or("cleanup accepted an unavailable active credential")?;
    assert_eq!(failure.code, "model_keychain_cleanup_pending");
    assert!(keys.removed.borrow().is_empty());
    assert!(optional_record::<Pending>(regular(&ctx.config.join(PENDING))?.as_deref())?.is_some());
    keys.fail_load.set(false);
    recover_setup(&ctx, Some(&raw), &keys)?;
    recover_setup(&ctx, Some(&raw), &keys)?;
    assert_eq!(keys.removed.borrow().as_slice(), [previous.account()]);
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );
    Ok(())
}

#[test]
fn wizard_bind_existing_item_confirms_address_and_saves_a_locator() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keychain_file = tempfile::NamedTempFile::new_in("/private/tmp")?;
    let keys = FixtureKeys::default();
    let path = keychain_file.path().to_str().ok_or("fixture path")?;
    let mut answers = [
        "b",
        "https://example.test",
        "fixture-model",
        path,
        "provider",
        "account",
        "y",
        "y",
    ]
    .into_iter();
    let result = wizard(
        &ctx,
        None,
        None,
        &keys,
        |_| answers.next().map(str::to_owned).ok_or_else(invalid),
        || {
            Err(Error::new(
                "fixture",
                "binding should not ask for new key material",
            ))
        },
        |profile, token, until| {
            assert_eq!(keys.last_deadline.get(), Some(until));
            assert_eq!(profile.endpoint, "https://example.test/chat/completions");
            assert_eq!(token, "fixture-token-private");
            Ok(())
        },
    )?;
    assert_eq!(result["mode"], "remote");
    assert_eq!(keys.reads.get(), 1);
    assert_eq!(keys.added.borrow().len(), 1);
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn expired_setup_deadline_refuses_the_connection_callback() -> TestResult {
    let p = profile("https://example.test", "fixture-model")?;
    let error = test_before(&p, "fixture-token-private", Instant::now(), |_, _, _| {
        panic!("expired authentication must not reach the network callback")
    })
    .err()
    .ok_or("deadline should fail")?;
    assert_eq!(error.code, "model_keychain_timeout");
    Ok(())
}

#[test]
fn confirmed_retention_allows_replacement_without_erasing_an_unknown_add() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys::default();
    let previous = profile("https://example.test", "first-model")?;
    let current = profile("https://example.test", "second-model")?;
    publish(
        &ctx,
        None,
        &previous,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )?;
    let old = regular(&ctx.config.join(PROFILE))?.ok_or("old profile")?;
    keys.fail_load.set(true);
    assert!(
        publish(
            &ctx,
            Some(&old),
            &current,
            Some(Secret::New("fixture-token-private")),
            &keys
        )
        .is_err()
    );
    let active = regular(&ctx.config.join(PROFILE))?.ok_or("current profile")?;
    let pending_path = ctx.config.join(PENDING);
    let positive = regular(&pending_path)?.ok_or("cleanup record")?;
    let mut pending = optional_record::<Pending>(Some(&positive))?.ok_or("pending")?;
    pending.added = false;
    let unknown = serde_json::to_vec(&pending)?;
    write_cas(&pending_path, Some(&positive), &unknown)?;
    assert!(retain_setup(&ctx, Some(&active), Some(&unknown)).is_err());
    assert!(retain_setup(&ctx, Some(&active), Some(&positive)).is_err());
    assert_eq!(regular(&pending_path)?.as_deref(), Some(unknown.as_slice()));
    write_cas(&pending_path, Some(&unknown), &positive)?;
    let reads = keys.reads.get();
    let menu = |answer: &str| {
        let mut choices = ["k", answer].into_iter();
        wizard(
            &ctx,
            Some(&active),
            Some(current.clone()),
            &keys,
            |_| choices.next().map(str::to_owned).ok_or_else(invalid),
            || panic!("retention requested secret input"),
            |_, _, _| panic!("retention dispatched HTTP"),
        )
    };
    assert_eq!(menu("n")?["mode"], "cancelled");
    assert_eq!(
        regular(&pending_path)?.as_deref(),
        Some(positive.as_slice())
    );
    assert_eq!(menu("y")?["cleanup"], "previous-entry-retained");
    assert_eq!(keys.reads.get(), reads);
    assert!(keys.removed.borrow().is_empty());
    assert_eq!(regular(&pending_path)?.as_deref(), Some(b"null".as_slice()));
    keys.fail_load.set(false);
    let replacement = profile("https://example.test", "third-model")?;
    publish(
        &ctx,
        Some(&active),
        &replacement,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )?;
    assert!(read(&ctx)?.is_some_and(|p| p == replacement));
    assert_eq!(keys.removed.borrow().as_slice(), [current.account()]);
    Ok(())
}

#[test]
fn corrupt_or_mismatched_cleanup_records_cannot_use_a_scoped_keychain_entry() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let current = profile("https://example.test", "fixture-model")?;
    let other = profile("https://other.example.test", "fixture-model")?;
    let mut pending = Pending {
        profile: current.clone(),
        previous: Some(current.clone()),
        added: true,
        tagged: true,
    };
    let keys = FixtureKeys::default();
    let path = dir.path().join(PENDING);
    let duplicate = serde_json::to_vec(&pending)?;
    std::fs::write(&path, &duplicate)?;
    assert!(recover_pending(dir.path(), Some(&current), &keys).is_err());
    assert!(retainable(Some(&current), Some(&duplicate)).is_err());
    pending.previous = Some(other.clone());
    pending.tagged = false;
    assert!(retainable(Some(&current), Some(&serde_json::to_vec(&pending)?)).is_err());
    pending.tagged = true;
    let mismatched = serde_json::to_vec(&pending)?;
    assert!(retainable(Some(&other), Some(&mismatched)).is_err());
    assert!(retainable(None, Some(&mismatched)).is_err());
    assert!(retainable(Some(&current), None).is_err());
    let mut invalid = other;
    invalid.model = "not a valid-model".into();
    pending.previous = Some(invalid);
    let invalid = serde_json::to_vec(&pending)?;
    std::fs::write(&path, &invalid)?;
    assert!(recover_pending(dir.path(), Some(&current), &keys).is_err());
    assert!(retainable(Some(&current), Some(&invalid)).is_err());
    assert_eq!(std::fs::read(&path)?, invalid);
    assert_eq!(keys.reads.get(), 0);
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn isolated_setup_never_loads_the_host_keychain() -> TestResult {
    let p = profile("https://example.test", "fixture-model")?;
    let name = tempfile::NamedTempFile::new_in("/private/tmp")?;
    let item = ExternalItem::bind(name.path(), "fixture".into(), "only".into())?;
    let isolated = NativeKeychain(None);
    assert!(isolated.load(&p, deadline(10)).is_err());
    assert!(isolated.external(&item, deadline(10)).is_err());
    assert_eq!(
        read_error(ReadFailure::Keychain).code,
        "model_keychain_unavailable"
    );
    Ok(())
}

#[test]
fn saved_key_actions_require_a_profile_and_valid_model_before_loading() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys::default();
    for action in ["t", "r", "m"] {
        let error = wizard(
            &ctx,
            None,
            None,
            &keys,
            |_| Ok(action.into()),
            || Ok(String::new()),
            |_, _, _| Ok(()),
        )
        .err()
        .ok_or("no profile should stop")?;
        assert_eq!(error.code, "model_profile_missing");
    }
    let active = profile("https://example.test", "model")?;
    let mut answers = ["m", "unsafe model"].into_iter();
    assert!(
        wizard(
            &ctx,
            None,
            Some(active),
            &keys,
            |_| answers.next().map(str::to_owned).ok_or_else(invalid),
            || Ok(String::new()),
            |_, _, _| Ok(())
        )
        .is_err()
    );
    assert_eq!(keys.reads.get(), 0);
    assert!(!ctx.config.exists());
    Ok(())
}

#[test]
fn borrowed_setup_cancel_and_provider_failure_leave_no_credential_metadata() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys::default();
    let file = tempfile::NamedTempFile::new_in("/private/tmp")?;
    let path = file.path().to_str().ok_or("path")?;
    for confirmation in ["n", "y"] {
        let mut answers = [
            "b",
            "https://example.test",
            "model",
            path,
            "fixture",
            "profile",
            confirmation,
        ]
        .into_iter();
        let result = wizard(
            &ctx,
            None,
            None,
            &keys,
            |_| answers.next().map(str::to_owned).ok_or_else(invalid),
            || panic!("binding asked for a plaintext key"),
            |_, _, _| Err(Error::attention("simulated_request_denied", "fixture")),
        );
        if confirmation == "n" {
            assert_eq!(result?["mode"], "cancelled");
            assert_eq!(keys.reads.get(), 0);
        } else {
            assert_eq!(
                result.err().ok_or("test must fail")?.code,
                "simulated_request_denied"
            );
        }
        assert!(keys.added.borrow().is_empty());
        assert!(!ctx.config.exists());
    }
    Ok(())
}

#[test]
fn failed_insert_keeps_predecessor_and_can_recover_confirmed_absence() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        dir.path().to_str().ok_or("home")?,
        "recommend",
        "--setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys::default();
    let current = profile("https://example.test", "current")?;
    let new = profile("https://other.example.test", "replacement")?;
    let key = Secret::New("fixture-token-private");
    publish(&ctx, None, &current, Some(key), &keys)?;
    let raw = regular(&ctx.config.join(PROFILE))?.ok_or("current profile")?;
    assert!(publish(&ctx, Some(&raw), &current, Some(key), &keys).is_err());
    keys.fail_add.set(true);
    let failure = publish(&ctx, Some(&raw), &new, Some(key), &keys)
        .err()
        .ok_or("insert failure")?;
    assert_eq!(failure.code, "fixture_add_failed");
    assert_eq!(
        regular(&ctx.config.join(PROFILE))?.as_deref(),
        Some(raw.as_slice())
    );
    assert!(keys.removed.borrow().is_empty());
    let recovery = recover_setup(&ctx, Some(&raw), &keys)?;
    assert_eq!(recovery["endpoint"], current.endpoint);
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );
    assert_eq!(keys.added.borrow().len(), 1);
    Ok(())
}

#[test]
fn expired_native_setup_reads_and_oversized_insert_stop_before_system_calls() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let keychain = NativeKeychain(Some(dir.path()));
    let previous = profile("https://example.test", "fixture")?;
    let fake_keychain = tempfile::NamedTempFile::new_in(dir.path())?;
    let item = ExternalItem::bind(fake_keychain.path(), "provider".into(), "account".into())?;
    let error = keychain
        .load(&previous, Instant::now())
        .err()
        .ok_or("expired local read")?;
    assert_eq!(error.code, "model_keychain_timeout");
    let error = keychain
        .external(&item, Instant::now())
        .err()
        .ok_or("expired external read")?;
    assert_eq!(error.code, "model_keychain_timeout");
    let error = keychain
        .add(&previous, Secret::New(&"a".repeat(4096)))
        .err()
        .ok_or("oversized key")?;
    assert_eq!(error.code, "model_keychain_unavailable");
    Ok(())
}
