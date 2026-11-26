use crate::{
    error::{Error, Result},
    policy::profile::{
        deny_fields,
        profile_fields::{self, BoolField, ConfigProfile, Intent, ParentRecord},
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Parent {
    #[serde(rename = "enabledPlugins")]
    EnabledPlugins,
    #[serde(rename = "permissions")]
    Permissions,
    #[serde(rename = "permissions.deny")]
    PermissionsDeny,
    #[serde(rename = "skills")]
    Skills,
    #[serde(rename = "skills.config")]
    SkillsConfig,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodexField {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub enabled: BoolField,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub table: ParentRecord,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "format", rename_all = "snake_case", deny_unknown_fields)]
pub enum Fields {
    Claude {
        #[serde(deserialize_with = "crate::policy::json::object_map")]
        plugins: BTreeMap<String, BoolField>,
        #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
        pstack: Option<deny_fields::Record>,
    },
    Codex {
        #[serde(deserialize_with = "crate::policy::json::object_map")]
        skills: BTreeMap<PathBuf, CodexField>,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub target: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub parents: BTreeMap<Parent, ParentRecord>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub fields: Fields,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_profile_registry",
        "配置字段账本的路径、格式或 owner 关系无法验证",
    )
}
fn parent_valid(parent: &ParentRecord) -> bool {
    parent.created_by_tool == (parent.original == profile_fields::ParentState::Missing)
}
impl File {
    pub fn validate(&self) -> Result<()> {
        crate::policy::paths::lexical(&self.target)?;
        if self.parents.values().any(|p| !parent_valid(p)) {
            return Err(invalid());
        }
        match &self.fields {
            Fields::Claude { plugins, pstack } => {
                if (!plugins.is_empty() && !self.parents.contains_key(&Parent::EnabledPlugins))
                    || (pstack.is_some()
                        && (!self.parents.contains_key(&Parent::Permissions)
                            || !self.parents.contains_key(&Parent::PermissionsDeny)))
                {
                    return Err(invalid());
                }
                if self
                    .parents
                    .keys()
                    .any(|p| matches!(p, Parent::Skills | Parent::SkillsConfig))
                    || plugins.len() > 5000
                {
                    return Err(invalid());
                }
                for (id, field) in plugins {
                    if !crate::capability::rules::capability_local::valid_plugin_id(id) {
                        return Err(invalid());
                    }
                    field.validate()?;
                    if field.claims.values().any(|c| {
                        !matches!(c.owner.profile, ConfigProfile::Claude(_))
                            || c.owner.intent != Intent::Plugins
                    }) {
                        return Err(invalid());
                    }
                }
                if let Some(pstack) = pstack {
                    pstack.validate()?;
                }
            }
            Fields::Codex { skills } => {
                if !skills.is_empty()
                    && (!self.parents.contains_key(&Parent::Skills)
                        || !self.parents.contains_key(&Parent::SkillsConfig))
                {
                    return Err(invalid());
                }
                if self
                    .parents
                    .keys()
                    .any(|p| !matches!(p, Parent::Skills | Parent::SkillsConfig))
                    || skills.len() > 5000
                {
                    return Err(invalid());
                }
                for (path, field) in skills {
                    crate::policy::paths::lexical(path)?;
                    field.enabled.validate()?;
                    if !parent_valid(&field.table)
                        || field.enabled.claims.values().any(|c| {
                            c.owner.profile != ConfigProfile::Codex
                                || c.owner.intent != Intent::CodexSkills
                        })
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        Ok(())
    }
}
pub fn validate(files: &BTreeMap<PathBuf, File>) -> Result<()> {
    if files.len() > 512 {
        return Err(invalid());
    }
    for (path, file) in files {
        if path != &file.target {
            return Err(invalid());
        }
        file.validate()?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/profile_registry_policy.rs"]
mod tests;
