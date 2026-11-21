#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{fs, os::unix::fs::PermissionsExt};
fn fixture(
    existing: bool,
) -> std::result::Result<(tempfile::TempDir, Pair), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path().join("root");
    fs::create_dir(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let target = root.join("native.json");
    if existing {
        fs::write(&target, b"old\0\xff")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640))?;
    }
    let namespace = files::namespace(&target)?;
    let state = files::observe(&namespace, 1024)?.0;
    let pair = stage(
        &Witness {
            path: target,
            namespace,
            state,
        },
        b"new\0\xfe",
        if existing { 0o640 } else { 0o600 },
    )?;
    Ok((temp, pair))
}
#[test]
fn source_file_exchange_and_missing_noreplace_roundtrip_exact_original_inode_uid_mode_and_binary_bytes()
-> TestResult {
    for existing in [false, true] {
        let (_temp, pair) = fixture(existing)?;
        assert_eq!(position(&pair)?, Position::Before);
        let before = pair.before.clone();
        assert!(publish_checked(&pair, || Ok(()))?.exchanged);
        assert_eq!(observe(&pair.live)?, Some(pair.after.clone()));
        assert_eq!(observe(&pair.retained)?, before);
        assert!(!publish_checked(&pair, || Ok(()))?.exchanged);
        assert!(restore_checked(&pair, || Ok(()))?.exchanged);
        assert_eq!(observe(&pair.live)?, before);
        assert_eq!(observe(&pair.retained)?, Some(pair.after.clone()));
        assert!(!restore_checked(&pair, || Ok(()))?.exchanged);
        assert_eq!(fs::read(pair.retained.path())?, b"new\0\xfe");
        if existing {
            assert_eq!(fs::read(pair.live.path())?, b"old\0\xff");
        }
    }
    Ok(())
}
#[test]
fn eight_last_check_windows_preserve_foreign_full_object_and_never_retry_unknown_pair() -> TestResult
{
    for existing in [false, true] {
        for reverse in [false, true] {
            for target in [false, true] {
                let (temp, pair) = fixture(existing)?;
                if reverse {
                    publish_checked(&pair, || Ok(()))?;
                }
                let endpoint = if target { &pair.live } else { &pair.retained };
                let foreign = std::cell::RefCell::new(None);
                let snapshot = std::cell::RefCell::new(None);
                let result = move_towards(
                    &pair,
                    if reverse {
                        Position::Before
                    } else {
                        Position::OwnAfter
                    },
                    || Ok(()),
                    || {
                        if endpoint.path().exists() {
                            fs::rename(endpoint.path(), temp.path().join("environment-displaced"))?;
                        }
                        fs::write(endpoint.path(), b"foreign\0\xff")?;
                        fs::set_permissions(endpoint.path(), fs::Permissions::from_mode(0o640))?;
                        *foreign.borrow_mut() = observe(endpoint)?;
                        *snapshot.borrow_mut() =
                            Some((observe(&pair.live)?, observe(&pair.retained)?));
                        Ok(())
                    },
                );
                assert!(
                    result.is_err(),
                    "existing={existing} reverse={reverse} target={target}"
                );
                let blocked = !existing && ((!reverse && target) || (reverse && !target));
                let destination = if blocked {
                    endpoint
                } else if target {
                    &pair.retained
                } else {
                    &pair.live
                };
                assert_eq!(observe(destination)?, *foreign.borrow());
                assert_eq!(fs::read(destination.path())?, b"foreign\0\xff");
                if blocked {
                    assert_eq!(
                        Some((observe(&pair.live)?, observe(&pair.retained)?)),
                        *snapshot.borrow()
                    );
                }
                assert_eq!(position(&pair)?, Position::Unknown);
                let before_retry = (observe(&pair.live)?, observe(&pair.retained)?);
                assert!(restore_checked(&pair, || Ok(())).is_err());
                assert!(publish_checked(&pair, || Ok(())).is_err());
                assert_eq!(
                    before_retry,
                    (observe(&pair.live)?, observe(&pair.retained)?)
                );
            }
        }
    }
    Ok(())
}
#[test]
fn file_pair_refuses_same_bytes_new_inode_mode_drift_deleted_retained_and_failed_final_guard()
-> TestResult {
    let (_temp, pair) = fixture(true)?;
    let before = (observe(&pair.live)?, observe(&pair.retained)?);
    assert!(publish_checked(&pair, || Err(invalid())).is_err());
    assert_eq!(before, (observe(&pair.live)?, observe(&pair.retained)?));
    fs::set_permissions(pair.retained.path(), fs::Permissions::from_mode(0o600))?;
    assert_eq!(position(&pair)?, Position::Unknown);
    assert!(publish_checked(&pair, || Ok(())).is_err());
    let (temp, pair) = fixture(true)?;
    fs::rename(pair.retained.path(), temp.path().join("saved"))?;
    fs::write(pair.retained.path(), b"new\0\xfe")?;
    fs::set_permissions(pair.retained.path(), fs::Permissions::from_mode(0o640))?;
    assert_eq!(position(&pair)?, Position::Unknown);
    let (_temp, pair) = fixture(true)?;
    publish_checked(&pair, || Ok(()))?;
    fs::remove_file(pair.retained.path())?;
    assert_eq!(position(&pair)?, Position::Unknown);
    assert!(restore_checked(&pair, || Ok(())).is_err());
    Ok(())
}
