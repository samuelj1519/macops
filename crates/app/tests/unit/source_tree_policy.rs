#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
fn id(inode: u64) -> FileId {
    FileId { device: 1, inode }
}
fn tree(inode: u64) -> Tree {
    Tree {
        identity: id(inode),
        uid: 501,
        mode: 0o755,
        entries: BTreeMap::from([(
            "SKILL.md".into(),
            Entry {
                identity: id(inode + 10),
                uid: 501,
                mode: 0o644,
                content: Content::File {
                    sha256: "a".repeat(64),
                    size: 4,
                },
            },
        )]),
    }
}
fn slot(parent: &str, inode: u64, name: &str) -> Slot {
    Slot {
        parent: DirectoryBinding {
            requested: parent.into(),
            canonical: parent.into(),
            identity: id(inode),
            lineage: vec![id(1), id(inode)],
            uid: 501,
            mode: 0o700,
        },
        name: name.into(),
    }
}
fn change() -> Change {
    Change {
        live: slot("/home/skills", 2, "alpha"),
        retained: slot("/home/stage", 3, "candidate"),
        before: tree(4),
        after: tree(5),
    }
}
#[test]
fn movable_object_identity_does_not_include_its_slot_but_pair_classification_does() -> TestResult {
    let c = change();
    validate_changes(std::slice::from_ref(&c), 501)?;
    assert_eq!(
        classify(&c, Some(&c.before), Some(&c.after)),
        Position::Before
    );
    assert_eq!(
        classify(&c, Some(&c.after), Some(&c.before)),
        Position::OwnAfter
    );
    for (a, b) in [
        (None, Some(&c.after)),
        (Some(&c.after), None),
        (Some(&c.before), Some(&c.before)),
    ] {
        assert_eq!(classify(&c, a, b), Position::Unknown);
    }
    let mut foreign = c.after.clone();
    foreign.identity = id(99);
    assert_eq!(foreign.shape().digest()?, c.after.shape().digest()?);
    assert_eq!(
        classify(&c, Some(&foreign), Some(&c.before)),
        Position::Unknown
    );
    let live = c.live.project(&c.after);
    let stage = c.retained.project(&c.after);
    assert_eq!(live.identity, stage.identity);
    assert_ne!(live.requested, stage.requested);
    assert_ne!(live.lineage, stage.lineage);
    assert_eq!(live.lineage.last(), Some(&c.after.identity));
    Ok(())
}
#[test]
fn baseline_semantics_include_full_modes_and_empty_directories_but_not_leaf_inode_churn()
-> TestResult {
    let base = tree(4);
    let digest = base.shape().digest()?;
    let mut edited = base.clone();
    edited.entries.get_mut("SKILL.md").ok_or("file")?.identity = id(100);
    assert_eq!(edited.shape().digest()?, digest);
    edited.entries.get_mut("SKILL.md").ok_or("file")?.mode = 0o600;
    assert_ne!(edited.shape().digest()?, digest);
    edited = base.clone();
    edited.entries.insert(
        "empty".into(),
        Entry {
            identity: id(101),
            uid: 501,
            mode: 0o755,
            content: Content::Directory,
        },
    );
    edited.validate(501)?;
    assert_ne!(edited.shape().digest()?, digest);
    let mut with_child = edited.clone();
    with_child
        .entries
        .insert("empty/file".into(), base.entries["SKILL.md"].clone());
    with_child.validate(501)?;
    Ok(())
}
#[test]
fn source_tree_rejects_unowned_unsafe_or_incomplete_members_before_authorization() -> TestResult {
    let mut cases = vec![];
    let mut t = tree(4);
    t.identity.device = 0;
    cases.push(t);
    let mut t = tree(4);
    t.uid = 0;
    cases.push(t);
    let mut t = tree(4);
    t.mode = 0o777;
    cases.push(t);
    let mut t = tree(4);
    t.mode = 0o1755;
    cases.push(t);
    let mut t = tree(4);
    t.entries.values_mut().for_each(|e| e.uid = 0);
    cases.push(t);
    let mut t = tree(4);
    t.entries.values_mut().for_each(|e| e.identity.device = 2);
    cases.push(t);
    let mut t = tree(4);
    let e = t.entries.remove("SKILL.md").ok_or("file")?;
    t.entries.insert(".".into(), e);
    cases.push(t);
    let mut t = tree(4);
    let e = t.entries.remove("SKILL.md").ok_or("file")?;
    t.entries.insert("../out".into(), e);
    cases.push(t);
    let mut t = tree(4);
    let e = t.entries.remove("SKILL.md").ok_or("file")?;
    t.entries.insert("missing/file".into(), e);
    cases.push(t);
    let mut t = tree(4);
    let e = t.entries["SKILL.md"].clone();
    t.entries.insert("SKILL.md/child".into(), e);
    cases.push(t);
    let mut t = tree(4);
    t.entries.values_mut().for_each(|e| {
        e.content = Content::File {
            sha256: "bad".into(),
            size: 1,
        }
    });
    cases.push(t);
    let mut t = tree(4);
    t.entries.values_mut().for_each(|e| {
        e.content = Content::File {
            sha256: "a".repeat(64),
            size: 16 * 1024 * 1024 + 1,
        }
    });
    cases.push(t);
    let mut t = tree(4);
    let e = t.entries["SKILL.md"].clone();
    t.entries = (0..10001)
        .map(|n| (format!("file{n}"), e.clone()))
        .collect();
    cases.push(t);
    let mut t = tree(4);
    let mut e = t.entries["SKILL.md"].clone();
    e.content = Content::File {
        sha256: "a".repeat(64),
        size: 16 * 1024 * 1024,
    };
    t.entries = (0..5).map(|n| (format!("file{n}"), e.clone())).collect();
    cases.push(t);
    for t in cases {
        assert!(t.validate(501).is_err());
    }
    Ok(())
}
#[test]
fn fixed_slots_cannot_alias_escape_or_swap_objects_from_another_device() {
    let good = change();
    let mut cases = vec![];
    let mut c = good.clone();
    c.live.name = "../outside".into();
    cases.push(c);
    let mut c = good.clone();
    c.live.parent.canonical = "/other".into();
    cases.push(c);
    let mut c = good.clone();
    c.live.parent.lineage.clear();
    cases.push(c);
    let mut c = good.clone();
    c.live.parent.requested = "relative".into();
    cases.push(c);
    let mut c = good.clone();
    c.before.identity = c.after.identity.clone();
    cases.push(c);
    let mut c = good.clone();
    c.after.identity.device = 2;
    cases.push(c);
    let mut c = good.clone();
    c.retained.parent.identity.device = 2;
    cases.push(c);
    let mut c = good.clone();
    c.retained = c.live.clone();
    cases.push(c);
    let mut c = good.clone();
    c.retained = slot("/home/skills/alpha", 4, "nested");
    cases.push(c);
    for c in cases {
        assert!(validate_changes(&[c], 501).is_err());
    }
    assert!(validate_changes(&[good.clone(), good.clone()], 501).is_err());
    assert!(validate_changes(&vec![good; 129], 501).is_err());
}
#[test]
fn source_batches_enforce_cumulative_entry_and_byte_budgets_and_validate_each_root() -> TestResult {
    let good = change();
    let mut c = good.clone();
    c.before.identity.inode = 0;
    assert!(validate_changes(&[c], 501).is_err());
    let mut c = good.clone();
    c.live.parent.identity.inode = 0;
    assert!(validate_changes(&[c], 501).is_err());
    let mut c = good.clone();
    c.live.parent.mode = 0o777;
    assert!(validate_changes(&[c], 501).is_err());
    let mut c = good.clone();
    c.before
        .entries
        .get_mut("SKILL.md")
        .ok_or("file")?
        .identity
        .inode = 0;
    assert!(validate_changes(&[c], 501).is_err());
    let mut c = good.clone();
    c.before.entries.insert(
        "empty".into(),
        Entry {
            identity: id(90),
            uid: 501,
            mode: 0o700,
            content: Content::Directory,
        },
    );
    validate_changes(&[c], 501)?;
    for entries in [false, true] {
        let mut batch = vec![];
        for i in 0..3 {
            let mut c = good.clone();
            c.live.name = format!("live-{i}");
            c.retained.name = format!("new-{i}");
            c.before.identity = id(10000 + i * 2);
            c.after.identity = id(10001 + i * 2);
            for tree in [&mut c.before, &mut c.after] {
                tree.entries.clear();
                for j in 0..if entries { 9000 } else { 4 } {
                    tree.entries.insert(
                        format!("file-{j}"),
                        Entry {
                            identity: id(100 + j),
                            uid: 501,
                            mode: 0o644,
                            content: Content::File {
                                sha256: "a".repeat(64),
                                size: if entries { 0 } else { 16 * 1024 * 1024 },
                            },
                        },
                    );
                }
            }
            batch.push(c);
        }
        assert!(validate_changes(&batch, 501).is_err());
    }
    Ok(())
}
#[test]
fn distinct_slot_spelling_cannot_hide_a_duplicate_physical_root_in_a_batch() -> TestResult {
    let first = change();
    let mut second = first.clone();
    second.live.name = "ALPHA".into();
    second.retained.name = "other-candidate".into();
    second.after.identity = id(90);
    assert!(validate_changes(&[first.clone(), second.clone()], 501).is_err());
    second.before.identity = id(91);
    validate_changes(&[first, second], 501)?;
    Ok(())
}
