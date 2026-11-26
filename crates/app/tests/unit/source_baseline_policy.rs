#![cfg(test)]
use super::*;
use crate::{
    policy::{
        governance::DirectoryBinding,
        source_tree::{Content, Entry},
    },
    test_support::TestResult,
};
use std::os::unix::ffi::OsStringExt;
fn fixture() -> (Identity, Slot, Tree) {
    let source = Identity {
        member: "capability:alpha".into(),
        repository: "owner/repo".into(),
        skill_path: "skills/alpha".into(),
        canonical_name: "alpha".into(),
    };
    let parent = FileId {
        device: 1,
        inode: 2,
    };
    let slot = Slot {
        parent: DirectoryBinding {
            requested: "/home/.agents/skills".into(),
            canonical: "/home/.agents/skills".into(),
            identity: parent.clone(),
            lineage: vec![parent],
            uid: 501,
            mode: 0o755,
        },
        name: "alpha".into(),
    };
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
                    sha256: "a".repeat(64),
                    size: 10,
                },
            },
        )]),
    };
    (source, slot, tree)
}
#[test]
fn adoption_records_complete_shape_and_keeps_legacy_and_future_top_level_bytes() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let baseline = Baseline::capture(source.clone(), slot.clone(), &tree, home, 501)?;
    let raw=br#"{ "assets" : {"old":{"revision":"retained","unknown" : [1, 2]}}, "future":{"raw":"\u0061"} }"#;
    let after = adopt(Some(raw), baseline.clone(), home, 501)?;
    let text = String::from_utf8(after.clone())?;
    assert!(text.contains(r#""assets" : {"old":{"revision":"retained","unknown" : [1, 2]}}"#));
    assert!(text.contains(r#""future":{"raw":"\u0061"}"#));
    let book = read(Some(&after), home, 501)?;
    assert_eq!(book.roots[&source.member], baseline);
    book.roots[&source.member].authorize(&source, &slot, &tree, home, 501)?;
    assert_eq!(adopt(Some(&after), baseline, home, 501)?, after);
    assert!(read(None, home, 501)?.roots.is_empty());
    Ok(())
}
#[test]
fn preexisting_mode_or_empty_directory_drift_cannot_be_blessed_by_a_legal_patch() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let baseline = Baseline::capture(source.clone(), slot.clone(), &tree, home, 501)?;
    let mut changed = tree.clone();
    changed.entries.get_mut("SKILL.md").ok_or("file")?.mode = 0o600;
    assert!(
        baseline
            .authorize(&source, &slot, &changed, home, 501)
            .is_err()
    );
    changed = tree.clone();
    changed.entries.insert(
        "empty".into(),
        Entry {
            identity: FileId {
                device: 1,
                inode: 5,
            },
            uid: 501,
            mode: 0o755,
            content: Content::Directory,
        },
    );
    assert!(
        baseline
            .authorize(&source, &slot, &changed, home, 501)
            .is_err()
    );
    changed = tree.clone();
    changed.identity.inode = 90;
    assert!(
        baseline
            .authorize(&source, &slot, &changed, home, 501)
            .is_err()
    );
    changed = tree.clone();
    changed
        .entries
        .get_mut("SKILL.md")
        .ok_or("file")?
        .identity
        .inode = 99;
    baseline.authorize(&source, &slot, &changed, home, 501)?;
    Ok(())
}
#[test]
fn baseline_namespace_rejects_unknown_version_arrays_duplicates_and_wrong_scope() {
    let home = Path::new("/home");
    for raw in [
        b"[]".as_slice(),
        br#"{"_macops_sources":null}"#,
        br#"{"_macops_sources":[1,{}]}"#,
        br#"{"_macops_sources":{"schema_version":2,"roots":{}}}"#,
        br#"{"_macops_sources":{"schema_version":1,"roots":{},"extra":true}}"#,
        br#"{"assets":{},"assets":{}}"#,
    ] {
        assert!(read(Some(raw), home, 501).is_err());
    }
    let (source, mut slot, tree) = fixture();
    slot.name = "other".into();
    assert!(Baseline::capture(source, slot, &tree, home, 501).is_err());
}
#[test]
fn a_second_member_cannot_claim_the_same_canonical_root_or_enter_a_duplicate_book() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let first = Baseline::capture(source, slot, &tree, home, 501)?;
    let raw = adopt(None, first.clone(), home, 501)?;
    let mut second = first.clone();
    second.source.member = "asset:alpha".into();
    assert_eq!(
        adopt(Some(&raw), second.clone(), home, 501)
            .err()
            .ok_or("conflict")?
            .code,
        "source_baseline_owner_conflict"
    );
    let mut doc: Value = serde_json::from_slice(&raw)?;
    doc[KEY]["roots"]["asset:alpha"] = serde_json::to_value(second)?;
    assert!(read(Some(&serde_json::to_vec(&doc)?), home, 501).is_err());
    Ok(())
}
fn patch(baseline: &Baseline) -> crate::capability::rules::capability_tx::FileChange {
    use crate::capability::rules::{
        capability_tx::{Blob, FileChange, Namespace, State},
        delivery::ChangeKind,
    };
    FileChange {
        path: baseline.slot.path().join("SKILL.md"),
        kind: ChangeKind::Frontmatter,
        namespace: Namespace {
            ancestor: baseline.slot.project(&fixture().2),
            remaining: vec!["SKILL.md".into()],
        },
        before: State::File {
            identity: FileId {
                device: 1,
                inode: 4,
            },
            sha256: "a".repeat(64),
            mode: 0o644,
        },
        before_blob: Some(Blob {
            sha256: "a".repeat(64),
            size: 10,
        }),
        after_blob: Some(Blob {
            sha256: "b".repeat(64),
            size: 8,
        }),
        after_mode: Some(0o600),
    }
}
#[test]
fn own_metadata_projection_updates_only_selected_content_full_mode_and_explicit_private_parent()
-> TestResult {
    use crate::{
        capability::rules::capability_tx::{Namespace, State},
        policy::source_tree::ShapeEntry,
    };
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let baseline = Baseline::capture(source, slot, &tree, home, 501)?;
    let edit = patch(&baseline);
    let after = project(&baseline, std::slice::from_ref(&edit), home, 501)?;
    assert_eq!(after.root_identity, baseline.root_identity);
    assert_eq!(after.shape.entries["SKILL.md"].mode, 0o600);
    assert_eq!(
        after.shape.entries["SKILL.md"].content,
        Content::File {
            sha256: "b".repeat(64),
            size: 8
        }
    );
    let mut generated = edit.clone();
    generated.path = baseline.slot.path().join("agents/openai.yaml");
    generated.kind = crate::capability::rules::delivery::ChangeKind::CodexPolicy;
    generated.before = State::Missing;
    generated.before_blob = None;
    generated.namespace = Namespace {
        ancestor: baseline.slot.project(&tree),
        remaining: vec!["agents".into(), "openai.yaml".into()],
    };
    let after = project(&baseline, std::slice::from_ref(&generated), home, 501)?;
    assert_eq!(
        after.shape.entries["SKILL.md"],
        baseline.shape.entries["SKILL.md"]
    );
    assert_eq!(
        after.shape.entries["agents"],
        ShapeEntry {
            uid: 501,
            mode: 0o700,
            content: Content::Directory
        }
    );
    assert_eq!(after.shape.entries["agents/openai.yaml"].mode, 0o600);
    let mut removed = edit.clone();
    removed.after_blob = None;
    removed.after_mode = None;
    assert!(
        !project(&baseline, &[removed], home, 501)?
            .shape
            .entries
            .contains_key("SKILL.md")
    );
    let mut unselected = edit;
    unselected.path = "/home/.claude/settings.json".into();
    assert_eq!(project(&baseline, &[unselected], home, 501)?, baseline);
    Ok(())
}
#[test]
fn metadata_projection_rejects_unknown_parents_modes_members_and_unmatched_before() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let baseline = Baseline::capture(source, slot, &tree, home, 501)?;
    let original = patch(&baseline);
    let mut cases = vec![];
    let mut p = original.clone();
    p.kind = crate::capability::rules::delivery::ChangeKind::ProfileJson;
    cases.push(p);
    let mut p = original.clone();
    p.before = crate::capability::rules::capability_tx::State::Missing;
    cases.push(p);
    let mut p = original.clone();
    p.path = baseline.slot.path();
    cases.push(p);
    let mut p = original.clone();
    p.after_mode = None;
    cases.push(p);
    let mut p = original.clone();
    p.after_blob = None;
    cases.push(p);
    let mut p = original.clone();
    p.after_mode = Some(0o1600);
    cases.push(p);
    let mut p = original.clone();
    p.after_blob.as_mut().ok_or("blob")?.sha256 = "bad".into();
    cases.push(p);
    let mut p = original.clone();
    p.after_blob.as_mut().ok_or("blob")?.size = 16 * 1024 * 1024 + 1;
    cases.push(p);
    let mut p = original.clone();
    p.path = baseline.slot.path().join("missing/file");
    p.before = crate::capability::rules::capability_tx::State::Missing;
    cases.push(p);
    let mut p = original.clone();
    p.path = baseline.slot.path().join("SKILL.md/file");
    p.before = crate::capability::rules::capability_tx::State::Missing;
    cases.push(p);
    for p in cases {
        assert!(project(&baseline, &[p], home, 501).is_err());
    }
    assert!(project(&baseline, &[original.clone(), original], home, 501).is_err());
    Ok(())
}
#[test]
fn corrupt_baseline_identity_scope_and_record_cannot_replace_governed_metadata() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let baseline = Baseline::capture(source, slot, &tree, home, 501)?;
    for member in [String::new(), "m".repeat(257), "bad\nmember".into()] {
        let mut b = baseline.clone();
        b.source.member = member;
        assert!(b.validate(home, 501).is_err());
    }
    let mut b = baseline.clone();
    b.source.repository = "Owner/Repo".into();
    assert!(b.validate(home, 501).is_err());
    let mut b = baseline.clone();
    b.source.repository = "bad".into();
    assert!(b.validate(home, 501).is_err());
    let mut b = baseline.clone();
    b.source.skill_path = "../out".into();
    assert!(b.validate(home, 501).is_err());
    let mut b = baseline.clone();
    b.source.canonical_name = "bad/name".into();
    assert!(b.validate(home, 501).is_err());
    let mut b = baseline.clone();
    b.slot.parent.identity.inode = 0;
    assert!(b.validate(home, 501).is_err());
    let mut b = baseline.clone();
    b.root_identity.device = 0;
    assert!(b.authorize(&b.source, &b.slot, &tree, home, 501).is_err());
    assert!(baseline.validate(Path::new("relative"), 501).is_err());
    let mut bad_tree = tree.clone();
    bad_tree.uid = 0;
    assert!(
        Baseline::capture(
            baseline.source.clone(),
            baseline.slot.clone(),
            &bad_tree,
            home,
            501
        )
        .is_err()
    );
    assert!(
        baseline
            .authorize(&baseline.source, &baseline.slot, &bad_tree, home, 501)
            .is_err()
    );
    let mut book = Record {
        schema_version: 1,
        roots: BTreeMap::new(),
    };
    book.roots.insert("wrong-member".into(), baseline.clone());
    assert!(
        read(
            Some(&serde_json::to_vec(&serde_json::json!({KEY:book}))?),
            home,
            501
        )
        .is_err()
    );
    let mut invalid = baseline.clone();
    invalid.shape.mode = 0o777;
    book.roots = BTreeMap::from([(invalid.source.member.clone(), invalid)]);
    assert!(
        read(
            Some(&serde_json::to_vec(&serde_json::json!({KEY:book}))?),
            home,
            501
        )
        .is_err()
    );
    assert!(adopt(Some(b"[]"), baseline.clone(), home, 501).is_err());
    let mut invalid = baseline.clone();
    invalid.shape.mode = 0o777;
    assert!(adopt(None, invalid, home, 501).is_err());
    let mut p = patch(&baseline);
    p.path = baseline
        .slot
        .path()
        .join(std::ffi::OsString::from_vec(vec![255]));
    assert!(project(&baseline, &[p], home, 501).is_err());
    Ok(())
}
#[test]
fn baseline_book_enforces_root_and_serialized_document_budgets_without_dropping_old_assets()
-> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let base = Baseline::capture(source, slot, &tree, home, 501)?;
    let mut book = Record {
        schema_version: 1,
        roots: BTreeMap::new(),
    };
    for i in 0..129 {
        let mut b = base.clone();
        b.source.canonical_name = format!("skill-{i}");
        b.slot.name = b.source.canonical_name.clone();
        b.source.member = format!("member-{i}");
        if i < 128 {
            book.roots.insert(b.source.member.clone(), b);
        } else {
            let raw = serde_json::to_vec(&serde_json::json!({KEY:book}))?;
            assert!(adopt(Some(&raw), b.clone(), home, 501).is_err());
            book.roots.insert(b.source.member.clone(), b);
        }
    }
    assert!(
        read(
            Some(&serde_json::to_vec(&serde_json::json!({KEY:book}))?),
            home,
            501
        )
        .is_err()
    );
    let raw = serde_json::to_vec(
        &serde_json::json!({"assets":{},"future":"x".repeat(16*1024*1024-128)}),
    )?;
    assert!(adopt(Some(&raw), base, home, 501).is_err());
    Ok(())
}
#[test]
fn reserved_paths_must_be_absolute_and_disjoint_in_both_ancestor_directions() -> TestResult {
    let root = Path::new("/home/.agents/skills/alpha");
    disjoint(root, &["/home/.local/state/macops".into()])?;
    for path in ["/home/.agents/skills", "/home/.agents/skills/alpha/state"] {
        assert!(disjoint(root, &[path.into()]).is_err());
    }
    assert!(disjoint(Path::new("relative"), &[]).is_err());
    assert!(disjoint(root, &["relative".into()]).is_err());
    Ok(())
}
#[test]
fn non_utf8_governance_paths_cannot_serialize_an_adoption_and_invalid_baselines_cannot_project()
-> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let mut baseline = Baseline::capture(source, slot, &tree, home, 501)?;
    let edit = patch(&baseline);
    baseline.shape.uid = 0;
    assert!(project(&baseline, &[edit], home, 501).is_err());
    baseline.shape.uid = 501;
    let raw_home = std::path::PathBuf::from(std::ffi::OsString::from_vec(
        b"/home/non-utf8-\xff".to_vec(),
    ));
    baseline.slot.parent.requested = raw_home.join(".agents/skills");
    baseline.slot.parent.canonical = baseline.slot.parent.requested.clone();
    baseline.validate(&raw_home, 501)?;
    assert!(adopt(None, baseline, &raw_home, 501).is_err());
    Ok(())
}
#[test]
fn different_canonical_spelling_cannot_assign_one_physical_root_to_two_members() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let first = Baseline::capture(source, slot, &tree, home, 501)?;
    let raw = adopt(None, first.clone(), home, 501)?;
    let mut other = first;
    other.source.member = "asset:ALPHA".into();
    other.source.canonical_name = "ALPHA".into();
    other.slot.name = "ALPHA".into();
    assert_eq!(
        adopt(Some(&raw), other.clone(), home, 501)
            .err()
            .ok_or("physical conflict")?
            .code,
        "source_baseline_owner_conflict"
    );
    let mut value: Value = serde_json::from_slice(&raw)?;
    value[KEY]["roots"]["asset:ALPHA"] = serde_json::to_value(other)?;
    assert!(read(Some(&serde_json::to_vec(&value)?), home, 501).is_err());
    Ok(())
}

#[test]
fn stored_revision_is_typed_and_the_full_namespace_rejects_a_129th_root() -> TestResult {
    let (source, slot, tree) = fixture();
    let home = Path::new("/home");
    let mut baseline = Baseline::capture(source, slot, &tree, home, 501)?;
    let revision = Revision {
        requested_ref: None,
        commit: "a".repeat(40),
        skill_tree: "b".repeat(40),
        base_sha256: "c".repeat(64),
    };
    for variant in 0..4 {
        let mut invalid_revision = revision.clone();
        match variant {
            0 => invalid_revision.requested_ref = Some("bad~1".into()),
            1 => invalid_revision.commit = "bad".into(),
            2 => invalid_revision.skill_tree = "bad".into(),
            _ => invalid_revision.base_sha256 = "bad".into(),
        }
        baseline.revision = Some(invalid_revision);
        assert!(baseline.validate(home, 501).is_err());
    }
    baseline.revision = Some(revision);
    let mut book = Record {
        schema_version: 1,
        roots: BTreeMap::new(),
    };
    for index in 0..128 {
        let mut entry = baseline.clone();
        entry.source.canonical_name = format!("skill-{index}");
        entry.source.member = format!("asset:skill-{index}");
        entry.slot.name = entry.source.canonical_name.clone();
        entry.root_identity.inode = 1000 + index;
        book.roots.insert(entry.source.member.clone(), entry);
    }
    let raw = serde_json::to_vec(&serde_json::json!({"assets":{},"_macops_sources":book}))?;
    assert!(adopt(Some(&raw), baseline, home, 501).is_err());
    Ok(())
}
