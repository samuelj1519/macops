use crate::{
    capability_store as store,
    error::{Error, Result},
    paths::Directory,
    policy::capability_tx::{Candidate, DirectoryEntry, Manifest, Pending, State},
};
use std::{collections::BTreeMap, ffi::OsStr, os::unix::ffi::OsStringExt, path::Path};

fn changed() -> Error {
    Error::attention(
        "capability_transaction_changed",
        "来源、目标或账本存在外部修改；保留批次与备份",
    )
}
pub fn candidates(pending: &Pending) -> impl Iterator<Item = &Candidate> {
    pending
        .files
        .iter()
        .chain(&pending.ledgers)
        .chain(&pending.rollbacks)
}
pub fn entry(state: &State) -> Result<Option<DirectoryEntry>> {
    Ok(match state {
        State::Missing => None,
        State::File { identity, mode, .. } => Some(DirectoryEntry {
            kind: "file".into(),
            identity: identity.clone(),
            mode: *mode,
        }),
        State::Link {
            identity,
            mode: Some(mode),
            ..
        } => Some(DirectoryEntry {
            kind: "symlink".into(),
            identity: identity.clone(),
            mode: *mode,
        }),
        State::Link { mode: None, .. } => {
            return Err(Error::attention(
                "unobserved_link_mode",
                "旧只读链接证据没有可写 mode",
            ));
        }
    })
}
pub fn listing(root: &Directory) -> Result<BTreeMap<String, DirectoryEntry>> {
    let mut entries = BTreeMap::new();
    for (count, item) in root.names()?.enumerate() {
        if count > 5000 {
            return Err(changed());
        }
        let item = item.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        if matches!(item.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        let name = std::ffi::OsString::from_vec(item.file_name().to_bytes().to_vec());
        let text = name.to_str().ok_or_else(changed)?.to_owned();
        let meta = root.metadata(&name)?.ok_or_else(changed)?;
        entries.insert(
            text,
            DirectoryEntry {
                kind: meta.kind.as_str().into(),
                identity: meta.identity,
                mode: meta.mode,
            },
        );
    }
    root.verify()?;
    Ok(entries)
}
pub fn known_state(path: &Path, before: &State, current: &State, pending: &Pending) -> bool {
    current == before || candidates(pending).any(|c| c.target == path && c.after == *current)
}
pub fn staged_path(candidate: &Candidate, pending: &Pending) -> Result<Option<std::path::PathBuf>> {
    let Some(staged) = &candidate.staged else {
        return Ok(None);
    };
    for parent in &pending.parents {
        if let Ok(tail) = staged.strip_prefix(&parent.staged)
            && store::parent_published(parent)?
        {
            return Ok(Some(parent.target.join(tail)));
        }
    }
    Ok(Some(staged.clone()))
}
fn verify_parents(pending: &Pending) -> Result<()> {
    for parent in &pending.parents {
        let published = store::parent_published(parent)?;
        let path = if published {
            &parent.target
        } else {
            &parent.staged
        };
        let root = match Directory::open(path) {
            Ok(root) => root,
            Err(error)
                if error.code == "path_missing"
                    && pending.retired_parents.contains(&parent.target) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let actual = store::binding(&root, path)?;
        if actual.identity != parent.binding.identity
            || actual.lineage != parent.binding.lineage
            || actual.uid != parent.binding.uid
            || actual.mode != parent.binding.mode
        {
            return Err(changed());
        }
        let mut expected = BTreeMap::new();
        for candidate in
            candidates(pending).filter(|c| c.target.parent() == Some(parent.target.as_path()))
        {
            let name = candidate.target.file_name().ok_or_else(changed)?;
            let target = path.join(name);
            let state = store::observe(&store::namespace(&target)?, 64 * 1024 * 1024)?.0;
            if state != State::Missing
                && !candidates(pending).any(|c| c.target == candidate.target && c.after == state)
            {
                return Err(changed());
            }
            update(&mut expected, &target, entry(&state)?)?;
            if let Some(stage) = staged_path(candidate, pending)? {
                let state = store::observe(&store::namespace(&stage)?, 64 * 1024 * 1024)?.0;
                if state != State::Missing && state != candidate.after {
                    return Err(changed());
                }
                update(&mut expected, &stage, entry(&state)?)?;
            }
        }
        if listing(&root)? != expected {
            return Err(changed());
        }
    }
    Ok(())
}
fn verify_authorities(manifest: &Manifest) -> Result<()> {
    if let Some(authority) = &manifest.links {
        for binding in std::iter::once(&authority.canonical).chain(authority.profiles.values()) {
            store::open(&crate::policy::capability_tx::Namespace {
                ancestor: binding.clone(),
                remaining: vec![".probe".into()],
            })?;
        }
    }
    if let Some(configuration) = &manifest.configuration {
        for client in configuration
            .targets
            .values()
            .flat_map(|t| t.clients.values())
        {
            store::open(&crate::policy::capability_tx::Namespace {
                ancestor: client.live_root.clone(),
                remaining: vec![".probe".into()],
            })?;
            if let Some(source) = &client.source {
                store::open(&crate::policy::capability_tx::Namespace {
                    ancestor: source.root.clone(),
                    remaining: vec![".probe".into()],
                })?;
            }
        }
    }
    Ok(())
}
pub fn verify(manifest: &Manifest, pending: &Pending) -> Result<()> {
    verify_authorities(manifest)?;
    verify_parents(pending)?;
    for file in &manifest.files {
        let current = store::observe(&file.namespace, 64 * 1024 * 1024)?.0;
        if !known_state(&file.path, &file.before, &current, pending) {
            return Err(changed());
        }
    }
    for ledger in &manifest.ledgers {
        let current = store::observe(&ledger.namespace, 64 * 1024 * 1024)?.0;
        if !known_state(&ledger.path, &ledger.before, &current, pending) {
            return Err(changed());
        }
    }
    for directory in &manifest.directories {
        let ns = store::namespace(&directory.binding.requested.join(".probe"))?;
        if ns.ancestor != directory.binding {
            return Err(changed());
        }
        let root = store::open(&ns)?;
        let mut expected = directory.entries.clone();
        for file in &manifest.files {
            if file.path.parent() == Some(root.path.as_path()) {
                let state = store::observe(&file.namespace, 64 * 1024 * 1024)?.0;
                update(&mut expected, &file.path, entry(&state)?)?;
            }
        }
        for candidate in candidates(pending) {
            if let Some(path) = staged_path(candidate, pending)?
                && path.parent() == Some(root.path.as_path())
            {
                let observed = store::observe(&store::namespace(&path)?, 64 * 1024 * 1024)?.0;
                if observed != State::Missing && observed != candidate.after {
                    return Err(changed());
                }
                update(&mut expected, &path, entry(&observed)?)?;
            }
        }
        for parent in &pending.parents {
            for path in [&parent.staged, &parent.target] {
                if path.parent() == Some(root.path.as_path()) {
                    let meta = root.metadata(path.file_name().ok_or_else(changed)?)?;
                    if let Some(meta) = meta {
                        if meta.identity != parent.binding.identity
                            || meta.mode != parent.binding.mode
                            || meta.kind != crate::paths::EntryKind::Directory
                        {
                            return Err(changed());
                        }
                        update(
                            &mut expected,
                            path,
                            Some(DirectoryEntry {
                                kind: "directory".into(),
                                identity: meta.identity,
                                mode: meta.mode,
                            }),
                        )?;
                    } else {
                        update(&mut expected, path, None)?;
                    }
                }
            }
        }
        if listing(&root)? != expected {
            return Err(changed());
        }
    }
    for input in &manifest.inputs {
        if let Some(parent) = pending.parents.iter().find(|p| p.target == input.path)
            && input.state == State::Missing
            && store::parent_published(parent)?
        {
            continue;
        }
        let current = store::observe(&input.namespace, 64 * 1024 * 1024)?.0;
        let legacy = manifest.schema_version == 2
            && matches!((&input.state,&current),
            (State::Link {identity:a,text:x,mode:None},State::Link {identity:b,text:y,..}) if a==b && x==y);
        if !legacy && !known_state(&input.path, &input.state, &current, pending) {
            return Err(changed());
        }
    }
    Ok(())
}
fn update(
    entries: &mut BTreeMap<String, DirectoryEntry>,
    path: &Path,
    value: Option<DirectoryEntry>,
) -> Result<()> {
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(changed)?;
    if let Some(value) = value {
        entries.insert(name.into(), value);
    } else {
        entries.remove(name);
    }
    Ok(())
}
