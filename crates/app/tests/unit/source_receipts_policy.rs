#![cfg(test)]
use super::*;
use crate::{
    domain::{CapabilityQueryKind, ConfigAnchor, FileId},
    policy::{governance::DirectoryBinding, source_tree::Entry},
    test_support::TestResult,
};
use std::collections::BTreeMap;
fn fixtures() -> (LocalCapabilityQuery, Vec<u8>, Slot, Tree) {
    let parent = FileId {
        device: 1,
        inode: 2,
    };
    let slot = Slot {
        parent: DirectoryBinding {
            requested: "/home/.agents/skills".into(),
            canonical: "/home/.agents/skills".into(),
            identity: parent.clone(),
            lineage: vec![parent.clone()],
            uid: 501,
            mode: 0o755,
        },
        name: "demo".into(),
    };
    let query = LocalCapabilityQuery {
        kind: CapabilityQueryKind::LocalV1,
        capability: CapabilityKind::SkillsCli,
        native_id: "demo".into(),
        manager: "skills-cli:github".into(),
        source: Some("owner/repo".into()),
        record: ConfigAnchor {
            requested: "/home/.agents/.skill-lock.json".into(),
            canonical_parent: "/home/.agents".into(),
            parent_identity: FileId {
                device: 1,
                inode: 9,
            },
            name: ".skill-lock.json".into(),
        },
        payload: ConfigAnchor {
            requested: slot.path(),
            canonical_parent: slot.parent.canonical.clone(),
            parent_identity: parent,
            name: "demo".into(),
        },
        settings: None,
    };
    let record=br#"{"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github","skillFolderHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}"#.to_vec();
    let tree = Tree {
        identity: FileId {
            device: 1,
            inode: 3,
        },
        uid: 501,
        mode: 0o755,
        entries: BTreeMap::from([(
            "SKILL.md".into(),
            Entry {
                identity: FileId {
                    device: 1,
                    inode: 4,
                },
                uid: 501,
                mode: 0o644,
                content: Content::File {
                    sha256: "b".repeat(64),
                    size: 20,
                },
            },
        )]),
    };
    (query, record, slot, tree)
}
#[test]
fn projected_native_stamp_uses_the_fixed_live_parent_and_known_candidate_identity() -> TestResult {
    let (query, record, slot, mut tree) = fixtures();
    let before = known_skill("demo", &query, &record, &slot, &tree, 501)?;
    tree.identity.inode = 30;
    let after = known_skill("demo", &query, &record, &slot, &tree, 501)?;
    assert_ne!(before, after);
    let Observation::CapabilityPack {
        pack: Some(stamp), ..
    } = after
    else {
        return Err("stamp".into());
    };
    assert_eq!(stamp.payload_identity.inode, 30);
    assert_eq!(stamp.file_count, 1);
    assert_eq!(
        stamp.payload_digest,
        native::payload_tree_digest("demo", &[("SKILL.md".into(), false, "b".repeat(64))])?
    );
    Ok(())
}
#[test]
fn receipt_projection_rejects_scope_rebinding_missing_records_reserved_entries_and_native_limits()
-> TestResult {
    let (query, record, slot, tree) = fixtures();
    for variant in 0..9 {
        let mut q = query.clone();
        let mut s = slot.clone();
        let mut t = tree.clone();
        let mut raw = record.clone();
        match variant {
            0 => q.native_id = "different".into(),
            1 => q.settings = Some(q.record.clone()),
            2 => q.capability = CapabilityKind::ClaudePlugin,
            3 => q.payload.requested = "/other".into(),
            4 => q.payload.parent_identity.inode = 99,
            5 => s.parent.uid = 0,
            6 => t.uid = 0,
            7 => raw = br#"{"version":3,"skills":{}}"#.to_vec(),
            _ => raw = b"{".to_vec(),
        }
        assert!(
            known_skill("demo", &q, &raw, &s, &t, 501).is_err(),
            "{variant}"
        );
    }
    let mut t = tree.clone();
    t.entries.get_mut("SKILL.md").ok_or("skill")?.content = Content::File {
        sha256: "b".repeat(64),
        size: native::FILE_LIMIT + 1,
    };
    assert!(known_skill("demo", &query, &record, &slot, &t, 501).is_err());
    let mut t = tree.clone();
    t.entries.insert(
        ".DS_Store".into(),
        Entry {
            identity: FileId {
                device: 1,
                inode: 8,
            },
            uid: 501,
            mode: 0o755,
            content: Content::Directory,
        },
    );
    assert!(known_skill("demo", &query, &record, &slot, &t, 501).is_err());
    let mut t = tree.clone();
    t.entries.clear();
    assert!(known_skill("demo", &query, &record, &slot, &t, 501).is_err());
    let mut t = tree.clone();
    let extra = t.entries["SKILL.md"].clone();
    for i in 0..native::PAYLOAD_ENTRY_LIMIT {
        t.entries.insert(format!("extra-{i}"), extra.clone());
    }
    assert!(known_skill("demo", &query, &record, &slot, &t, 501).is_err());
    let mut t = tree;
    let mut path = String::new();
    for i in 0..native::PAYLOAD_DEPTH_LIMIT {
        if i > 0 {
            path.push('/');
        }
        path.push('a');
        t.entries.insert(
            path.clone(),
            Entry {
                identity: FileId {
                    device: 1,
                    inode: 100 + i as u64,
                },
                uid: 501,
                mode: 0o755,
                content: Content::Directory,
            },
        );
    }
    assert!(known_skill("demo", &query, &record, &slot, &t, 501).is_err());
    Ok(())
}
#[test]
fn ignored_regular_markers_do_not_change_native_stamp_but_nested_leases_are_rejected() -> TestResult
{
    let (query, record, slot, mut tree) = fixtures();
    let before = known_skill("demo", &query, &record, &slot, &tree, 501)?;
    let entry = tree.entries["SKILL.md"].clone();
    tree.entries.insert(".DS_Store".into(), entry.clone());
    tree.entries.insert(".in_use".into(), entry.clone());
    assert_eq!(
        known_skill("demo", &query, &record, &slot, &tree, 501)?,
        before
    );
    tree.entries.insert(
        "scripts".into(),
        Entry {
            identity: FileId {
                device: 1,
                inode: 50,
            },
            uid: 501,
            mode: 0o755,
            content: Content::Directory,
        },
    );
    tree.entries.insert("scripts/.in_use".into(), entry);
    assert!(known_skill("demo", &query, &record, &slot, &tree, 501).is_err());
    Ok(())
}

#[test]
fn known_native_receipt_rejects_an_invalid_record_anchor_or_absent_source() {
    let (mut query, raw, slot, tree) = fixtures();
    query.record.name = "../lock".into();
    assert!(known_skill("demo", &query, &raw, &slot, &tree, 501).is_err());
    let (mut query, raw, slot, tree) = fixtures();
    query.source = None;
    assert!(known_skill("demo", &query, &raw, &slot, &tree, 501).is_err());
}
