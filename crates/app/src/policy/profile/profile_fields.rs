use crate::{
    capability::rules::delivery::Profile,
    error::{Error, Result},
    policy::config_local::valid_digest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Presence<T> {
    Missing,
    Present(T),
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Live,
    Source,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(
    tag = "harness",
    content = "profile",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ConfigProfile {
    Claude(Profile),
    Codex,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Intent {
    Plugins,
    Pstack,
    CodexSkills,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub profile: ConfigProfile,
    pub role: Role,
    pub intent: Intent,
}
impl Owner {
    pub fn id(&self) -> String {
        let profile = match self.profile {
            ConfigProfile::Claude(profile) => format!("claude-{}", profile.name()),
            ConfigProfile::Codex => "codex".into(),
        };
        let role = match self.role {
            Role::Live => "live",
            Role::Source => "source",
        };
        let intent = match self.intent {
            Intent::Plugins => "plugins",
            Intent::Pstack => "pstack",
            Intent::CodexSkills => "codex-skills",
        };
        format!("profile-policy:{profile}:{role}:{intent}")
    }
    pub fn validate(&self) -> Result<()> {
        if matches!(
            (self.profile, self.intent),
            (ConfigProfile::Codex, Intent::Plugins | Intent::Pstack)
                | (ConfigProfile::Claude(_), Intent::CodexSkills)
        ) {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub owner: Owner,
    pub intent_sha256: String,
    pub desired: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BoolField {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub original: Presence<bool>,
    pub applied: bool,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub claims: BTreeMap<String, Claim>,
}
#[derive(Debug)]
pub struct Proposal {
    pub after: Presence<bool>,
    pub record: Option<BoolField>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_profile_field_record",
        "字段所有权、声明或原值记录不一致",
    )
}
impl Claim {
    fn validate(&self, id: &str) -> Result<()> {
        self.owner.validate()?;
        if !valid_digest(&self.intent_sha256) || self.owner.id() != id {
            return Err(invalid());
        }
        Ok(())
    }
}
impl BoolField {
    pub fn validate(&self) -> Result<()> {
        if self.claims.is_empty() || self.claims.len() > 10000 {
            return Err(invalid());
        }
        for (id, claim) in &self.claims {
            claim.validate(id)?;
            if claim.desired != self.applied {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
pub fn merge(
    current: &Presence<bool>,
    previous: Option<&BoolField>,
    selected: &BTreeMap<String, Option<Claim>>,
) -> Result<Proposal> {
    if let Some(previous) = previous {
        previous.validate()?;
    }
    if selected.len() > 10000 {
        return Err(invalid());
    }
    if selected.is_empty() {
        return Ok(Proposal {
            after: current.clone(),
            record: previous.cloned(),
        });
    }
    if let Some(previous) = previous
        && *current != previous.original
        && *current != Presence::Present(previous.applied)
    {
        return Err(Error::attention(
            "profile_owned_field_drift",
            "自有配置字段与原值及已应用值均不符，保留用户修改",
        ));
    }
    let mut claims = previous.map_or_default(|p| p.claims.clone());
    for (id, change) in selected {
        if let Some(change) = change {
            change.validate(id)?;
            claims.insert(id.clone(), change.clone());
        } else {
            claims.remove(id);
        }
    }
    let Some(applied) = claims.values().next().map(|c| c.desired) else {
        return Ok(Proposal {
            after: previous.map_or_else(|| current.clone(), |p| p.original.clone()),
            record: None,
        });
    };
    if claims.values().any(|c| c.desired != applied) {
        return Err(Error::attention(
            "profile_field_claim_conflict",
            "同一配置字段存在冲突 owner 意图，未建立变更",
        ));
    }
    let original = previous.map_or_else(|| current.clone(), |p| p.original.clone());
    let record = BoolField {
        original,
        applied,
        claims,
    };
    Ok(Proposal {
        after: Presence::Present(applied),
        record: Some(record),
    })
}

#[cfg(test)]
#[path = "../../../tests/unit/profile_fields_policy.rs"]
mod tests;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParentState {
    Missing,
    PresentEmpty,
    PresentNonEmpty,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ParentRecord {
    pub original: ParentState,
    pub created_by_tool: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParentAction {
    Keep,
    Remove,
}
impl ParentRecord {
    pub fn capture(original: ParentState) -> Self {
        Self {
            original,
            created_by_tool: original == ParentState::Missing,
        }
    }
    pub fn after(&self, current: ParentState, has_claims: bool) -> Result<ParentAction> {
        if self.created_by_tool != (self.original == ParentState::Missing) {
            return Err(invalid());
        }
        if !has_claims && self.created_by_tool && current == ParentState::PresentEmpty {
            Ok(ParentAction::Remove)
        } else {
            Ok(ParentAction::Keep)
        }
    }
}
