use super::*;
use crate::test_support::TestResult;

const FIXTURE_TOKEN: &[u8] = b"fixture-token-private";

fn profile() -> Result<Profile> {
    Ok(serde_json::from_value(serde_json::json!({
        "schemaVersion": 1, "endpoint": "https://example.test/chat/completions",
        "model": "example-model", "account": format!("setup-{}", "1".repeat(64)), "enabled": true
    }))?)
}

#[test]
fn retained_legacy_and_tagged_credentials_use_the_right_namespace() -> TestResult {
    let config = Path::new("/private/tmp/model-config");
    let profile = profile()?;
    let legacy = resolve(&profile, config, FIXTURE_TOKEN.to_vec(), |_| {
        Err(unavailable())
    })?;
    assert_eq!(legacy, FIXTURE_TOKEN);
    let bytes = encode(
        &profile,
        config,
        Secret::New(std::str::from_utf8(FIXTURE_TOKEN)?),
    )?;
    let owned = resolve(&profile, config, bytes.clone(), |_| Err(unavailable()))?;
    assert_eq!(owned, FIXTURE_TOKEN);
    let mut moved = profile.clone();
    moved.endpoint = "https://other.example.test/chat/completions".into();
    assert!(resolve(&moved, config, bytes.clone(), |_| Err(unavailable())).is_err());
    assert!(
        resolve(&profile, Path::new("/private/tmp/other"), bytes, |_| Err(
            unavailable()
        ))
        .is_err()
    );
    Ok(())
}

#[test]
fn borrowed_reference_does_not_copy_the_key_and_rejects_rebinding_or_replacement() -> TestResult {
    let f = tempfile::NamedTempFile::new_in("/private/tmp")?;
    let item = ExternalItem::bind(f.path(), "my provider".into(), "primary".into())?;
    assert!(
        ExternalItem::bind(
            f.path(),
            "org.macops.recommend.deepseek".into(),
            "default".into()
        )
        .is_err()
    );
    let reference = BorrowedItem::verified(item, std::str::from_utf8(FIXTURE_TOKEN)?);
    let p = profile()?;
    let cfg = Path::new("/private/tmp/model-config");
    let data = encode(&p, cfg, Secret::Existing(&reference))?;
    assert!(
        !data
            .windows(FIXTURE_TOKEN.len())
            .any(|part| part == FIXTURE_TOKEN)
    );
    assert_eq!(
        resolve(&p, cfg, data.clone(), |item| {
            assert_eq!(item.path, f.path());
            assert_eq!(item.service, "my provider");
            assert_eq!(item.account, "primary");
            Ok(FIXTURE_TOKEN.to_vec())
        })?,
        FIXTURE_TOKEN
    );
    let error = resolve(&p, cfg, data.clone(), |_| {
        Ok(b"fixture-changed-token".to_vec())
    })
    .err()
    .ok_or("replaced token was accepted")?;
    assert_eq!(error.code, "model_keychain_binding_changed");
    assert!(resolve(&p, cfg, data, |_| Ok(FORMAT.to_vec())).is_err());
    assert!(
        external(&ExternalItem {
            file: FileId {
                device: 0,
                inode: 0
            },
            ..reference.item
        })
        .is_err()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_result_requires_an_unambiguous_match() -> TestResult {
    assert!(!native::unique_match(0)?);
    assert!(native::unique_match(1)?);
    assert!(native::unique_match(2).is_err());
    Ok(())
}

#[test]
fn only_proven_slots_are_eligible_for_automatic_rotation_cleanup() -> TestResult {
    let config = Path::new("/private/tmp/model-config");
    let p = profile()?;
    assert!(!eligible_removal(config, &p, FIXTURE_TOKEN, true)?);
    assert!(eligible_removal(config, &p, FIXTURE_TOKEN, false)?);
    let owned = encode(&p, config, Secret::New(std::str::from_utf8(FIXTURE_TOKEN)?))?;
    assert!(eligible_removal(config, &p, &owned, true)?);
    assert!(eligible_removal(Path::new("/private/tmp/other"), &p, &owned, true).is_err());
    let wrong_kind = [FORMAT, stamp(&p, config).as_bytes(), &[99]].concat();
    assert!(eligible_removal(config, &p, &wrong_kind, true).is_err());
    let broken_ref = [FORMAT, stamp(&p, config).as_bytes(), &[BORROWED], b"{}"].concat();
    let error = eligible_removal(config, &p, &broken_ref, true)
        .err()
        .ok_or("bad locator")?;
    assert_eq!(error.code, "model_keychain_unavailable");
    Ok(())
}

#[test]
fn lookup_rechecks_single_item_and_deletion_rechecks_the_exact_result() -> TestResult {
    use std::cell::Cell;
    let none = verified_item(
        || Ok(false),
        || -> Result<(Vec<u8>, u8)> {
            panic!("missing keychain entry must avoid reading password")
        },
    )?;
    assert!(none.is_none());
    assert!(verified_item(|| Err(unavailable()), || Ok((FIXTURE_TOKEN, 7))).is_err());
    assert!(
        verified_item(
            || Ok(true),
            || -> Result<(Vec<u8>, u8)> { Err(unavailable()) }
        )
        .is_err()
    );
    let checks = Cell::new(0);
    assert!(
        verified_item(
            || {
                checks.set(checks.get() + 1);
                Ok(checks.get() == 1)
            },
            || Ok((FIXTURE_TOKEN, 7))
        )
        .is_err()
    );
    assert!(verified_item(|| Ok(true), || Ok((vec![0; MAX_VALUE + 1], 7))).is_err());

    let present = Cell::new(true);
    let deleted = Cell::new(0);
    let erase = |item| {
        deleted.set(item);
        present.set(false);
    };
    delete_verified(
        || Ok(present.get()),
        || Ok((FIXTURE_TOKEN, 7)),
        |_| Ok(false),
        erase,
    )?;
    assert_eq!(deleted.get(), 0);
    assert!(
        delete_verified(
            || Ok(present.get()),
            || Ok((FIXTURE_TOKEN, 7)),
            |_| Err(unavailable()),
            erase
        )
        .is_err()
    );
    assert_eq!(deleted.get(), 0);
    assert!(
        delete_verified(
            || Ok(present.get()),
            || Ok((FIXTURE_TOKEN, 7)),
            |_| Ok(true),
            |_| ()
        )
        .is_err()
    );
    delete_verified(
        || Ok(present.get()),
        || Ok((FIXTURE_TOKEN, 7)),
        |_| Ok(true),
        erase,
    )?;
    assert_eq!(deleted.get(), 7);
    assert!(!present.get());
    delete_verified(
        || Ok(false),
        || -> Result<(Vec<u8>, u8)> { Err(unavailable()) },
        |_| Ok(true),
        erase,
    )?;
    Ok(())
}

#[test]
fn stored_borrowed_load_checks_owned_lookup_then_exact_external_scope_and_deadline() -> TestResult {
    use std::cell::Cell;
    use std::time::Duration;
    let file = tempfile::NamedTempFile::new_in("/private/tmp")?;
    let p = profile()?;
    let config = Path::new("/private/tmp/model-config");
    let service = p.service(config);
    let item = ExternalItem::bind(file.path(), "provider".into(), "owner".into())?;
    let locator = BorrowedItem::verified(item, std::str::from_utf8(FIXTURE_TOKEN)?);
    let bytes = encode(&p, config, Secret::Existing(&locator))?;
    let reads = Cell::new(0);
    let fixture = |path: Option<&Path>, name: &str, account: &str| {
        reads.set(reads.get() + 1);
        if let Some(path) = path {
            assert_eq!(path, file.path());
            assert_eq!((name, account), ("provider", "owner"));
            Ok(Some(FIXTURE_TOKEN.to_vec()))
        } else {
            assert_eq!((name, account), (service.as_str(), p.account()));
            Ok(Some(bytes.clone()))
        }
    };
    assert_eq!(load_from(&p, config, None, fixture)?, FIXTURE_TOKEN);
    assert_eq!(reads.get(), 2);
    assert!(
        load_from(
            &p,
            config,
            Some(
                Instant::now()
                    .checked_sub(Duration::from_secs(1))
                    .ok_or("fixture deadline")?
            ),
            fixture
        )
        .is_err()
    );
    assert_eq!(reads.get(), 3);
    assert!(load_from(&p, config, None, |_, _, _| Ok(None)).is_err());
    assert!(load_from(&p, config, None, |_, _, _| Err(unavailable())).is_err());
    let entry = ExternalItem::bind(file.path(), "provider".into(), "owner".into())?;
    assert!(external_from(&entry, |_, _, _| Ok(None)).is_err());
    assert!(external_from(&entry, |_, _, _| Ok(Some(b"not valid".to_vec()))).is_err());
    let repl = tempfile::NamedTempFile::new_in("/private/tmp")?;
    assert!(
        external_from(&entry, |_, _, _| {
            std::fs::rename(repl.path(), file.path())?;
            Ok(Some(FIXTURE_TOKEN.to_vec()))
        })
        .is_err()
    );
    Ok(())
}

#[test]
fn malformed_slot_and_location_validation_fail_closed() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let file = temp.path().join("plain");
    std::fs::write(&file, b"fixture")?;
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&file, &link)?;
    assert!(file_identity(&link).is_err());
    assert!(file_identity(temp.path()).is_err());
    assert!(file_identity(&temp.path().join("missing")).is_err());
    for (service, account) in [
        ("", "account"),
        ("provider", ""),
        ("provider", "account\nsecond"),
        ("ORG.MACOPS.RECOMMEND.deepseek", "account"),
    ] {
        assert!(ExternalItem::bind(&file, service.into(), account.into()).is_err());
    }
    let p = profile()?;
    let config = Path::new("/private/tmp/model-config");
    assert!(encode(&p, config, Secret::New(&"x".repeat(MAX_VALUE))).is_err());
    let invalid_owned = [FORMAT, stamp(&p, config).as_bytes(), &[OWNED], b"bad"].concat();
    assert!(slot(&p, config, &invalid_owned).is_err());
    let mut borrowed = BorrowedItem::verified(
        ExternalItem::bind(&file, "provider".into(), "owner".into())?,
        std::str::from_utf8(FIXTURE_TOKEN)?,
    );
    borrowed.fingerprint = "unverified".into();
    assert!(
        slot(
            &p,
            config,
            &encode(&p, config, Secret::Existing(&borrowed))?
        )
        .is_err()
    );
    assert!(slot(&p, config, b"unverified").is_err());
    Ok(())
}

#[test]
fn bounded_reads_cover_completion_expiry_and_keychain_failure() -> TestResult {
    use std::time::Duration;
    assert_eq!(
        bounded(Instant::now() + Duration::from_secs(2), || Ok(
            FIXTURE_TOKEN.to_vec()
        ))
        .map_err(|_| unavailable())?,
        FIXTURE_TOKEN
    );
    assert_eq!(
        bounded(Instant::now() + Duration::from_secs(2), || Err(
            unavailable()
        ))
        .err(),
        Some(ReadFailure::Keychain)
    );
    assert_eq!(
        bounded(
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .ok_or("fixture deadline")?,
            || panic!("expired read started")
        )
        .err(),
        Some(ReadFailure::Deadline)
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn native_missing_and_denied_matches_stay_distinct_in_isolated_scope() -> TestResult {
    use security_framework::base::Error as KeychainError;
    assert!(!native::unique_search(Err(KeychainError::from_code(
        -25300
    )))?);
    assert!(native::unique_search(Err(KeychainError::from_code(-25308))).is_err());
    assert!(!native::unique_search(Ok(vec![]))?);
    let dir = tempfile::tempdir_in("/private/tmp")?;
    // The injected Keychain file path is explicit and never falls back to the user's domains.
    assert!(!matches!(
        native::read(
            Some(&dir.path().join("absent.keychain-db")),
            "fixture",
            "none"
        ),
        Ok(Some(_))
    ));
    Ok(())
}

#[test]
fn expired_bound_slot_read_cancels_before_opening_a_keychain() -> TestResult {
    let p = profile()?;
    let expired = Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .ok_or("fixture time")?;
    let result = load_until(&p, Path::new("/private/tmp/model-config"), expired);
    assert_eq!(result.err(), Some(ReadFailure::Deadline));
    Ok(())
}

#[test]
fn fixed_entry_is_bound_to_the_confirmed_endpoint_and_update_generation() -> TestResult {
    let area = Path::new("/fixture/config");
    let p = |endpoint, suffix: &str| -> Result<Profile> {
        Ok(serde_json::from_value(serde_json::json!({
            "schemaVersion":2,"enabled":true,"endpoint":endpoint,"model":"fixture",
            "account":format!("setup-{}",suffix.repeat(64))
        }))?)
    };
    let first = p("https://example.test/chat/completions", "a")?;
    let next = p("https://other.example.test/chat/completions", "b")?;
    let rebound = p("https://other.example.test/chat/completions", "a")?;
    let raw = encode(&first, area, Secret::New("synthetic-token-original"))?;
    assert!(matches!(
        classify_entry(area, Some(&first), &next, Some(&raw))?,
        EntryState::Previous
    ));
    assert!(matches!(
        classify_entry(area, Some(&first), &next, None)?,
        EntryState::Missing
    ));
    assert!(slot(&next, area, &raw).is_err());
    assert!(slot(&rebound, area, &raw).is_err());
    assert!(slot(&first, Path::new("/different/config"), &raw).is_err());
    assert!(slot(&first, area, b"synthetic-foreign-key").is_err());
    assert!(classify_entry(area, None, &next, Some(&raw)).is_err());
    let changed = encode(&next, area, Secret::New("synthetic-token-new"))?;
    assert!(matches!(
        classify_entry(area, Some(&first), &next, Some(&changed))?,
        EntryState::Next
    ));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn quiet_keychain_access_preserves_interaction_state_for_nested_reads_and_errors() -> TestResult {
    use security_framework::os::macos::keychain::SecKeychain;

    let initial = SecKeychain::user_interaction_allowed()?;
    native::without_interaction(|| {
        assert!(!SecKeychain::user_interaction_allowed().map_err(|_| unavailable())?);
        assert!(native::without_interaction(|| Err::<(), _>(unavailable())).is_err());
        assert!(!SecKeychain::user_interaction_allowed().map_err(|_| unavailable())?);
        Ok(())
    })?;
    assert_eq!(SecKeychain::user_interaction_allowed()?, initial);
    Ok(())
}

#[test]
fn fixed_item_update_targets_the_verified_item_and_rejects_unowned_slots() -> TestResult {
    use std::cell::Cell;

    let present = Cell::new(false);
    let cursor = || Ok(present.get());
    let found = Cell::new(0);
    let find = || {
        found.set(found.get() + 1);
        Ok((b"owned".as_slice(), 7))
    };
    assert!(
        write_verified(cursor, find, None::<fn(&[u8]) -> bool>, |item| {
            assert_eq!(item, None);
            present.set(true);
            Ok(())
        })
        .is_ok()
    );
    assert_eq!(found.get(), 0);
    let reject_write = |_| Err(Error::new("unexpected-write", "synthetic"));
    for eligible in [None, Some((|_: &[u8]| false) as fn(&[u8]) -> bool)] {
        let failure = write_verified(cursor, find, eligible, reject_write)
            .err()
            .ok_or("unowned entry")?;
        assert_eq!(failure.code, "model_keychain_unavailable");
    }
    assert_eq!(found.get(), 1);
    assert!(
        write_verified(
            cursor,
            find,
            Some(|bytes: &[u8]| bytes == b"owned"),
            |item| {
                assert_eq!(item, Some(7));
                Ok(())
            }
        )
        .is_ok()
    );
    assert!(
        write_verified(cursor, find, Some(|_: &[u8]| true), |item| {
            assert_eq!(item, Some(7));
            present.set(false);
            Ok(())
        })
        .is_err()
    );
    assert_eq!(found.get(), 3);
    Ok(())
}

#[test]
fn fixed_entry_never_updates_after_the_original_item_disappears() -> TestResult {
    use std::cell::Cell;

    let probes = Cell::new(0);
    let present = || {
        probes.set(probes.get() + 1);
        Ok(probes.get() == 1)
    };
    let find = || -> Result<(&'static [u8], u8)> { panic!("item no longer exists") };
    let result = write_verified(present, find, Some(|_: &[u8]| true), |_| {
        panic!("item identity changed")
    });
    assert!(result.is_err());
    assert_eq!(probes.get(), 2);
    let result = write_verified(
        || Ok(true),
        || Ok((b"owned".as_slice(), 7)),
        Some(|_: &[u8]| true),
        |_| Err(Error::attention("synthetic-write-denied", "fixture")),
    );
    assert_eq!(
        result.err().ok_or("write failure")?.code,
        "synthetic-write-denied"
    );
    Ok(())
}

#[test]
fn fixed_slot_can_reference_an_external_key_without_copying_it() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let external_path = temp.path().join("fixture.keychain");
    std::fs::write(&external_path, b"synthetic metadata")?;
    let external_item = ExternalItem::bind(&external_path, "provider".into(), "test".into())?;
    let token = "synthetic-borrowed-key";
    let borrowed = BorrowedItem::verified(external_item, token);
    let mut raw_profile = serde_json::to_value(profile()?)?;
    raw_profile["schemaVersion"] = serde_json::json!(2);
    let current = serde_json::from_value::<Profile>(raw_profile)?;
    let config = temp.path();
    let pointer = encode(&current, config, Secret::Existing(&borrowed))?;
    assert!(
        !pointer
            .windows(token.len())
            .any(|bytes| bytes == token.as_bytes())
    );
    let result = load_from(&current, config, None, |scope, service, account| {
        if let Some(item) = scope {
            assert_eq!(item, external_path);
            assert_eq!((service, account), ("provider", "test"));
            Ok(Some(token.as_bytes().to_vec()))
        } else {
            assert_eq!(
                (service, account),
                ("org.macops.recommend.api-key", "default")
            );
            Ok(Some(pointer.clone()))
        }
    })?;
    assert_eq!(result, token.as_bytes());
    Ok(())
}
