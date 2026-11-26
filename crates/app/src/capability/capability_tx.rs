use crate::{
    capability::{
        capability_guard as guard, capability_store as files,
        invocation_inventory::Assessment,
        rules::capability_tx::{
            self as policy, Blob, Candidate, FileChange, LedgerChange, LedgerSlot, Manifest,
            Namespace, Pending, State, Witness,
        },
    },
    context::Context,
    domain::{Action, GovernanceCategory, Kind, Observation, Package, Plan, Provider, fingerprint},
    error::{Error, Result},
    operations::{
        governance::Governance,
        store::{Journal, Phase, Store},
    },
    platform::fs::Directory,
    policy::governance::Dataset,
};
use serde_json::{Value, json};
use std::{ffi::OsStr, os::unix::fs::MetadataExt, path::Path};

fn invalid() -> Error {
    Error::attention(
        "invalid_capability_transaction",
        "能力包批次证据缺失或不匹配，保留原状",
    )
}
fn io(error: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}
fn decode<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T> {
    let value = crate::policy::json::parse(raw).map_err(|_| invalid())?;
    serde_json::from_value::<crate::policy::json::Object<T>>(value)
        .map(|v| v.0)
        .map_err(|_| invalid())
}
fn bytes(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn private_child(root: &Directory, name: &str, exclusive: bool) -> Result<Directory> {
    match rustix::fs::mkdirat(root.file(), name, rustix::fs::Mode::RWXU) {
        Ok(()) => root.file().sync_all()?,
        Err(rustix::io::Errno::EXIST) if !exclusive => (),
        Err(error) => return Err(io(error)),
    }
    let child = root.child(OsStr::new(name))?;
    let meta = child.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw()
        || (exclusive && meta.mode() & 0o7777 != 0o700)
        || (!exclusive && meta.mode() & 0o7022 != 0)
    {
        return Err(invalid());
    }
    Ok(child)
}
fn backup(authority: &Governance, change: &str) -> Result<Directory> {
    let path = &authority.paths.backups;
    crate::platform::fs::write_path(path)?;
    let parent_path = path.parent().ok_or_else(invalid)?;
    let root = match Directory::open(parent_path) {
        Ok(root) => root,
        Err(error)
            if error.code == "path_missing"
                && parent_path.file_name() == Some(OsStr::new(".agents")) =>
        {
            let home = Directory::open(parent_path.parent().ok_or_else(invalid)?)?;
            private_child(&home, ".agents", true)?
        }
        Err(error) => return Err(error),
    };
    let parent = private_child(
        &root,
        path.file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(invalid)?,
        false,
    )?;
    private_child(&parent, change, true)
}
fn saved_blob(blobs: &files::Blobs, bytes: Option<&[u8]>) -> Result<Option<Blob>> {
    bytes.map(|b| blobs.put(b)).transpose()
}
fn witness(path: &Path) -> Result<(Witness, Option<Vec<u8>>)> {
    let namespace = files::namespace(path)?;
    let (state, bytes) = files::observe(&namespace, 64 * 1024 * 1024)?;
    Ok((
        Witness {
            path: path.into(),
            namespace,
            state,
        },
        bytes,
    ))
}
fn ledger(
    blobs: &files::Blobs,
    slot: LedgerSlot,
    before: Witness,
    raw: Option<&[u8]>,
    after: Option<&[u8]>,
) -> Result<LedgerChange> {
    Ok(LedgerChange {
        slot,
        path: before.path,
        namespace: before.namespace,
        before: before.state,
        before_blob: saved_blob(blobs, raw)?,
        after_blob: saved_blob(blobs, after)?,
    })
}

fn manifest(
    ctx: &Context,
    authority: &Governance,
    assessment: &Assessment,
    operation: &str,
    change: &str,
    blobs: &files::Blobs,
) -> Result<Manifest> {
    let (mut inputs, mut directories) = assessment.reader.witnesses()?;
    inputs.push(witness(&ctx.state.join("migration-ledger.json"))?.0);
    let mut changes = vec![];
    for patch in &assessment.patches {
        crate::platform::fs::write_path(&patch.target.path)?;
        let (before, raw) = witness(&patch.target.path)?;
        if raw != patch.before {
            return Err(invalid());
        }
        changes.push(FileChange {
            path: patch.target.path.clone(),
            kind: patch.target.kind.into(),
            namespace: before.namespace,
            before: before.state,
            before_blob: saved_blob(blobs, raw.as_deref())?,
            after_blob: saved_blob(blobs, patch.after.as_deref())?,
            after_mode: patch.after_mode,
        });
    }
    for file in &changes {
        let binding = &file.namespace.ancestor;
        if !directories
            .iter()
            .any(|d| d.binding.requested == binding.requested)
        {
            let root = files::open(&file.namespace)?;
            directories.push(policy::DirectoryWitness {
                binding: binding.clone(),
                entries: guard::listing(&root)?,
            });
        }
    }
    for path in [
        &authority.paths.managed,
        &authority.paths.overlays,
        &ctx.state.join("receipts.json"),
    ] {
        crate::platform::fs::write_path(path)?;
    }
    let (managed_witness, managed) = witness(&authority.paths.dataset(Dataset::ManagedAssets))?;
    let (overlay_witness, overlay) = witness(&authority.paths.overlays)?;
    if overlay != assessment.record_before {
        return Err(invalid());
    }
    let (receipt_witness, receipt_before) = witness(&ctx.state.join("receipts.json"))?;
    let receipt_after = crate::capability::capability_receipts::project(
        ctx,
        receipt_before.as_deref(),
        &assessment.patches,
        operation,
    )?;
    let managed_after =
        crate::source::source_inventory::project_metadata(ctx, managed.as_deref(), &changes)?;
    let ledgers = vec![
        ledger(
            blobs,
            LedgerSlot::Managed,
            managed_witness,
            managed.as_deref(),
            managed_after.as_deref(),
        )?,
        ledger(
            blobs,
            LedgerSlot::Overlays,
            overlay_witness,
            overlay.as_deref(),
            assessment.record_after.as_deref(),
        )?,
        ledger(
            blobs,
            LedgerSlot::Receipts,
            receipt_witness,
            receipt_before.as_deref(),
            receipt_after.as_deref(),
        )?,
    ];
    let result = Manifest {
        schema_version: policy::POLICY_MANIFEST_VERSION,
        layout: None,
        links: None,
        configuration: None,
        operation_id: operation.into(),
        change_id: change.into(),
        category: GovernanceCategory::Policy,
        home: ctx.home.clone(),
        state: ctx.state.clone(),
        settings: ctx.settings.clone(),
        pending: authority.paths.pending.clone(),
        inputs,
        directories,
        files: changes,
        ledgers,
    };
    policy::validate_manifest(&result)?;
    Ok(result)
}
fn journal(ctx: &Context, manifest: &Manifest, path: &Path, digest: &str) -> Result<Journal> {
    let package = Package {
        id: format!("governance:{}", policy::category_name(manifest.category)),
        name: match manifest.category {
            GovernanceCategory::Policy => "调用策略同步",
            GovernanceCategory::Links => "能力链接同步",
            GovernanceCategory::Source => "能力来源交付",
        }
        .into(),
        kind: Kind::Skill,
        official_url: String::new(),
        provider: Provider::Governance {
            category: manifest.category,
            manifest: path.into(),
            manifest_sha256: digest.into(),
        },
        paths: vec![],
        exact_version_required: false,
    };
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Maintain,
        before: Observation::Governance {
            fingerprint: fingerprint(&manifest.inputs)?,
        },
        target: Some(digest.into()),
        command: None,
        query: None,
        timeout_secs: 60,
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    crate::operations::new_journal_with_id(ctx, plan, manifest.operation_id.clone())
}
fn save_pending(root: &Directory, manifest: &Manifest, pending: &Pending) -> Result<()> {
    Store::atomic(root.file(), "capability-candidates.json", pending)?;
    crate::platform::fs::write_path(&manifest.pending)?;
    let namespace = files::namespace(&manifest.pending)?;
    let parent = files::parent(&namespace)?.ok_or_else(invalid)?;
    Store::atomic(
        parent.file(),
        manifest
            .pending
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(invalid)?,
        pending,
    )
}
fn empty_pending(manifest: &Manifest, digest: &str) -> Pending {
    Pending {
        schema_version: manifest.schema_version,
        operation_id: manifest.operation_id.clone(),
        manifest_sha256: digest.into(),
        parents: vec![],
        files: vec![],
        ledgers: vec![],
        rollbacks: vec![],
        retired_parents: vec![],
    }
}
fn stage_change(
    namespace: &Namespace,
    after: Option<&Blob>,
    mode: u32,
    link: bool,
    blobs: &files::Blobs,
    pending: &mut Pending,
) -> Result<Candidate> {
    let raw = after.map(|blob| blobs.get(blob)).transpose()?;
    if files::parent(namespace)?.is_none() {
        let target_parent = files::target_path(namespace)
            .parent()
            .ok_or_else(invalid)?
            .to_path_buf();
        if !pending.parents.iter().any(|p| p.target == target_parent) {
            pending
                .parents
                .push(files::prepare_parent(namespace)?.ok_or_else(invalid)?);
        }
        let parent = pending
            .parents
            .iter()
            .find(|p| p.target == target_parent)
            .ok_or_else(invalid)?;
        let actual = parent
            .staged
            .join(namespace.remaining.last().ok_or_else(invalid)?);
        let mut candidate = stage_content(&files::namespace(&actual)?, raw.as_deref(), mode, link)?;
        candidate.target = files::target_path(namespace);
        return Ok(candidate);
    }
    stage_content(namespace, raw.as_deref(), mode, link)
}
fn stage_content(
    namespace: &Namespace,
    raw: Option<&[u8]>,
    mode: u32,
    link: bool,
) -> Result<Candidate> {
    if link && let Some(raw) = raw {
        return files::stage_link(
            namespace,
            &crate::capability::rules::delivery::LinkText::from_bytes(raw)?,
        );
    }
    files::stage(namespace, raw, mode)
}
fn prepare(
    root: &Directory,
    manifest: &Manifest,
    blobs: &files::Blobs,
    pending: &mut Pending,
) -> Result<()> {
    for file in &manifest.files {
        let candidate = stage_change(
            &file.namespace,
            file.after_blob.as_ref(),
            file.after_mode.unwrap_or(0o600),
            file.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink,
            blobs,
            pending,
        )?;
        pending.files.push(candidate);
        save_pending(root, manifest, pending)?;
        if file.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink
            && let State::Link {
                text,
                mode: Some(mode),
                ..
            } = &file.before
        {
            let probe = files::stage_link(&file.namespace, text)?;
            pending.rollbacks.push(probe.clone());
            save_pending(root, manifest, pending)?;
            if !matches!(probe.after,State::Link{mode:Some(actual),..} if actual==*mode) {
                return Err(Error::attention(
                    "unsupported_link_mode",
                    "当前环境不能精确还原受管链接权限，未发布目标变更",
                ));
            }
        }
    }
    for ledger in &manifest.ledgers {
        if ledger.before_blob == ledger.after_blob {
            continue;
        }
        let candidate = stage_change(
            &ledger.namespace,
            ledger.after_blob.as_ref(),
            0o600,
            false,
            blobs,
            pending,
        )?;
        pending.ledgers.push(candidate);
        save_pending(root, manifest, pending)?;
    }
    Ok(())
}
fn candidate_for(candidate: &Candidate, pending: &Pending) -> Result<Candidate> {
    let mut candidate = candidate.clone();
    candidate.staged = guard::staged_path(&candidate, pending)?;
    Ok(candidate)
}
fn clear_terminal(
    manifest: &Manifest,
    pending: &Pending,
    blobs: &files::Blobs,
    after: bool,
) -> Result<()> {
    clear_pending_checked(manifest, pending, || {
        guard::verify(manifest, pending)?;
        let terminal = if after {
            all_after(manifest, pending)?
        } else {
            all_before(manifest, pending)?
        };
        if !terminal {
            return Err(invalid());
        }
        sources_terminal(manifest, blobs, after)
    })
}
fn clear_pending_checked(
    manifest: &Manifest,
    pending: &Pending,
    check: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let (witness, raw) = witness(&manifest.pending)?;
    let expected = bytes(pending)?;
    if raw.as_deref() != Some(expected.as_slice()) {
        return Err(invalid());
    }
    crate::platform::fs::write_path(&manifest.pending)?;
    let parent = files::parent(&witness.namespace)?.ok_or_else(invalid)?;
    let leaf = manifest.pending.file_name().ok_or_else(invalid)?;
    files::cas(&witness.namespace, &witness.state)?;
    check()?;
    parent.verify()?;
    files::cas(&witness.namespace, &witness.state)?;
    rustix::fs::unlinkat(parent.file(), leaf, rustix::fs::AtFlags::empty()).map_err(io)?;
    parent.file().sync_all()?;
    Ok(())
}
fn receipts_after(manifest: &Manifest, blobs: &files::Blobs) -> Result<()> {
    let ledger = manifest
        .ledgers
        .iter()
        .find(|l| l.slot == LedgerSlot::Receipts)
        .ok_or_else(invalid)?;
    let raw = ledger
        .after_blob
        .as_ref()
        .map(|b| blobs.get(b))
        .transpose()?;
    crate::capability::capability_receipts::verify_after(raw.as_deref(), &manifest.operation_id)
}
fn sources_terminal(manifest: &Manifest, blobs: &files::Blobs, after: bool) -> Result<()> {
    let ledger = manifest
        .ledgers
        .iter()
        .find(|l| l.slot == LedgerSlot::Managed)
        .ok_or_else(invalid)?;
    let blob = if after {
        &ledger.after_blob
    } else {
        &ledger.before_blob
    };
    let raw = blob.as_ref().map(|b| blobs.get(b)).transpose()?;
    crate::source::source_inventory::verify_metadata(
        &manifest.home,
        raw.as_deref(),
        &manifest.files,
    )
}
fn execute(
    store: &Store,
    root: &Directory,
    blobs: &files::Blobs,
    manifest: &Manifest,
    pending: &mut Pending,
    journal: &mut Journal,
) -> Result<Value> {
    let result: Result<Value> = (|| {
        prepare(root, manifest, blobs, pending)?;
        guard::verify(manifest, pending)?;
        journal.phase = Phase::Running;
        store.save(journal)?;
        for parent in &pending.parents {
            guard::verify(manifest, pending)?;
            files::publish_parent(parent)?;
        }
        for (change, candidate) in manifest.files.iter().zip(&pending.files) {
            guard::verify(manifest, pending)?;
            let candidate = candidate_for(candidate, pending)?;
            if change.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink {
                files::publish_link(
                    &manifest.home,
                    &change.namespace,
                    &change.before,
                    &candidate,
                )?;
            } else {
                files::publish(&change.namespace, &change.before, &candidate)?;
            }
        }
        receipts_after(manifest, blobs)?;
        for change in &manifest.ledgers {
            if let Some(candidate) = pending.ledgers.iter().find(|c| c.target == change.path) {
                guard::verify(manifest, pending)?;
                files::publish(
                    &change.namespace,
                    &change.before,
                    &candidate_for(candidate, pending)?,
                )?;
            }
        }
        cleanup_stages(manifest, pending)?;
        guard::verify(manifest, pending)?;
        receipts_after(manifest, blobs)?;
        sources_terminal(manifest, blobs, true)?;
        clear_terminal(manifest, pending, blobs, true)?;
        journal.phase = Phase::Succeeded;
        journal.message = "原生能力调用策略批次已核验提交；备份已保留".into();
        store.save(journal)?;
        Ok(
            json!({"backend":"rust","status":"succeeded","operation_id":journal.id,"change_id":manifest.change_id,"changed":manifest.files.len(),"persistentWrites":true}),
        )
    })();
    if let Err(error) = &result {
        journal.phase = Phase::Unknown;
        journal.message = format!("{}；保留 pending 与备份，不自动重跑", error.code);
        store.save(journal)?;
    }
    result
}

pub fn sync(ctx: &Context, change: Option<&str>, dry_run: bool, yes: bool) -> Result<Value> {
    let initial = crate::operations::governance::capture(ctx)?;
    initial.ensure_settled()?;
    let assessment = crate::capability::invocation_inventory::assess(ctx, &initial)?;
    let summary = assessment.summary();
    assessment.verify()?;
    initial.verify()?;
    if dry_run {
        return Ok(
            json!({"backend":"rust","mode":"dry-run","plan":summary,"persistentWrites":false}),
        );
    }
    if !assessment.missing.is_empty() {
        return Err(Error::attention(
            "invocation_targets_missing",
            "声明目标不完整，未开始整批写入",
        ));
    }
    if assessment.patches.is_empty() && assessment.record_before == assessment.record_after {
        return Ok(json!({"backend":"rust","status":"same","persistentWrites":false}));
    }
    crate::operations::confirm("同步声明的能力调用策略并保留精确备份", yes)?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    assessment.verify()?;
    let authority = crate::operations::governance::capture(ctx)?;
    authority.ensure_settled()?;
    if authority.paths != initial.paths {
        return Err(invalid());
    }
    let operation = crate::operations::store::operation_id();
    let change = change.unwrap_or(&operation);
    crate::skills::rules::skills::parse_change_id(change).map_err(|_| invalid())?;
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
    execute(&store, &root, &blobs, &manifest, &mut pending, &mut journal)
}

#[cfg(test)]
#[path = "../../tests/unit/capability_tx.rs"]
mod tests;

fn load(
    store: &Store,
    journal: &Journal,
) -> Result<(Directory, files::Blobs, Manifest, Pending, bool)> {
    let Provider::Governance {
        manifest: path,
        manifest_sha256,
        category,
    } = &journal.plan.package.provider
    else {
        return Err(invalid());
    };
    let root = Directory::open(path.parent().ok_or_else(invalid)?)?;
    let metadata = root.file().metadata()?;
    if metadata.uid() != nix::unistd::geteuid().as_raw() || metadata.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    let (raw, metadata) =
        root.regular_config_snapshot(OsStr::new("capability-plan.json"), 64 * 1024 * 1024)?;
    if files::hash(&raw) != *manifest_sha256 || metadata.mode != 0o600 {
        return Err(invalid());
    }
    let manifest: Manifest = decode(&raw)?;
    policy::validate_manifest(&manifest)?;
    if manifest.category != *category
        || manifest.operation_id != journal.id
        || manifest.state != store.path
        || manifest.home != journal.home
        || manifest.settings != journal.settings
        || root.path.file_name() != Some(OsStr::new(&manifest.change_id))
    {
        return Err(invalid());
    }
    let (pending_file, pending_raw) = witness(&manifest.pending)?;
    let present = pending_raw.is_some();
    let pending: Pending = if let Some(raw) = pending_raw {
        if !matches!(pending_file.state, State::File { mode: 0o600, .. }) {
            return Err(invalid());
        }
        decode(&raw)?
    } else if let Some(raw) =
        root.read_regular(OsStr::new("capability-candidates.json"), 64 * 1024 * 1024)?
    {
        decode(&raw)?
    } else {
        empty_pending(&manifest, manifest_sha256)
    };
    policy::validate_pending(&manifest, &pending, manifest_sha256)?;
    let blobs = files::Blobs::open(&root.path.join("blobs"))?;
    Ok((root, blobs, manifest, pending, present))
}
fn all_after(manifest: &Manifest, pending: &Pending) -> Result<bool> {
    if pending.files.len() != manifest.files.len()
        || pending.ledgers.len()
            != manifest
                .ledgers
                .iter()
                .filter(|l| l.before_blob != l.after_blob)
                .count()
    {
        return Ok(false);
    }
    for change in &manifest.files {
        let candidate = pending
            .files
            .iter()
            .find(|c| c.target == change.path)
            .ok_or_else(invalid)?;
        if files::observe(&change.namespace, 64 * 1024 * 1024)?.0 != candidate.after {
            return Ok(false);
        }
    }
    for change in &manifest.ledgers {
        let expected = pending
            .ledgers
            .iter()
            .find(|c| c.target == change.path)
            .map_or(&change.before, |c| &c.after);
        if files::observe(&change.namespace, 64 * 1024 * 1024)?.0 != *expected {
            return Ok(false);
        }
    }
    Ok(true)
}
fn all_before(manifest: &Manifest, pending: &Pending) -> Result<bool> {
    for file in &manifest.files {
        let current = files::observe(&file.namespace, 64 * 1024 * 1024)?.0;
        if !before_state(&file.path, &file.before, &current, pending) {
            return Ok(false);
        }
    }
    for ledger in &manifest.ledgers {
        let current = files::observe(&ledger.namespace, 64 * 1024 * 1024)?.0;
        if !before_state(&ledger.path, &ledger.before, &current, pending) {
            return Ok(false);
        }
    }
    for candidate in guard::candidates(pending) {
        if let Some(path) = guard::staged_path(candidate, pending)?
            && files::observe(&files::namespace(&path)?, 64 * 1024 * 1024)?.0 != State::Missing
        {
            return Ok(false);
        }
    }
    for parent in &pending.parents {
        for path in [&parent.staged, &parent.target] {
            match Directory::open(path) {
                Ok(_) => return Ok(false),
                Err(error) if error.code == "path_missing" => (),
                Err(error) => return Err(error),
            }
        }
    }
    Ok(true)
}
fn receipts_before(manifest: &Manifest, blobs: &files::Blobs) -> Result<()> {
    let ledger = manifest
        .ledgers
        .iter()
        .find(|l| l.slot == LedgerSlot::Receipts)
        .ok_or_else(invalid)?;
    let raw = ledger
        .before_blob
        .as_ref()
        .map(|b| blobs.get(b))
        .transpose()?;
    let paths: Vec<_> = manifest.files.iter().map(|f| f.path.as_path()).collect();
    crate::capability::capability_receipts::verify_before(raw.as_deref(), &paths)
}

fn same_before_content(before: &State, current: &State) -> bool {
    match (before, current) {
        (State::Missing, State::Missing) => true,
        (
            State::File {
                sha256: a, mode: x, ..
            },
            State::File {
                sha256: b, mode: y, ..
            },
        ) => a == b && x == y,
        (
            State::Link {
                text: a,
                mode: Some(x),
                ..
            },
            State::Link {
                text: b,
                mode: Some(y),
                ..
            },
        ) => a == b && x == y,
        _ => false,
    }
}
fn before_state(path: &Path, before: &State, current: &State, pending: &Pending) -> bool {
    current == before
        || (same_before_content(before, current)
            && pending
                .rollbacks
                .iter()
                .any(|c| c.target == path && c.after == *current))
}
fn rollback_one(
    root: &Directory,
    manifest: &Manifest,
    pending: &mut Pending,
    blobs: &files::Blobs,
    namespace: &Namespace,
    before: &State,
    blob: Option<&Blob>,
) -> Result<()> {
    guard::verify(manifest, pending)?;
    let path = files::target_path(namespace);
    let current = files::observe(namespace, 64 * 1024 * 1024)?.0;
    if before_state(&path, before, &current, pending) {
        return Ok(());
    }
    let link = manifest.files.iter().any(|f| {
        f.path == path && f.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink
    });
    let candidate = if link {
        link_rollback_candidate(root, manifest, pending, namespace, before)?
    } else {
        let raw = blob.map(|b| blobs.get(b)).transpose()?;
        let mode = match before {
            State::File { mode, .. } => *mode,
            State::Missing => 0o600,
            State::Link { .. } => return Err(invalid()),
        };
        let candidate = files::stage(namespace, raw.as_deref(), mode)?;
        pending.rollbacks.push(candidate.clone());
        save_pending(root, manifest, pending)?;
        candidate
    };
    guard::verify(manifest, pending)?;
    if link {
        files::publish_link(&manifest.home, namespace, &current, &candidate)
    } else {
        files::publish(namespace, &current, &candidate)
    }
}
fn link_rollback_candidate(
    root: &Directory,
    manifest: &Manifest,
    pending: &mut Pending,
    namespace: &Namespace,
    before: &State,
) -> Result<Candidate> {
    let path = files::target_path(namespace);
    if let State::Link {
        text,
        mode: Some(mode),
        ..
    } = before
    {
        for candidate in pending
            .rollbacks
            .iter()
            .rev()
            .filter(|c| c.target == path && same_before_content(before, &c.after))
        {
            let candidate = candidate_for(candidate, pending)?;
            if let Some(staged) = &candidate.staged
                && files::observe(&files::namespace(staged)?, 4096)?.0 == candidate.after
            {
                return Ok(candidate);
            }
        }
        let candidate = files::stage_link(namespace, text)?;
        pending.rollbacks.push(candidate.clone());
        save_pending(root, manifest, pending)?;
        if !matches!(candidate.after,State::Link{mode:Some(actual),..} if actual==*mode) {
            return Err(Error::attention(
                "unsupported_link_mode",
                "无法精确恢复原链接权限；保留现场",
            ));
        }
        return Ok(candidate);
    }
    if *before != State::Missing {
        return Err(invalid());
    }
    let candidate = Candidate {
        target: path,
        staged: None,
        after: State::Missing,
    };
    pending.rollbacks.push(candidate.clone());
    save_pending(root, manifest, pending)?;
    Ok(candidate)
}

fn cleanup_stages(_manifest: &Manifest, pending: &Pending) -> Result<()> {
    for candidate in guard::candidates(pending) {
        let Some(path) = guard::staged_path(candidate, pending)? else {
            continue;
        };
        let namespace = files::namespace(&path)?;
        let current = files::observe(&namespace, 64 * 1024 * 1024)?.0;
        if current == State::Missing {
            continue;
        }
        if current != candidate.after {
            return Err(invalid());
        }
        if matches!(current, State::Link { .. }) {
            files::remove_link_stage(&namespace, &current)?;
        } else {
            files::publish(
                &namespace,
                &current,
                &Candidate {
                    target: path,
                    staged: None,
                    after: State::Missing,
                },
            )?;
        }
    }
    Ok(())
}
fn cleanup(root: &Directory, manifest: &Manifest, pending: &mut Pending) -> Result<()> {
    cleanup_stages(manifest, pending)?;
    for parent in pending.parents.clone().into_iter().rev() {
        let path = if files::parent_published(&parent)? {
            &parent.target
        } else {
            &parent.staged
        };
        let directory = match Directory::open(path) {
            Ok(directory) => directory,
            Err(error)
                if error.code == "path_missing"
                    && pending.retired_parents.contains(&parent.target) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        if directory.identity() != &parent.binding.identity
            || !guard::listing(&directory)?.is_empty()
        {
            return Err(invalid());
        }
        if !pending.retired_parents.contains(&parent.target) {
            pending.retired_parents.push(parent.target.clone());
        }
        save_pending(root, manifest, pending)?;
        directory.verify()?;
        if !guard::listing(&directory)?.is_empty() {
            return Err(invalid());
        }
        let owner = Directory::open(path.parent().ok_or_else(invalid)?)?;
        if owner
            .metadata(path.file_name().ok_or_else(invalid)?)?
            .is_none_or(|m| m.identity != parent.binding.identity)
        {
            return Err(invalid());
        }
        rustix::fs::unlinkat(
            owner.file(),
            path.file_name().ok_or_else(invalid)?,
            rustix::fs::AtFlags::REMOVEDIR,
        )
        .map_err(io)?;
        owner.file().sync_all()?;
    }
    Ok(())
}
fn rollback(
    root: &Directory,
    manifest: &Manifest,
    pending: &mut Pending,
    blobs: &files::Blobs,
) -> Result<()> {
    for ledger in manifest.ledgers.iter().rev() {
        rollback_one(
            root,
            manifest,
            pending,
            blobs,
            &ledger.namespace,
            &ledger.before,
            ledger.before_blob.as_ref(),
        )?;
    }
    for file in manifest.files.iter().rev() {
        rollback_one(
            root,
            manifest,
            pending,
            blobs,
            &file.namespace,
            &file.before,
            file.before_blob.as_ref(),
        )?;
    }
    cleanup(root, manifest, pending)?;
    guard::verify(manifest, pending)?;
    receipts_before(manifest, blobs)?;
    sources_terminal(manifest, blobs, false)?;
    clear_terminal(manifest, pending, blobs, false)
}

fn recovery_scope_changed() -> Error {
    Error::attention(
        "capability_recovery_scope_changed",
        "当前 HOME、settings 或已使用的原生来源位置已改变；保持批次和备份",
    )
}
pub fn recover_journal(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(json!(journal));
    }
    if journal.home != ctx.home || journal.settings != ctx.settings || journal.state != ctx.state {
        return Err(recovery_scope_changed());
    }
    let (root, blobs, manifest, mut pending, present) = load(store, journal)?;
    let frozen =
        crate::capability::rules::delivery::native_lock_path(&journal.home, &journal.scope);
    if manifest.inputs.iter().any(|i| i.path == frozen)
        && crate::capability::rules::delivery::native_lock_path(&ctx.home, &ctx.scope) != frozen
    {
        return Err(recovery_scope_changed());
    }
    let authority = crate::operations::governance::capture(ctx)?;
    if manifest.pending != authority.paths.pending
        || manifest.ledgers.iter().any(|l| match l.slot {
            LedgerSlot::Managed => l.path != authority.paths.managed,
            LedgerSlot::Overlays => l.path != authority.paths.overlays,
            LedgerSlot::Receipts => l.path != ctx.state.join("receipts.json"),
            LedgerSlot::Native => true,
        })
    {
        return Err(invalid());
    }
    if let Some(layout) = &manifest.layout
        && (layout.config != ctx.config || layout.data != ctx.data || layout.state != ctx.state)
    {
        return Err(invalid());
    }
    if manifest.category == GovernanceCategory::Policy {
        if manifest.schema_version == policy::POLICY_MANIFEST_VERSION
            && policy::recipe_book_witness(&manifest)?.path
                != crate::skills::native_skills::book_path(ctx)
        {
            return Err(recovery_scope_changed());
        }
        crate::capability::invocation_inventory::protect_fixed_recipe_files(
            ctx,
            manifest.files.iter().map(|file| file.path.as_path()),
        )?;
    }
    if let Some(configuration) = &manifest.configuration {
        if ctx.chezmoi != configuration.mapping_inputs || ctx.isolated != configuration.isolated {
            return Err(recovery_scope_changed());
        }
        if let Some(root) = &configuration.codex_root
            && ctx
                .harness
                .codex
                .clone()
                .unwrap_or_else(|| ctx.home.join(".codex"))
                != *root
        {
            return Err(recovery_scope_changed());
        }
    }
    if resolve_failed {
        return Err(Error::attention(
            "capability_pending_cannot_abandon",
            "必须先验证并恢复批次，不跳过整批一致性核验",
        ));
    }
    crate::operations::confirm("核验并收口能力包批次；不会重新执行安装器", yes)?;
    guard::verify(&manifest, &pending)?;
    if all_after(&manifest, &pending)? {
        cleanup_stages(&manifest, &pending)?;
        receipts_after(&manifest, &blobs)?;
        sources_terminal(&manifest, &blobs, true)?;
        if present {
            clear_terminal(&manifest, &pending, &blobs, true)?;
        }
        journal.phase = Phase::Succeeded;
        journal.message = "全部目标和账本已为 after，向前完成；未重跑外部动作".into();
    } else if present {
        rollback(&root, &manifest, &mut pending, &blobs)?;
        journal.phase = Phase::Failed;
        journal.message = "已从精确 before 或自身候选回退全部文件与账本；未重跑外部动作".into();
    } else if all_before(&manifest, &pending)? {
        receipts_before(&manifest, &blobs)?;
        sources_terminal(&manifest, &blobs, false)?;
        journal.phase = Phase::Failed;
        journal.message = "全部目标与账本已为 before，暂存已清理；恢复回退后的日志收口".into();
    } else {
        return Err(invalid());
    }
    store.save(journal)?;
    Ok(
        json!({"backend":"rust","status":journal.phase,"phase":journal.phase,"operation_id":journal.id,"change_id":manifest.change_id,"persistentWrites":true}),
    )
}

pub fn recover_policy(
    ctx: &Context,
    change: Option<&str>,
    dry_run: bool,
    yes: bool,
) -> Result<Value> {
    let raw = crate::operations::governance::inspect(ctx, |authority| {
        Ok(witness(&authority.paths.pending)?.1)
    })?;
    let Some(raw) = raw else {
        return Ok(
            json!({"backend":"rust","status":"not-needed","mode":"no-op","persistentWrites":false}),
        );
    };
    let value = crate::policy::json::parse(&raw).map_err(|_| invalid())?;
    if !matches!(
        value.get("schema_version").and_then(Value::as_u64),
        Some(2..=5)
    ) {
        return Err(Error::attention(
            "capability_recovery_required",
            "旧版或未知 pending 保留审查，不调用旧安装器",
        ));
    }
    let operation_id = if value.get("schema_version").and_then(Value::as_u64) == Some(5) {
        decode::<crate::source::rules::source_tx::Pending>(&raw)?.operation_id
    } else {
        decode::<Pending>(&raw)?.operation_id
    };
    if !crate::policy::operation::state::valid_id(&operation_id) {
        return Err(invalid());
    }
    if let Some(change) = change {
        let path = ctx
            .state
            .join("operations")
            .join(format!("{operation_id}.json"));
        let journal: Journal = crate::platform::fs::regular_json(&path)?;
        crate::operations::store::validate_journal(&journal)?;
        let Provider::Governance { manifest, .. } = journal.plan.package.provider else {
            return Err(invalid());
        };
        if manifest.parent().and_then(Path::file_name) != Some(OsStr::new(change)) {
            return Err(Error::attention(
                "capability_change_id_mismatch",
                "未决事务的 change-id 不匹配",
            ));
        }
    }
    crate::operations::recover(ctx, &operation_id, dry_run, false, yes)
}

pub fn write(
    ctx: &Context,
    command: &crate::cli::Command,
    dry_run: bool,
    yes: bool,
) -> Result<Option<Value>> {
    use crate::cli::{Command, Policy, SkillCmd};
    let Command::Skill {
        project: _,
        details: _,
        only: _,
        command: Some(command),
    } = command
    else {
        return Ok(None);
    };
    if let SkillCmd::Policy {
        command:
            Policy::Sync {
                change_id: Some(id),
            }
            | Policy::Recover {
                change_id: Some(id),
            },
    } = command
    {
        crate::skills::rules::skills::parse_change_id(id)
            .map_err(|_| Error::new("invalid_change_id", "change-id 无效"))?;
    }
    let result = match command {
        SkillCmd::Sync(selection) => sync_links(ctx, selection, dry_run, yes)?,
        SkillCmd::Policy {
            command: Policy::Sync { change_id },
        } => sync(ctx, change_id.as_deref(), dry_run, yes)?,
        SkillCmd::Policy {
            command: Policy::Recover { change_id },
        } => recover_policy(ctx, change_id.as_deref(), dry_run, yes)?,
        _ => return Ok(None),
    };
    Ok(Some(result))
}

fn links_manifest(
    ctx: &Context,
    authority: &Governance,
    assessment: &crate::capability::link_inventory::Assessment,
    operation: &str,
    change: &str,
    blobs: &files::Blobs,
) -> Result<Manifest> {
    let (inputs, mut directories) = assessment.reader.witnesses();
    let mut changes = vec![];
    for patch in &assessment.patches {
        let (current, raw) = witness(&patch.path)?;
        if current.state != patch.before || raw != patch.before_bytes {
            return Err(invalid());
        }
        changes.push(FileChange {
            path: patch.path.clone(),
            kind: crate::capability::rules::delivery::ChangeKind::ManagedLink,
            namespace: patch.namespace.clone(),
            before: patch.before.clone(),
            before_blob: saved_blob(blobs, raw.as_deref())?,
            after_blob: saved_blob(blobs, patch.after.as_ref().map(|t| t.as_str().as_bytes()))?,
            after_mode: None,
        });
    }
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
    let mut ledgers = vec![];
    for (slot, path) in [
        (LedgerSlot::Managed, authority.paths.managed.clone()),
        (LedgerSlot::Overlays, authority.paths.overlays.clone()),
        (LedgerSlot::Receipts, ctx.state.join("receipts.json")),
    ] {
        crate::platform::fs::write_path(&path)?;
        let (before, raw) = witness(&path)?;
        ledgers.push(ledger(blobs, slot, before, raw.as_deref(), raw.as_deref())?);
    }
    let manifest = Manifest {
        schema_version: 3,
        layout: Some(crate::policy::governance::Layout {
            config: ctx.config.clone(),
            data: ctx.data.clone(),
            state: ctx.state.clone(),
            cache: ctx.cache.clone(),
        }),
        links: Some(assessment.authority.clone()),
        configuration: None,
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
        ledgers,
    };
    policy::validate_manifest(&manifest)?;
    Ok(manifest)
}
#[path = "capability_sync.rs"]
mod delivery_sync;
pub fn sync_links(
    ctx: &Context,
    selection: &crate::cli::SkillSelection,
    dry_run: bool,
    yes: bool,
) -> Result<Value> {
    delivery_sync::sync(ctx, selection, dry_run, yes)
}
