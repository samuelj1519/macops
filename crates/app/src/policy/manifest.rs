use crate::{
    domain::{Kind, Provider},
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_ENTRIES: usize = 4096;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    BrewFormula,
    BrewCask,
    Npm,
    Mise,
    Bun,
    Cargo,
    Uv,
    Chezmoi,
    Capability,
    Other,
}

pub fn channel(provider: &Provider) -> Channel {
    match provider {
        Provider::BrewFormula { .. } | Provider::Brew { cask: false, .. } => Channel::BrewFormula,
        Provider::Brew { cask: true, .. } => Channel::BrewCask,
        Provider::Npm { .. } | Provider::NpmObserved { .. } => Channel::Npm,
        Provider::Mise { .. } => Channel::Mise,
        Provider::Bun { .. } => Channel::Bun,
        Provider::Cargo { .. } => Channel::Cargo,
        Provider::Uv { .. } | Provider::UvObserved { .. } => Channel::Uv,
        Provider::ConfigObserved { .. } => Channel::Chezmoi,
        Provider::CapabilityObserved { .. } => Channel::Capability,
        _ => Channel::Other,
    }
}

pub fn supports_replay(provider: &Provider) -> bool {
    matches!(
        provider,
        Provider::Npm {
            authority: Some(crate::domain::NpmWriteAuthority {
                registry: Some(_),
                ..
            }),
            ..
        }
    )
}

pub fn concrete_npm_version(value: &str) -> bool {
    let (core, pre) = value
        .split_once('-')
        .map_or((value, None), |(v, p)| (v, Some(p)));
    let mut parts = core.split('.');
    (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            decimal(part)
                && part
                    .parse::<u64>()
                    .is_ok_and(|n| n <= 9_007_199_254_740_991)
        })
    }) && parts.next().is_none()
        && pre.is_none_or(prerelease)
}

fn decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
}

fn prerelease(value: &str) -> bool {
    value.split('.').all(|word| {
        !word.is_empty()
            && word.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && (!word.bytes().all(|b| b.is_ascii_digit()) || decimal(word))
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub kind: Kind,
    pub channel: Channel,
    pub recipe_sha256: String,
    pub scope_sha256: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub entries: Vec<Entry>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Aligned,
    Unregistered,
    RecipeChanged,
    ScopeChanged,
    NeedsNativeObservation,
    VersionDifference,
}

fn invalid() -> Error {
    Error::new("manifest_invalid", "清单格式、身份、摘要或版本不符合约束")
}

pub fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+~-".contains(&byte))
}

pub fn validate(manifest: &Manifest) -> Result<()> {
    use crate::policy::config_local::valid_digest;
    if manifest.schema_version != 1 || manifest.entries.len() > MAX_ENTRIES {
        return Err(invalid());
    }
    let mut ids = BTreeSet::new();
    for entry in &manifest.entries {
        if entry.id.is_empty()
            || entry.id.len() > 512
            || entry.id.chars().any(char::is_control)
            || !ids.insert(entry.id.as_str())
            || !valid_digest(&entry.recipe_sha256)
            || entry
                .scope_sha256
                .as_deref()
                .is_some_and(|value| !valid_digest(value))
            || entry
                .version
                .as_deref()
                .is_some_and(|value| !valid_version(value))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Replay {
    ReadOnly,
    Inconsistent,
    NeedsIntent,
    VerifyNativeState,
}

#[derive(Debug, Serialize)]
pub struct Row<'a> {
    pub id: &'a str,
    pub status: Status,
    pub replay: Replay,
    pub current_version: Option<&'a str>,
    pub requested_version: Option<&'a str>,
}

pub fn plan<'a>(
    requested: &'a Manifest,
    current: &'a BTreeMap<String, Entry>,
    replayable: &BTreeSet<String>,
    selected_scope: &str,
) -> Vec<Row<'a>> {
    requested
        .entries
        .iter()
        .map(|entry| {
            let found = current.get(&entry.id);
            let status = match found {
                None => Status::Unregistered,
                Some(local)
                    if local.channel != entry.channel
                        || local.kind != entry.kind
                        || local.recipe_sha256 != entry.recipe_sha256 =>
                {
                    Status::RecipeChanged
                }
                Some(local)
                    if entry.scope_sha256.as_deref().is_some_and(|wanted| {
                        wanted != selected_scope
                            || local
                                .scope_sha256
                                .as_deref()
                                .is_some_and(|recorded| wanted != recorded)
                    }) =>
                {
                    Status::ScopeChanged
                }
                Some(local)
                    if local.scope_sha256.is_none()
                        && (entry.scope_sha256.is_some() || local.version.is_some())
                        || local.version.is_none() && entry.version.is_some() =>
                {
                    Status::NeedsNativeObservation
                }
                Some(local) if entry.version.is_some() && local.version != entry.version => {
                    Status::VersionDifference
                }
                Some(_) => Status::Aligned,
            };
            let replay = if !replayable.contains(&entry.id) {
                Replay::ReadOnly
            } else if matches!(status, Status::RecipeChanged | Status::ScopeChanged) {
                Replay::Inconsistent
            } else if entry.scope_sha256.is_none()
                || !entry.version.as_deref().is_some_and(concrete_npm_version)
            {
                Replay::NeedsIntent
            } else {
                Replay::VerifyNativeState
            };
            Row {
                id: &entry.id,
                status,
                replay,
                current_version: found.and_then(|local| local.version.as_deref()),
                requested_version: entry.version.as_deref(),
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "../../tests/unit/manifest.rs"]
mod tests;
