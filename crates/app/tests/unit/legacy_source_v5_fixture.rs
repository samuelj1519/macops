use super::*;
use crate::policy::{
    invocation::{self, MetadataKind},
    json_fields::{self, Edit},
    source_payload::{Entry, Payload},
    source_tx::UpgradedRoot,
};

// A historical wire fixture, not an upgrade implementation: two fixed synthetic
// roots and their recorded O/N images. It performs no source discovery or fetch.
pub(super) fn legacy_v5_fixture() -> std::result::Result<PreparedUpgrade, Box<dyn std::error::Error>>
{
    let (temp, ctx) = complete_upgrade_context()?;
    let authority = crate::operations::governance::capture(&ctx)?;
    let managed_before = authority.read(
        crate::policy::governance::Dataset::ManagedAssets,
        16 * 1024 * 1024,
    )?;
    let native_before = fs::read(crate::policy::delivery::native_lock_path(
        &ctx.home, &ctx.scope,
    ))?;
    let definitions_before = authority.read(
        crate::policy::governance::Dataset::AssetDefinitions,
        16 * 1024 * 1024,
    )?;
    let overlays_before = authority.read(
        crate::policy::governance::Dataset::InvocationOverlays,
        16 * 1024 * 1024,
    )?;
    let rules_before = authority.read(
        crate::policy::governance::Dataset::InvocationRules,
        1024 * 1024,
    )?;
    let applied = crate::policy::source_invocation::applied(
        &ctx.home,
        rules_before.as_deref(),
        overlays_before.as_deref(),
    )?;
    let mut roots = vec![];
    let mut edits = vec![];
    for key in ["asset:alpha", "skill:demo"] {
        let (root, rows) = legacy_root(&ctx, &authority, &applied, key)?;
        roots.push(root);
        edits.extend(rows);
    }
    let created_at = crate::operations::store::now();
    let native =
        crate::policy::source_projection::native(Some(&native_before), &roots, created_at)?;
    for root in &mut roots {
        if let Some(spec) = &mut root.native_after {
            spec.expected = crate::policy::source_receipts::known_skill(
                &spec.package.id,
                &spec.query,
                native.as_deref().ok_or("native image")?,
                &root.change.live,
                &root.change.after,
                root.change.after.uid,
            )?;
        }
    }
    let managed = crate::policy::source_projection::managed(
        managed_before.as_deref(),
        &roots,
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    let definitions =
        crate::policy::source_projection::definitions(definitions_before.as_deref(), &roots)?;
    let overlays = overlays_before
        .as_deref()
        .map(|raw| json_fields::apply_governance(raw, &edits))
        .transpose()?;
    let (store, batch, journal) = legacy_batch(
        &ctx,
        &authority,
        roots,
        &[native, definitions, managed, overlays],
        created_at,
    )?;
    Ok((temp, ctx, store, batch, journal))
}

fn legacy_payload(name: &str) -> Payload {
    Payload {
        entries: std::collections::BTreeMap::from([(
            "SKILL.md".into(),
            Entry::File {
                mode: 0o644,
                bytes: format!("---\nname: {name}\n---\nnew\n").into_bytes(),
            },
        )]),
    }
}

fn legacy_root(
    ctx: &Context,
    authority: &crate::operations::governance::Governance,
    applied: &crate::policy::source_invocation::Applied,
    key: &str,
) -> std::result::Result<(UpgradedRoot, Vec<Edit>), Box<dyn std::error::Error>> {
    let mut edits = vec![];
    let current = crate::source::source_inventory::assess_adoption(ctx, authority, key, false)?
        .ok_or("legacy source")?;
    let mut payload = legacy_payload(&current.baseline.source.canonical_name);
    let base_sha256 = payload.shape(0, 0o700).digest()?;
    let base_shape = payload.shape(current.tree.uid, current.tree.mode);
    let intent = crate::policy::source_invocation::intent(
        applied.declaration.as_ref(),
        &current.baseline.source.canonical_name,
        None,
    )?;
    if intent.manual() {
        for (relative, kind) in [
            ("SKILL.md", MetadataKind::Frontmatter),
            ("agents/openai.yaml", MetadataKind::CodexPolicy),
        ] {
            let original = match payload.entries.get(relative) {
                Some(Entry::File { bytes, .. }) => Some(bytes.clone()),
                _ => None,
            };
            let mode = if relative == "SKILL.md" {
                Some(0o644)
            } else {
                None
            };
            let proposal = invocation::propose(
                kind,
                &current.baseline.source.canonical_name,
                "policy",
                original.as_deref(),
                mode,
                None,
            )?;
            if relative.starts_with("agents/") {
                payload
                    .entries
                    .insert("agents".into(), Entry::Directory { mode: 0o700 });
            }
            payload.entries.insert(
                relative.into(),
                Entry::File {
                    mode: proposal.after_mode.ok_or("mode")?,
                    bytes: proposal.after.ok_or("metadata")?,
                },
            );
            edits.push(Edit {
                path: vec![
                    "files".into(),
                    current
                        .baseline
                        .slot
                        .path()
                        .join(relative)
                        .to_str()
                        .ok_or("path")?
                        .into(),
                ],
                value: Some(serde_json::to_value(proposal.row.ok_or("legacy overlay")?)?),
            });
        }
    }
    let parent = Directory::open(&current.baseline.slot.parent.requested)?;
    let retained = tempfile::Builder::new()
        .prefix(".legacy-v5-")
        .tempdir_in(&parent.path)?
        .keep();
    let retained = Directory::open(&retained)?;
    fs::set_permissions(&retained.path, fs::Permissions::from_mode(0o700))?;
    let (retained, after) = crate::source::source_payload::materialize(
        &retained,
        "payload",
        &payload,
        current.tree.mode,
    )?;
    let root = UpgradedRoot {
        baseline: current.baseline.clone(),
        change: crate::policy::source_tree::Change {
            live: current.baseline.slot.clone(),
            retained,
            before: current.tree,
            after,
        },
        commit: "c".repeat(40),
        skill_tree: "d".repeat(40),
        explicit_pin: false,
        requested_ref: if current.native.is_some() {
            None
        } else {
            Some("main".into())
        },
        base_sha256,
        base_shape,
        invocation: intent,
        native_before: current.native.clone(),
        native_after: current.native,
    };
    Ok((root, edits))
}

fn legacy_batch(
    ctx: &Context,
    authority: &crate::operations::governance::Governance,
    roots: Vec<UpgradedRoot>,
    images: &[Option<Vec<u8>>; 4],
    created_at: u64,
) -> std::result::Result<(Store, Batch, Journal), Box<dyn std::error::Error>> {
    let invocation_path = authority
        .paths
        .dataset(crate::policy::governance::Dataset::InvocationRules);
    let invocation = witness(&invocation_path)?.0;
    let store = Store::lock(ctx)?;
    let operation = crate::operations::store::operation_id();
    let root = backup(authority, &operation)?;
    let blobs = files::Blobs::create(&root)?;
    let mut manifest = Manifest {
        schema_version: 5,
        operation_id: operation.clone(),
        change_id: operation.clone(),
        home: ctx.home.clone(),
        settings: ctx.settings.clone(),
        layout: Layout {
            config: ctx.config.clone(),
            data: ctx.data.clone(),
            state: ctx.state.clone(),
            cache: ctx.cache.clone(),
        },
        scope: ctx.scope.clone(),
        pending: authority.paths.pending.clone(),
        created_at,
        action: SourceAction::Upgrade {
            roots,
            invocation: Box::new(invocation.clone()),
        },
        inputs: vec![invocation],
        ledgers: vec![],
    };
    for (index, (path, slot)) in policy::paths(&manifest, &authority.paths)
        .into_iter()
        .zip(policy::SLOTS)
        .enumerate()
    {
        let (witness, before) = witness(&path)?;
        let after = if index < 4 {
            images[index].clone()
        } else {
            receipts(before.as_deref(), &manifest)?
        };
        let after_mode = after
            .as_ref()
            .map(|_| policy::mode(&witness.state).unwrap_or(0o600));
        let pair = if before == after {
            None
        } else {
            Some(crate::source::source_files::stage(
                &witness,
                after.as_deref().ok_or("image")?,
                after_mode.ok_or("mode")?,
            )?)
        };
        manifest.ledgers.push(Ledger {
            slot,
            witness,
            before: before.as_deref().map(|b| blobs.put(b)).transpose()?,
            after: after.as_deref().map(|b| blobs.put(b)).transpose()?,
            after_mode,
            pair,
        });
    }
    policy::validate(&manifest, &authority.paths, nix::unistd::geteuid().as_raw())?;
    verify_projection(&manifest, &blobs)?;
    assert_eq!(
        manifest
            .ledgers
            .iter()
            .filter(|ledger| ledger.pair.is_some())
            .count(),
        5
    );
    let (mut batch, mut journal) = persist(ctx, &store, root, blobs, manifest)?;
    batch.save(&store)?;
    batch.prepare(&store)?;
    journal.phase = Phase::Running;
    store.save(&journal)?;
    Ok((store, batch, journal))
}
