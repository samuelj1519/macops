#![cfg(test)]
use super::*;
use crate::{policy::governance::DirectoryBinding, test_support::TestResult};
fn id(inode: u64) -> FileId {
    FileId { device: 1, inode }
}
fn pair(index: u64) -> Pair {
    let parent = DirectoryBinding {
        requested: format!("/home/p{index}").into(),
        canonical: format!("/home/p{index}").into(),
        identity: id(10 + index * 10),
        lineage: vec![id(1), id(10 + index * 10)],
        uid: 501,
        mode: 0o755,
    };
    let mut retained = parent.clone();
    retained.requested = parent.requested.join(".macops-source-ledger-test");
    retained.canonical = retained.requested.clone();
    retained.identity = id(11 + index * 10);
    retained.lineage.push(retained.identity.clone());
    retained.mode = 0o700;
    Pair {
        live: Slot {
            parent,
            name: "native.json".into(),
        },
        retained: Slot {
            parent: retained,
            name: "ledger".into(),
        },
        before: Some(Object {
            identity: id(12 + index * 10),
            uid: 501,
            mode: 0o640,
            size: 3,
            sha256: "a".repeat(64),
        }),
        after: Object {
            identity: id(13 + index * 10),
            uid: 501,
            mode: 0o640,
            size: 3,
            sha256: "b".repeat(64),
        },
    }
}
#[test]
fn source_file_pair_classifies_only_exact_healthy_positions_including_full_uid_mode_size_and_identity()
-> TestResult {
    let p = pair(0);
    validate_batch(std::slice::from_ref(&p), &[], 501)?;
    assert_eq!(
        classify(&p, p.before.as_ref(), Some(&p.after)),
        Position::Before
    );
    assert_eq!(
        classify(&p, Some(&p.after), p.before.as_ref()),
        Position::OwnAfter
    );
    assert!(matches!(p.after.state(), State::File { mode: 0o640, .. }));
    for variant in 0..5 {
        let mut foreign = p.after.clone();
        match variant {
            0 => foreign.identity.inode += 100,
            1 => foreign.uid = 502,
            2 => foreign.mode = 0o600,
            3 => foreign.size = 4,
            _ => foreign.sha256 = "f".repeat(64),
        }
        assert_eq!(
            classify(&p, p.before.as_ref(), Some(&foreign)),
            Position::Unknown
        );
    }
    let mut missing = p;
    missing.before = None;
    missing.validate(501)?;
    assert_eq!(
        classify(&missing, None, Some(&missing.after)),
        Position::Before
    );
    assert_eq!(
        classify(&missing, Some(&missing.after), None),
        Position::OwnAfter
    );
    assert_eq!(classify(&missing, None, None), Position::Unknown);
    Ok(())
}
#[test]
fn source_pair_authority_rejects_unsafe_objects_parents_cross_volume_and_nonprivate_or_alias_slots()
-> TestResult {
    for variant in 0..12 {
        let mut p = pair(0);
        match variant {
            0 => p.live.parent.uid = 0,
            1 => p.retained.parent.uid = 0,
            2 => p.after.identity.device = 2,
            3 => p.after.identity.inode = 0,
            4 => p.after.uid = 0,
            5 => p.after.mode = 0o1777,
            6 => p.after.size = 16 * 1024 * 1024 + 1,
            7 => p.after.sha256 = "bad".into(),
            8 => p.retained.parent.identity.device = 2,
            9 => p.retained.parent.mode = 0o755,
            10 => p.before.as_mut().ok_or("before")?.identity = p.after.identity.clone(),
            _ => p.before.as_mut().ok_or("before")?.uid = 0,
        }
        assert!(p.validate(501).is_err(), "{variant}");
    }
    for variant in 0..5 {
        let mut p = pair(0);
        match variant {
            0 => p.retained.name = "other".into(),
            1 => {
                p.retained.parent.requested = "/other/.macops-source-ledger-test".into();
                p.retained.parent.canonical = p.retained.parent.requested.clone();
            }
            2 => p.retained.parent.lineage = vec![id(1), id(11)],
            3 => {
                p.retained.parent.requested = p.live.parent.requested.join("nonprivate");
                p.retained.parent.canonical = p.retained.parent.requested.clone();
            }
            _ => p.retained.parent.requested = "relative".into(),
        }
        assert!(p.validate(501).is_err());
    }
    Ok(())
}
#[test]
fn source_file_batch_rejects_duplicate_physical_objects_slots_and_ancestor_scope_and_cumulative_bytes()
-> TestResult {
    let p = pair(0);
    assert!(validate_batch(&vec![p.clone(); 6], &[], 501).is_err());
    let mut b = pair(1);
    b.before.as_mut().ok_or("before")?.identity = p.after.identity.clone();
    assert!(validate_batch(&[p.clone(), b], &[], 501).is_err());
    let mut b = pair(1);
    b.live.parent.identity = p.live.parent.identity.clone();
    b.live.parent.lineage = p.live.parent.lineage.clone();
    b.retained.parent.lineage = b.live.parent.lineage.clone();
    b.retained
        .parent
        .lineage
        .push(b.retained.parent.identity.clone());
    assert!(validate_batch(&[p.clone(), b], &[], 501).is_err());
    let mut b = pair(1);
    b.live.parent.requested = p.live.path();
    b.live.parent.canonical = b.live.parent.requested.clone();
    b.retained.parent.requested = b.live.parent.requested.join(".macops-source-ledger-test");
    b.retained.parent.canonical = b.retained.parent.requested.clone();
    assert!(validate_batch(&[p, b], &[], 501).is_err());
    let mut many = (0..5).map(pair).collect::<Vec<_>>();
    for p in &mut many {
        p.before.as_mut().ok_or("before")?.size = 16 * 1024 * 1024;
        p.after.size = 16 * 1024 * 1024;
    }
    assert!(validate_batch(&many, &[], 501).is_err());
    Ok(())
}

fn root_change() -> Change {
    use crate::policy::source_tree::{Content, Entry, Tree};
    let parent = DirectoryBinding {
        requested: "/home/.agents/skills".into(),
        canonical: "/home/.agents/skills".into(),
        identity: id(90),
        lineage: vec![id(1), id(90)],
        uid: 501,
        mode: 0o755,
    };
    let mut retained = parent.clone();
    retained.requested = parent.requested.join("stage");
    retained.canonical = retained.requested.clone();
    retained.identity = id(91);
    retained.lineage.push(id(91));
    retained.mode = 0o700;
    let tree = |inode| Tree {
        identity: id(inode),
        uid: 501,
        mode: 0o755,
        entries: std::collections::BTreeMap::from([(
            "SKILL.md".into(),
            Entry {
                identity: id(inode + 2),
                uid: 501,
                mode: 0o644,
                content: Content::File {
                    sha256: "c".repeat(64),
                    size: 1,
                },
            },
        )]),
    };
    Change {
        live: Slot {
            parent,
            name: "alpha".into(),
        },
        retained: Slot {
            parent: retained,
            name: "payload".into(),
        },
        before: tree(92),
        after: tree(93),
    }
}
#[test]
fn file_pair_batch_checks_source_roots_and_rejects_a_metadata_object_hardlinked_into_payload()
-> TestResult {
    let p = pair(0);
    let root = root_change();
    validate_batch(std::slice::from_ref(&p), std::slice::from_ref(&root), 501)?;
    let mut invalid_root = root.clone();
    invalid_root.before.uid = 0;
    assert!(validate_batch(std::slice::from_ref(&p), &[invalid_root], 501).is_err());
    let mut alias = root;
    alias
        .before
        .entries
        .get_mut("SKILL.md")
        .ok_or("skill")?
        .identity = p.before.as_ref().ok_or("before")?.identity.clone();
    assert!(validate_batch(std::slice::from_ref(&p), &[alias], 501).is_err());
    let mut invalid_pair = p;
    invalid_pair.after.mode = 0o1777;
    assert!(validate_batch(&[invalid_pair], &[], 501).is_err());
    Ok(())
}
