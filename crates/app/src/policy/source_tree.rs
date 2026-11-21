use crate::{
    domain::FileId,
    error::{Error, Result},
    policy::{config_local::valid_digest, governance::DirectoryBinding, source_git},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    Directory,
    File { sha256: String, size: u64 },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    pub uid: u32,
    pub mode: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub content: Content,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Tree {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    pub uid: u32,
    pub mode: u32,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub entries: BTreeMap<String, Entry>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ShapeEntry {
    pub uid: u32,
    pub mode: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub content: Content,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Shape {
    pub uid: u32,
    pub mode: u32,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub entries: BTreeMap<String, ShapeEntry>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub parent: DirectoryBinding,
    pub name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Change {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub live: Slot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub retained: Slot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub before: Tree,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub after: Tree,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Before,
    OwnAfter,
    Unknown,
}
fn invalid() -> Error {
    Error::attention(
        "unverified_source_tree",
        "来源目录的身份、范围、成员或权限无法验证",
    )
}
fn identity(id: &FileId) -> Result<()> {
    if id.device == 0 || id.inode == 0 {
        return Err(invalid());
    }
    Ok(())
}
fn metadata(uid: u32, mode: u32, directory: bool, owner: u32) -> Result<()> {
    if uid != owner || mode > 0o777 || (directory && mode & 0o022 != 0) {
        return Err(invalid());
    }
    Ok(())
}
impl Shape {
    pub fn validate(&self, owner: u32) -> Result<()> {
        metadata(self.uid, self.mode, true, owner)?;
        if self.entries.len() > 10000 {
            return Err(invalid());
        }
        let mut bytes = 0u64;
        for (path, entry) in &self.entries {
            let relative = source_git::RelativePath::parse(path)?;
            if relative.parts().is_empty() {
                return Err(invalid());
            }
            let directory = matches!(entry.content, Content::Directory);
            metadata(entry.uid, entry.mode, directory, owner)?;
            if let Some((parent, _)) = path.rsplit_once('/')
                && !self
                    .entries
                    .get(parent)
                    .is_some_and(|e| matches!(e.content, Content::Directory))
            {
                return Err(invalid());
            }
            if let Content::File { sha256, size } = &entry.content {
                if !valid_digest(sha256) || *size > 16 * 1024 * 1024 {
                    return Err(invalid());
                }
                bytes += *size;
                if bytes > 64 * 1024 * 1024 {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> Result<String> {
        crate::domain::fingerprint(&("macops-source-tree-v1", self))
    }
}
impl Tree {
    pub fn shape(&self) -> Shape {
        Shape {
            uid: self.uid,
            mode: self.mode,
            entries: self
                .entries
                .iter()
                .map(|(p, e)| {
                    (
                        p.clone(),
                        ShapeEntry {
                            uid: e.uid,
                            mode: e.mode,
                            content: e.content.clone(),
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn validate(&self, owner: u32) -> Result<()> {
        identity(&self.identity)?;
        self.shape().validate(owner)?;
        for entry in self.entries.values() {
            identity(&entry.identity)?;
            if entry.identity.device != self.identity.device {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
impl Slot {
    pub fn path(&self) -> PathBuf {
        self.parent.requested.join(&self.name)
    }
    pub fn validate(&self, owner: u32) -> Result<()> {
        crate::policy::paths::lexical(&self.parent.requested)?;
        source_git::Component::parse(&self.name)?;
        identity(&self.parent.identity)?;
        metadata(self.parent.uid, self.parent.mode, true, owner)?;
        if self.parent.canonical != self.parent.requested
            || self.parent.lineage.last() != Some(&self.parent.identity)
            || self.parent.lineage.len() > 64
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn project(&self, tree: &Tree) -> DirectoryBinding {
        let mut lineage = self.parent.lineage.clone();
        lineage.push(tree.identity.clone());
        DirectoryBinding {
            requested: self.path(),
            canonical: self.path(),
            identity: tree.identity.clone(),
            lineage,
            uid: tree.uid,
            mode: tree.mode,
        }
    }
}
pub fn classify(change: &Change, live: Option<&Tree>, retained: Option<&Tree>) -> Position {
    if live == Some(&change.before) && retained == Some(&change.after) {
        Position::Before
    } else if live == Some(&change.after) && retained == Some(&change.before) {
        Position::OwnAfter
    } else {
        Position::Unknown
    }
}
pub fn validate_changes(changes: &[Change], owner: u32) -> Result<()> {
    if changes.len() > 128 {
        return Err(invalid());
    }
    let mut targets = vec![];
    let mut root_ids = std::collections::BTreeSet::new();
    let mut entries = 0;
    let mut bytes = 0;
    for change in changes {
        change.live.validate(owner)?;
        change.retained.validate(owner)?;
        change.before.validate(owner)?;
        change.after.validate(owner)?;
        if change.before.identity == change.after.identity
            || change.live.parent.identity.device != change.retained.parent.identity.device
            || change.before.identity.device != change.live.parent.identity.device
            || change.after.identity.device != change.live.parent.identity.device
        {
            return Err(invalid());
        }
        for tree in [&change.before, &change.after] {
            if !root_ids.insert((tree.identity.device, tree.identity.inode)) {
                return Err(invalid());
            }
            entries += tree.entries.len();
            for entry in tree.entries.values() {
                if let Content::File { size, .. } = entry.content {
                    bytes += size;
                }
            }
        }
        if entries > 50000 || bytes > 128 * 1024 * 1024 {
            return Err(invalid());
        }
        targets.push(change.live.path());
        targets.push(change.retained.path());
    }
    for (i, a) in targets.iter().enumerate() {
        if targets
            .iter()
            .skip(i + 1)
            .any(|b| a.starts_with(b) || b.starts_with(a))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/source_tree_policy.rs"]
mod tests;
