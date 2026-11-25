use super::*;
use crate::{
    cli::Cli, context::Context, native_skills_files::write_cas, store::Store,
    test_support::TestResult,
};
use clap::Parser;
use std::cell::{Cell, RefCell};

fn profile(base: &str, model: &str) -> Result<Profile> {
    let mut legacy = super::profile(base, model)?;
    legacy.schema_version = 1;
    Ok(legacy)
}

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
    single: RefCell<Option<Profile>>,
    single_token: RefCell<Option<Vec<u8>>>,
    rotations: Cell<usize>,
    interrupt_single: Cell<bool>,
    ignore_single_write: Cell<bool>,
    file_edit_on_read: RefCell<Option<(std::path::PathBuf, Vec<u8>)>>,
    file_edit_on_load: RefCell<Option<(std::path::PathBuf, Vec<u8>)>>,
    file_edit_on_contains: RefCell<Option<(std::path::PathBuf, Vec<u8>)>>,
    lease_to_replace_before_write: RefCell<Option<std::path::PathBuf>>,
}

impl KeyStore for FixtureKeys {
    fn add(
        &self,
        profile: &Profile,
        source: Secret<'_>,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        guard()?;
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
    fn write_single(
        &self,
        previous: Option<&Profile>,
        next: &Profile,
        source: Secret<'_>,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if let Some(lease) = self.lease_to_replace_before_write.take() {
            std::fs::rename(&lease, lease.with_extension("old"))?;
            private(&lease)?;
        }
        guard()?;
        let mut slot = self.single.borrow_mut();
        if self.fail_add.get() {
            return Err(keychain_error());
        }
        match (slot.as_ref(), previous.filter(|p| p.uses_single_entry())) {
            (Some(old), Some(expected)) if same_entry(old, expected) => {
                self.rotations.set(self.rotations.get() + 1);
            }
            (None, _) => {
                self.added.borrow_mut().push(next.account.clone());
            }
            _ => return Err(keychain_error()),
        }
        if let Secret::New(token) = source {
            recommend_model::api_token(token.as_bytes())?;
        }
        if !self.ignore_single_write.get() {
            *slot = Some(next.clone());
            self.single_token.replace(Some(match source {
                Secret::New(token) => token.as_bytes().to_vec(),
                Secret::Existing(_) => b"fixture-token-private".to_vec(),
            }));
        }
        if self.interrupt_single.get() {
            return Err(keychain_error());
        }
        Ok(())
    }

    fn entry_state(&self, previous: Option<&Profile>, next: &Profile) -> Result<EntryState> {
        if let Some((path, content)) = self.file_edit_on_read.take() {
            std::fs::write(path, content)?;
        }
        match self.single.borrow().as_ref() {
            None => Ok(EntryState::Missing),
            Some(slot) if same_entry(slot, next) => Ok(EntryState::Next),
            Some(slot) if previous.is_some_and(|p| same_entry(slot, p)) => Ok(EntryState::Previous),
            _ => Err(keychain_error()),
        }
    }

    fn remove(
        &self,
        profile: &Profile,
        require_tag: bool,
        guard: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        guard()?;
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
        if let Some((path, bytes)) = self.file_edit_on_contains.take() {
            std::fs::write(path, bytes)?;
        }
        if profile.uses_single_entry() {
            return Ok(self.single.borrow().is_some());
        }
        Ok(self.added.borrow().contains(&profile.account))
    }
    fn load(&self, profile: &Profile, deadline: Instant) -> Result<Vec<u8>> {
        if let Some((path, bytes)) = self.file_edit_on_load.take() {
            std::fs::write(path, bytes)?;
        }
        self.last_deadline.set(Some(deadline));
        self.reads.set(self.reads.get() + 1);
        if self.fail_load.get()
            || (profile.uses_single_entry()
                && !matches!(self.entry_state(None, profile), Ok(EntryState::Next)))
        {
            return Err(keychain_error());
        }
        if profile.uses_single_entry() {
            return self
                .single_token
                .borrow()
                .clone()
                .ok_or_else(keychain_error);
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
            .add(&p, Secret::New("fixture-token-private"), &|| Ok(()))
            .is_err()
    );
    assert!(isolation.remove(&p, true, &|| Ok(())).is_err());
    assert!(isolation.contains(&p).is_err());
    Ok(())
}

#[test]
fn malformed_stored_profiles_cannot_redirect_an_existing_key() -> TestResult {
    let expected = profile("https://model.example.test/v1", "fixture-model")?;
    for (name, value) in [
        ("schemaVersion", json!(3)),
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys {
        added: RefCell::new(vec![]),
        removed: RefCell::new(vec![]),
        ..FixtureKeys::default()
    };
    let first = profile("https://api.example.test/v1", "example-model")?;
    assert!(read(&ctx.config)?.is_none());
    publish(
        &ctx,
        None,
        &first,
        Some(Secret::New("fixture-token-private")),
        &keys,
    )?;
    assert!(read(&ctx.config)?.is_some_and(|p| p == first));
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
    recover_pending(&lock_profile(&ctx, Some(&raw))?, Some(&first), &keys)?;
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
    assert_eq!(status(read(&ctx.config)?.as_ref())["mode"], "local");
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
    let paused_raw = regular(&ctx.config.join(PROFILE))?;
    recover_pending(
        &lock_profile(&ctx, paused_raw.as_deref())?,
        Some(&paused),
        &keys,
    )?;
    assert_eq!(regular(&transition)?.as_deref(), Some(b"null".as_slice()));
    keys.add(&uncertain, Secret::New("fixture-token-private"), &|| Ok(()))?;
    let pending = serde_json::to_vec(&Pending {
        profile: uncertain,
        added: false,
        previous: None,
        tagged: false,
    })?;
    write_cas(&transition, Some(b"null"), &pending)?;
    let error = recover_pending(
        &lock_profile(&ctx, paused_raw.as_deref())?,
        Some(&paused),
        &keys,
    )
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    keys.add(&pending, Secret::New(mock), &|| Ok(()))?;
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    assert!(read(&ctx.config)?.is_some_and(|p| p == first));
    assert_eq!(run(&["m", "next-model", "y", "y"])?["mode"], "local");
    assert_eq!(run(&["r", "y", "y"])?["mode"], "remote");
    let last = read(&ctx.config)?.ok_or("saved profile")?;
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    assert!(read(&ctx.config)?.is_some_and(|p| p == next));
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    assert!(retain_setup(&ctx, Some(&active), Some(&unknown), &keys).is_err());
    assert!(retain_setup(&ctx, Some(&active), Some(&positive), &keys).is_err());
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
    assert!(read(&ctx.config)?.is_some_and(|p| p == replacement));
    assert_eq!(keys.removed.borrow().as_slice(), [current.account()]);
    Ok(())
}

#[test]
fn corrupt_or_mismatched_cleanup_records_cannot_use_a_scoped_keychain_entry() -> TestResult {
    let (_dir, ctx, _keys) = single_fixture()?;
    let current = profile("https://example.test", "fixture-model")?;
    let other = profile("https://other.example.test", "fixture-model")?;
    let mut pending = Pending {
        profile: current.clone(),
        previous: Some(current.clone()),
        added: true,
        tagged: true,
    };
    let keys = FixtureKeys::default();
    let lock = lock_profile(&ctx, None)?;
    let path = lock.config.path.join(PENDING);
    let duplicate = serde_json::to_vec(&pending)?;
    std::fs::write(&path, &duplicate)?;
    assert!(recover_pending(&lock, Some(&current), &keys).is_err());
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
    assert!(recover_pending(&lock, Some(&current), &keys).is_err());
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
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
        .add(&previous, Secret::New(&"a".repeat(4096)), &|| Ok(()))
        .err()
        .ok_or("oversized key")?;
    assert_eq!(error.code, "model_keychain_unavailable");
    let first = super::profile("https://example.test", "fixed")?;
    let malformed = "x".repeat(4096);
    assert!(
        keychain
            .write_single(None, &first, Secret::New(&malformed), &|| Ok(()))
            .is_err()
    );
    assert!(NativeKeychain(None).entry_state(None, &first).is_err());
    Ok(())
}

fn single_fixture() -> Result<(tempfile::TempDir, RecommendSetupContext, FixtureKeys)> {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "mo",
        "--home",
        dir.path().to_str().ok_or_else(invalid)?,
        "recommend",
        "--setup",
    ])
    .map_err(|_| invalid())?;
    let ctx = RecommendSetupContext::from_cli(&cli, &mut vec![])?;
    Ok((dir, ctx, FixtureKeys::default()))
}

#[test]
fn wizard_rotates_a_single_item_and_keeps_the_same_key_for_model_edits() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let setup = |choices: &[&str], token: Option<&str>, expected: &str| {
        let before = regular(&ctx.config.join(PROFILE))?;
        let current = decode(before.as_deref())?;
        let mut choices = choices.iter().copied();
        wizard(
            &ctx,
            before.as_deref(),
            current,
            &keys,
            |_| choices.next().map(str::to_owned).ok_or_else(invalid),
            || token.map(str::to_owned).ok_or_else(invalid),
            |_, received, _| {
                assert_eq!(received, expected);
                Ok(())
            },
        )
    };
    let original = "synthetic-token-first";
    let replacement = "synthetic-token-replacement";
    setup(&["1", "y", "y"], Some(original), original)?;
    let first = read(&ctx.config)?.ok_or("first profile")?;
    setup(
        &["2", "https://other.example.test/v1", "model-two", "y", "y"],
        Some(replacement),
        replacement,
    )?;
    let second = read(&ctx.config)?.ok_or("second profile")?;
    assert_eq!(first.service(&ctx.config), "org.macops.recommend.api-key");
    assert_eq!(second.service(&ctx.config), first.service(&ctx.config));
    assert_eq!(first.account(), "default");
    assert_eq!(second.account(), first.account());
    assert_ne!(first.credential_version(), second.credential_version());
    assert_ne!(first.endpoint, second.endpoint);
    assert!(keys.load(&first, deadline(15)).is_err());
    assert_eq!(keys.added.borrow().len(), 1);
    assert_eq!(keys.rotations.get(), 1);
    assert!(keys.removed.borrow().is_empty());

    setup(&["m", "model-three", "y", "y"], None, replacement)?;
    let edited = read(&ctx.config)?.ok_or("edited profile")?;
    assert_eq!(edited.model, "model-three");
    assert!(same_entry(&edited, &second));
    assert_eq!(keys.rotations.get(), 1);
    let raw = regular(&ctx.config.join(PROFILE))?.ok_or("active config")?;
    assert!(!String::from_utf8(raw)?.contains(replacement));
    Ok(())
}

#[test]
fn recovery_finishes_the_original_single_slot_update_after_interruption() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let next = super::profile("https://other.example.test", "next")?;
    let original = Secret::New("synthetic-token-first");
    let new = Secret::New("synthetic-token-replacement");
    publish(&ctx, None, &first, Some(original), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("previous profile")?;
    keys.interrupt_single.set(true);
    let failure = publish(&ctx, Some(&before), &next, Some(new), &keys)
        .err()
        .ok_or("expected interruption")?;
    assert_eq!(failure.code, "model_keychain_update_pending");
    assert!(read(&ctx.config)?.is_some_and(|active| active == first));
    assert!(keys.load(&first, deadline(15)).is_err());
    let pending = regular(&ctx.config.join(PENDING))?.ok_or("pending metadata")?;
    assert!(!String::from_utf8(pending)?.contains("synthetic-token-"));
    let mut disabled = first.clone();
    disabled.enabled = false;
    assert!(disable_profile(&ctx, Some(&before), &disabled).is_err());
    assert!(publish(&ctx, Some(&before), &next, Some(new), &keys).is_err());

    keys.interrupt_single.set(false);
    let recovered = recover_setup(&ctx, Some(&before), &keys)?;
    assert_eq!(recovered["endpoint"], next.endpoint);
    assert_eq!(
        keys.load(&next, deadline(15))?,
        b"synthetic-token-replacement"
    );
    assert_eq!(keys.rotations.get(), 1);
    assert_eq!(keys.added.borrow().len(), 1);
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );
    Ok(())
}

#[test]
fn recovery_preserves_the_old_key_when_the_single_slot_write_never_landed() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let next = super::profile("https://example.test", "next")?;
    let secret = Secret::New("synthetic-token-first");
    publish(&ctx, None, &first, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("previous profile")?;
    keys.ignore_single_write.set(true);
    assert!(publish(&ctx, Some(&before), &next, Some(secret), &keys).is_err());
    let pending = regular(&ctx.config.join(PENDING))?.ok_or("pending record")?;

    let mut changed = first.clone();
    changed.model = "changed-outside-setup".into();
    let tampered = serde_json::to_vec(&changed)?;
    write_cas(&ctx.config.join(PROFILE), Some(&before), &tampered)?;
    assert!(recover_setup(&ctx, Some(&tampered), &keys).is_err());
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(pending.as_slice())
    );
    write_cas(&ctx.config.join(PROFILE), Some(&tampered), &before)?;
    assert_eq!(
        recover_setup(&ctx, Some(&before), &keys)?["model"],
        first.model
    );
    assert_eq!(keys.load(&first, deadline(15))?, b"synthetic-token-first");
    assert!(keys.load(&next, deadline(15)).is_err());
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );
    Ok(())
}

#[test]
fn migration_retires_only_the_old_owned_entry_after_verifying_the_replacement() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let previous = profile("https://example.test", "legacy")?;
    let next = super::profile("https://other.example.test", "new")?;
    let secret = Secret::New("fixture-token-private");
    publish(&ctx, None, &previous, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("legacy profile")?;
    keys.fail_load.set(true);
    let failure = publish(&ctx, Some(&before), &next, Some(secret), &keys)
        .err()
        .ok_or("expected cleanup pending")?;
    assert_eq!(failure.code, "model_keychain_cleanup_pending");
    assert!(read(&ctx.config)?.is_some_and(|profile| profile == next));
    assert!(keys.retire_requests.borrow().is_empty());
    assert!(keys.removed.borrow().is_empty());

    keys.fail_load.set(false);
    let current = regular(&ctx.config.join(PROFILE))?.ok_or("new profile")?;
    recover_setup(&ctx, Some(&current), &keys)?;
    assert_eq!(
        keys.retire_requests.borrow().as_slice(),
        [previous.account()]
    );
    assert_eq!(keys.removed.borrow().as_slice(), [previous.account()]);
    assert_eq!(keys.load(&next, deadline(15))?, b"fixture-token-private");
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );
    Ok(())
}

#[test]
fn unowned_fixed_item_refuses_first_setup_before_recording_an_intent() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let other_config = super::profile("https://example.test", "unowned")?;
    let requested = super::profile("https://new.example.test", "chosen")?;
    keys.single.replace(Some(other_config));
    let error = publish(
        &ctx,
        None,
        &requested,
        Some(Secret::New("synthetic-token-new")),
        &keys,
    )
    .err()
    .ok_or("expected another configuration to own this slot")?;
    assert_eq!(error.code, "model_keychain_slot_conflict");
    assert!(regular(&ctx.config.join(PROFILE))?.is_none());
    assert!(regular(&ctx.config.join(PENDING))?.is_none());
    assert!(keys.added.borrow().is_empty());
    assert_eq!(keys.rotations.get(), 0);
    Ok(())
}

#[test]
fn single_pending_recovery_can_clear_an_unwritten_first_setup() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let requested = super::profile("https://example.test", "chosen")?;
    let key = Secret::New("synthetic-token-new");
    keys.fail_add.set(true);
    assert!(publish(&ctx, None, &requested, Some(key), &keys).is_err());
    assert!(regular(&ctx.config.join(PROFILE))?.is_none());
    assert_eq!(recover_setup(&ctx, None, &keys)?["mode"], "local");
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );

    keys.fail_add.set(false);
    publish(&ctx, None, &requested, Some(key), &keys)?;
    assert!(keys.load(&requested, deadline(15)).is_ok());
    Ok(())
}

#[test]
fn can_retain_a_legacy_item_only_after_proving_the_new_single_credential_was_committed()
-> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let previous = profile("https://example.test", "legacy")?;
    let next = super::profile("https://other.example.test", "new")?;
    let secret = Secret::New("fixture-token-private");
    publish(&ctx, None, &previous, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("legacy profile")?;
    keys.interrupt_single.set(true);
    assert!(publish(&ctx, Some(&before), &next, Some(secret), &keys).is_err());
    let pending = regular(&ctx.config.join(PENDING))?.ok_or("pending")?;
    assert!(retain_setup(&ctx, Some(&before), Some(&pending), &keys).is_err());

    keys.fail_retire.set(true);
    assert!(recover_setup(&ctx, Some(&before), &keys).is_err());
    let active = regular(&ctx.config.join(PROFILE))?.ok_or("published profile")?;
    assert!(read(&ctx.config)?.is_some_and(|profile| profile == next));
    let recorded = keys.single.take();
    assert!(recover_setup(&ctx, Some(&active), &keys).is_err());
    assert!(retain_setup(&ctx, Some(&active), Some(&pending), &keys).is_err());
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(pending.as_slice())
    );
    keys.single.replace(recorded);

    assert_eq!(
        retain_setup(&ctx, Some(&active), Some(&pending), &keys)?["cleanup"],
        "previous-entry-retained"
    );
    assert!(keys.removed.borrow().is_empty());
    let mut paused = next;
    paused.enabled = false;
    disable_profile(&ctx, Some(&active), &paused)?;
    assert!(read(&ctx.config)?.is_some_and(|profile| !profile.enabled));
    Ok(())
}

#[test]
fn recovery_rechecks_profile_bytes_when_the_keychain_lookup_takes_time() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let next = super::profile("https://other.example.test", "next")?;
    let secret = Secret::New("synthetic-token-first");
    publish(&ctx, None, &first, Some(secret), &keys)?;
    let path = ctx.config.join(PROFILE);
    let before = regular(&path)?.ok_or("profile")?;
    keys.ignore_single_write.set(true);
    assert!(publish(&ctx, Some(&before), &next, Some(secret), &keys).is_err());
    let pending = regular(&ctx.config.join(PENDING))?.ok_or("pending")?;
    let mut edited = first.clone();
    edited.model = "changed-during-keychain-lookup".into();
    let bytes = serde_json::to_vec(&edited)?;
    keys.file_edit_on_read
        .replace(Some((path.clone(), bytes.clone())));

    assert!(recover_setup(&ctx, Some(&before), &keys).is_err());
    assert_eq!(regular(&path)?.as_deref(), Some(bytes.as_slice()));
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(pending.as_slice())
    );
    assert_eq!(keys.load(&edited, deadline(15))?, b"synthetic-token-first");
    Ok(())
}

#[test]
fn corrupt_single_journals_cannot_clear_or_replace_the_credential() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let next = super::profile("https://other.example.test", "next")?;
    let secret = Secret::New("synthetic-token-first");
    publish(&ctx, None, &first, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
    let pending_path = ctx.config.join(PENDING);
    let base = json!({"profile":next,"previous":first,"added":false,"tagged":true});

    for (field, replacement) in [
        ("added", json!(true)),
        ("tagged", json!(false)),
        ("previous", json!(next)),
        ("previous", json!(null)),
    ] {
        let mut edited = base.clone();
        edited[field] = replacement;
        let old = regular(&pending_path)?;
        let bytes = serde_json::to_vec(&edited)?;
        write_cas(&pending_path, old.as_deref(), &bytes)?;
        assert!(
            recover_setup(&ctx, Some(&before), &keys).is_err(),
            "{field}"
        );
        assert_eq!(regular(&pending_path)?.as_deref(), Some(bytes.as_slice()));
        assert_eq!(
            regular(&ctx.config.join(PROFILE))?.as_deref(),
            Some(before.as_slice())
        );
    }
    let mut malformed = base.clone();
    malformed["previous"]["model"] = json!("bad model");
    let old = regular(&pending_path)?;
    write_cas(
        &pending_path,
        old.as_deref(),
        &serde_json::to_vec(&malformed)?,
    )?;
    assert!(recover_setup(&ctx, Some(&before), &keys).is_err());

    let old = regular(&pending_path)?;
    let bytes = serde_json::to_vec(&base)?;
    write_cas(&pending_path, old.as_deref(), &bytes)?;
    let alien = super::profile("https://unbound.example.test", "unbound")?;
    keys.single.replace(Some(alien));
    assert!(recover_setup(&ctx, Some(&before), &keys).is_err());
    assert_eq!(regular(&pending_path)?.as_deref(), Some(bytes.as_slice()));
    assert!(keys.removed.borrow().is_empty());
    assert_eq!(keys.rotations.get(), 0);
    Ok(())
}

#[test]
fn setup_requires_checking_the_pending_single_write_before_asking_for_a_new_key() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let next = super::profile("https://other.example.test", "next")?;
    let secret = Secret::New("synthetic-token-first");
    publish(&ctx, None, &first, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
    keys.fail_add.set(true);
    assert!(publish(&ctx, Some(&before), &next, Some(secret), &keys).is_err());
    let pending = regular(&ctx.config.join(PENDING))?.ok_or("pending intent")?;
    for action in ["1", "2", "3", "b", "r", "m"] {
        let result = wizard(
            &ctx,
            Some(&before),
            Some(first.clone()),
            &keys,
            |prompt| {
                assert_eq!(prompt, "选择");
                Ok(action.into())
            },
            || panic!("pending operation requested a new key"),
            |_, _, _| panic!("pending operation contacted the provider"),
        );
        let error = result.err().ok_or("expected setup to stop")?;
        assert_eq!(error.code, "model_keychain_update_pending");
    }
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(pending.as_slice())
    );
    assert_eq!(keys.load(&first, deadline(15))?, b"synthetic-token-first");
    Ok(())
}

#[test]
fn cleanup_checks_that_the_replacement_still_owns_the_profile_after_loading_the_key() -> TestResult
{
    let (_dir, ctx, keys) = single_fixture()?;
    let legacy = profile("https://example.test", "legacy")?;
    let fixed = super::profile("https://other.example.test", "new")?;
    let selected = Secret::New("fixture-token-private");
    publish(&ctx, None, &legacy, Some(selected), &keys)?;
    let path = ctx.config.join(PROFILE);
    let before = regular(&path)?.ok_or("legacy profile")?;
    let mut edited = fixed.clone();
    edited.model = "changed-during-cleanup".into();
    let replacement = serde_json::to_vec(&edited)?;
    keys.file_edit_on_load
        .replace(Some((path.clone(), replacement.clone())));

    let error = publish(&ctx, Some(&before), &fixed, Some(selected), &keys)
        .err()
        .ok_or("expected cleanup to stop")?;
    assert_eq!(error.code, "model_keychain_update_pending");
    assert_eq!(regular(&path)?.as_deref(), Some(replacement.as_slice()));
    assert!(keys.retire_requests.borrow().is_empty());
    assert!(keys.removed.borrow().is_empty());
    assert!(optional_record::<Pending>(regular(&ctx.config.join(PENDING))?.as_deref())?.is_some());
    Ok(())
}

#[test]
fn singleton_metadata_write_refuses_a_replaced_configuration_directory() -> TestResult {
    let (dir, mut ctx, _keys) = single_fixture()?;
    ctx.config = dir.path().join("external-config");
    let lock = lock_profile(&ctx, None)?;
    let file = lock.config.path.join(PENDING);
    lock.write(PENDING, None, b"null")?;
    let moved = ctx.config.with_extension("old");
    std::fs::rename(&ctx.config, &moved)?;
    let new_directory = private(&ctx.config)?;
    write_cas(&new_directory.path.join(PENDING), None, b"null")?;
    let error = lock
        .write(PENDING, Some(b"null"), b"changed")
        .err()
        .ok_or("expected directory identity to have changed")?;
    assert_eq!(error.code, "directory_changed");
    assert_eq!(regular(&file)?.as_deref(), Some(b"null".as_slice()));
    assert_eq!(
        regular(&moved.join(PENDING))?.as_deref(),
        Some(b"null".as_slice())
    );
    Ok(())
}

#[test]
fn malformed_replacement_key_preserves_the_retiring_legacy_item() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let previous = profile("https://example.test", "legacy")?;
    let next = super::profile("https://other.example.test", "new")?;
    let secret = Secret::New("fixture-token-private");
    publish(&ctx, None, &previous, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("legacy profile")?;
    keys.fail_retire.set(true);
    assert!(publish(&ctx, Some(&before), &next, Some(secret), &keys).is_err());
    keys.fail_retire.set(false);
    let active = regular(&ctx.config.join(PROFILE))?.ok_or("new profile")?;
    let pending = regular(&ctx.config.join(PENDING))?.ok_or("cleanup record")?;
    let requests = keys.retire_requests.borrow().len();
    keys.single_token.replace(Some(b"bad".to_vec()));

    let error = recover_setup(&ctx, Some(&active), &keys)
        .err()
        .ok_or("invalid key")?;
    assert_eq!(error.code, "model_keychain_cleanup_pending");
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(pending.as_slice())
    );
    assert_eq!(keys.retire_requests.borrow().len(), requests);
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn changed_config_snapshot_prevents_the_first_fixed_keychain_write() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let old = profile("https://example.test", "legacy")?;
    let next = super::profile("https://other.example.test", "next")?;
    let secret = Secret::New("fixture-token-private");
    publish(&ctx, None, &old, Some(secret), &keys)?;
    let path = ctx.config.join(PROFILE);
    let before = regular(&path)?.ok_or("profile")?;
    let mut edited = old.clone();
    edited.model = "changed-before-fixed-write".into();
    let edited = serde_json::to_vec(&edited)?;
    keys.file_edit_on_contains
        .replace(Some((path.clone(), edited.clone())));

    let error = publish(&ctx, Some(&before), &next, Some(secret), &keys)
        .err()
        .ok_or("expected replace to stop")?;
    assert_eq!(error.code, "model_keychain_update_pending");
    assert_eq!(regular(&path)?.as_deref(), Some(edited.as_slice()));
    assert!(keys.single.borrow().is_none());
    assert_eq!(keys.added.borrow().len(), 1);
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn a_deleted_fixed_item_can_be_recreated_with_a_confirmed_new_key() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let old = super::profile("https://example.test", "old")?;
    let next = super::profile("https://other.example.test", "next")?;
    publish(
        &ctx,
        None,
        &old,
        Some(Secret::New("synthetic-original-key")),
        &keys,
    )?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
    keys.single.take();
    keys.single_token.take();
    assert!(keys.load(&old, deadline(15)).is_err());
    publish(
        &ctx,
        Some(&before),
        &next,
        Some(Secret::New("synthetic-new-key")),
        &keys,
    )?;
    assert_eq!(old.service(&ctx.config), next.service(&ctx.config));
    assert_eq!(keys.rotations.get(), 0);
    assert_eq!(keys.load(&next, deadline(15))?, b"synthetic-new-key");
    assert!(keys.load(&old, deadline(15)).is_err());
    Ok(())
}

#[test]
fn access_to_an_explicit_synthetic_keychain_file_cannot_change_profile_state() -> TestResult {
    let (_dir, ctx, _keys) = single_fixture()?;
    let dummy = tempfile::NamedTempFile::new_in(&ctx.home)?;
    std::fs::write(dummy.path(), b"not a keychain")?;
    let external = ExternalItem::bind(dummy.path(), "fixture".into(), "account".into())?;
    let native = NativeKeychain(Some(&ctx.config));
    let error = native
        .external(&external, deadline(1))
        .err()
        .ok_or("invalid fixture was accepted")?;
    assert!(matches!(
        error.code.as_str(),
        "model_keychain_unavailable"
            | "model_keychain_timeout"
            | "model_keychain_item_missing"
            | "model_keychain_authorization_required"
    ));
    assert!(!ctx.config.exists());
    assert!(!ctx.home.join(".local/state/macops").exists());
    Ok(())
}

#[test]
fn a_stale_setup_confirmation_cannot_write_another_settings_version() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let next = super::profile("https://other.example.test", "next")?;
    let secret = Secret::New("synthetic-token-first");
    publish(&ctx, None, &first, Some(secret), &keys)?;
    let path = ctx.config.join(PROFILE);
    let before = regular(&path)?.ok_or("profile")?;
    let mut changed = first.clone();
    changed.model = "preserve-this-edit".into();
    write_cas(&path, Some(&before), &serde_json::to_vec(&changed)?)?;

    let error = publish(&ctx, Some(&before), &next, Some(secret), &keys)
        .err()
        .ok_or("stale settings accepted")?;
    assert_eq!(error.code, "model_setup_stale");
    assert!(read(&ctx.config)?.is_some_and(|profile| profile == changed));
    assert_eq!(keys.rotations.get(), 0);
    assert_eq!(keys.added.borrow().len(), 1);
    Ok(())
}

#[test]
fn recovery_rejects_a_legacy_journal_that_claims_the_current_fixed_item() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let current = super::profile("https://example.test", "current")?;
    let other = profile("https://example.test", "legacy")?;
    publish(
        &ctx,
        None,
        &current,
        Some(Secret::New("synthetic-current-key")),
        &keys,
    )?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
    let pending = ctx.config.join(PENDING);
    let foreign = serde_json::to_vec(&Pending {
        profile: other,
        previous: Some(current),
        added: true,
        tagged: true,
    })?;
    write_cas(&pending, Some(b"null"), &foreign)?;
    let error = recover_setup(&ctx, Some(&before), &keys)
        .err()
        .ok_or("unsafe legacy entry accepted")?;
    assert_eq!(error.code, "model_keychain_update_pending");
    assert!(keys.removed.borrow().is_empty());
    assert_eq!(regular(&pending)?.as_deref(), Some(foreign.as_slice()));
    Ok(())
}

fn maintenance_context(ctx: &RecommendSetupContext) -> Result<Context> {
    let cli = Cli::try_parse_from([
        "mo",
        "--home",
        ctx.home.to_str().ok_or_else(invalid)?,
        "history",
    ])
    .map_err(|_| invalid())?;
    Context::from_cli(&cli, &mut vec![])
}

fn fixture_maintenance_journal(ctx: &Context) -> Result<crate::store::Journal> {
    let mut journal = crate::test_support::journal()?;
    journal.home.clone_from(&ctx.home);
    journal.scope.clone_from(&ctx.scope);
    journal.scope_sha256 = crate::domain::fingerprint(&journal.scope)?;
    journal.state.clone_from(&ctx.state);
    journal.cache.clone_from(&ctx.cache);
    journal.settings.clone_from(&ctx.settings);
    Ok(journal)
}

fn stage_fixture_maintenance(ctx: &Context) -> Result<()> {
    let store = Store::lock(ctx)?;
    store.save(&fixture_maintenance_journal(ctx)?)
}

#[test]
fn model_settings_can_save_when_maintenance_starts_at_each_wizard_boundary() -> TestResult {
    for boundary in ["before-test", "during-test", "at-save"] {
        let (_dir, ctx, keys) = single_fixture()?;
        let maintenance = maintenance_context(&ctx)?;
        let sent = Cell::new(0);
        let data = wizard(
            &ctx,
            None,
            None,
            &keys,
            |prompt| match prompt {
                "选择" => Ok("1".into()),
                "发送最小测试请求？[y/N]" => {
                    if boundary == "before-test" {
                        stage_fixture_maintenance(&maintenance)?;
                    }
                    Ok("y".into())
                }
                "连接成功。保存设置？[y/N]" => {
                    if boundary == "at-save" {
                        stage_fixture_maintenance(&maintenance)?;
                    }
                    Ok("y".into())
                }
                _ => panic!("unexpected setup prompt"),
            },
            || Ok("synthetic-key-to-test".into()),
            |_, _, _| {
                sent.set(sent.get() + 1);
                if boundary == "during-test" {
                    stage_fixture_maintenance(&maintenance)?;
                }
                Ok(())
            },
        )?;
        assert_eq!(data["mode"], "remote");
        assert_eq!(sent.get(), 1);
        assert_eq!(keys.added.borrow().len(), 1);
        let after = crate::store::active(&maintenance)?.ok_or("original maintenance")?;
        assert_eq!(after.id, "OP-1");
        assert_eq!(after.phase, crate::store::Phase::Running);
        assert!(read(&ctx.config)?.is_some());
    }
    Ok(())
}

#[test]
fn a_read_only_saved_key_test_can_run_during_pending_maintenance() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let maintenance = maintenance_context(&ctx)?;
    let profile = super::profile("https://example.test", "fixture-model")?;
    publish(
        &ctx,
        None,
        &profile,
        Some(Secret::New("synthetic-key-to-test")),
        &keys,
    )?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
    stage_fixture_maintenance(&maintenance)?;
    let active = std::fs::read(maintenance.state.join("active.json"))?;
    let data = wizard(
        &ctx,
        Some(&before),
        Some(profile),
        &keys,
        |prompt| match prompt {
            "选择" => Ok("t".into()),
            "发送最小测试请求？[y/N]" => Ok("y".into()),
            _ => panic!("read-only test requested a save"),
        },
        || panic!("saved-key test requested a new key"),
        |_, key, _| {
            assert_eq!(key, "synthetic-key-to-test");
            Ok(())
        },
    )?;
    assert_eq!(data["connection"], "verified");
    assert_eq!(
        std::fs::read(maintenance.state.join("active.json"))?,
        active
    );
    assert_eq!(
        regular(&ctx.config.join(PROFILE))?.as_deref(),
        Some(before.as_slice())
    );
    assert_eq!(keys.added.borrow().len(), 1);
    Ok(())
}

#[test]
fn maintenance_started_during_saved_key_lookup_does_not_block_model_setup() -> TestResult {
    for binding_external in [false, true] {
        let (_dir, ctx, keys) = single_fixture()?;
        let maintenance = maintenance_context(&ctx)?;
        let profile = super::profile("https://example.test", "fixture-model")?;
        publish(
            &ctx,
            None,
            &profile,
            Some(Secret::New("synthetic-key-to-test")),
            &keys,
        )?;
        let before = regular(&ctx.config.join(PROFILE))?.ok_or("profile")?;
        let active = maintenance.state.join("active.json");
        std::fs::create_dir_all(&maintenance.state)?;
        let journal = serde_json::to_vec(&fixture_maintenance_journal(&maintenance)?)?;
        keys.file_edit_on_load
            .replace(Some((active.clone(), journal.clone())));
        let external = tempfile::NamedTempFile::new_in(&ctx.home)?;
        let answers = if binding_external {
            vec![
                "b",
                "https://new.example.test",
                "new-model",
                external.path().to_str().ok_or("fixture keychain")?,
                "fixture",
                "account",
                "y",
                "y",
            ]
        } else {
            vec!["m", "new-model", "y", "y"]
        };
        let mut answers = answers.into_iter();
        let result = wizard(
            &ctx,
            Some(&before),
            Some(profile),
            &keys,
            |_| answers.next().map(str::to_owned).ok_or_else(invalid),
            || panic!("requested a new key instead of using the chosen entry"),
            |_, _, _| Ok(()),
        )?;
        assert_eq!(result["model"], "new-model");
        assert_eq!(std::fs::read(active)?, journal);
        assert!(read(&ctx.config)?.is_some_and(|p| p.model == "new-model"));
        assert_eq!(keys.added.borrow().len(), 1);
    }
    Ok(())
}

#[test]
fn setup_can_stay_local_without_a_profile_during_pending_maintenance() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let maintenance = maintenance_context(&ctx)?;
    stage_fixture_maintenance(&maintenance)?;
    let active = maintenance.state.join("active.json");
    let before = std::fs::read(&active)?;
    let data = wizard(
        &ctx,
        None,
        None,
        &keys,
        |prompt| {
            assert_eq!(prompt, "选择");
            Ok("3".into())
        },
        || panic!("local recommendations requested a key"),
        |_, _, _| panic!("local recommendations sent a test request"),
    )?;
    assert_eq!(data["mode"], "local");
    assert_eq!(std::fs::read(active)?, before);
    assert!(!ctx.config.exists());
    Ok(())
}

#[test]
fn a_held_maintenance_lock_cannot_block_a_credential_save() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let maintenance = maintenance_context(&ctx)?;
    let store = Store::lock(&maintenance)?;
    store.save(&fixture_maintenance_journal(&maintenance)?)?;
    let active = maintenance.state.join("active.json");
    let before = std::fs::read(&active)?;
    assert!(Store::lock(&maintenance).is_err());

    let profile = super::profile("https://example.test", "local-settings")?;
    publish(
        &ctx,
        None,
        &profile,
        Some(Secret::New("synthetic-key-to-test")),
        &keys,
    )?;
    assert!(read(&ctx.config)?.is_some_and(|saved| saved == profile));
    assert_eq!(std::fs::read(active)?, before);
    assert_eq!(
        store.check_pending().err().ok_or("maintenance block")?.code,
        "recovery_required"
    );
    Ok(())
}

#[test]
fn setup_writes_share_one_account_lock_across_configuration_roots() -> TestResult {
    let (dir, ctx, _keys) = single_fixture()?;
    let other = RecommendSetupContext {
        home: ctx.home.clone(),
        config: dir.path().join("alternate-config"),
        timeout_secs: ctx.timeout_secs,
        isolated: ctx.isolated,
    };
    let lock = lock_profile(&ctx, None)?;
    let busy = lock_profile(&other, None)
        .err()
        .ok_or("expected a shared lock")?;
    assert_eq!(busy.code, "model_setup_busy");
    assert!(!other.config.exists());
    drop(lock);
    lock_profile(&other, None)?;

    let login = RecommendSetupContext {
        isolated: false,
        ..ctx
    };
    let redirected = RecommendSetupContext {
        home: other.config.clone(),
        config: other.config,
        timeout_secs: login.timeout_secs,
        isolated: false,
    };
    assert_eq!(lock_path(&login)?, lock_path(&redirected)?);
    Ok(())
}

#[test]
fn losing_the_held_setup_lock_during_an_item_lookup_prevents_the_key_write() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let first = super::profile("https://example.test", "first")?;
    let later = super::profile("https://other.example.test", "later")?;
    publish(
        &ctx,
        None,
        &first,
        Some(Secret::New("synthetic-original-key")),
        &keys,
    )?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("current profile")?;
    keys.lease_to_replace_before_write
        .replace(Some(lock_path(&ctx)?));

    let failure = publish(
        &ctx,
        Some(&before),
        &later,
        Some(Secret::New("synthetic-next-key")),
        &keys,
    )
    .err()
    .ok_or("detached lock published a key")?;
    assert_eq!(failure.code, "model_keychain_update_pending");
    assert_eq!(keys.rotations.get(), 0);
    assert_eq!(keys.load(&first, deadline(15))?, b"synthetic-original-key");
    assert_eq!(
        regular(&ctx.config.join(PROFILE))?.as_deref(),
        Some(before.as_slice())
    );
    assert_eq!(
        recover_setup(&ctx, Some(&before), &keys)?["model"],
        first.model
    );
    Ok(())
}

#[test]
fn setup_does_not_publish_metadata_after_the_lock_permissions_change() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let (_dir, ctx, _keys) = single_fixture()?;
    let lock = lock_profile(&ctx, None)?;
    std::fs::set_permissions(lock_path(&ctx)?, std::fs::Permissions::from_mode(0o755))?;
    let error = lock
        .write(PROFILE, None, b"{}")
        .err()
        .ok_or("lock mode was ignored")?;
    assert_eq!(error.code, "model_setup_lock_changed");
    assert!(!ctx.config.join(PROFILE).exists());
    Ok(())
}

#[test]
fn a_pending_intent_created_during_keychain_lookup_is_never_replaced() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let legacy = profile("https://example.test", "legacy")?;
    let competing = profile("https://example.test", "old-binary")?;
    let next = super::profile("https://other.example.test", "new-binary")?;
    let token = Secret::New("fixture-token-private");
    publish(&ctx, None, &legacy, Some(token), &keys)?;
    let profile_path = ctx.config.join(PROFILE);
    let before = regular(&profile_path)?.ok_or("legacy profile")?;
    let pending = ctx.config.join(PENDING);
    let competing_intent = serde_json::to_vec(&Pending {
        profile: competing,
        previous: Some(legacy),
        added: false,
        tagged: true,
    })?;
    keys.file_edit_on_contains
        .replace(Some((pending.clone(), competing_intent.clone())));

    assert!(publish(&ctx, Some(&before), &next, Some(token), &keys).is_err());
    assert_eq!(
        regular(&pending)?.as_deref(),
        Some(competing_intent.as_slice())
    );
    assert_eq!(regular(&profile_path)?.as_deref(), Some(before.as_slice()));
    assert_eq!(keys.added.borrow().len(), 1);
    assert!(keys.single.borrow().is_none());
    assert!(keys.removed.borrow().is_empty());
    Ok(())
}

#[test]
fn legacy_cleanup_preserves_the_original_key_if_its_intent_changes_during_lookup() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let previous = profile("https://example.test", "legacy")?;
    let replacement = super::profile("https://other.example.test", "current")?;
    let secret = Secret::New("fixture-token-private");
    publish(&ctx, None, &previous, Some(secret), &keys)?;
    let before = regular(&ctx.config.join(PROFILE))?.ok_or("legacy profile")?;
    let pending = ctx.config.join(PENDING);
    let foreign = serde_json::to_vec(&Pending {
        profile: profile("https://example.test", "other-intent")?,
        previous: Some(previous),
        added: false,
        tagged: true,
    })?;
    keys.file_edit_on_load
        .replace(Some((pending.clone(), foreign.clone())));

    let error = publish(&ctx, Some(&before), &replacement, Some(secret), &keys)
        .err()
        .ok_or("expected changed-intent error")?;
    assert_eq!(error.code, "model_keychain_update_pending");
    assert!(keys.removed.borrow().is_empty());
    assert_eq!(regular(&pending)?.as_deref(), Some(foreign.as_slice()));
    assert!(read(&ctx.config)?.is_some_and(|p| p == replacement));
    Ok(())
}

#[test]
fn a_setup_writer_cannot_replace_a_pending_record_visible_before_its_write() -> TestResult {
    let (_dir, ctx, _keys) = single_fixture()?;
    let lock = lock_profile(&ctx, None)?;
    let profile = super::profile("https://example.test", "original")?;
    let pending = serde_json::to_vec(&Pending {
        profile,
        previous: None,
        added: false,
        tagged: true,
    })?;
    lock.write(PENDING, None, &pending)?;
    let error = lock.empty_intent().err().ok_or("pending record accepted")?;
    assert_eq!(error.code, "model_keychain_update_pending");
    assert_eq!(
        regular(&ctx.config.join(PENDING))?.as_deref(),
        Some(pending.as_slice())
    );
    Ok(())
}

#[test]
fn legacy_cleanup_cannot_remove_a_key_after_its_profile_changed_during_lookup() -> TestResult {
    let (_dir, ctx, keys) = single_fixture()?;
    let old = profile("https://example.test", "first")?;
    let replacement = profile("https://other.example.test", "next")?;
    let token = Secret::New("fixture-token-private");
    publish(&ctx, None, &old, Some(token), &keys)?;
    let profile_file = ctx.config.join(PROFILE);
    let before = regular(&profile_file)?.ok_or("old profile")?;
    let mut edited = replacement.clone();
    edited.model = "changed-during-keychain-approval".into();
    let edited = serde_json::to_vec(&edited)?;
    keys.file_edit_on_load
        .replace(Some((profile_file.clone(), edited.clone())));

    let err = publish(&ctx, Some(&before), &replacement, Some(token), &keys)
        .err()
        .ok_or("removed an old key on an obsolete configuration")?;
    assert_eq!(err.code, "model_keychain_cleanup_pending");
    assert_eq!(regular(&profile_file)?.as_deref(), Some(edited.as_slice()));
    assert!(keys.removed.borrow().is_empty());
    assert!(optional_record::<Pending>(regular(&ctx.config.join(PENDING))?.as_deref())?.is_some());
    Ok(())
}

#[test]
fn a_changed_intent_during_keychain_verification_cannot_publish_profile_metadata() -> TestResult {
    for recovering in [false, true] {
        let (_dir, ctx, keys) = single_fixture()?;
        let first = super::profile("https://example.test", "first")?;
        let next = super::profile("https://other.example.test", "new")?;
        let secret = Secret::New("synthetic-original-key");
        publish(&ctx, None, &first, Some(secret), &keys)?;
        let path = ctx.config.join(PROFILE);
        let before = regular(&path)?.ok_or("profile")?;
        let pending = ctx.config.join(PENDING);
        if recovering {
            keys.interrupt_single.set(true);
            assert!(publish(&ctx, Some(&before), &next, Some(secret), &keys).is_err());
        }
        let other_record = serde_json::to_vec(&json!({"recovery":"foreign-record"}))?;
        keys.file_edit_on_read
            .replace(Some((pending.clone(), other_record.clone())));
        let failure = if recovering {
            recover_setup(&ctx, Some(&before), &keys).err()
        } else {
            publish(&ctx, Some(&before), &next, Some(secret), &keys).err()
        }
        .ok_or("profile was published using a replaced intent")?;
        assert_eq!(failure.code, "model_keychain_update_pending");
        assert_eq!(regular(&path)?.as_deref(), Some(before.as_slice()));
        assert_eq!(regular(&pending)?.as_deref(), Some(other_record.as_slice()));
        assert!(keys.load(&first, deadline(15)).is_err());
        assert!(keys.removed.borrow().is_empty());
    }
    Ok(())
}
