use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::cell::RefCell;

struct FixtureKeys {
    added: RefCell<Vec<String>>,
    removed: RefCell<Vec<String>>,
}

impl KeyStore for FixtureKeys {
    fn add(&self, profile: &Profile, token: &str) -> Result<()> {
        assert_eq!(token, "fixture-token-private");
        self.added.borrow_mut().push(profile.account.clone());
        Ok(())
    }
    fn remove(&self, profile: &Profile) -> Result<()> {
        self.removed.borrow_mut().push(profile.account.clone());
        Ok(())
    }
    fn contains(&self, profile: &Profile) -> Result<bool> {
        Ok(self.added.borrow().contains(&profile.account))
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
    assert!(isolation.add(&p, "fixture-token-private").is_err());
    assert!(isolation.remove(&p).is_err());
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
        "setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys {
        added: RefCell::new(vec![]),
        removed: RefCell::new(vec![]),
    };
    let first = profile("https://api.example.test/v1", "example-model")?;
    assert!(read(&ctx)?.is_none());
    publish(&ctx, None, &first, Some("fixture-token-private"), &keys)?;
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
            Some("fixture-token-private"),
            &keys
        )
        .is_err()
    );
    let uncertain = profile("https://uncertain.example.test", "third-model")?;
    let not_added = serde_json::to_vec(&Pending {
        profile: uncertain.clone(),
        added: false,
    })?;
    write_cas(&transition, Some(b"null"), &not_added)?;
    recover_pending(&ctx.config, Some(&paused), &keys)?;
    assert_eq!(regular(&transition)?.as_deref(), Some(b"null".as_slice()));
    keys.add(&uncertain, "fixture-token-private")?;
    let pending = serde_json::to_vec(&Pending {
        profile: uncertain,
        added: false,
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
        "setup",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let keys = FixtureKeys {
        added: RefCell::new(vec![]),
        removed: RefCell::new(vec![]),
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
            |selected, key| {
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
    keys.add(&pending, mock)?;
    let raw_pending = serde_json::to_vec(&Pending {
        profile: pending,
        added: false,
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
