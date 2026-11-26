use crate::{
    domain::FileId,
    error::{Error, Result},
    policy::{
        json_fields::{self, Edit},
        source_git::{Component, RelativePath, Repository},
        source_tree::{Shape, Slot, Tree},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};
const KEY: &str = "_macops_sources";
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub member: String,
    pub repository: String,
    pub skill_path: String,
    pub canonical_name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub requested_ref: Option<String>,
    pub commit: String,
    pub skill_tree: String,
    pub base_sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub source: Identity,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub slot: Slot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub root_identity: FileId,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub shape: Shape,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub revision: Option<Revision>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub roots: BTreeMap<String, Baseline>,
}
fn invalid() -> Error {
    Error::attention(
        "unverified_source_baseline",
        "完整来源基线缺失、损坏或范围不匹配",
    )
}
impl Identity {
    pub fn validate(&self) -> Result<()> {
        if self.member.is_empty()
            || self.member.len() > 256
            || self.member.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        if Repository::parse(&self.repository)?.key() != self.repository {
            return Err(invalid());
        }
        RelativePath::parse(&self.skill_path)?;
        Component::parse(&self.canonical_name)?;
        Ok(())
    }
}
impl Baseline {
    pub fn capture(
        source: Identity,
        slot: Slot,
        tree: &Tree,
        home: &Path,
        owner: u32,
    ) -> Result<Self> {
        tree.validate(owner)?;
        let baseline = Self {
            source,
            slot,
            root_identity: tree.identity.clone(),
            shape: tree.shape(),
            revision: None,
        };
        baseline.validate(home, owner)?;
        Ok(baseline)
    }
    pub fn validate(&self, home: &Path, owner: u32) -> Result<()> {
        self.source.validate()?;
        if let Some(revision) = &self.revision {
            crate::policy::source_git::Reference::parse(revision.requested_ref.as_deref())?;
            crate::policy::source_git::ObjectId::parse(&revision.commit)?;
            crate::policy::source_git::ObjectId::parse(&revision.skill_tree)?;
            if !crate::policy::config_local::valid_digest(&revision.base_sha256) {
                return Err(invalid());
            }
        }
        self.slot.validate(owner)?;
        self.shape.validate(owner)?;
        if self.root_identity.device == 0
            || self.root_identity.inode == 0
            || self.root_identity.device != self.slot.parent.identity.device
            || self.slot.path()
                != crate::policy::paths::lexical(home)?
                    .join(".agents/skills")
                    .join(&self.source.canonical_name)
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn authorize(
        &self,
        source: &Identity,
        slot: &Slot,
        observed: &Tree,
        home: &Path,
        owner: u32,
    ) -> Result<()> {
        self.validate(home, owner)?;
        observed.validate(owner)?;
        if &self.source != source
            || &self.slot != slot
            || self.root_identity != observed.identity
            || self.shape != observed.shape()
        {
            return Err(Error::attention(
                "source_baseline_drift",
                "来源根已改变；未随升级或自身元数据变更重新采纳",
            ));
        }
        Ok(())
    }
}
fn record(value: &Value, home: &Path, owner: u32) -> Result<Record> {
    if !value.is_object() {
        return Err(invalid());
    }
    let Some(namespace) = value.get(KEY) else {
        return Ok(Record {
            schema_version: 1,
            roots: BTreeMap::new(),
        });
    };
    let crate::policy::json::Object(book) =
        serde_json::from_value::<crate::policy::json::Object<Record>>(namespace.clone())?;
    if book.schema_version != 1 || book.roots.len() > 128 {
        return Err(invalid());
    }
    let mut slots = std::collections::BTreeSet::new();
    let mut root_ids = std::collections::BTreeSet::new();
    for (member, baseline) in &book.roots {
        baseline.validate(home, owner)?;
        if !slots.insert(baseline.slot.path())
            || !root_ids.insert((baseline.root_identity.device, baseline.root_identity.inode))
        {
            return Err(invalid());
        }
        if member != &baseline.source.member {
            return Err(invalid());
        }
    }
    Ok(book)
}
pub fn validate_embedded(value: &Value, home: &Path, owner: u32) -> Result<()> {
    record(value, home, owner).map(|_| ())
}

pub fn read(raw: Option<&[u8]>, home: &Path, owner: u32) -> Result<Record> {
    let value = raw
        .map(json_fields::parse_governance)
        .transpose()?
        .unwrap_or_else(|| serde_json::json!({"assets":{}}));
    record(&value, home, owner)
}
pub fn adopt(raw: Option<&[u8]>, baseline: Baseline, home: &Path, owner: u32) -> Result<Vec<u8>> {
    baseline.validate(home, owner)?;
    let mut book = read(raw, home, owner)?;
    if book.roots.values().any(|b| {
        b.source.member != baseline.source.member
            && (b.slot.path() == baseline.slot.path() || b.root_identity == baseline.root_identity)
    }) {
        return Err(Error::attention(
            "source_baseline_owner_conflict",
            "同一来源根已有另一成员的完整基线，需先审查归属",
        ));
    }
    book.roots.insert(baseline.source.member.clone(), baseline);
    if book.roots.len() > 128 {
        return Err(invalid());
    }
    let value = serde_json::to_value(book)?;
    json_fields::apply_governance(
        raw.unwrap_or(br#"{"assets":{}}"#),
        &[Edit {
            path: vec![KEY.into()],
            value: Some(value),
        }],
    )
}

#[cfg(test)]
#[path = "../../../tests/unit/source_baseline_policy.rs"]
mod tests;

pub fn project(
    baseline: &Baseline,
    changes: &[crate::capability::rules::capability_tx::FileChange],
    home: &Path,
    owner: u32,
) -> Result<Baseline> {
    use crate::{
        capability::rules::{capability_tx::State, delivery::ChangeKind},
        policy::source_tree::{Content, ShapeEntry},
    };
    baseline.validate(home, owner)?;
    let mut result = baseline.clone();
    let root = baseline.slot.path();
    let mut seen = std::collections::BTreeSet::new();
    for change in changes {
        let Ok(relative) = change.path.strip_prefix(&root) else {
            continue;
        };
        if !matches!(
            change.kind,
            ChangeKind::Frontmatter | ChangeKind::CodexPolicy
        ) || !seen.insert(&change.path)
        {
            return Err(invalid());
        }
        let path = relative.to_str().ok_or_else(invalid)?;
        RelativePath::parse(path)?;
        match (&change.before, baseline.shape.entries.get(path)) {
            (State::Missing, None) => (),
            (State::File { sha256, mode, .. }, Some(entry))
                if entry.mode == *mode
                    && matches!(&entry.content,Content::File{sha256:expected,..} if expected==sha256) =>
                {}
            _ => return Err(invalid()),
        }
        match (&change.after_blob, change.after_mode) {
            (None, None) => {
                result.shape.entries.remove(path);
            }
            (Some(blob), Some(mode))
                if crate::capability::rules::capability_tx::blob_valid(blob)
                    && blob.size <= 16 * 1024 * 1024
                    && mode <= 0o777 =>
            {
                if let Some((parent, _)) = path.rsplit_once('/')
                    && !result.shape.entries.contains_key(parent)
                {
                    if change.namespace.remaining.len() != 2
                        || change.path.parent().and_then(Path::parent)
                            != Some(change.namespace.ancestor.requested.as_path())
                    {
                        return Err(invalid());
                    }
                    result.shape.entries.insert(
                        parent.into(),
                        ShapeEntry {
                            uid: owner,
                            mode: 0o700,
                            content: Content::Directory,
                        },
                    );
                }
                result.shape.entries.insert(
                    path.into(),
                    ShapeEntry {
                        uid: owner,
                        mode,
                        content: Content::File {
                            sha256: blob.sha256.clone(),
                            size: blob.size,
                        },
                    },
                );
            }
            _ => return Err(invalid()),
        }
    }
    result.validate(home, owner)?;
    Ok(result)
}

pub fn disjoint(root: &Path, reserved: &[std::path::PathBuf]) -> Result<()> {
    crate::policy::paths::lexical(root)?;
    for path in reserved {
        crate::policy::paths::lexical(path)?;
        if root.starts_with(path) || path.starts_with(root) {
            return Err(Error::attention(
                "source_path_overlap",
                "来源载荷与状态、配置、缓存或恢复目录重叠",
            ));
        }
    }
    Ok(())
}
