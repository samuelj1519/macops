use crate::{
    capability::{
        capability_local,
        invocation_inventory::Patch,
        rules::capability_local::{self as policy, StampInput},
    },
    context::Context,
    domain::{Observation, QuerySpec, Receipts, fingerprint},
    error::{Error, Result},
};

fn book(bytes: &[u8]) -> Result<Receipts> {
    let value = crate::policy::json::parse(bytes)?;
    let crate::policy::json::Object(receipts) =
        serde_json::from_value::<crate::policy::json::Object<Receipts>>(value)?;
    crate::policy::operation::state::receipt_schema(&receipts)?;
    Ok(receipts)
}
#[derive(Clone, Copy)]
pub struct ContentChange<'a> {
    pub path: &'a std::path::Path,
    pub after: Option<&'a [u8]>,
    pub mode: Option<u32>,
}
pub fn project(
    ctx: &Context,
    before: Option<&[u8]>,
    patches: &[Patch],
    operation: &str,
) -> Result<Option<Vec<u8>>> {
    let changes: Vec<_> = patches
        .iter()
        .map(|p| ContentChange {
            path: &p.target.path,
            after: p.after.as_deref(),
            mode: p.after_mode,
        })
        .collect();
    project_changes(ctx, before, &changes, operation)
}
pub fn project_changes(
    ctx: &Context,
    before: Option<&[u8]>,
    patches: &[ContentChange<'_>],
    operation: &str,
) -> Result<Option<Vec<u8>>> {
    let Some(before) = before else {
        return Ok(None);
    };
    let mut receipts = book(before)?;
    let mut changed = false;
    for (id, receipt) in &mut receipts.installations {
        if *id != receipt.package.id {
            return Err(invalid());
        }
        let Some(QuerySpec::LocalCapability(query)) = &receipt.query else {
            continue;
        };
        let selected: Vec<_> = patches
            .iter()
            .filter(|p| p.path.starts_with(&query.payload.requested))
            .collect();
        let settings = query
            .settings
            .as_ref()
            .and_then(|s| patches.iter().find(|p| p.path == s.requested));
        if selected.is_empty() && settings.is_none() {
            continue;
        }
        if receipt.scope.as_ref() != Some(&ctx.scope) {
            return Err(Error::attention(
                "capability_scope_conflict",
                "受影响的采纳基线属于其他作用域",
            ));
        }
        policy::binding(
            &receipt.package,
            receipt.query.as_ref().ok_or_else(invalid)?,
        )?;
        let current = capability_local::observe_default(id, query)?;
        if fingerprint(&current)? != fingerprint(&receipt.observation)? {
            return Err(Error::attention(
                "capability_baseline_drift",
                "能力包采纳基线已经漂移，不能直接覆盖",
            ));
        }
        let Observation::CapabilityPack {
            capability,
            pack: Some(mut stamp),
        } = current
        else {
            return Err(invalid());
        };
        let mut snapshot = capability_local::snapshot_payload(
            capability,
            &query.payload,
            &mut (64 * 1024 * 1024),
        )?
        .ok_or_else(invalid)?;
        check_snapshot(id, &stamp, &snapshot)?;
        if let Some(settings) = settings {
            project_settings(id, query, settings, &mut stamp)?;
        }
        for patch in selected {
            let path = patch
                .path
                .strip_prefix(&query.payload.requested)
                .map_err(|_| invalid())?;
            let rel = path.to_str().ok_or_else(invalid)?.to_owned();
            snapshot.entries.retain(|(name, _, _)| name != &rel);
            if let Some(bytes) = &patch.after {
                snapshot.entries.push((
                    rel,
                    patch.mode.ok_or_else(invalid)? & 0o111 != 0,
                    crate::capability::capability_store::hash(bytes),
                ));
            }
        }
        snapshot.entries.sort_by(|a, b| a.0.cmp(&b.0));
        receipt.observation = Observation::CapabilityPack {
            capability,
            pack: Some(policy::build_stamp(StampInput {
                id,
                record_digest: stamp.record_digest,
                payload_digest: policy::payload_tree_digest(id, &snapshot.entries)?,
                payload_identity: snapshot.identity,
                file_count: snapshot.entries.len() as u64,
                revision: stamp.revision,
                configured_enabled: stamp.configured_enabled,
                lineage: &snapshot.lineage,
            })?),
        };
        receipt.operation_id = operation.into();
        receipt.updated_at = crate::operations::store::now();
        changed = true;
    }
    if changed {
        let mut bytes = serde_json::to_vec_pretty(&receipts)?;
        bytes.push(b'\n');
        Ok(Some(bytes))
    } else {
        Ok(Some(before.into()))
    }
}
fn project_settings(
    id: &str,
    query: &crate::domain::LocalCapabilityQuery,
    settings: &ContentChange<'_>,
    stamp: &mut crate::domain::CapabilityStamp,
) -> Result<()> {
    let (before_record, after_record) =
        capability_local::project_plugin_settings(id, query, settings.after.ok_or_else(invalid)?)?;
    if before_record.digest != stamp.record_digest
        || before_record.revision != stamp.revision
        || before_record.configured_enabled != stamp.configured_enabled
    {
        return Err(Error::attention(
            "capability_baseline_drift",
            "插件登记在计算配置后态期间改变",
        ));
    }
    stamp.record_digest = after_record.digest;
    stamp.revision = after_record.revision;
    stamp.configured_enabled = after_record.configured_enabled;
    Ok(())
}
fn invalid() -> Error {
    Error::attention(
        "invalid_capability_receipt",
        "受影响的能力包采纳记录不可验证",
    )
}

pub fn verify_after(bytes: Option<&[u8]>, operation: &str) -> Result<()> {
    let Some(bytes) = bytes else {
        return Ok(());
    };
    let receipts = book(bytes)?;
    for (id, receipt) in &receipts.installations {
        if receipt.operation_id != operation {
            continue;
        }
        let Some(QuerySpec::LocalCapability(query)) = &receipt.query else {
            continue;
        };
        let current = capability_local::observe_default(id, query)?;
        if fingerprint(&current)? != fingerprint(&receipt.observation)? {
            return Err(Error::attention(
                "capability_after_mismatch",
                "实际能力包与本批目标基线不符",
            ));
        }
    }
    Ok(())
}

fn check_snapshot(
    id: &str,
    stamp: &crate::domain::CapabilityStamp,
    snapshot: &capability_local::PayloadSnapshot,
) -> Result<()> {
    let checked = policy::build_stamp(StampInput {
        id,
        record_digest: stamp.record_digest.clone(),
        payload_digest: policy::payload_tree_digest(id, &snapshot.entries)?,
        payload_identity: snapshot.identity.clone(),
        file_count: snapshot.entries.len() as u64,
        revision: stamp.revision.clone(),
        configured_enabled: stamp.configured_enabled,
        lineage: &snapshot.lineage,
    })?;
    if &checked != stamp {
        return Err(Error::attention(
            "capability_baseline_drift",
            "载荷在生成目标基线期间改变",
        ));
    }
    Ok(())
}

pub fn verify_before(bytes: Option<&[u8]>, paths: &[&std::path::Path]) -> Result<()> {
    let Some(bytes) = bytes else {
        return Ok(());
    };
    let receipts = book(bytes)?;
    for (id, receipt) in &receipts.installations {
        let Some(QuerySpec::LocalCapability(query)) = &receipt.query else {
            continue;
        };
        if !paths.iter().any(|path| {
            path.starts_with(&query.payload.requested)
                || query
                    .settings
                    .as_ref()
                    .is_some_and(|s| *path == s.requested)
        }) {
            continue;
        }
        let current = capability_local::observe_default(id, query)?;
        if fingerprint(&current)? != fingerprint(&receipt.observation)? {
            return Err(Error::attention(
                "capability_before_mismatch",
                "回退后的完整能力载荷与原采纳基线不符，保留现场",
            ));
        }
    }
    Ok(())
}
