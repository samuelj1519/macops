use crate::{
    capability::{
        capability_store as files,
        rules::capability_tx::{Namespace, State, Witness},
    },
    context::Context,
    domain::{
        Action, GovernanceCategory, Journal, Kind, Observation, Package, Phase, Plan, Provider,
        QuerySpec, Receipt, Receipts, fingerprint,
    },
    error::{Error, Result},
    operations::{governance::Governance, store::Store},
    platform::fs::Directory,
    policy::governance::Layout,
    source::{
        rules::source_tx::{
            self as policy, Action as SourceAction, Ledger, Manifest, ObservedRoot, Pending,
            Progress, Slot,
        },
        source_inventory::Adoption,
    },
};
use serde_json::{Value, json};
use std::{ffi::OsStr, os::unix::fs::MetadataExt, path::Path};
fn invalid() -> Error {
    policy::invalid()
}
fn io(error: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}
fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut raw = serde_json::to_vec_pretty(value)?;
    raw.push(b'\n');
    Ok(raw)
}
fn decode<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T> {
    let value = crate::policy::json_fields::parse_governance(raw)?;
    serde_json::from_value::<crate::policy::json::Object<T>>(value)
        .map(|v| v.0)
        .map_err(|_| invalid())
}
fn owned(namespace: &Namespace, state: &State) -> Result<()> {
    if let State::File { identity, .. } = state {
        let parent = files::parent(namespace)?.ok_or_else(invalid)?;
        let name = namespace.remaining.last().ok_or_else(invalid)?;
        let stat = rustix::fs::statat(
            parent.file(),
            name.as_str(),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io)?;
        if stat.st_uid != nix::unistd::geteuid().as_raw()
            || u64::try_from(stat.st_dev).map_err(|_| invalid())? != identity.device
            || stat.st_ino != identity.inode
        {
            return Err(invalid());
        }
        parent.verify()?;
    }
    Ok(())
}
fn witness(path: &Path) -> Result<(Witness, Option<Vec<u8>>)> {
    let namespace = files::namespace(path)?;
    let (state, raw) = files::observe(&namespace, 16 * 1024 * 1024)?;
    owned(&namespace, &state)?;
    Ok((
        Witness {
            path: path.into(),
            namespace,
            state,
        },
        raw,
    ))
}
fn private_child(parent: &Directory, name: &str, exclusive: bool) -> Result<Directory> {
    match rustix::fs::mkdirat(parent.file(), name, rustix::fs::Mode::RWXU) {
        Ok(()) => parent.file().sync_all()?,
        Err(rustix::io::Errno::EXIST) if !exclusive => (),
        Err(e) => return Err(io(e)),
    }
    let root = parent.child(OsStr::new(name))?;
    let meta = root.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
        return Err(invalid());
    }
    Ok(root)
}
fn backup(authority: &Governance, change: &str) -> Result<Directory> {
    let path = &authority.paths.backups;
    crate::platform::fs::write_path(path)?;
    let parent = Directory::open(path.parent().ok_or_else(invalid)?)?;
    let backups = private_child(
        &parent,
        path.file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(invalid)?,
        false,
    )?;
    private_child(&backups, change, true)
}
fn receipts(before: Option<&[u8]>, manifest: &Manifest) -> Result<Option<Vec<u8>>> {
    let natives: Vec<_> = match &manifest.action {
        SourceAction::Adopt { root } => root.native.iter().collect(),
        SourceAction::Upgrade { roots, .. } => roots
            .iter()
            .filter_map(|r| r.native_after.as_ref())
            .collect(),
    };
    if natives.is_empty() {
        return Ok(before.map(<[u8]>::to_vec));
    }
    let mut book: Receipts = before.map(decode).transpose()?.unwrap_or(Receipts {
        schema_version: 1,
        ..Receipts::default()
    });
    crate::policy::operation::state::receipt_schema(&book)?;
    for native in natives {
        if let SourceAction::Upgrade { roots, .. } = &manifest.action {
            let previous = book
                .installations
                .get(&native.package.id)
                .ok_or_else(invalid)?;
            let before = roots
                .iter()
                .find_map(|root| {
                    root.native_before
                        .as_ref()
                        .filter(|old| old.package.id == native.package.id)
                })
                .ok_or_else(invalid)?;
            if previous.scope.as_ref() != Some(&manifest.scope)
                || previous.query.as_ref()
                    != Some(&QuerySpec::LocalCapability(before.query.clone()))
                || previous.observation != before.expected
                || fingerprint(&previous.package)? != fingerprint(&before.package)?
            {
                return Err(invalid());
            }
        }
        if let Some(previous) = book.installations.get(&native.package.id)
            && previous.scope.as_ref() != Some(&manifest.scope)
        {
            return Err(Error::attention(
                "capability_scope_conflict",
                "既有能力包基线属于其他作用域",
            ));
        }
        book.installations.insert(
            native.package.id.clone(),
            Receipt {
                scope: Some(manifest.scope.clone()),
                query: Some(QuerySpec::LocalCapability(native.query.clone())),
                package: native.package.clone(),
                observation: native.expected.clone(),
                operation_id: manifest.operation_id.clone(),
                updated_at: manifest.created_at,
            },
        );
    }
    Ok(Some(encode(&book)?))
}
fn freeze(
    ctx: &Context,
    authority: &Governance,
    assessment: &Adoption,
    operation: &str,
    change: &str,
    blobs: &files::Blobs,
) -> Result<Manifest> {
    let (mut inputs, _) = assessment.reader.witnesses();
    let mut manifest = Manifest {
        schema_version: 5,
        operation_id: operation.into(),
        change_id: change.into(),
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
        created_at: crate::operations::store::now(),
        action: SourceAction::Adopt {
            root: Box::new(ObservedRoot {
                baseline: assessment.baseline.clone(),
                tree: assessment.tree.clone(),
                native: assessment.native.clone(),
            }),
        },
        inputs: vec![],
        ledgers: vec![],
    };
    let paths = policy::paths(&manifest, &authority.paths);
    for (path, slot) in paths.iter().zip(policy::SLOTS) {
        crate::platform::fs::write_path(path)?;
        let (witness, before) = witness(path)?;
        if slot == Slot::Managed && before != assessment.managed_before {
            return Err(invalid());
        }
        if inputs
            .iter()
            .any(|i| i.path == *path && i.state != witness.state)
        {
            return Err(invalid());
        }
        let after = match slot {
            Slot::Managed => Some(assessment.managed_after.clone()),
            Slot::Receipts => receipts(before.as_deref(), &manifest)?,
            _ => before.clone(),
        };
        let after_mode = after
            .as_ref()
            .map(|_| policy::mode(&witness.state).unwrap_or(0o600));
        manifest.ledgers.push(Ledger {
            slot,
            witness,
            before: before.as_deref().map(|b| blobs.put(b)).transpose()?,
            after: after.as_deref().map(|b| blobs.put(b)).transpose()?,
            after_mode,
            pair: None,
        });
    }
    inputs.retain(|i| !paths.contains(&i.path));
    manifest.inputs = inputs;
    policy::validate(&manifest, &authority.paths, nix::unistd::geteuid().as_raw())?;
    Ok(manifest)
}
fn journal(ctx: &Context, manifest: &Manifest, path: &Path, digest: &str) -> Result<Journal> {
    let package = Package {
        id: "governance:source".into(),
        name: "能力来源采纳".into(),
        kind: Kind::Skill,
        official_url: String::new(),
        provider: Provider::Governance {
            category: GovernanceCategory::Source,
            manifest: path.into(),
            manifest_sha256: digest.into(),
        },
        paths: vec![],
        exact_version_required: false,
    };
    let plan = Plan {
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Maintain,
        before: Observation::Governance {
            fingerprint: fingerprint(&manifest.action)?,
        },
        target: Some(digest.into()),
        command: None,
        brew: None,
        npm_binding: None,
        uv_owner: None,
        query: None,
        timeout_secs: 60,
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    crate::operations::new_journal_with_id(ctx, plan, manifest.operation_id.clone())
}
fn verify_root(manifest: &Manifest) -> Result<()> {
    match &manifest.action {
        SourceAction::Adopt { root } => {
            if crate::source::source_tree::observe(&root.baseline.slot)?.as_ref()
                != Some(&root.tree)
            {
                return Err(invalid());
            }
            verify_native(root.native.as_ref())
        }
        SourceAction::Upgrade { roots, .. } => {
            for root in roots {
                if crate::source::source_tree::position(&root.change)?
                    == crate::policy::source_tree::Position::Unknown
                {
                    return Err(invalid());
                }
            }
            Ok(())
        }
    }
}
fn verify_native(native: Option<&policy::NativeSpec>) -> Result<()> {
    if let Some(native) = native
        && crate::capability::capability_local::observe_default(&native.package.id, &native.query)?
            != native.expected
    {
        return Err(invalid());
    }
    Ok(())
}
fn terminal_roots(manifest: &Manifest, after: bool) -> Result<bool> {
    verify_root(manifest)?;
    if let SourceAction::Upgrade { roots, .. } = &manifest.action {
        for root in roots {
            let expected = if after {
                crate::policy::source_tree::Position::OwnAfter
            } else {
                crate::policy::source_tree::Position::Before
            };
            if crate::source::source_tree::position(&root.change)? != expected {
                return Ok(false);
            }
            verify_native(if after {
                root.native_after.as_ref()
            } else {
                root.native_before.as_ref()
            })?;
        }
    }
    Ok(true)
}
fn ledger_position(
    ledger: &Ledger,
    progress: &Progress,
) -> Result<crate::policy::source_tree::Position> {
    use crate::policy::source_tree::Position;
    if let Some(pair) = &ledger.pair {
        return crate::source::source_files::position(pair);
    }
    let current = files::observe(&ledger.witness.namespace, 16 * 1024 * 1024)?.0;
    owned(&ledger.witness.namespace, &current)?;
    Ok(if policy::is_before(ledger, progress, &current) {
        Position::Before
    } else if policy::is_after(ledger, progress, &current) {
        Position::OwnAfter
    } else {
        Position::Unknown
    })
}
fn verify(manifest: &Manifest, pending: &Pending) -> Result<()> {
    verify_root(manifest)?;
    for input in &manifest.inputs {
        files::cas(&input.namespace, &input.state)?;
    }
    for (ledger, progress) in manifest.ledgers.iter().zip(&pending.ledgers) {
        if ledger_position(ledger, progress)? == crate::policy::source_tree::Position::Unknown {
            return Err(invalid());
        }
    }
    Ok(())
}
struct Batch {
    root: Directory,
    blobs: files::Blobs,
    manifest: Manifest,
    pending: Pending,
    marker: Option<Vec<u8>>,
}
impl Batch {
    fn control(&self, store: &Store) -> Result<()> {
        store.verify_namespace()?;
        for root in [&self.root, &self.blobs.directory] {
            root.verify()?;
            let meta = root.file().metadata()?;
            if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn guard(&self, store: &Store) -> Result<()> {
        self.control(store)?;
        verify(&self.manifest, &self.pending)?;
        if witness(&self.manifest.pending)?.1 != self.marker {
            return Err(invalid());
        }
        Ok(())
    }
    fn save(&mut self, store: &Store) -> Result<()> {
        self.control(store)?;
        let raw = encode(&self.pending)?;
        let (before, current) = witness(&self.manifest.pending)?;
        if current != self.marker {
            return Err(invalid());
        }
        Store::atomic_bytes(self.root.file(), "source-candidates.json", &raw, false)?;
        let candidate = files::stage(&before.namespace, Some(&raw), 0o600)?;
        files::publish_checked(&before.namespace, &before.state, &candidate, || {
            self.control(store)
        })?;
        self.marker = Some(raw);
        Ok(())
    }
    fn cleanup_files(&self, store: &Store) -> Result<()> {
        for progress in &self.pending.ledgers {
            for candidate in [&progress.forward, &progress.restored]
                .into_iter()
                .flatten()
            {
                let Some(path) = &candidate.staged else {
                    continue;
                };
                let namespace = files::namespace(path)?;
                let (current, _) = files::observe(&namespace, 16 * 1024 * 1024)?;
                if current == State::Missing {
                    continue;
                }
                if current != candidate.after || !matches!(current, State::File { .. }) {
                    return Err(invalid());
                }
                owned(&namespace, &current)?;
                crate::platform::fs::write_path(path)?;
                let parent = files::parent(&namespace)?.ok_or_else(invalid)?;
                let name = namespace.remaining.last().ok_or_else(invalid)?;
                files::cas(&namespace, &current)?;
                self.guard(store)?;
                rustix::fs::unlinkat(parent.file(), name.as_str(), rustix::fs::AtFlags::empty())
                    .map_err(io)?;
                parent.file().sync_all()?;
                parent.verify()?;
            }
        }
        self.guard(store)
    }
    fn clear(&mut self, store: &Store, after: bool) -> Result<()> {
        self.guard(store)?;
        if !self.matches(after)? {
            return Err(invalid());
        }
        self.cleanup_files(store)?;
        let (before, current) = witness(&self.manifest.pending)?;
        if current != self.marker || current.is_none() {
            return Err(invalid());
        }
        let parent = files::parent(&before.namespace)?.ok_or_else(invalid)?;
        let name = before.namespace.remaining.last().ok_or_else(invalid)?;
        files::cas(&before.namespace, &before.state)?;
        self.guard(store)?;
        if !self.matches(after)? {
            return Err(invalid());
        }
        parent.verify()?;
        files::cas(&before.namespace, &before.state)?;
        self.control(store)?;
        rustix::fs::unlinkat(parent.file(), name.as_str(), rustix::fs::AtFlags::empty())
            .map_err(io)?;
        parent.file().sync_all()?;
        self.marker = None;
        Ok(())
    }
    fn matches(&self, after: bool) -> Result<bool> {
        use crate::policy::source_tree::Position;
        for (ledger, progress) in self.manifest.ledgers.iter().zip(&self.pending.ledgers) {
            let position = ledger_position(ledger, progress)?;
            let expected = if after && ledger.before != ledger.after {
                Position::OwnAfter
            } else {
                Position::Before
            };
            if position != expected {
                return Ok(false);
            }
        }
        terminal_roots(&self.manifest, after)
    }
    fn prepare(&mut self, store: &Store) -> Result<()> {
        if matches!(self.manifest.action, SourceAction::Upgrade { .. }) {
            return self.guard(store);
        }
        for index in 0..self.manifest.ledgers.len() {
            let ledger = &self.manifest.ledgers[index];
            if ledger.before == ledger.after {
                continue;
            }
            let raw = ledger
                .after
                .as_ref()
                .map(|b| self.blobs.get(b))
                .transpose()?;
            let candidate = files::stage(
                &ledger.witness.namespace,
                raw.as_deref(),
                ledger.after_mode.unwrap_or(0o600),
            )?;
            self.pending.ledgers[index].forward = Some(candidate);
            self.save(store)?;
        }
        Ok(())
    }
    fn publish(
        &mut self,
        store: &Store,
    ) -> Result<Vec<crate::source::source_tree::ExchangeOutcome>> {
        self.publish_with(store, |_| Ok(()))
    }
    fn publish_with(
        &self,
        store: &Store,
        mut before: impl FnMut(Slot) -> Result<()>,
    ) -> Result<Vec<crate::source::source_tree::ExchangeOutcome>> {
        let mut outcomes = vec![];
        if let SourceAction::Upgrade { roots, .. } = &self.manifest.action {
            for root in roots {
                self.guard(store)?;
                outcomes.push(crate::source::source_tree::publish_checked(
                    &root.change,
                    || self.guard(store),
                )?);
                self.guard(store)?;
            }
        }
        for (ledger, progress) in self.manifest.ledgers.iter().zip(&self.pending.ledgers) {
            if ledger.before == ledger.after {
                continue;
            }
            self.guard(store)?;
            before(ledger.slot)?;
            if let Some(pair) = &ledger.pair {
                outcomes.push(crate::source::source_files::publish_checked(pair, || {
                    self.guard(store)
                })?);
            } else {
                files::publish_checked(
                    &ledger.witness.namespace,
                    &ledger.witness.state,
                    progress.forward.as_ref().ok_or_else(invalid)?,
                    || self.guard(store),
                )?;
            }
            self.guard(store)?;
        }
        Ok(outcomes)
    }
    fn rollback(&mut self, store: &Store) -> Result<()> {
        self.rollback_with(store, |_| Ok(()))
    }
    fn rollback_with(
        &mut self,
        store: &Store,
        mut before: impl FnMut(Slot) -> Result<()>,
    ) -> Result<()> {
        for index in (0..self.manifest.ledgers.len()).rev() {
            self.guard(store)?;
            let ledger = &self.manifest.ledgers[index];
            if let Some(pair) = &ledger.pair {
                if crate::source::source_files::position(pair)?
                    == crate::policy::source_tree::Position::OwnAfter
                {
                    before(ledger.slot)?;
                    crate::source::source_files::restore_checked(pair, || self.guard(store))?;
                }
                self.guard(store)?;
                continue;
            }
            let current = files::observe(&ledger.witness.namespace, 16 * 1024 * 1024)?.0;
            if policy::is_before(ledger, &self.pending.ledgers[index], &current) {
                continue;
            }
            let candidate = if let Some(candidate) = &self.pending.ledgers[index].restored {
                candidate.clone()
            } else {
                let raw = ledger
                    .before
                    .as_ref()
                    .map(|b| self.blobs.get(b))
                    .transpose()?;
                let candidate = files::stage(
                    &ledger.witness.namespace,
                    raw.as_deref(),
                    policy::mode(&ledger.witness.state).unwrap_or(0o600),
                )?;
                self.pending.ledgers[index].restored = Some(candidate.clone());
                self.save(store)?;
                candidate
            };
            self.guard(store)?;
            before(self.manifest.ledgers[index].slot)?;
            files::publish_checked(
                &self.manifest.ledgers[index].witness.namespace,
                &current,
                &candidate,
                || self.guard(store),
            )?;
        }
        if let SourceAction::Upgrade { roots, .. } = &self.manifest.action {
            for root in roots.iter().rev() {
                self.guard(store)?;
                crate::source::source_tree::restore_checked(&root.change, || self.guard(store))?;
            }
        }
        if !self.matches(false)? {
            return Err(invalid());
        }
        self.guard(store)?;
        Ok(())
    }
}
fn result(journal: &Journal, manifest: &Manifest) -> Value {
    let (action, members) = match &manifest.action {
        SourceAction::Adopt { root } => ("adopt", vec![&root.baseline.source.member]),
        SourceAction::Upgrade { roots, .. } => (
            "upgrade",
            roots.iter().map(|r| &r.baseline.source.member).collect(),
        ),
    };
    let package = match &manifest.action {
        SourceAction::Adopt { root } => root
            .native
            .as_ref()
            .map_or(&journal.plan.package, |spec| &spec.package),
        SourceAction::Upgrade { .. } => &journal.plan.package,
    };
    json!({"backend":"rust","status":journal.phase,"phase":journal.phase,"id":journal.id,"operation_id":journal.id,"change_id":manifest.change_id,"member":members.first(),"members":members,"plan":{"action":action,"package":package},"persistentWrites":true})
}
fn execute(store: &Store, batch: &mut Batch, journal: &mut Journal) -> Result<Value> {
    let execution: Result<()> = (|| {
        batch.prepare(store)?;
        verify(&batch.manifest, &batch.pending)?;
        journal.phase = Phase::Running;
        store.save(journal)?;
        let exchanges = batch.publish(store)?;
        if !batch.matches(true)? {
            return Err(invalid());
        }
        verify(&batch.manifest, &batch.pending)?;
        batch.clear(store, true)?;
        journal.phase = Phase::Succeeded;
        journal.message = format!(
            "完整来源基线与五项账本已共同提交；交换 {} 个来源根，{} 个 syscall 错误已由实际位置核验",
            exchanges.iter().filter(|outcome| outcome.exchanged).count(),
            exchanges
                .iter()
                .filter(|outcome| outcome.syscall_error.is_some())
                .count()
        );
        store.save(journal)?;
        Ok(())
    })();
    if let Err(error) = execution {
        journal.phase = Phase::Unknown;
        journal.message.clone_from(&error.message);
        store.save(journal)?;
        return Err(Error::attention(
            "source_recovery_required",
            "来源更新的结果还没确认。运行 mo recover 查看记录。",
        ));
    }
    Ok(result(journal, &batch.manifest))
}
fn initialize(
    ctx: &Context,
    authority: &Governance,
    assessment: &Adoption,
    store: &Store,
) -> Result<(Batch, Journal)> {
    store.verify_namespace()?;
    let operation = crate::operations::store::operation_id();
    let root = backup(authority, &operation)?;
    let blobs = files::Blobs::create(&root)?;
    let manifest = freeze(ctx, authority, assessment, &operation, &operation, &blobs)?;
    persist(ctx, store, root, blobs, manifest)
}
fn persist(
    ctx: &Context,
    store: &Store,
    root: Directory,
    blobs: files::Blobs,
    manifest: Manifest,
) -> Result<(Batch, Journal)> {
    let operation = manifest.operation_id.clone();
    store.verify_namespace()?;
    let raw = encode(&manifest)?;
    crate::policy::json_fields::parse_governance(&raw)?;
    let digest = files::hash(&raw);
    Store::atomic_bytes(root.file(), "capability-plan.json", &raw, true)?;
    let journal = journal(
        ctx,
        &manifest,
        &root.path.join("capability-plan.json"),
        &digest,
    )?;
    let pending = Pending {
        schema_version: 5,
        operation_id: operation,
        manifest_sha256: digest,
        ledgers: policy::SLOTS
            .iter()
            .map(|_| Progress {
                forward: None,
                restored: None,
            })
            .collect(),
    };
    let batch = Batch {
        root,
        blobs,
        manifest,
        pending,
        marker: None,
    };
    Store::atomic_new(batch.root.file(), "source-candidates.json", &batch.pending)?;
    verify(&batch.manifest, &batch.pending)?;
    store.verify_namespace()?;
    store.save(&journal)?;
    Ok((batch, journal))
}
pub fn adopt(
    ctx: &Context,
    target: crate::cli::AdoptTarget<'_>,
    dry_run: bool,
    yes: bool,
) -> Result<Option<Value>> {
    let initial = crate::operations::governance::capture(ctx)?;
    let Some(assessment) =
        crate::source::source_inventory::assess_adoption(ctx, &initial, target.key, target.exact)?
    else {
        return Ok(None);
    };
    if dry_run {
        return Ok(Some(
            json!({"backend":"rust","mode":"dry-run","member":assessment.baseline.source.member,"root":assessment.baseline.slot.path(),"scope":"complete-existing-root","persistentWrites":false}),
        ));
    }
    crate::operations::confirm("采纳当前能力包完整目录范围并保留恢复证据", yes)?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    assessment.verify()?;
    let authority = crate::operations::governance::capture(ctx)?;
    authority.ensure_settled()?;
    if initial.paths != authority.paths {
        return Err(invalid());
    }
    let (mut batch, mut journal) = initialize(ctx, &authority, &assessment, &store)?;
    batch.save(&store)?;
    execute(&store, &mut batch, &mut journal).map(Some)
}
#[cfg(test)]
#[path = "../../tests/unit/source_tx.rs"]
mod tests;
fn verify_projection(manifest: &Manifest, blobs: &files::Blobs) -> Result<()> {
    let managed = &manifest.ledgers[2];
    let before = managed.before.as_ref().map(|b| blobs.get(b)).transpose()?;
    let after = managed.after.as_ref().map(|b| blobs.get(b)).transpose()?;
    let expected = match &manifest.action {
        SourceAction::Adopt { root } => Some(crate::source::rules::source_baseline::adopt(
            before.as_deref(),
            root.baseline.clone(),
            &manifest.home,
            nix::unistd::geteuid().as_raw(),
        )?),
        SourceAction::Upgrade { roots, invocation } => {
            let raw = |index: usize, after: bool| -> Result<Option<Vec<u8>>> {
                let ledger = &manifest.ledgers[index];
                (if after { &ledger.after } else { &ledger.before })
                    .as_ref()
                    .map(|blob| blobs.get(blob))
                    .transpose()
            };
            files::cas(&invocation.namespace, &invocation.state)?;
            let rules = files::observe(&invocation.namespace, 1024 * 1024)?.1;
            let applied = crate::source::rules::source_invocation::applied(
                &manifest.home,
                rules.as_deref(),
                raw(3, false)?.as_deref(),
            )?;
            crate::source::source_invocation::verify(
                &manifest.home,
                &applied,
                roots,
                raw(3, true)?.as_deref(),
            )?;
            let native = crate::source::rules::source_projection::native(
                raw(0, false)?.as_deref(),
                roots,
                manifest.created_at,
            )?;
            if native != raw(0, true)?
                || crate::source::rules::source_projection::definitions(
                    raw(1, false)?.as_deref(),
                    roots,
                )? != raw(1, true)?
            {
                return Err(invalid());
            }
            crate::source::rules::source_projection::receipt_specs(roots, native.as_deref())?;
            crate::source::rules::source_projection::overlays(
                raw(3, false)?.as_deref(),
                raw(3, true)?.as_deref(),
                roots,
                &manifest.home,
            )?;
            crate::source::rules::source_projection::managed(
                before.as_deref(),
                roots,
                &manifest.home,
                nix::unistd::geteuid().as_raw(),
            )?
        }
    };
    if after != expected {
        return Err(invalid());
    }
    let receipt = &manifest.ledgers[4];
    let before = receipt.before.as_ref().map(|b| blobs.get(b)).transpose()?;
    let expected = receipts(before.as_deref(), manifest)?;
    if receipt.after.as_ref().map(|b| blobs.get(b)).transpose()? != expected {
        return Err(invalid());
    }
    for ledger in &manifest.ledgers {
        for blob in [&ledger.before, &ledger.after].into_iter().flatten() {
            blobs.get(blob)?;
        }
    }
    Ok(())
}
fn load(ctx: &Context, store: &Store, journal: &Journal) -> Result<Batch> {
    store.verify_namespace()?;
    let Provider::Governance {
        category: GovernanceCategory::Source,
        manifest: path,
        manifest_sha256: digest,
    } = &journal.plan.package.provider
    else {
        return Err(invalid());
    };
    let authority = crate::operations::governance::capture(ctx)?;
    let root = Directory::open(path.parent().ok_or_else(invalid)?)?;
    let meta = root.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.mode() & 0o7777 != 0o700
        || root.path.parent() != Some(authority.paths.backups.as_path())
    {
        return Err(invalid());
    }
    let (raw, meta) =
        root.regular_config_snapshot(OsStr::new("capability-plan.json"), 16 * 1024 * 1024)?;
    if files::hash(&raw) != *digest || meta.mode != 0o600 {
        return Err(invalid());
    }
    let manifest: Manifest = decode(&raw)?;
    policy::validate(&manifest, &authority.paths, nix::unistd::geteuid().as_raw())?;
    if manifest.home != ctx.home
        || manifest.settings != ctx.settings
        || manifest.layout.state != store.path
        || manifest.layout.config != ctx.config
        || manifest.layout.data != ctx.data
        || crate::capability::rules::delivery::native_lock_path(&ctx.home, &ctx.scope)
            != crate::capability::rules::delivery::native_lock_path(&manifest.home, &manifest.scope)
        || manifest.scope != journal.scope
        || manifest.operation_id != journal.id
        || root.path.file_name() != Some(OsStr::new(&manifest.change_id))
    {
        return Err(invalid());
    }
    let (witness, marker) = witness(&manifest.pending)?;
    if marker.is_some() && policy::mode(&witness.state) != Some(0o600) {
        return Err(invalid());
    }
    let pending: Pending = if let Some(raw) = &marker {
        decode(raw)?
    } else {
        let (raw, meta) =
            root.regular_config_snapshot(OsStr::new("source-candidates.json"), 16 * 1024 * 1024)?;
        if meta.mode != 0o600 {
            return Err(invalid());
        }
        decode(&raw)?
    };
    policy::validate_pending(&manifest, &pending, digest)?;
    let blobs = files::Blobs::open(&root.path.join("blobs"))?;
    verify_projection(&manifest, &blobs)?;
    authority.verify()?;
    Ok(Batch {
        root,
        blobs,
        manifest,
        pending,
        marker,
    })
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
    if resolve_failed {
        return Err(Error::attention(
            "source_pending_cannot_abandon",
            "必须核验来源批次，不跳过一致性恢复",
        ));
    }
    let mut batch = load(ctx, store, journal)?;
    crate::operations::confirm(
        "核验来源批次并完成提交或逆序回退，不重新下载或执行安装器",
        yes,
    )?;
    batch.guard(store)?;
    if batch.matches(true)? {
        if batch.marker.is_some() {
            batch.clear(store, true)?;
        }
        journal.phase = Phase::Succeeded;
        journal.message = "来源根与全部五项账本已经处于目标态，完成日志收口".into();
    } else if batch.marker.is_some() {
        batch.rollback(store)?;
        batch.clear(store, false)?;
        journal.phase = Phase::Failed;
        journal.message = "五项账本逆序恢复，来源载荷保持原样".into();
    } else if batch.matches(false)? {
        journal.phase = Phase::Failed;
        journal.message = "全部来源账本已经恢复，完成日志收口".into();
    } else {
        return Err(invalid());
    }
    store.save(journal)?;
    Ok(result(journal, &batch.manifest))
}
