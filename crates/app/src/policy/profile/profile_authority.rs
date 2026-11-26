use crate::{
    config::rules::config_tx::ConfigRootInputs,
    error::{Error, Result},
    policy::{
        governance::DirectoryBinding,
        profile::profile_fields::{ConfigProfile, Intent, Owner, Role},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaticSource {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub root: DirectoryBinding,
    pub destination: PathBuf,
    pub relative: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Client {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub owner: Owner,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub live_root: DirectoryBinding,
    pub live: PathBuf,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub source: Option<StaticSource>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub clients: BTreeMap<String, Client>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub mapping_inputs: ConfigRootInputs,
    pub isolated: bool,
    pub codex_root: Option<PathBuf>,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub targets: BTreeMap<PathBuf, Target>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_profile_write_authority",
        "profile 文件、owner 或静态来源映射不一致",
    )
}
fn binding(binding: &DirectoryBinding) -> Result<()> {
    crate::policy::paths::lexical(&binding.requested)?;
    if binding.canonical != binding.requested
        || binding.identity.device == 0
        || binding.identity.inode == 0
        || binding.lineage.last() != Some(&binding.identity)
        || binding.lineage.len() > 64
        || binding.mode > 0o7777
    {
        return Err(invalid());
    }
    Ok(())
}
pub fn source_target(source: &StaticSource) -> Result<(PathBuf, PathBuf)> {
    binding(&source.root)?;
    crate::policy::paths::lexical(&source.destination)?;
    let parts: Vec<_> = source.relative.components().collect();
    if source.relative.is_absolute() || parts.is_empty() || parts.len() > 32 {
        return Err(invalid());
    }
    let mut mapped = source.destination.clone();
    for (index, part) in parts.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err(invalid());
        };
        let name = name.to_str().ok_or_else(invalid)?;
        let target = crate::config::rules::chezmoi::source_name(name, index + 1 < parts.len())?
            .ok_or_else(invalid)?;
        if target.template || target.symlink {
            return Err(invalid());
        }
        mapped.push(target.target);
    }
    Ok((source.root.requested.join(&source.relative), mapped))
}
impl Authority {
    pub fn validate(&self, home: &Path) -> Result<()> {
        if self.targets.len() > 512 {
            return Err(invalid());
        }
        if let Some(root) = &self.codex_root {
            crate::policy::paths::lexical(root)?;
        }
        for (path, target) in &self.targets {
            crate::policy::paths::lexical(path)?;
            if target.clients.is_empty() || target.clients.len() > 10000 {
                return Err(invalid());
            }
            for (id, client) in &target.clients {
                client.owner.validate()?;
                if client.owner.id() != *id {
                    return Err(invalid());
                }
                binding(&client.live_root)?;
                let file = match client.owner.profile {
                    ConfigProfile::Claude(profile) => {
                        if client.live_root.requested != profile.root(home)? {
                            return Err(invalid());
                        }
                        "settings.json"
                    }
                    ConfigProfile::Codex => {
                        if Some(&client.live_root.requested) != self.codex_root.as_ref() {
                            return Err(invalid());
                        }
                        "config.toml"
                    }
                };
                if client.live != client.live_root.requested.join(file) {
                    return Err(invalid());
                }
                match client.owner.role {
                    Role::Live => {
                        if path != &client.live || client.source.is_some() {
                            return Err(invalid());
                        }
                    }
                    Role::Source => {
                        let (source, live) =
                            source_target(client.source.as_ref().ok_or_else(invalid)?)?;
                        if source != *path || live != client.live {
                            return Err(invalid());
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub fn target_kind(
        &self,
        path: &Path,
    ) -> Result<crate::capability::rules::delivery::ChangeKind> {
        let target = self.targets.get(path).ok_or_else(invalid)?;
        let mut kind = None;
        for client in target.clients.values() {
            let next = match client.owner.profile {
                ConfigProfile::Claude(_) => {
                    crate::capability::rules::delivery::ChangeKind::ProfileJson
                }
                ConfigProfile::Codex => crate::capability::rules::delivery::ChangeKind::ProfileToml,
            };
            if kind.is_some_and(|old| old != next) {
                return Err(invalid());
            }
            kind = Some(next);
            if !matches!(
                (client.owner.profile, client.owner.intent),
                (ConfigProfile::Claude(_), Intent::Plugins | Intent::Pstack)
                    | (ConfigProfile::Codex, Intent::CodexSkills)
            ) {
                return Err(invalid());
            }
        }
        kind.ok_or_else(invalid)
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/profile_authority_policy.rs"]
mod tests;
