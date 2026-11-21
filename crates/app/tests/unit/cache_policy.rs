#![cfg(test)]
use super::*;
use crate::{domain::*, test_support::*};

#[test]
fn ownership_is_not_granted_by_reuse_or_current_reference() {
    let mut b = book();
    let hash = "a".repeat(64);
    assert!(!register_current(&mut b, &hash, false));
    assert_eq!(b.current, Some(hash.clone()));
    assert!(b.owned.is_empty());
    assert!(register_current(&mut b, &hash, true));
    assert!(register_current(&mut b, &hash, false));
    let list = review_names(
        vec!["indexes".into(), "user.json".into()],
        vec![
            ".macops-scope.json".into(),
            format!("{hash}.json"),
            "foreign.json".into(),
        ],
        &b.owned,
    );
    assert_eq!(list, vec!["indexes/foreign.json", "user.json"]);
}

#[test]
fn registry_and_scope_identity_fail_closed() -> TestResult {
    assert_eq!(
        digest(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert!(valid_hash(&"a".repeat(64)));
    for h in ["", &"a".repeat(63), &"A".repeat(64), &"x".repeat(64)] {
        assert!(!valid_hash(h));
    }
    let id = identity();
    require_identity(&id, &id)?;
    let mut bad = id.clone();
    bad.indexes.inode += 1;
    assert!(require_identity(&id, &bad).is_err());
    let marker = Marker {
        format: "macops-index-scope-v1".into(),
        scope_id: id.scope_id.clone(),
    };
    validate_marker(&marker)?;
    let mut bad = marker;
    bad.format = "other".into();
    assert!(validate_marker(&bad).is_err());
    bad.format = "macops-index-scope-v1".into();
    bad.scope_id = "bad".into();
    assert!(validate_marker(&bad).is_err());
    let b = book();
    unchanged_marker(None, None)?;
    let good = Marker {
        format: "macops-index-scope-v1".into(),
        scope_id: b.identity.scope_id.clone(),
    };
    unchanged_marker(Some(&b), Some(&good))?;
    assert!(unchanged_marker(Some(&b), None).is_err());
    assert!(unchanged_marker(Some(&b), Some(&bad)).is_err());
    let key = fingerprint(&id)?;
    let mut registry = Registry {
        schema_version: 1,
        format: "macops-index-book-v1".into(),
        scopes: BTreeMap::from([(key.clone(), b.clone())]),
    };
    registry.validate()?;
    assert_eq!(
        registry
            .book(std::path::Path::new("/other"), &id, &key)
            .identity,
        id
    );
    assert!(
        registry
            .book(std::path::Path::new("/new"), &id, "new")
            .owned
            .is_empty()
    );
    for field in 0..6 {
        let mut bad = Registry {
            schema_version: registry.schema_version,
            format: registry.format.clone(),
            scopes: registry.scopes.clone(),
        };
        match field {
            0 => bad.schema_version = 2,
            1 => bad.format = "other".into(),
            2 => {
                let mut b = bad.scopes.remove(&key).ok_or("book")?;
                b.identity.scope_id = "bad".into();
                bad.scopes.insert(key.clone(), b);
            }
            3 => bad.scopes.get_mut(&key).ok_or("book")?.current = Some("bad".into()),
            4 => {
                bad.scopes
                    .get_mut(&key)
                    .ok_or("book")?
                    .owned
                    .insert("bad".into());
            }
            _ => {
                let b = bad.scopes.remove(&key).ok_or("book")?;
                bad.scopes.insert("wrong-key".into(), b);
            }
        }
        assert!(bad.validate().is_err());
    }
    registry.scopes.clear();
    registry.validate()?;
    Ok(())
}

#[test]
fn each_candidate_identity_field_and_cache_binding_must_match() -> TestResult {
    let c = Candidate {
        name: "index".into(),
        device: 1,
        inode: 2,
        sha256: "a".repeat(64),
    };
    assert!(candidate_matches(&c, &c));
    for field in 0..4 {
        let mut other = c.clone();
        match field {
            0 => other.name = "other".into(),
            1 => other.device += 1,
            2 => other.inode += 1,
            _ => other.sha256 = "b".repeat(64),
        }
        assert!(!candidate_matches(&c, &other));
    }
    let p = package();
    let s = scope();
    let mut index = Index::empty();
    assert!(indexed_version(&index, &p, &s)?.is_none());
    index.entries.insert(
        p.id.clone(),
        Available {
            version: "1.0.0".into(),
            source_sha256: fingerprint(&p)?,
            scope_sha256: fingerprint(&s)?,
        },
    );
    assert_eq!(indexed_version(&index, &p, &s)?, Some("1.0.0".into()));
    let mut changed = s.clone();
    changed.home = "/other".into();
    assert!(indexed_version(&index, &p, &changed)?.is_none());
    let mut changed = p.clone();
    changed.name = "other".into();
    assert!(indexed_version(&index, &changed, &s)?.is_none());
    Ok(())
}

#[test]
fn native_cache_advice_is_scoped_and_identity_key_is_compatible() -> TestResult {
    for provider in [
        Provider::Brew {
            package: "demo".into(),
            cask: false,
        },
        Provider::Npm {
            package: "demo".into(),
            prefix: "/prefix".into(),
            authority: None,
        },
        Provider::Bun {
            package: "demo".into(),
        },
        Provider::Uv {
            binding: None,
            package: "demo".into(),
        },
        Provider::Mise {
            package: "demo".into(),
        },
        Provider::Macops,
    ] {
        let text = cache_advice(&provider);
        assert!(text.contains("macops"));
        assert_ne!(text, "");
    }
    let id = identity();
    assert_eq!(identity_key(&id), fingerprint(&id)?);
    Ok(())
}

#[test]
fn refreshes_and_cleanup_policy_preserve_scope_and_unknown_objects() -> TestResult {
    let p = package();
    let s = scope();
    let mut index = Index::empty();
    let mut b = book();
    apply_refresh(&mut index, &mut b, &p, &s, "1.0.0".into(), 42)?;
    assert_eq!(b.queried_at.get(&p.id), Some(&42));
    let row = outdated_row(
        &p,
        &s,
        &Observation::Present {
            evidence: None,
            version: "1.0.0".into(),
        },
        index.entries.get(&p.id),
        Some(42),
    )?;
    assert_eq!(row["status"], "current");
    assert_eq!(
        outdated_row(
            &p,
            &s,
            &Observation::Missing { evidence: None },
            index.entries.get(&p.id),
            None
        )?["status"],
        "different"
    );
    assert_eq!(
        outdated_row(&p, &s, &Observation::Missing { evidence: None }, None, None)?["status"],
        "unknown"
    );
    let id = identity();
    let key = identity_key(&id);
    let registry = Registry {
        schema_version: 1,
        format: "macops-index-book-v1".into(),
        scopes: BTreeMap::from([(key.clone(), b.clone())]),
    };
    registered_scope(&registry, &key, std::path::Path::new("/fixture/cache"))?;
    assert!(registered_scope(&registry, "other", std::path::Path::new("/fixture/cache")).is_err());
    registered_scope(&registry, "other", std::path::Path::new("/new"))?;
    require_digest("a", "a")?;
    assert!(require_digest("a", "b").is_err());
    let c = Candidate {
        name: "index".into(),
        device: 1,
        inode: 2,
        sha256: "a".into(),
    };
    fresh_cleanup(std::slice::from_ref(&c), std::slice::from_ref(&c))?;
    assert!(fresh_cleanup(&[c], &[]).is_err());
    b.owned.insert("a".into());
    b.owned.insert("b".into());
    retain_remaining(&mut b, &BTreeMap::from([("b.json".into(), "b".into())]));
    assert_eq!(b.owned, BTreeSet::from(["b".into()]));
    Ok(())
}

#[test]
fn unsupported_encoding_cannot_advance_refresh_or_outdated_state() {
    use std::os::unix::ffi::OsStringExt;
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![255]));
    let mut p = package();
    p.paths.push(OwnedPath {
        path: path.clone(),
        kind: PathKind::Payload,
    });
    let s = scope();
    let mut i = Index::empty();
    let mut b = book();
    assert!(apply_refresh(&mut i, &mut b, &p, &s, "1.0.0".into(), 3).is_err());
    assert!(i.entries.is_empty());
    assert!(b.queried_at.is_empty());
    let mut s = scope();
    s.home = path;
    assert!(apply_refresh(&mut i, &mut b, &package(), &s, "1.0.0".into(), 3).is_err());
    assert!(outdated_row(&p, &s, &Observation::Missing { evidence: None }, None, None).is_err());
}
