#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
#[test]
fn native_update_timestamp_uses_bounded_utc_gregorian_dates_and_keeps_unix_second_precision()
-> TestResult {
    for (seconds, expected) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400, "2000-02-29T00:00:00Z"),
        (4_107_542_400, "2100-03-01T00:00:00Z"),
        (1_700_000_000, "2023-11-14T22:13:20Z"),
        (253_402_300_799, "9999-12-31T23:59:59Z"),
    ] {
        assert_eq!(timestamp(seconds)?, expected);
    }
    assert!(timestamp(253_402_300_800).is_err());
    Ok(())
}

fn overlay_fixture(
    base_agents: bool,
) -> Result<(
    UpgradedRoot,
    crate::capability::rules::invocation::OverlayRecord,
    crate::capability::rules::invocation::OverlayRecord,
)> {
    let (live, private) = overlay_slots();
    let (old_payload, old_book) = old_overlay_payload(&live)?;
    let before = tree(&old_payload, 4);
    let baseline = Baseline::capture(
        Identity {
            member: "asset:alpha".into(),
            repository: "owner/repo".into(),
            skill_path: "skills/alpha".into(),
            canonical_name: "alpha".into(),
        },
        live.clone(),
        &before,
        Path::new("/home"),
        501,
    )?;
    let mut payload = Payload {
        entries: std::collections::BTreeMap::from([(
            "SKILL.md".into(),
            Entry::File {
                mode: 0o644,
                bytes: b"---\nname: alpha\n---\nnew\n".to_vec(),
            },
        )]),
    };
    if base_agents {
        payload
            .entries
            .insert("agents".into(), Entry::Directory { mode: 0o755 });
    }
    let base_shape = payload.shape(501, 0o755);
    let base_sha256 = payload.shape(0, 0o700).digest()?;
    let mut next = old_book.clone();
    for relative in ["SKILL.md", "agents/openai.yaml"] {
        let row = old_book
            .files
            .get(&live.path().join(relative))
            .ok_or_else(invalid)?;
        let row = legacy_overlay_fixture_row(&mut payload, relative, row)?;
        next.files.insert(live.path().join(relative), row);
    }
    let after = tree(&payload, 1000);
    Ok((
        UpgradedRoot {
            baseline,
            change: crate::policy::source_tree::Change {
                live,
                retained: Slot {
                    parent: private,
                    name: "payload".into(),
                },
                before,
                after,
            },
            commit: "c".repeat(40),
            skill_tree: "d".repeat(40),
            explicit_pin: false,
            requested_ref: None,
            base_sha256,
            base_shape,
            invocation: crate::source::rules::source_invocation::Intent::ExplicitManual,
            native_before: None,
            native_after: None,
        },
        old_book,
        next,
    ))
}
#[test]
fn overlay_projection_preserves_existing_base_directory_and_removes_mirrored_rows_when_new_upstream_is_not_manual()
-> TestResult {
    let home = Path::new("/home");
    let (mut root, old, next) = overlay_fixture(true)?;
    let mut edits = vec![];
    project_overlay_root(&root, &old, &next, home, &mut edits)?;
    assert_eq!(edits.len(), 2);
    root.invocation = crate::source::rules::source_invocation::Intent::Mirror {
        upstream_manual: false,
    };
    root.change.after.entries = root
        .base_shape
        .entries
        .iter()
        .map(|(path, entry)| {
            (
                path.clone(),
                crate::policy::source_tree::Entry {
                    identity: crate::domain::FileId {
                        device: 1,
                        inode: 100,
                    },
                    uid: entry.uid,
                    mode: entry.mode,
                    content: entry.content.clone(),
                },
            )
        })
        .collect();
    let mut edits = vec![];
    project_overlay_root(&root, &old, &old, home, &mut edits)?;
    assert_eq!(edits.len(), 2);
    assert!(edits.iter().all(|edit| edit.value.is_none()));
    Ok(())
}
#[test]
fn pure_overlay_guards_reject_invalid_paths_rows_and_file_in_place_of_generated_parent()
-> TestResult {
    use crate::capability::rules::invocation::MetadataKind;
    let home = Path::new("/home");
    let (root, old, next) = overlay_fixture(false)?;
    let mut invalid_path = old.clone();
    invalid_path.files.insert(
        root.change.live.path().join("unsupported.md"),
        old.files.values().next().ok_or_else(invalid)?.clone(),
    );
    assert!(project_overlay_root(&root, &invalid_path, &next, home, &mut vec![]).is_err());
    let mut invalid_row = old.clone();
    invalid_row
        .files
        .get_mut(&root.change.live.path().join("SKILL.md"))
        .ok_or_else(invalid)?
        .original = Some("!".into());
    assert!(project_overlay_root(&root, &invalid_row, &next, home, &mut vec![]).is_err());
    let mut invalid_next = next.clone();
    invalid_next
        .files
        .get_mut(&root.change.live.path().join("SKILL.md"))
        .ok_or_else(invalid)?
        .original = Some("!".into());
    assert!(
        project_row(
            &root,
            &old,
            &invalid_next,
            "SKILL.md",
            MetadataKind::Frontmatter
        )
        .is_err()
    );
    let mut parent_file = root.clone();
    parent_file.base_shape.entries.insert(
        "agents".into(),
        crate::policy::source_tree::ShapeEntry {
            uid: 501,
            mode: 0o644,
            content: crate::policy::source_tree::Content::File {
                sha256: "a".repeat(64),
                size: 1,
            },
        },
    );
    let mut normalized = parent_file.base_shape.clone();
    normalized.uid = 0;
    normalized.mode = 0o700;
    for entry in normalized.entries.values_mut() {
        entry.uid = 0;
    }
    parent_file.base_sha256 = normalized.digest()?;
    assert!(project_overlay_root(&parent_file, &old, &next, home, &mut vec![]).is_err());
    let mut unexpected = root.clone();
    unexpected.change.after.entries.insert(
        "unplanned".into(),
        crate::policy::source_tree::Entry {
            identity: crate::domain::FileId {
                device: 1,
                inode: 9999,
            },
            uid: 501,
            mode: 0o644,
            content: crate::policy::source_tree::Content::File {
                sha256: "a".repeat(64),
                size: 1,
            },
        },
    );
    assert!(project_overlay_root(&unexpected, &old, &next, home, &mut vec![]).is_err());
    let mut invalid_live = root;
    invalid_live.change.live.name = "devnorm".into();
    assert!(project_overlay_root(&invalid_live, &old, &next, home, &mut vec![]).is_err());
    Ok(())
}

use crate::{
    capability::rules::invocation::{self, MetadataKind},
    domain::FileId,
    policy::{
        governance::DirectoryBinding,
        source_tree::{Entry as TreeEntry, Slot, Tree},
    },
    source::rules::{
        source_baseline::Identity,
        source_payload::{Entry, Payload},
    },
};

fn id(inode: u64) -> FileId {
    FileId { device: 1, inode }
}
fn overlay_slots() -> (Slot, DirectoryBinding) {
    let parent = DirectoryBinding {
        requested: "/home/.agents/skills".into(),
        canonical: "/home/.agents/skills".into(),
        identity: id(2),
        lineage: vec![id(1), id(2)],
        uid: 501,
        mode: 0o755,
    };
    let live = Slot {
        parent: parent.clone(),
        name: "alpha".into(),
    };
    let mut private = parent;
    private.requested = private.requested.join("stage");
    private.canonical = private.requested.clone();
    private.identity = id(3);
    private.lineage.push(id(3));
    private.mode = 0o700;
    (live, private)
}
fn old_overlay_payload(live: &Slot) -> Result<(Payload, invocation::OverlayRecord)> {
    let old_bytes = b"---\nname: alpha\n---\nold\n";
    let frontend = invocation::propose(
        MetadataKind::Frontmatter,
        "alpha",
        "local",
        Some(old_bytes),
        Some(0o644),
        None,
    )?
    .row
    .ok_or_else(invalid)?;
    let codex = invocation::propose(
        MetadataKind::CodexPolicy,
        "alpha",
        "local",
        None,
        None,
        None,
    )?
    .row
    .ok_or_else(invalid)?;
    let old_book = invocation::OverlayRecord {
        schema_version: 1,
        policy_hash: Some("a".repeat(64)),
        files: std::collections::BTreeMap::from([
            (live.path().join("SKILL.md"), frontend.clone()),
            (live.path().join("agents/openai.yaml"), codex.clone()),
        ]),
        profile_policy_hash: None,
        profiles: std::collections::BTreeMap::new(),
    };
    let old_payload = Payload {
        entries: std::collections::BTreeMap::from([
            (
                "SKILL.md".into(),
                Entry::File {
                    mode: 0o644,
                    bytes: frontend.bytes()?.1,
                },
            ),
            ("agents".into(), Entry::Directory { mode: 0o700 }),
            (
                "agents/openai.yaml".into(),
                Entry::File {
                    mode: 0o600,
                    bytes: codex.bytes()?.1,
                },
            ),
        ]),
    };
    Ok((old_payload, old_book))
}
fn tree(payload: &Payload, inode: u64) -> Tree {
    let shape = payload.shape(501, 0o755);
    Tree {
        identity: id(inode),
        uid: 501,
        mode: 0o755,
        entries: shape
            .entries
            .into_iter()
            .enumerate()
            .map(|(index, (path, entry))| {
                (
                    path,
                    TreeEntry {
                        identity: id(inode + 10 + index as u64),
                        uid: entry.uid,
                        mode: entry.mode,
                        content: entry.content,
                    },
                )
            })
            .collect(),
    }
}

fn legacy_overlay_fixture_row(
    payload: &mut Payload,
    path: &str,
    old: &crate::capability::rules::invocation::OverlayRow,
) -> Result<crate::capability::rules::invocation::OverlayRow> {
    let (raw, mode) = match payload.entries.get(path) {
        Some(Entry::File { bytes, mode }) => (Some(bytes.as_slice()), Some(*mode)),
        None => (None, None),
        _ => return Err(invalid()),
    };
    let proposed = crate::capability::rules::invocation::propose(
        old.kind, &old.label, &old.owner, raw, mode, None,
    )?;
    let bytes = crate::capability::rules::invocation::transform(old.kind, raw)?;
    if path.starts_with("agents/") && !payload.entries.contains_key("agents") {
        payload
            .entries
            .insert("agents".into(), Entry::Directory { mode: 0o700 });
    }
    payload.entries.insert(
        path.into(),
        Entry::File {
            mode: old.applied_mode,
            bytes,
        },
    );
    proposed.row.ok_or_else(invalid)
}

#[test]
fn historical_overlay_recovery_rejects_missing_rows_changed_modes_and_bytewise_after_substitution()
-> TestResult {
    let home = Path::new("/home");
    let (root, old, next) = overlay_fixture(true)?;
    let before = serde_json::to_vec(&old)?;
    let mut edits = vec![];
    project_overlay_root(&root, &old, &next, home, &mut edits)?;
    let after = crate::policy::json_fields::apply_governance(&before, &edits)?;
    overlays(
        Some(&before),
        Some(&after),
        std::slice::from_ref(&root),
        home,
    )?;
    assert!(overlays(Some(b"{"), Some(&after), std::slice::from_ref(&root), home).is_err());
    assert!(overlays(Some(&before), Some(b"{"), std::slice::from_ref(&root), home).is_err());
    let mut different_bytes = after;
    different_bytes.push(b' ');
    assert!(
        overlays(
            Some(&before),
            Some(&different_bytes),
            std::slice::from_ref(&root),
            home
        )
        .is_err()
    );
    for variant in 0..6 {
        let mut changed = root.clone();
        let mut book = next.clone();
        let key = changed.change.live.path().join("SKILL.md");
        match variant {
            0 => changed.base_sha256 = "f".repeat(64),
            1 => {
                changed.change.before.entries.remove("SKILL.md");
            }
            2 => {
                changed
                    .change
                    .before
                    .entries
                    .get_mut("SKILL.md")
                    .ok_or("entry")?
                    .mode = 0o600;
            }
            3 => {
                book.files.remove(&key);
            }
            4 => book.files.get_mut(&key).ok_or("row")?.owner = "another-owner".into(),
            _ => book.files.get_mut(&key).ok_or("row")?.original_mode = Some(0o600),
        }
        assert!(
            overlays(
                Some(&before),
                Some(&serde_json::to_vec(&book)?),
                &[changed],
                home
            )
            .is_err(),
            "variant {variant}"
        );
    }
    Ok(())
}
