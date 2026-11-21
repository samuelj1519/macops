use crate::{
    domain::FileId,
    error::{Error, Result},
    policy::{
        capability_tx::State,
        source_tree::{Change, Position, Slot},
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Object {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    pub uid: u32,
    pub mode: u32,
    pub size: u64,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub live: Slot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub retained: Slot,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub before: Option<Object>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub after: Object,
}
pub fn invalid() -> Error {
    Error::attention(
        "unverified_source_file_pair",
        "来源账本文件对的身份、完整属性或固定位置不能验证",
    )
}
impl Object {
    pub fn state(&self) -> State {
        State::File {
            identity: self.identity.clone(),
            sha256: self.sha256.clone(),
            mode: self.mode,
        }
    }
    fn validate(&self, owner: u32, device: u64) -> Result<()> {
        if self.identity.device != device
            || self.identity.inode == 0
            || self.uid != owner
            || self.mode > 0o777
            || self.size > 16 * 1024 * 1024
            || !crate::policy::config_local::valid_digest(&self.sha256)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Pair {
    pub fn validate(&self, owner: u32) -> Result<()> {
        self.live.validate(owner)?;
        self.retained.validate(owner)?;
        let device = self.live.parent.identity.device;
        self.after.validate(owner, device)?;
        let mut expected_lineage = self.live.parent.lineage.clone();
        expected_lineage.push(self.retained.parent.identity.clone());
        if self.retained.name != "ledger"
            || self.retained.parent.requested.parent() != Some(self.live.parent.requested.as_path())
            || self.retained.parent.lineage != expected_lineage
            || self
                .retained
                .parent
                .requested
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| !name.starts_with(".macops-source-ledger-"))
            || self.retained.parent.identity.device != device
            || self.retained.parent.mode != 0o700
        {
            return Err(invalid());
        }
        if let Some(before) = &self.before {
            before.validate(owner, device)?;
            if before.identity == self.after.identity {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
pub fn classify(pair: &Pair, live: Option<&Object>, retained: Option<&Object>) -> Position {
    if live == pair.before.as_ref() && retained == Some(&pair.after) {
        Position::Before
    } else if live == Some(&pair.after) && retained == pair.before.as_ref() {
        Position::OwnAfter
    } else {
        Position::Unknown
    }
}
pub fn validate_batch(pairs: &[Pair], roots: &[Change], owner: u32) -> Result<()> {
    if pairs.len() > 5 {
        return Err(invalid());
    }
    crate::policy::source_tree::validate_changes(roots, owner)?;
    let mut objects = BTreeSet::new();
    let mut slots = BTreeSet::new();
    let mut paths = vec![];
    let mut bytes = 0u64;
    for root in roots {
        paths.extend([root.live.path(), root.retained.path()]);
        for tree in [&root.before, &root.after] {
            for entry in tree.entries.values() {
                if let crate::policy::source_tree::Content::File { size, .. } = entry.content {
                    bytes += size;
                }
            }
        }
    }
    for pair in pairs {
        pair.validate(owner)?;
        for object in pair.before.iter().chain(std::iter::once(&pair.after)) {
            let key = (object.identity.device, object.identity.inode);
            if !objects.insert(key)
                || roots.iter().any(|root| {
                    [&root.before, &root.after].iter().any(|tree| {
                        tree.identity == object.identity
                            || tree
                                .entries
                                .values()
                                .any(|entry| entry.identity == object.identity)
                    })
                })
            {
                return Err(invalid());
            }
            bytes += object.size;
        }
        for slot in [&pair.live, &pair.retained] {
            if !slots.insert((
                slot.parent.identity.device,
                slot.parent.identity.inode,
                slot.name.to_ascii_lowercase(),
            )) {
                return Err(invalid());
            }
            paths.push(slot.path());
        }
    }
    if bytes > 128 * 1024 * 1024 {
        return Err(invalid());
    }
    for (index, path) in paths.iter().enumerate() {
        if paths
            .iter()
            .skip(index + 1)
            .any(|other| path.starts_with(other) || other.starts_with(path))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/source_files_policy.rs"]
mod tests;
