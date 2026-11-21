#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
fn scope() -> Scope {
    Scope {
        home: "/home".into(),
        native_lock: "/home/.agents/.skill-lock.json".into(),
        definitions: "/home/.config/macops/manifests/managed-agent-assets.json".into(),
    }
}
fn candidate(member: &str, checked_at: u64) -> Candidate {
    Candidate {
        member: member.into(),
        repository: "owner/repo".into(),
        locator_sha256: "a".repeat(64),
        requested_ref: None,
        commit: "b".repeat(40),
        skill_tree: "c".repeat(40),
        payload_sha256: "d".repeat(64),
        checked_at,
    }
}
#[test]
fn scoped_partial_refresh_preserves_every_unselected_members_individual_evidence() -> TestResult {
    let s = scope();
    let before =
        Index::empty(s.clone())?.merge(&s, vec![candidate("a", 10), candidate("b", 10)], 20)?;
    let mut selected = candidate("a", 20);
    selected.commit = "e".repeat(40);
    let after = before.merge(&s, vec![selected.clone()], 20)?;
    assert_eq!(after.entries["b"], before.entries["b"]);
    assert_eq!(after.entries["a"], selected);
    assert_eq!(read(&serde_json::to_vec(&after)?, &s, 20)?, after);
    assert_eq!(after.get(&s, "a", &"a".repeat(64), 20)?, &selected);
    assert_eq!(
        after
            .get(&s, "a", &"f".repeat(64), 20)
            .err()
            .ok_or("stale")?
            .code,
        "source_candidate_stale"
    );
    assert_eq!(
        after
            .get(&s, "missing", &"a".repeat(64), 20)
            .err()
            .ok_or("missing")?
            .code,
        "source_candidate_missing"
    );
    Ok(())
}
#[test]
fn cache_cannot_cross_scope_accept_a_duplicate_or_turn_legacy_unknown_data_into_a_new_index()
-> TestResult {
    let s = scope();
    let index = Index::empty(s.clone())?;
    assert!(
        index
            .merge(&s, vec![candidate("a", 10), candidate("a", 10)], 20)
            .is_err()
    );
    let mut other = s.clone();
    other.home = "/other".into();
    assert!(index.validate(&other, 20).is_err());
    assert!(read(br#"{"schema_version":1,"revisions":{}}"#, &s, 20).is_err());
    assert!(read(br#"{"schema_version":1,"schema_version":1}"#, &s, 20).is_err());
    for variant in 0..7 {
        let mut c = candidate("a", 10);
        match variant {
            0 => c.checked_at = 21,
            1 => c.commit = "bad".into(),
            2 => c.skill_tree = "0".repeat(40),
            3 => c.payload_sha256 = "bad".into(),
            4 => c.repository = "Owner/Repo".into(),
            5 => c.requested_ref = Some("main~1".into()),
            _ => c.locator_sha256 = "bad".into(),
        }
        assert!(index.merge(&s, vec![c], 20).is_err());
    }
    Ok(())
}

#[test]
fn index_validation_rejects_invalid_scope_member_mapping_and_excessive_partial_merge() -> TestResult
{
    let s = scope();
    let index = Index::empty(s.clone())?;
    let mut bad_scope = s.clone();
    bad_scope.home = "relative".into();
    assert!(Index::empty(bad_scope.clone()).is_err());
    assert!(index.validate(&bad_scope, 20).is_err());
    let mut invalid_self = index.clone();
    invalid_self.scope = bad_scope;
    assert!(invalid_self.validate(&s, 20).is_err());
    assert!(invalid_self.merge(&s, vec![], 20).is_err());
    assert!(invalid_self.get(&s, "a", &"a".repeat(64), 20).is_err());
    let mut bad = index.clone();
    bad.entries.insert("other".into(), candidate("a", 10));
    assert!(bad.validate(&s, 20).is_err());
    for member in [String::new(), "a\nb".into(), "a".repeat(257)] {
        assert!(candidate(&member, 10).validate(20).is_err());
    }
    assert!(candidate("a", 0).validate(20).is_err());
    let mut bad = index.clone();
    bad.schema_version = 2;
    assert!(read(&serde_json::to_vec(&bad)?, &s, 20).is_err());
    let full = index.merge(
        &s,
        (0..128).map(|n| candidate(&format!("m{n}"), 10)).collect(),
        20,
    )?;
    assert!(full.merge(&s, vec![candidate("extra", 10)], 20).is_err());
    assert!(!legacy(b"[]")?);
    assert!(legacy(b"{").is_err());
    Ok(())
}

#[test]
fn malformed_repository_cached_candidate_and_unserializable_or_oversize_scope_fail_closed()
-> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let s = scope();
    let mut bad = candidate("a", 10);
    bad.repository = "../repo".into();
    assert!(bad.validate(20).is_err());
    let mut index = Index::empty(s.clone())?;
    index.entries.insert("a".into(), candidate("a", 0));
    assert!(index.validate(&s, 20).is_err());
    let mut non_utf8 = s.clone();
    non_utf8.home = std::ffi::OsString::from_vec(vec![b'/', 255]).into();
    assert!(
        Index::empty(non_utf8.clone())?
            .merge(&non_utf8, vec![], 20)
            .is_err()
    );
    let mut oversized = s;
    oversized.home = format!("/{}", "a".repeat(16 * 1024 * 1024)).into();
    assert!(
        Index::empty(oversized.clone())?
            .merge(&oversized, vec![], 20)
            .is_err()
    );
    Ok(())
}

#[test]
fn legacy_index_is_recognized_only_with_the_complete_original_typed_shape() -> TestResult {
    let good = serde_json::json!({"schema_version":1,"sources_sha256":"a".repeat(64),"checked_at":10,"checked_at_by_asset":{"alpha":10},"revisions":{"alpha":{"revision":"artifact","future":["kept"]}}});
    assert!(legacy(&serde_json::to_vec(&good)?)?);
    for (key, value) in [
        ("schema_version", serde_json::json!(2)),
        ("sources_sha256", serde_json::json!("bad")),
        ("checked_at", serde_json::Value::Null),
        ("checked_at_by_asset", serde_json::json!({"alpha":null})),
        ("revisions", serde_json::json!({"alpha":null})),
    ] {
        let mut changed = good.clone();
        changed[key] = value;
        assert!(!legacy(&serde_json::to_vec(&changed)?)?);
    }
    Ok(())
}
