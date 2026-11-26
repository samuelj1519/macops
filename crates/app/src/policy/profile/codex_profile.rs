use crate::{
    error::{Error, Result},
    policy::profile::{
        codex_fields,
        profile_fields::{self, ConfigProfile, Intent, Owner, ParentRecord, Presence},
        profile_registry::{self, CodexField, Fields, Parent},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct Request {
    pub owner: Owner,
    pub intent_sha256: String,
    pub disabled: Option<BTreeSet<PathBuf>>,
}
#[derive(Debug)]
pub struct Proposal {
    pub after: Vec<u8>,
    pub record: Option<profile_registry::File>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_codex_profile_fields",
        "Codex 字段意图或原值无法验证",
    )
}
fn requested_paths(
    old: &BTreeMap<PathBuf, CodexField>,
    requests: &[Request],
) -> Result<BTreeSet<PathBuf>> {
    let mut ids = BTreeSet::new();
    let mut paths: BTreeSet<_> = old.keys().cloned().collect();
    for request in requests {
        request.owner.validate()?;
        if request.owner.profile != ConfigProfile::Codex
            || request.owner.intent != Intent::CodexSkills
            || !crate::policy::config_local::valid_digest(&request.intent_sha256)
            || !ids.insert(request.owner.id())
        {
            return Err(invalid());
        }
        if let Some(disabled) = &request.disabled {
            if disabled.len() > 5000 {
                return Err(invalid());
            }
            for path in disabled {
                crate::policy::paths::lexical(path)?;
                paths.insert(path.clone());
            }
        }
    }
    Ok(paths)
}
pub fn plan(
    target: &Path,
    raw: &[u8],
    previous: Option<&profile_registry::File>,
    requests: &[Request],
) -> Result<Proposal> {
    crate::policy::paths::lexical(target)?;
    let observed = codex_fields::observe(raw)?;
    let (old, mut parents) = if let Some(previous) = previous {
        previous.validate()?;
        if previous.target != target {
            return Err(invalid());
        }
        let Fields::Codex { skills } = &previous.fields else {
            return Err(invalid());
        };
        (skills.clone(), previous.parents.clone())
    } else {
        (BTreeMap::new(), BTreeMap::new())
    };
    if requests.is_empty() {
        return Ok(Proposal {
            after: raw.into(),
            record: previous.cloned(),
        });
    }
    let paths = requested_paths(&old, requests)?;
    let mut skills = BTreeMap::new();
    let mut changes = vec![];
    for path in paths {
        let text = path.to_str().ok_or_else(invalid)?;
        let current = observed
            .entries
            .get(text)
            .map_or(Presence::Missing, |s| s.enabled.clone());
        let mut selected = BTreeMap::new();
        for request in requests {
            let claim = if request.disabled.as_ref().is_some_and(|s| s.contains(&path)) {
                Some(profile_fields::Claim {
                    owner: request.owner.clone(),
                    intent_sha256: request.intent_sha256.clone(),
                    desired: false,
                })
            } else {
                None
            };
            selected.insert(request.owner.id(), claim);
        }
        let proposal =
            profile_fields::merge(&current, old.get(&path).map(|r| &r.enabled), &selected)?;
        let table = old.get(&path).map_or_else(
            || {
                ParentRecord::capture(if observed.entries.contains_key(text) {
                    profile_fields::ParentState::PresentNonEmpty
                } else {
                    profile_fields::ParentState::Missing
                })
            },
            |r| r.table.clone(),
        );
        let remove = proposal.record.is_none()
            && table.created_by_tool
            && proposal.after == Presence::Missing;
        if proposal.after != current || remove {
            changes.push(codex_fields::Change {
                path: text.into(),
                enabled: proposal.after.clone(),
                remove_created_table: remove,
            });
        }
        if let Some(enabled) = proposal.record {
            skills.insert(path, CodexField { enabled, table });
        }
    }
    if !skills.is_empty() {
        parents
            .entry(Parent::Skills)
            .or_insert_with(|| ParentRecord::capture(observed.skills_parent));
        parents
            .entry(Parent::SkillsConfig)
            .or_insert_with(|| ParentRecord::capture(observed.config_parent));
    }
    let after = codex_fields::patch(raw, &changes)?;
    let after = codex_fields::prune_parents(&after, &parents, !skills.is_empty())?;
    let record = if skills.is_empty() {
        None
    } else {
        Some(profile_registry::File {
            target: target.into(),
            parents,
            fields: Fields::Codex { skills },
        })
    };
    if let Some(record) = &record {
        record.validate()?;
    }
    Ok(Proposal { after, record })
}
#[cfg(test)]
#[path = "../../../tests/unit/codex_profile_policy.rs"]
mod tests;
