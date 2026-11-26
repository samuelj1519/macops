use crate::{
    error::{Error, Result},
    policy::json_fields::{self, Edit},
    source::rules::{
        source_baseline::{self, Baseline, Revision},
        source_tx::UpgradedRoot,
    },
};
use serde_json::Value;
use std::path::Path;
fn invalid() -> Error {
    Error::attention(
        "unverified_source_projection",
        "来源登记或候选账本投影不能验证",
    )
}
pub fn native(
    before: Option<&[u8]>,
    roots: &[UpgradedRoot],
    created_at: u64,
) -> Result<Option<Vec<u8>>> {
    let selected = roots
        .iter()
        .filter_map(|root| root.native_before.as_ref().map(|spec| (root, spec)))
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(before.map(<[u8]>::to_vec));
    }
    let raw = before.ok_or_else(invalid)?;
    let value = crate::policy::json::parse(raw)?;
    let updated_at = timestamp(created_at)?;
    let mut edits = vec![];
    for (root, before_native) in selected {
        let row = value
            .get("skills")
            .and_then(|v| v.get(&root.baseline.source.canonical_name))
            .and_then(Value::as_object)
            .ok_or_else(invalid)?;
        if value.get("version").and_then(Value::as_u64) != Some(3)
            || row.get("sourceType").and_then(Value::as_str) != Some("github")
            || row.get("skillPath").and_then(Value::as_str)
                != Some(&root.baseline.source.skill_path)
        {
            return Err(invalid());
        }
        let observed = crate::source::rules::source_receipts::known_skill(
            &before_native.package.id,
            &before_native.query,
            raw,
            &root.change.live,
            &root.change.before,
            root.change.before.uid,
        )?;
        if observed != before_native.expected {
            return Err(invalid());
        }
        let path = vec!["skills".into(), root.baseline.source.canonical_name.clone()];
        let mut clock_path = path.clone();
        clock_path.push("updatedAt".into());
        edits.push(Edit {
            path: clock_path,
            value: Some(Value::String(updated_at.clone())),
        });
        let mut hash_path = path.clone();
        hash_path.push("skillFolderHash".into());
        edits.push(Edit {
            path: hash_path,
            value: Some(Value::String(root.skill_tree.clone())),
        });
        if root.explicit_pin {
            let mut ref_path = path;
            ref_path.push("ref".into());
            edits.push(Edit {
                path: ref_path,
                value: Some(Value::String(root.commit.clone())),
            });
        } else if row.get("ref").and_then(Value::as_str) != root.requested_ref.as_deref() {
            return Err(invalid());
        }
    }
    Ok(Some(json_fields::apply_governance(raw, &edits)?))
}
pub fn definitions(before: Option<&[u8]>, roots: &[UpgradedRoot]) -> Result<Option<Vec<u8>>> {
    let selected = roots
        .iter()
        .filter(|root| root.native_before.is_none())
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return Ok(before.map(<[u8]>::to_vec));
    }
    let raw = before.ok_or_else(invalid)?;
    let document = crate::policy::json::parse(raw)?;
    let mut edits = vec![];
    for root in selected {
        let name = &root.baseline.source.canonical_name;
        let row = document
            .get("assets")
            .and_then(|v| v.get(name))
            .ok_or_else(invalid)?;
        let rules = crate::source::rules::source_payload::Rules::declared(row)?;
        if rules.repository.key() != root.baseline.source.repository
            || row.get("path").and_then(Value::as_str) != Some(&root.baseline.source.skill_path)
            || root.baseline.source.member != format!("asset:{name}")
        {
            return Err(invalid());
        }
        edits.push(Edit {
            path: vec!["assets".into(), name.clone(), "revision".into()],
            value: Some(Value::String(root.commit.clone())),
        });
    }
    Ok(Some(json_fields::apply_governance(raw, &edits)?))
}
pub fn managed(
    before: Option<&[u8]>,
    roots: &[UpgradedRoot],
    home: &Path,
    owner: u32,
) -> Result<Option<Vec<u8>>> {
    let book = source_baseline::read(before, home, owner)?;
    let mut result = before.map(<[u8]>::to_vec);
    for root in roots {
        let existing = book
            .roots
            .get(&root.baseline.source.member)
            .ok_or_else(|| {
                Error::attention("source_baseline_required", "升级前必须明确采纳完整来源根")
            })?;
        if existing != &root.baseline {
            return Err(invalid());
        }
        existing.authorize(
            &root.baseline.source,
            &root.change.live,
            &root.change.before,
            home,
            owner,
        )?;
        let mut next = Baseline::capture(
            root.baseline.source.clone(),
            root.change.live.clone(),
            &root.change.after,
            home,
            owner,
        )?;
        next.revision = Some(Revision {
            requested_ref: root.requested_ref.clone(),
            commit: root.commit.clone(),
            skill_tree: root.skill_tree.clone(),
            base_sha256: root.base_sha256.clone(),
        });
        result = Some(source_baseline::adopt(
            result.as_deref(),
            next,
            home,
            owner,
        )?);
    }
    Ok(result)
}
pub fn receipt_specs(roots: &[UpgradedRoot], native_after: Option<&[u8]>) -> Result<()> {
    for root in roots {
        if let Some(spec) = &root.native_after {
            let expected = crate::source::rules::source_receipts::known_skill(
                &spec.package.id,
                &spec.query,
                native_after.ok_or_else(invalid)?,
                &root.change.live,
                &root.change.after,
                root.change.after.uid,
            )?;
            if expected != spec.expected {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

pub fn overlays(
    before: Option<&[u8]>,
    after: Option<&[u8]>,
    roots: &[UpgradedRoot],
    home: &Path,
) -> Result<()> {
    use crate::capability::rules::invocation;
    let Some(raw) = before else {
        if after.is_some() || roots.iter().any(|root| root.invocation.manual()) {
            return Err(invalid());
        }
        for root in roots {
            if root.base_shape != root.change.after.shape() {
                return Err(invalid());
            }
        }
        return Ok(());
    };
    let old = invocation::record(home, raw)?;
    let next_raw = after.ok_or_else(invalid)?;
    let next = invocation::record(home, next_raw)?;
    let mut edits = vec![];
    for root in roots {
        project_overlay_root(root, &old, &next, home, &mut edits)?;
    }
    if !matches!(json_fields::apply_governance(raw,&edits),Ok(ref expected) if expected.as_slice()==next_raw)
    {
        return Err(invalid());
    }
    Ok(())
}

fn project_overlay_root(
    root: &UpgradedRoot,
    old: &crate::capability::rules::invocation::OverlayRecord,
    next: &crate::capability::rules::invocation::OverlayRecord,
    home: &Path,
    edits: &mut Vec<Edit>,
) -> Result<()> {
    use crate::capability::rules::invocation::{self, MetadataKind};
    let mut shape = root.base_shape.clone();
    let mut normalized = shape.clone();
    normalized.uid = 0;
    normalized.mode = 0o700;
    for entry in normalized.entries.values_mut() {
        entry.uid = 0;
    }
    if !matches!(normalized.digest(),Ok(ref digest) if digest==&root.base_sha256) {
        return Err(invalid());
    }
    for (path, row) in &old.files {
        let Ok(relative) = path.strip_prefix(root.change.live.path()) else {
            continue;
        };
        invocation::metadata_access(home, path, row.kind)?;
        row.bytes()?;
        let entry = root
            .change
            .before
            .entries
            .get(relative.to_string_lossy().as_ref())
            .ok_or_else(invalid)?;
        if entry.mode != row.applied_mode
            || !matches!(&entry.content,crate::policy::source_tree::Content::File{sha256,..} if sha256==&row.applied_hash)
        {
            return Err(invalid());
        }
        if !root.invocation.manual() {
            edits.push(Edit {
                path: vec!["files".into(), path.to_string_lossy().into_owned()],
                value: None,
            });
        }
    }
    if root.invocation.manual() {
        for (relative, kind) in [
            ("SKILL.md", MetadataKind::Frontmatter),
            ("agents/openai.yaml", MetadataKind::CodexPolicy),
        ] {
            let path = root.change.live.path().join(relative);
            invocation::metadata_access(home, &path, kind)?;
            let (row, applied) = project_row(root, old, next, relative, kind)?;
            if let Some((parent, _)) = relative.rsplit_once('/') {
                match shape.entries.get(parent) {
                    None => {
                        shape.entries.insert(
                            parent.into(),
                            crate::policy::source_tree::ShapeEntry {
                                uid: root.change.after.uid,
                                mode: 0o700,
                                content: crate::policy::source_tree::Content::Directory,
                            },
                        );
                    }
                    Some(entry)
                        if matches!(
                            entry.content,
                            crate::policy::source_tree::Content::Directory
                        ) => {}
                    _ => return Err(invalid()),
                }
            }
            shape.entries.insert(
                relative.into(),
                crate::policy::source_tree::ShapeEntry {
                    uid: root.change.after.uid,
                    mode: row.applied_mode,
                    content: crate::policy::source_tree::Content::File {
                        sha256: invocation::hash(&applied),
                        size: applied.len() as u64,
                    },
                },
            );
            edits.push(Edit {
                path: vec!["files".into(), path.to_string_lossy().into_owned()],
                value: Some(serde_json::json!(row)),
            });
        }
    }
    if shape != root.change.after.shape() {
        return Err(invalid());
    }
    Ok(())
}
fn project_row<'a>(
    root: &UpgradedRoot,
    old: &crate::capability::rules::invocation::OverlayRecord,
    next: &'a crate::capability::rules::invocation::OverlayRecord,
    relative: &str,
    kind: crate::capability::rules::invocation::MetadataKind,
) -> Result<(
    &'a crate::capability::rules::invocation::OverlayRow,
    Vec<u8>,
)> {
    use crate::{capability::rules::invocation, policy::source_tree::Content};
    let path = root.change.live.path().join(relative);
    let previous = old.files.get(&path);
    let row = next.files.get(&path).ok_or_else(invalid)?;
    let (original, applied) = row.bytes()?;
    let base = root.base_shape.entries.get(relative);
    let owner = old
        .files
        .get(&root.change.live.path().join("SKILL.md"))
        .map_or("local", |row| row.owner.as_str());
    let label = previous.map_or(root.baseline.source.canonical_name.as_str(), |row| {
        row.label.as_str()
    });
    let owner = previous.map_or(owner, |row| row.owner.as_str());
    if row.kind != kind
        || row.label != label
        || row.owner != owner
        || row.applied_mode
            != previous.map_or(row.original_mode.unwrap_or(0o600), |row| row.applied_mode)
    {
        return Err(invalid());
    }
    match (base, original) {
        (None, None) if row.original_mode.is_none() => (),
        (Some(entry), Some(bytes))
            if row.original_mode == Some(entry.mode)
                && matches!(&entry.content,Content::File{sha256,size} if *sha256==invocation::hash(&bytes) && *size==bytes.len() as u64) =>
            {}
        _ => return Err(invalid()),
    }
    Ok((row, applied))
}
pub fn timestamp(seconds: u64) -> Result<String> {
    if seconds > 253_402_300_799 {
        return Err(invalid());
    }
    let leap = |year: u64| {
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
    };
    let mut days = seconds / 86400;
    let mut year = 1970;
    loop {
        let length = if leap(year) { 366 } else { 365 };
        if days < length {
            break;
        }
        days -= length;
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for length in months {
        if days < length {
            break;
        }
        days -= length;
        month += 1;
    }
    let time = seconds % 86400;
    Ok(format!(
        "{year:04}-{month:02}-{:02}T{:02}:{:02}:{:02}Z",
        days + 1,
        time / 3600,
        time / 60 % 60,
        time % 60
    ))
}
#[cfg(test)]
#[path = "../../../tests/unit/source_projection_policy.rs"]
mod tests;
