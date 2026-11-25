use super::{Cache, Managed, io, package, policy, validate_cache};
use crate::{
    context::Context,
    domain::{Plan, Provider, QuerySpec, fingerprint},
    error::{Error, Result},
    operations,
    paths::Directory,
    policy::chezmoi_native::{Query, QueryKind, SourceChange, SourceMode},
    store::{Journal, Phase, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sources::SourceDelta;
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[path = "chezmoi_source_io.rs"]
mod sources;

const CACHE: &str = "chezmoi-metadata.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Publication {
    operation_id: String,
    plan_sha256: String,
    view_sha256: String,
}

pub(super) fn forget_target(ctx: &Context, selection: &str) -> Result<PathBuf> {
    sources::check_scope(ctx)?;
    if !selection.starts_with("config:") {
        return crate::policy::paths::lexical(Path::new(selection));
    }
    let cache = io::read::<Cache>(ctx, CACHE)?.ok_or_else(sources::unsupported)?;
    validate_cache(ctx, &cache)?;
    cache
        .entries
        .values()
        .find(|entry| {
            crate::policy::config_local::config_id(&entry.absolute).is_ok_and(|id| id == selection)
        })
        .map(|entry| entry.absolute.clone())
        .ok_or_else(|| Error::attention("chezmoi_target_unmanaged", "当前原生清单没有该配置 ID"))
}

pub(super) fn run(
    ctx: &Context,
    target: &Path,
    dry: bool,
    yes: bool,
    mode: SourceMode,
) -> Result<Value> {
    let inputs = sources::inspect(ctx, target, mode, SourceDelta::None)?;
    if dry {
        return Ok(json!({
            "mode":"dry-run",
            "backend":"chezmoi-source",
            "action":mode.action(),
            "target":target,
            "sourceParent":inputs.parent.path,
            "sourceName":inputs.managed.as_ref().map_or_else(
                || "pending-native-verification".into(),
                |item| item.source_relative.display().to_string(),
            ),
            "effect":if mode == SourceMode::Adopt {"plain-source-file"} else {"forget-source-file"},
            "nativeInvocations":0,
            "persistentWrites":false
        }));
    }
    let staged = sources::stage(ctx, &inputs, mode)?;
    let proof = SourceChange {
        owner: inputs.owner,
        target: inputs.target,
        candidate: staged.candidate,
        draft_directory: staged.identity,
        source_parent: inputs.parent.path.clone(),
        parent_identity: inputs.parent.identity().clone(),
        old_tree_sha256: inputs.tree_sha256,
        old_cache_sha256: fingerprint(&inputs.cache)?,
    };
    let plan = plan(ctx, &inputs.binding, &staged.source, &proof, mode)?;
    let effect = match mode {
        SourceMode::Adopt => format!(
            "将 {} 纳入管理并写入明文源 {}。实际配置文件保持原样。",
            target.display(),
            staged.source.display()
        ),
        SourceMode::Forget => format!(
            "停止管理 {}，移出源 {}。实际配置文件保持原样。",
            target.display(),
            staged.source.display()
        ),
    };
    operations::confirm(&effect, yes)?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let mut journal = operations::new_journal(ctx, plan)?;

    check_before_source(ctx, &journal, &proof)?;
    let _ = staged.draft.keep();
    store.save(&journal)?;
    publish_source(ctx, &store, &journal, &proof)?;
    recover(ctx, &store, &mut journal, &proof, mode)
}

fn verify_before_source(
    ctx: &Context,
    journal: &Journal,
    proof: &SourceChange,
) -> Result<(sources::Inputs, Directory)> {
    let mode = source_mode(journal)?;
    let inputs = recheck(ctx, journal, proof, false)?;
    let draft = owned_draft(journal, proof)?;
    let source = source(journal)?;
    let leaf = source.file_name().ok_or_else(unknown)?;
    match mode {
        SourceMode::Adopt => {
            check_candidate(proof, &draft)?;
            if inputs.parent.metadata(leaf)?.is_some() {
                return Err(stale());
            }
        }
        SourceMode::Forget => {
            check_object(source, proof)?;
            if draft.metadata(OsStr::new(sources::DRAFT_LEAF))?.is_some() {
                return Err(unknown());
            }
        }
    }
    Ok((inputs, draft))
}

fn check_before_source(ctx: &Context, journal: &Journal, proof: &SourceChange) -> Result<()> {
    verify_before_source(ctx, journal, proof).map(|_| ())
}

fn publish_source(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    proof: &SourceChange,
) -> Result<()> {
    store.verify_namespace()?;
    let (verified, draft) = verify_before_source(ctx, journal, proof)?;
    let leaf = source(journal)?.file_name().ok_or_else(unknown)?;
    verified.parent.verify()?;
    let candidate = OsStr::new(sources::DRAFT_LEAF);
    let parent = &verified.parent;
    match source_mode(journal)? {
        SourceMode::Adopt => move_file(&draft, candidate, parent, leaf),
        SourceMode::Forget => retire_file(parent, leaf, &draft, proof),
    }
}

fn retire_file(
    parent: &Directory,
    leaf: &OsStr,
    draft: &Directory,
    proof: &SourceChange,
) -> Result<()> {
    let retained = OsStr::new(sources::DRAFT_LEAF);
    move_file(parent, leaf, draft, retained)?;
    if check_candidate(proof, draft).is_err() {
        restore_retained(parent, leaf, draft)?;
        return Err(unknown());
    }
    Ok(())
}

fn restore_retained(parent: &Directory, leaf: &OsStr, draft: &Directory) -> Result<()> {
    draft.verify()?;
    parent.verify()?;
    move_file(draft, OsStr::new(sources::DRAFT_LEAF), parent, leaf)
}

fn move_file(from: &Directory, old: &OsStr, to: &Directory, new: &OsStr) -> Result<()> {
    rustix::fs::renameat_with(
        from.file(),
        old,
        to.file(),
        new,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| Error::attention("chezmoi_source_publish_uncertain", error.to_string()))?;
    from.file().sync_all()?;
    to.file().sync_all()?;
    Ok(())
}

fn plan(
    ctx: &Context,
    binding: &policy::Binding,
    source: &Path,
    proof: &SourceChange,
    mode: SourceMode,
) -> Result<Plan> {
    let package = package(&proof.target.path, source)?;
    Ok(Plan {
        brew: None,
        uv_owner: None,
        npm_binding: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: mode.action(),
        before: mode.before(),
        target: None,
        command: None,
        query: Some(QuerySpec::NativeConfig(Query {
            kind: QueryKind::NativeV1,
            binding_sha256: fingerprint(binding)?,
            targets: vec![proof.target.path.clone()],
            source_adopt: (mode == SourceMode::Adopt).then(|| Box::new(proof.clone())),
            source_forget: (mode == SourceMode::Forget).then(|| Box::new(proof.clone())),
        })),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    })
}

fn source(journal: &Journal) -> Result<&Path> {
    match &journal.plan.package.provider {
        Provider::ConfigObserved {
            source: Some(source),
            ..
        } => Ok(source),
        _ => Err(sources::unsupported()),
    }
}

fn source_mode(journal: &Journal) -> Result<SourceMode> {
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err(unknown());
    };
    query
        .source_change()
        .map(|(mode, _)| mode)
        .ok_or_else(unknown)
}

fn stale() -> Error {
    Error::attention(
        "chezmoi_source_plan_stale",
        "原 owner、文件或管理映射已变化",
    )
}

fn unknown() -> Error {
    Error::attention(
        "chezmoi_source_outcome_unknown",
        "无法证明源、阶段文件和缓存的原操作结果，保留操作阻断",
    )
}

fn recheck(
    ctx: &Context,
    journal: &Journal,
    proof: &SourceChange,
    published: bool,
) -> Result<sources::Inputs> {
    let source = source(journal)?;
    let mode = source_mode(journal)?;
    let delta = match (mode, published) {
        (_, false) => SourceDelta::None,
        (SourceMode::Adopt, true) => SourceDelta::Added(source),
        (SourceMode::Forget, true) => SourceDelta::Retired(source, &proof.candidate),
    };
    let inputs = sources::inspect(ctx, &proof.target.path, mode, delta)?;
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err(stale());
    };
    if fingerprint(&inputs.binding)? != query.binding_sha256
        || fingerprint(&inputs.cache)? != proof.old_cache_sha256
        || inputs.owner != proof.owner
        || inputs.target != proof.target
        || inputs.parent.path != proof.source_parent
        || inputs.parent.identity() != &proof.parent_identity
        || inputs.tree_sha256 != proof.old_tree_sha256
    {
        return Err(stale());
    }
    Ok(inputs)
}

fn owned_draft(journal: &Journal, proof: &SourceChange) -> Result<Directory> {
    let parent = proof.candidate.path.parent().ok_or_else(unknown)?;
    if parent.parent() != Some(&journal.state)
        || !parent
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|leaf| leaf.starts_with(sources::DRAFT_PREFIX))
        || proof.candidate.path.file_name() != Some(OsStr::new(sources::DRAFT_LEAF))
    {
        return Err(unknown());
    }
    let dir = Directory::open(parent)?;
    let meta = dir.file().metadata()?;
    if dir.path != parent
        || dir.identity() != &proof.draft_directory
        || meta.uid() != nix::unistd::geteuid().as_raw()
        || meta.permissions().mode() & 0o7777 != 0o700
    {
        return Err(unknown());
    }
    dir.verify()?;
    Ok(dir)
}

fn check_object(path: &Path, proof: &SourceChange) -> Result<()> {
    let (_, actual) = sources::file(path, 256 * 1024)?;
    let mut expected = proof.candidate.clone();
    expected.path = path.to_path_buf();
    if actual != expected {
        return Err(unknown());
    }
    Ok(())
}

fn check_candidate(proof: &SourceChange, draft: &Directory) -> Result<()> {
    check_object(&proof.candidate.path, proof)?;
    draft.verify()
}

fn verify_after(
    ctx: &Context,
    journal: &Journal,
    proof: &SourceChange,
    draft: &Directory,
) -> Result<sources::Inputs> {
    let inputs = recheck(ctx, journal, proof, true)?;
    let source = source(journal)?;
    match source_mode(journal)? {
        SourceMode::Adopt => {
            check_object(source, proof)?;
            if draft.metadata(OsStr::new(sources::DRAFT_LEAF))?.is_some() {
                return Err(unknown());
            }
        }
        SourceMode::Forget => {
            check_candidate(proof, draft)?;
            if inputs
                .parent
                .metadata(source.file_name().ok_or_else(unknown)?)?
                .is_some()
            {
                return Err(unknown());
            }
        }
    }
    draft.verify()?;
    inputs.parent.verify()?;
    draft.file().sync_all()?;
    inputs.parent.file().sync_all()?;
    Ok(inputs)
}

fn published(cache: &Cache, journal: &Journal) -> Result<bool> {
    let Some(mark) = &cache.source_publication else {
        return Ok(false);
    };
    if mark.operation_id != journal.id {
        return Ok(false);
    }
    let mut recorded = cache.clone();
    recorded.source_publication = None;
    if mark.plan_sha256 != journal.plan_sha256 || mark.view_sha256 != fingerprint(&recorded)? {
        return Err(unknown());
    }
    Ok(true)
}

fn publish_cache(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    proof: &SourceChange,
    draft: &Directory,
) -> Result<()> {
    let source = source(journal)?;
    let inputs = verify_after(ctx, journal, proof, draft)?;
    let relative = proof
        .target
        .path
        .strip_prefix(&ctx.home)
        .map_err(|_| unknown())?;
    let mut cache = inputs.cache;
    let source_relative = source
        .strip_prefix(&cache.binding.effective_source)
        .map_err(|_| unknown())?;
    match source_mode(journal)? {
        SourceMode::Adopt => {
            cache.entries.insert(
                relative.to_str().ok_or_else(unknown)?.into(),
                Managed {
                    absolute: proof.target.path.clone(),
                    source_absolute: source.to_path_buf(),
                    source_relative: source_relative.to_path_buf(),
                },
            );
        }
        SourceMode::Forget => {
            cache.entries.remove(relative.to_str().ok_or_else(unknown)?);
            cache.status.retain(|entry| entry.path != proof.target.path);
        }
    }
    cache.source_publication = None;
    let view_sha256 = fingerprint(&cache)?;
    cache.source_publication = Some(Publication {
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        view_sha256,
    });
    let bytes = serde_json::to_vec(&cache)?;
    Store::atomic_bytes_with_check(&store.root, CACHE, &bytes, false, || {
        store.verify_namespace()?;
        verify_after(ctx, journal, proof, draft)?;
        store.verify_namespace()
    })
}

pub(super) fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    proof: &SourceChange,
    mode: SourceMode,
) -> Result<Value> {
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        if ctx.scope == journal.scope
            && ctx.state == journal.state
            && sources::check_scope(ctx).is_ok()
        {
            cleanup_draft(journal, proof, mode);
        }
        return Ok(json!(journal));
    }
    sources::check_scope(ctx)?;
    if journal.home != ctx.home || journal.state != ctx.state || journal.process_group.is_some() {
        return Err(unknown());
    }
    let cache = io::read::<Cache>(ctx, CACHE)
        .map_err(|_| unknown())?
        .ok_or_else(unknown)?;
    validate_cache(ctx, &cache).map_err(|_| unknown())?;
    if published(&cache, journal)? {
        journal.phase = Phase::Succeeded;
        journal.message = "已核验原生源及本次管理清单发布".into();
    } else {
        let draft = owned_draft(journal, proof).map_err(|_| unknown())?;
        if mode == SourceMode::Forget {
            restore_unexpected(ctx, store, journal, proof, &draft, &cache)?;
        }
        if verify_before_source(ctx, journal, proof).is_ok() {
            journal.phase = Phase::Failed;
            journal.message = "源未发布，原候选与旧清单均完整".into();
        } else {
            publish_cache(ctx, store, journal, proof, &draft).map_err(|_| unknown())?;
            journal.phase = Phase::Succeeded;
            journal.message = "已核验原生源及本次管理清单发布".into();
        }
    }
    store.save(journal)?;
    cleanup_draft(journal, proof, mode);
    Ok(json!(journal))
}

fn restore_unexpected(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    proof: &SourceChange,
    draft: &Directory,
    cache: &Cache,
) -> Result<()> {
    let name = OsStr::new(sources::DRAFT_LEAF);
    let Some(retained) = draft.metadata(name)? else {
        return Ok(());
    };
    if check_candidate(proof, draft).is_ok() {
        return Ok(());
    }
    let parent = original_source_parent(ctx, journal, proof, cache)?;
    let leaf = source(journal)?.file_name().ok_or_else(unknown)?;
    if parent.metadata(leaf)?.is_some()
        || draft
            .metadata(name)?
            .is_none_or(|current| current.identity != retained.identity)
    {
        return Err(unknown());
    }
    store.verify_namespace()?;
    restore_retained(&parent, leaf, draft)?;
    parent.verify()?;
    if parent
        .metadata(leaf)?
        .is_some_and(|current| current.identity == retained.identity)
    {
        return Err(Error::attention(
            "chezmoi_source_outcome_unknown",
            "保留文件已放回原位，原操作仍需核对",
        ));
    }
    Err(unknown())
}

fn original_source_parent(
    ctx: &Context,
    journal: &Journal,
    proof: &SourceChange,
    cache: &Cache,
) -> Result<Directory> {
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err(unknown());
    };
    let owner_path = ctx.state.join("chezmoi-owner.json");
    if fingerprint(cache)? != proof.old_cache_sha256
        || fingerprint(&cache.binding)? != query.binding_sha256
        || io::stamp(&owner_path, true)? != proof.owner
        || cache.binding.destination != ctx.home
        || cache.binding.record_state != ctx.state
        || !proof
            .source_parent
            .starts_with(&cache.binding.effective_source)
    {
        return Err(unknown());
    }
    let root = Directory::open(&cache.binding.effective_source)?;
    if !cache
        .binding
        .anchors
        .iter()
        .any(|anchor| anchor.path == root.path && anchor.identity.as_ref() == Some(root.identity()))
    {
        return Err(unknown());
    }
    let parent = Directory::open(&proof.source_parent)?;
    if root.path != cache.binding.effective_source
        || parent.path != proof.source_parent
        || parent.identity() != &proof.parent_identity
    {
        return Err(unknown());
    }
    root.verify()?;
    parent.verify()?;
    Ok(parent)
}

fn cleanup_draft(journal: &Journal, proof: &SourceChange, mode: SourceMode) {
    if let Ok(draft) = owned_draft(journal, proof) {
        if (journal.phase, mode) == (Phase::Failed, SourceMode::Adopt)
            || (journal.phase, mode) == (Phase::Succeeded, SourceMode::Forget)
        {
            let _ = draft.unlink_regular(OsStr::new(sources::DRAFT_LEAF), |draft| {
                check_candidate(proof, draft)
            });
        }
        if draft.verify().is_ok() {
            let _ = fs::remove_dir(&draft.path);
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/chezmoi_source.rs"]
mod tests;
