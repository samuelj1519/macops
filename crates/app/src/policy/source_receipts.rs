use crate::{
    domain::{CapabilityKind, LocalCapabilityQuery, Observation},
    error::{Error, Result},
    policy::{
        capability_local::{self as native, PayloadMetadataDisposition},
        source_tree::{Content, Slot, Tree},
    },
};
fn invalid() -> Error {
    Error::attention(
        "unverified_source_receipt",
        "已知来源目录与原生记录不能形成完整采纳后态",
    )
}
fn entries(tree: &Tree, mut remaining: u64) -> Result<Vec<(String, bool, String)>> {
    if tree.entries.len() > native::PAYLOAD_ENTRY_LIMIT {
        return Err(invalid());
    }
    let mut result = vec![];
    for (path, entry) in &tree.entries {
        let depth = path.split('/').count() - 1;
        let name = path
            .rsplit_once('/')
            .map_or(path.as_str(), |(_, name)| name);
        match native::classify_payload_entry(
            name,
            depth,
            matches!(entry.content, Content::File { .. }),
        ) {
            PayloadMetadataDisposition::IgnoreRegularMarker => continue,
            PayloadMetadataDisposition::RejectReservedEntry => return Err(invalid()),
            PayloadMetadataDisposition::Include => (),
        }
        if depth >= native::PAYLOAD_DEPTH_LIMIT
            || (matches!(entry.content, Content::Directory)
                && depth + 1 >= native::PAYLOAD_DEPTH_LIMIT)
        {
            return Err(invalid());
        }
        if let Content::File { sha256, size } = &entry.content {
            if *size > native::FILE_LIMIT || *size > remaining {
                return Err(invalid());
            }
            remaining -= size;
            result.push((path.clone(), entry.mode & 0o111 != 0, sha256.clone()));
        }
    }
    if !result.iter().any(|(path, _, _)| path == "SKILL.md") {
        return Err(invalid());
    }
    Ok(result)
}
pub fn known_skill(
    id: &str,
    query: &LocalCapabilityQuery,
    record: &[u8],
    slot: &Slot,
    tree: &Tree,
    owner: u32,
) -> Result<Observation> {
    slot.validate(owner)?;
    tree.validate(owner)?;
    crate::policy::config_local::validate_anchor(&query.record)?;
    if query.native_id != slot.name
        || query.settings.is_some()
        || query.capability != CapabilityKind::SkillsCli
        || query.manager != "skills-cli:github"
        || query.payload.requested != slot.path()
        || query.payload.name != slot.name
        || query.payload.canonical_parent != slot.parent.canonical
        || query.payload.parent_identity != slot.parent.identity
        || tree.identity.device != slot.parent.identity.device
        || record.len() as u64 > native::FILE_LIMIT
    {
        return Err(invalid());
    }
    let (record_digest, revision) = native::parse_skill_record(
        id,
        record,
        &query.native_id,
        "github",
        query.source.as_deref().ok_or_else(invalid)?,
    )?
    .ok_or_else(invalid)?;
    let entries = entries(tree, native::PAYLOAD_BYTE_BUDGET - record.len() as u64)?;
    let projected = slot.project(tree);
    native::payload_tree_digest(id, &entries)
        .and_then(|payload_digest| {
            native::build_stamp(native::StampInput {
                id,
                record_digest,
                payload_digest,
                payload_identity: tree.identity.clone(),
                file_count: entries.len() as u64,
                revision,
                configured_enabled: None,
                lineage: &projected.lineage,
            })
        })
        .map(|stamp| Observation::CapabilityPack {
            capability: CapabilityKind::SkillsCli,
            pack: Some(stamp),
        })
}
#[cfg(test)]
#[path = "../../tests/unit/source_receipts_policy.rs"]
mod tests;
