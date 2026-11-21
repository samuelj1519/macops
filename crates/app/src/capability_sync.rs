use super::{
    Context, Error, FileChange, Governance, GovernanceCategory, LedgerChange, LedgerSlot, Manifest,
    Path, Result, Store, Value, backup, bytes, empty_pending, execute, files, guard, invalid,
    journal, json, ledger, policy, save_pending, saved_blob, witness,
};
use crate::{link_inventory, profile_inventory};

struct Assessment {
    links: Option<link_inventory::Assessment>,
    configuration: profile_inventory::Assessment,
    profiles_selected: bool,
}
impl Assessment {
    fn verify(&self) -> Result<()> {
        if let Some(links) = &self.links {
            links.verify()?;
        }
        self.configuration.verify()
    }
    fn changed(&self) -> bool {
        self.links.as_ref().is_some_and(|a| !a.patches.is_empty())
            || !self.configuration.patches.is_empty()
            || self.configuration.book_before != self.configuration.book_after
    }
    fn summary(&self) -> Value {
        let mut result = self.links.as_ref().map_or_else(
            || json!({"backend":"rust","scope":"global","results":[],"missing":[],"conflicts":[],"changes":[],"persistentWrites":false}),
            link_inventory::Assessment::summary,
        );
        result["operation"] = json!(if self.profiles_selected {
            "delivery"
        } else {
            "links"
        });
        result["profile_config_assessed"] = json!(self.profiles_selected);
        result["pending_config_policies"] = json!(self.configuration.pending);
        result["incomplete"] = json!(
            !self.configuration.pending.is_empty()
                || self
                    .links
                    .as_ref()
                    .is_some_and(|a| !a.missing.is_empty() || !a.conflicts.is_empty())
        );
        result["status"] = json!(if self.changed() { "drift" } else { "same" });
        let mut changes = result["changes"].as_array().cloned().unwrap_or_default();
        changes.extend(
            self.configuration
                .patches
                .iter()
                .map(|p| json!({"path":p.path,"action":"configure"})),
        );
        result["planned_changes"] = json!(changes.len());
        result["changes"] = json!(changes);
        result
    }
}
fn merge<T: Clone + PartialEq>(
    first: &mut Vec<T>,
    second: Vec<T>,
    key: impl Fn(&T) -> &Path,
) -> Result<()> {
    for item in second {
        if let Some(old) = first.iter().find(|old| key(old) == key(&item)) {
            if old != &item {
                return Err(invalid());
            }
        } else {
            first.push(item);
        }
    }
    Ok(())
}
fn changes(assessment: &Assessment, blobs: &files::Blobs) -> Result<Vec<FileChange>> {
    let mut changes = vec![];
    if let Some(links) = &assessment.links {
        for patch in &links.patches {
            let (current, raw) = witness(&patch.path)?;
            if current.state != patch.before || raw != patch.before_bytes {
                return Err(invalid());
            }
            changes.push(FileChange {
                path: patch.path.clone(),
                kind: crate::policy::delivery::ChangeKind::ManagedLink,
                namespace: patch.namespace.clone(),
                before: patch.before.clone(),
                before_blob: saved_blob(blobs, raw.as_deref())?,
                after_blob: saved_blob(blobs, patch.after.as_ref().map(|t| t.as_str().as_bytes()))?,
                after_mode: None,
            });
        }
    }
    for patch in &assessment.configuration.patches {
        let (current, raw) = witness(&patch.path)?;
        if current.state != patch.before || raw.as_deref() != Some(&patch.before_bytes) {
            return Err(invalid());
        }
        if changes.iter().any(|c| c.path == patch.path) {
            return Err(invalid());
        }
        changes.push(FileChange {
            path: patch.path.clone(),
            kind: assessment
                .configuration
                .authority
                .target_kind(&patch.path)?,
            namespace: patch.namespace.clone(),
            before: patch.before.clone(),
            before_blob: saved_blob(blobs, Some(&patch.before_bytes))?,
            after_blob: saved_blob(blobs, Some(&patch.after))?,
            after_mode: Some(patch.mode),
        });
    }
    Ok(changes)
}
fn ledgers(
    ctx: &Context,
    authority: &Governance,
    assessment: &Assessment,
    operation: &str,
    blobs: &files::Blobs,
) -> Result<Vec<LedgerChange>> {
    let content: Vec<_> = assessment
        .configuration
        .patches
        .iter()
        .map(|p| crate::capability_receipts::ContentChange {
            path: &p.path,
            after: Some(p.after.as_slice()),
            mode: Some(p.mode),
        })
        .collect();
    let mut ledgers = vec![];
    for (slot, path) in [
        (LedgerSlot::Managed, authority.paths.managed.clone()),
        (LedgerSlot::Overlays, authority.paths.overlays.clone()),
        (LedgerSlot::Receipts, ctx.state.join("receipts.json")),
    ] {
        crate::paths::write_path(&path)?;
        let (before, raw) = witness(&path)?;
        let after = match slot {
            LedgerSlot::Overlays => {
                if raw != assessment.configuration.book_before {
                    return Err(invalid());
                }
                assessment.configuration.book_after.clone()
            }
            LedgerSlot::Receipts => crate::capability_receipts::project_changes(
                ctx,
                raw.as_deref(),
                &content,
                operation,
            )?,
            LedgerSlot::Managed => crate::source_inventory::project_metadata(
                ctx,
                raw.as_deref(),
                &changes(assessment, blobs)?,
            )?,
            LedgerSlot::Native => return Err(invalid()),
        };
        ledgers.push(ledger(
            blobs,
            slot,
            before,
            raw.as_deref(),
            after.as_deref(),
        )?);
    }
    Ok(ledgers)
}
fn manifest(
    ctx: &Context,
    authority: &Governance,
    assessment: &Assessment,
    operation: &str,
    change: &str,
    blobs: &files::Blobs,
) -> Result<Manifest> {
    if assessment.configuration.authority.targets.is_empty()
        && let Some(links) = &assessment.links
    {
        return super::links_manifest(ctx, authority, links, operation, change, blobs);
    }
    let (mut inputs, mut directories) = assessment.configuration.reader.witnesses();
    if let Some(links) = &assessment.links {
        let (files, dirs) = links.reader.witnesses();
        merge(&mut inputs, files, |f| &f.path)?;
        merge(&mut directories, dirs, |d| &d.binding.requested)?;
    }
    let changes = changes(assessment, blobs)?;
    for file in &changes {
        if !directories
            .iter()
            .any(|d| d.binding.requested == file.namespace.ancestor.requested)
        {
            let root = files::open(&file.namespace)?;
            directories.push(policy::DirectoryWitness {
                binding: file.namespace.ancestor.clone(),
                entries: guard::listing(&root)?,
            });
        }
    }
    let manifest = Manifest {
        schema_version: 4,
        layout: Some(crate::policy::governance::Layout {
            config: ctx.config.clone(),
            data: ctx.data.clone(),
            state: ctx.state.clone(),
            cache: ctx.cache.clone(),
        }),
        links: assessment.links.as_ref().map(|a| a.authority.clone()),
        configuration: Some(assessment.configuration.authority.clone()),
        operation_id: operation.into(),
        change_id: change.into(),
        category: GovernanceCategory::Links,
        home: ctx.home.clone(),
        state: ctx.state.clone(),
        settings: ctx.settings.clone(),
        pending: authority.paths.pending.clone(),
        inputs,
        directories,
        files: changes,
        ledgers: ledgers(ctx, authority, assessment, operation, blobs)?,
    };
    policy::validate_manifest(&manifest)?;
    Ok(manifest)
}
pub(super) fn sync(
    ctx: &Context,
    selection: &crate::cli::SkillSelection,
    dry_run: bool,
    yes: bool,
) -> Result<Value> {
    let requested = crate::policy::delivery::selection(
        selection.name.as_deref(),
        &selection.only,
        selection.all,
        selection.revision.as_deref(),
        selection.change_id.as_deref(),
    )?;
    let selected = crate::policy::profile_select::split(requested)?;
    let initial = crate::governance::capture(ctx)?;
    initial.ensure_settled()?;
    let assessment = Assessment {
        configuration: profile_inventory::assess(ctx, &initial, &selected)?,
        links: selected
            .links
            .map(|r| link_inventory::assess(ctx, &initial, r))
            .transpose()?,
        profiles_selected: !selected.profiles.is_empty(),
    };
    let mut summary = assessment.summary();
    assessment.verify()?;
    initial.verify()?;
    if dry_run {
        summary["mode"] = json!("dry-run");
        return Ok(summary);
    }
    if assessment
        .links
        .as_ref()
        .is_some_and(|a| !a.missing.is_empty())
    {
        return Err(Error::attention(
            "link_targets_missing",
            "明确声明的 profile 或技能缺失，未开始整批写入",
        ));
    }
    if !assessment.changed() {
        return Ok(summary);
    }
    crate::operations::confirm("收敛已选能力链接与 profile 配置字段", yes)?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    assessment.verify()?;
    let authority = crate::governance::capture(ctx)?;
    authority.ensure_settled()?;
    if authority.paths != initial.paths {
        return Err(invalid());
    }
    let operation = crate::store::operation_id();
    let change = selection.change_id.as_deref().unwrap_or(&operation);
    let root = backup(&authority, change)?;
    let blobs = files::Blobs::create(&root)?;
    let manifest = manifest(ctx, &authority, &assessment, &operation, change, &blobs)?;
    let raw = bytes(&manifest)?;
    let digest = files::hash(&raw);
    Store::atomic_bytes(root.file(), "capability-plan.json", &raw, true)?;
    let mut journal = journal(
        ctx,
        &manifest,
        &root.path.join("capability-plan.json"),
        &digest,
    )?;
    store.save(&journal)?;
    let mut pending = empty_pending(&manifest, &digest);
    save_pending(&root, &manifest, &pending)?;
    let result = execute(&store, &root, &blobs, &manifest, &mut pending, &mut journal)?;
    for key in [
        "status",
        "operation_id",
        "change_id",
        "changed",
        "persistentWrites",
    ] {
        summary[key] = result[key].clone();
    }
    Ok(summary)
}

#[cfg(test)]
#[path = "../tests/unit/profile_sync.rs"]
mod tests;
