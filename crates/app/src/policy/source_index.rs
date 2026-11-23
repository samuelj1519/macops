use crate::{
    error::{Error, Result},
    policy::{
        config_local::valid_digest,
        json_fields,
        source_git::{ObjectId, Reference, Repository},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
const FORMAT: &str = "macops-source-index-v1";
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub home: PathBuf,
    pub native_lock: PathBuf,
    pub definitions: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub member: String,
    pub repository: String,
    pub locator_sha256: String,
    pub requested_ref: Option<String>,
    pub commit: String,
    pub skill_tree: String,
    pub payload_sha256: String,
    pub checked_at: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema_version: u32,
    pub format: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub scope: Scope,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub entries: BTreeMap<String, Candidate>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_source_index",
        "来源索引格式、作用域或候选依据不能验证",
    )
}
impl Scope {
    pub fn validate(&self) -> Result<()> {
        for path in [&self.home, &self.native_lock, &self.definitions] {
            crate::policy::paths::lexical(path)?;
        }
        Ok(())
    }
}
impl Candidate {
    pub fn validate(&self, now: u64) -> Result<()> {
        if self.member.is_empty()
            || self.member.len() > 256
            || self.member.chars().any(char::is_control)
            || Repository::parse(&self.repository)?.key() != self.repository
            || !valid_digest(&self.locator_sha256)
            || !valid_digest(&self.payload_sha256)
            || self.checked_at == 0
            || self.checked_at > now
        {
            return Err(invalid());
        }
        Reference::parse(self.requested_ref.as_deref())?;
        ObjectId::parse(&self.commit)?;
        ObjectId::parse(&self.skill_tree)?;
        Ok(())
    }
}
impl Index {
    pub fn empty(scope: Scope) -> Result<Self> {
        scope.validate()?;
        Ok(Self {
            schema_version: 1,
            format: FORMAT.into(),
            scope,
            entries: BTreeMap::new(),
        })
    }
    pub fn validate(&self, scope: &Scope, now: u64) -> Result<()> {
        scope.validate()?;
        self.scope.validate()?;
        if self.schema_version != 1
            || self.format != FORMAT
            || &self.scope != scope
            || self.entries.len() > 128
        {
            return Err(invalid());
        }
        for (member, candidate) in &self.entries {
            candidate.validate(now)?;
            if member != &candidate.member {
                return Err(invalid());
            }
        }
        Ok(())
    }
    pub fn merge(&self, scope: &Scope, candidates: Vec<Candidate>, now: u64) -> Result<Self> {
        self.validate(scope, now)?;
        let mut next = self.clone();
        let mut seen = std::collections::BTreeSet::new();
        for candidate in candidates {
            candidate.validate(now)?;
            if !seen.insert(candidate.member.clone()) {
                return Err(invalid());
            }
            next.entries.insert(candidate.member.clone(), candidate);
        }
        next.validate(scope, now)?;
        let raw = serde_json::to_vec(&next)?;
        json_fields::parse_governance(&raw)?;
        Ok(next)
    }
    pub fn get(&self, scope: &Scope, member: &str, locator: &str, now: u64) -> Result<&Candidate> {
        self.validate(scope, now)?;
        let candidate = self.entries.get(member).ok_or_else(|| {
            Error::attention(
                "source_candidate_missing",
                "所选来源尚无可验证候选，请先运行 mo skill refresh",
            )
        })?;
        if candidate.locator_sha256 != locator {
            return Err(Error::attention(
                "source_candidate_stale",
                "来源或交付声明已经改变，请先运行 mo skill refresh",
            ));
        }
        Ok(candidate)
    }
}
pub fn read(raw: &[u8], scope: &Scope, now: u64) -> Result<Index> {
    let value = json_fields::parse_governance(raw)?;
    let crate::policy::json::Object(index) =
        serde_json::from_value::<crate::policy::json::Object<Index>>(value)?;
    index.validate(scope, now)?;
    Ok(index)
}
pub fn legacy(raw: &[u8]) -> Result<bool> {
    let value = json_fields::parse_governance(raw)?;
    let Some(row) = value.as_object() else {
        return Ok(false);
    };
    let keys = [
        "schema_version",
        "sources_sha256",
        "checked_at",
        "checked_at_by_asset",
        "revisions",
    ];
    Ok(row.len() == keys.len()
        && row.keys().all(|k| keys.contains(&k.as_str()))
        && row.get("schema_version").and_then(Value::as_u64) == Some(1)
        && row
            .get("sources_sha256")
            .and_then(Value::as_str)
            .is_some_and(valid_digest)
        && row.get("checked_at").is_some_and(Value::is_u64)
        && row
            .get("checked_at_by_asset")
            .and_then(Value::as_object)
            .is_some_and(|m| m.values().all(Value::is_u64))
        && row
            .get("revisions")
            .and_then(Value::as_object)
            .is_some_and(|m| m.values().all(Value::is_object)))
}
#[cfg(test)]
#[path = "../../tests/unit/source_index_policy.rs"]
mod tests;
