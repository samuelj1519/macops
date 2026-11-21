use crate::{
    capability_store as files,
    error::{Error, Result},
    native_skills_files::{self as disk, Object},
    policy::{
        capability_tx::{Candidate, ParentCandidate, State, Witness},
        invocation::{self, MetadataKind},
        native_skills::Recipe,
        source_tree::{Slot, Tree},
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
fn invalid() -> Error {
    Error::attention(
        "native_skills_customization_pending",
        "原生已执行；定制证据不匹配，保留待恢复状态",
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    pub relative: String,
    pub before: Witness,
    pub before_bytes: Option<Vec<u8>>,
    pub after: Vec<u8>,
    pub mode: u32,
    pub parent: Option<ParentCandidate>,
    pub candidate: Option<Candidate>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Customization {
    pub slot: Slot,
    pub native_tree: Tree,
    pub patches: Vec<Patch>,
}
fn patch(root: &Path, relative: &str, after: Vec<u8>, license: bool) -> Result<Patch> {
    let path = root.join(relative);
    let namespace = files::namespace(&path)?;
    let (state, before) = files::observe(&namespace, 1024 * 1024)?;
    if matches!(state, State::Link { .. })
        || (license && before.as_ref().is_some_and(|b| *b != after))
    {
        return Err(invalid());
    }
    let mode = match &state {
        State::File { mode, .. } => *mode,
        State::Missing => 0o644,
        State::Link { .. } => return Err(invalid()),
    };
    let parent = files::prepare_parent(&namespace)?;
    Ok(Patch {
        relative: relative.into(),
        before: Witness {
            path,
            namespace,
            state,
        },
        before_bytes: before,
        after,
        mode,
        parent,
        candidate: None,
    })
}
pub fn prepare(
    root: &Path,
    recipe: &Recipe,
    licenses: &BTreeMap<String, Vec<u8>>,
    derived: bool,
) -> Result<Customization> {
    let Object::Directory { slot, tree, .. } = disk::capture(root)? else {
        return Err(invalid());
    };
    let mut patches = vec![];
    if recipe.manual_only && !derived {
        for (relative, kind) in [
            ("SKILL.md", MetadataKind::Frontmatter),
            ("agents/openai.yaml", MetadataKind::CodexPolicy),
        ] {
            let before = disk::regular(&root.join(relative))?;
            let after = invocation::transform(kind, before.as_deref())?;
            patches.push(patch(root, relative, after, false)?);
        }
    }
    for (name, bytes) in licenses {
        patches.push(patch(root, name, bytes.clone(), true)?);
    }
    if patches.len() > 4 {
        return Err(invalid());
    }
    Ok(Customization {
        slot,
        native_tree: tree,
        patches,
    })
}
fn check_stage(patch: &Patch) -> Result<()> {
    if let Some(candidate) = &patch.candidate {
        let staged_file = candidate.staged.as_ref().ok_or_else(invalid)?;
        let state = files::observe(&files::namespace(staged_file)?, 16 * 1024 * 1024)?.0;
        if state != State::Missing && state != candidate.after {
            return Err(invalid());
        }
    }
    Ok(())
}
impl Customization {
    pub fn validate(
        &self,
        native: &Object,
        recipe: &Recipe,
        licenses: &BTreeMap<String, Vec<u8>>,
        derived: bool,
    ) -> Result<()> {
        let Object::Directory { path, slot, tree } = native else {
            return Err(invalid());
        };
        if self.slot != *slot || self.native_tree != *tree || self.slot.path() != *path {
            return Err(invalid());
        }
        let mut expected = licenses.clone();
        if recipe.manual_only && !derived {
            for (relative, kind) in [
                ("SKILL.md", MetadataKind::Frontmatter),
                ("agents/openai.yaml", MetadataKind::CodexPolicy),
            ] {
                let mut selected = self.patches.iter().filter(|p| p.relative == relative);
                let patch = selected.next().ok_or_else(invalid)?;
                if selected.next().is_some() {
                    return Err(invalid());
                }
                expected.insert(
                    relative.into(),
                    invocation::transform(kind, patch.before_bytes.as_deref())?,
                );
            }
        }
        if self.patches.len() != expected.len() {
            return Err(invalid());
        }
        let mut seen = std::collections::BTreeSet::new();
        for patch in &self.patches {
            if !seen.insert(&patch.relative)
                || expected.get(&patch.relative) != Some(&patch.after)
                || patch.before.path != path.join(&patch.relative)
                || files::target_path(&patch.before.namespace) != patch.before.path
            {
                return Err(invalid());
            }
            validate_before(patch, tree)?;
            if licenses.contains_key(&patch.relative)
                && patch
                    .before_bytes
                    .as_ref()
                    .is_some_and(|b| b != &patch.after)
            {
                return Err(invalid());
            }
            validate_candidate(patch, path, tree)?;
        }
        self.verify()
    }
    pub fn verify(&self) -> Result<()> {
        let tree = crate::source_tree::observe(&self.slot)?.ok_or_else(invalid)?;
        if tree.identity != self.native_tree.identity
            || tree.uid != self.native_tree.uid
            || tree.mode != self.native_tree.mode
        {
            return Err(invalid());
        }
        let mut current = tree.entries;
        let mut before = self.native_tree.entries.clone();
        for patch in &self.patches {
            if patch.before.path != self.slot.path().join(&patch.relative)
                || patch.relative.contains("..")
            {
                return Err(invalid());
            }
            let (state, raw) = files::observe(&patch.before.namespace, 1024 * 1024)?;
            let accepted = state == patch.before.state
                || patch.candidate.as_ref().is_some_and(|c| state == c.after);
            if !accepted {
                return Err(invalid());
            }
            if patch.candidate.as_ref().is_some_and(|c| state == c.after)
                && raw.as_deref() != Some(patch.after.as_slice())
            {
                return Err(invalid());
            }
            current.remove(&patch.relative);
            before.remove(&patch.relative);
            check_stage(patch)?;
            if let Some(candidate) = &patch.candidate
                && let Some(path) = &candidate.staged
            {
                current.remove(
                    path.strip_prefix(self.slot.path())
                        .map_err(|_| invalid())?
                        .to_str()
                        .ok_or_else(invalid)?,
                );
            }
            if let Some(parent) = &patch.parent {
                let path = if files::parent_published(parent)? {
                    &parent.target
                } else {
                    &parent.staged
                };
                let root = crate::paths::Directory::open(path)?;
                if root.identity() != &parent.binding.identity {
                    return Err(invalid());
                }
                current.remove(
                    path.strip_prefix(self.slot.path())
                        .map_err(|_| invalid())?
                        .to_str()
                        .ok_or_else(invalid)?,
                );
            }
        }
        if current != before {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn complete(&self) -> Result<bool> {
        self.verify()?;
        for patch in &self.patches {
            let (state, raw) = files::observe(&patch.before.namespace, 1024 * 1024)?;
            if raw.as_deref() != Some(patch.after.as_slice())
                || !matches!(state,State::File{mode,..} if mode==patch.mode)
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub fn apply(
        &mut self,
        index: usize,
        checkpoint: impl FnOnce(&Self) -> Result<()>,
    ) -> Result<()> {
        self.verify()?;
        let patch = self.patches.get_mut(index).ok_or_else(invalid)?;
        if let Some(parent) = &patch.parent {
            files::publish_parent(parent)?;
        }
        let (state, raw) = files::observe(&patch.before.namespace, 1024 * 1024)?;
        if patch.candidate.as_ref().is_some_and(|c| state == c.after) {
            return Ok(());
        }
        if state != patch.before.state {
            return Err(invalid());
        }
        if raw.as_deref() == Some(patch.after.as_slice()) {
            return Ok(());
        }
        if patch.candidate.is_none() {
            patch.candidate = Some(files::stage(
                &patch.before.namespace,
                Some(&patch.after),
                patch.mode,
            )?);
        }
        checkpoint(self)?;
        self.verify()?;
        let patch = self.patches.get(index).ok_or_else(invalid)?;
        files::publish_checked(
            &patch.before.namespace,
            &patch.before.state,
            patch.candidate.as_ref().ok_or_else(invalid)?,
            || self.verify(),
        )?;
        self.verify()
    }
}
fn validate_before(patch: &Patch, tree: &Tree) -> Result<()> {
    match (
        tree.entries.get(&patch.relative),
        &patch.before.state,
        &patch.before_bytes,
    ) {
        (None, State::Missing, None) if patch.mode == 0o644 => Ok(()),
        (
            Some(entry),
            State::File {
                identity,
                sha256,
                mode,
            },
            Some(raw),
        ) if *identity == entry.identity
            && *mode == entry.mode
            && patch.mode == *mode
            && *sha256 == files::hash(raw)
            && entry.content
                == (crate::policy::source_tree::Content::File {
                    sha256: sha256.clone(),
                    size: raw.len() as u64,
                }) =>
        {
            Ok(())
        }
        _ => Err(invalid()),
    }
}
fn validate_candidate(patch: &Patch, root: &Path, tree: &Tree) -> Result<()> {
    if let Some(parent) = &patch.parent
        && (patch.relative != "agents/openai.yaml"
            || tree.entries.contains_key("agents")
            || parent.target != root.join("agents")
            || parent.staged.parent() != Some(root)
            || !parent
                .staged
                .file_name()
                .and_then(|p| p.to_str())
                .is_some_and(|p| p.starts_with(".OP-") && p.ends_with(".cap-dir")))
    {
        return Err(invalid());
    }
    if let Some(candidate) = &patch.candidate {
        let stage = candidate.staged.as_ref().ok_or_else(invalid)?;
        if candidate.target != patch.before.path
            || stage.parent() != candidate.target.parent()
            || !stage
                .file_name()
                .and_then(|p| p.to_str())
                .is_some_and(|p| p.starts_with(".OP-") && p.ends_with(".cap.tmp"))
            || tree.entries.contains_key(
                stage
                    .strip_prefix(root)
                    .map_err(|_| invalid())?
                    .to_str()
                    .ok_or_else(invalid)?,
            )
            || !matches!(&candidate.after,State::File{sha256,mode,..} if *sha256==files::hash(&patch.after)&&*mode==patch.mode)
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/unit/native_skills_steps.rs"]
mod tests;
