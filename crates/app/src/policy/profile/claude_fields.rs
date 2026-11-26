use crate::{
    error::{Error, Result},
    policy::{
        json_fields::{self, Edit},
        profile::{
            profile_fields::{
                self, BoolField, ConfigProfile, Intent, Owner, ParentAction, ParentRecord,
                ParentState, Presence,
            },
            profile_registry::{self, Fields, Parent},
        },
    },
};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
#[derive(Clone, Debug)]
pub enum Desired {
    Plugins(BTreeSet<String>),
    Pstack(Vec<String>),
}
#[derive(Clone, Debug)]
pub struct Request {
    pub owner: Owner,
    pub intent_sha256: String,
    pub desired: Option<Desired>,
}
#[derive(Debug)]
pub struct Proposal {
    pub after: Vec<u8>,
    pub record: Option<profile_registry::File>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_claude_profile_fields",
        "Claude 字段意图或原值无法验证",
    )
}
fn requests_valid(requests: &[Request]) -> Result<()> {
    let mut owners = BTreeSet::new();
    if requests.len() > 10000 {
        return Err(invalid());
    }
    for request in requests {
        request.owner.validate()?;
        if !matches!(request.owner.profile, ConfigProfile::Claude(_))
            || !crate::policy::config_local::valid_digest(&request.intent_sha256)
            || !owners.insert(request.owner.id())
        {
            return Err(invalid());
        }
        match (&request.desired, request.owner.intent) {
            (Some(Desired::Plugins(plugins)), Intent::Plugins) => {
                if plugins.len() > 5000
                    || plugins.iter().any(|id| {
                        !crate::capability::rules::capability_local::valid_plugin_id(id)
                            || id.to_ascii_lowercase().starts_with("devnorm")
                    })
                {
                    return Err(invalid());
                }
            }
            (Some(Desired::Pstack(entries)), Intent::Pstack) => {
                if entries.len() > 5000
                    || entries
                        .iter()
                        .any(|s| !crate::policy::profile::deny_fields::owned(s))
                {
                    return Err(invalid());
                }
            }
            (None, Intent::Plugins | Intent::Pstack) => (),
            _ => return Err(invalid()),
        }
    }
    Ok(())
}
fn at<'a>(value: &'a Value, path: &[&str]) -> Result<Option<&'a Value>> {
    let mut current = value;
    for key in path {
        let object = current.as_object().ok_or_else(invalid)?;
        let Some(next) = object.get(*key) else {
            return Ok(None);
        };
        current = next;
    }
    Ok(Some(current))
}
fn parent_state(value: Option<&Value>) -> Result<ParentState> {
    match value {
        None => Ok(ParentState::Missing),
        Some(Value::Object(map)) => Ok(if map.is_empty() {
            ParentState::PresentEmpty
        } else {
            ParentState::PresentNonEmpty
        }),
        Some(Value::Array(a)) => Ok(if a.is_empty() {
            ParentState::PresentEmpty
        } else {
            ParentState::PresentNonEmpty
        }),
        _ => Err(invalid()),
    }
}
fn bool_value(value: Option<&Value>) -> Result<Presence<bool>> {
    match value {
        None => Ok(Presence::Missing),
        Some(Value::Bool(value)) => Ok(Presence::Present(*value)),
        _ => Err(invalid()),
    }
}
fn plugins_plan(
    value: &Map<String, Value>,
    previous: &BTreeMap<String, BoolField>,
    requests: &[Request],
    parents: &mut BTreeMap<Parent, ParentRecord>,
    edits: &mut Vec<Edit>,
) -> Result<BTreeMap<String, BoolField>> {
    let selected: Vec<_> = requests
        .iter()
        .filter(|r| r.owner.intent == Intent::Plugins)
        .collect();
    if selected.is_empty() {
        return Ok(previous.clone());
    }
    let original = value.get("enabledPlugins");
    known_parent(original, parents.get(&Parent::EnabledPlugins))?;
    let current_plugins = original
        .map(|v| v.as_object().ok_or_else(invalid))
        .transpose()?;
    let mut names: BTreeSet<_> = previous.keys().cloned().collect();
    for request in &selected {
        if let Some(Desired::Plugins(plugins)) = &request.desired {
            names.extend(plugins.iter().cloned());
        }
    }
    let mut output = BTreeMap::new();
    for name in names {
        let current = bool_value(current_plugins.and_then(|map| map.get(&name)))?;
        let mut changes = BTreeMap::new();
        for request in &selected {
            let claim = if matches!(&request.desired,Some(Desired::Plugins(names)) if names.contains(&name))
            {
                Some(profile_fields::Claim {
                    owner: request.owner.clone(),
                    intent_sha256: request.intent_sha256.clone(),
                    desired: false,
                })
            } else {
                None
            };
            changes.insert(request.owner.id(), claim);
        }
        let proposal = profile_fields::merge(&current, previous.get(&name), &changes)?;
        if proposal.after != current {
            edits.push(Edit {
                path: vec!["enabledPlugins".into(), name.clone()],
                value: match proposal.after {
                    Presence::Missing => None,
                    Presence::Present(v) => Some(Value::Bool(v)),
                },
            });
        }
        if let Some(record) = proposal.record {
            output.insert(name, record);
        }
    }
    if !output.is_empty() && !parents.contains_key(&Parent::EnabledPlugins) {
        parents.insert(
            Parent::EnabledPlugins,
            ParentRecord::capture(map_state(current_plugins)),
        );
    }
    Ok(output)
}
fn known_parent(current: Option<&Value>, record: Option<&ParentRecord>) -> Result<()> {
    if record.is_some_and(|p| !p.created_by_tool) && current.is_none() {
        return Err(Error::attention(
            "profile_owned_parent_drift",
            "原有配置父容器被删除，保留用户修改",
        ));
    }
    Ok(())
}
fn map_state(value: Option<&Map<String, Value>>) -> ParentState {
    value.map_or(ParentState::Missing, |map| {
        if map.is_empty() {
            ParentState::PresentEmpty
        } else {
            ParentState::PresentNonEmpty
        }
    })
}
fn pstack_plan(
    value: &Map<String, Value>,
    previous: Option<&crate::policy::profile::deny_fields::Record>,
    requests: &[Request],
    parents: &mut BTreeMap<Parent, ParentRecord>,
    edits: &mut Vec<Edit>,
) -> Result<Option<crate::policy::profile::deny_fields::Record>> {
    let selected: Vec<_> = requests
        .iter()
        .filter(|r| r.owner.intent == Intent::Pstack)
        .collect();
    if selected.is_empty() {
        return Ok(previous.cloned());
    }
    let original = value.get("permissions");
    known_parent(original, parents.get(&Parent::Permissions))?;
    let permissions = original
        .map(|v| v.as_object().ok_or_else(invalid))
        .transpose()?;
    let before = permissions.and_then(|map| map.get("deny"));
    known_parent(before, parents.get(&Parent::PermissionsDeny))?;
    let current: Vec<String> = match before {
        None => vec![],
        Some(Value::Array(entries)) => entries
            .iter()
            .map(|v| v.as_str().map(str::to_owned).ok_or_else(invalid))
            .collect::<Result<_>>()?,
        _ => return Err(invalid()),
    };
    let mut changes = BTreeMap::new();
    for request in selected {
        let desired = match &request.desired {
            Some(Desired::Pstack(entries)) => Some(crate::policy::profile::deny_fields::Claim {
                owner: request.owner.clone(),
                intent_sha256: request.intent_sha256.clone(),
                desired: entries.clone(),
            }),
            None => None,
            _ => return Err(invalid()),
        };
        changes.insert(request.owner.id(), desired);
    }
    let proposal = crate::policy::profile::deny_fields::merge(&current, previous, &changes)?;
    if proposal.record.is_some() {
        parents
            .entry(Parent::Permissions)
            .or_insert_with(|| ParentRecord::capture(map_state(permissions)));
        let state = if before.is_none() {
            ParentState::Missing
        } else if current.is_empty() {
            ParentState::PresentEmpty
        } else {
            ParentState::PresentNonEmpty
        };
        parents
            .entry(Parent::PermissionsDeny)
            .or_insert_with(|| ParentRecord::capture(state));
    }
    let remove = proposal.record.is_none()
        && proposal.after.is_empty()
        && parents
            .get(&Parent::PermissionsDeny)
            .is_some_and(|p| p.created_by_tool);
    if remove && before.is_some() {
        edits.push(Edit {
            path: vec!["permissions".into(), "deny".into()],
            value: None,
        });
    } else if proposal.after != current || (before.is_none() && !proposal.after.is_empty()) {
        edits.push(Edit {
            path: vec!["permissions".into(), "deny".into()],
            value: Some(Value::Array(
                proposal.after.into_iter().map(Value::String).collect(),
            )),
        });
    }
    Ok(proposal.record)
}
fn prune(
    mut raw: Vec<u8>,
    parents: &BTreeMap<Parent, ParentRecord>,
    plugins: bool,
    pstack: bool,
) -> Result<Vec<u8>> {
    for (key, path, active) in [
        (Parent::PermissionsDeny, vec!["permissions", "deny"], pstack),
        (Parent::Permissions, vec!["permissions"], pstack),
        (Parent::EnabledPlugins, vec!["enabledPlugins"], plugins),
    ] {
        if let Some(record) = parents.get(&key) {
            let value = crate::policy::json::parse(&raw)?;
            if record.after(parent_state(at(&value, &path)?)?, active)? == ParentAction::Remove {
                raw = json_fields::apply(
                    &raw,
                    &[Edit {
                        path: path.into_iter().map(str::to_owned).collect(),
                        value: None,
                    }],
                )?;
            }
        }
    }
    Ok(raw)
}
pub fn plan(
    target: &Path,
    raw: &[u8],
    previous: Option<&profile_registry::File>,
    requests: &[Request],
) -> Result<Proposal> {
    requests_valid(requests)?;
    crate::policy::paths::lexical(target)?;
    let value = crate::policy::json::parse(raw)?;
    let value = value.as_object().ok_or_else(invalid)?;
    let (previous_plugins, previous_pstack, mut parents) = if let Some(previous) = previous {
        previous.validate()?;
        if previous.target != target {
            return Err(invalid());
        }
        let Fields::Claude { plugins, pstack } = &previous.fields else {
            return Err(invalid());
        };
        (plugins.clone(), pstack.clone(), previous.parents.clone())
    } else {
        (BTreeMap::new(), None, BTreeMap::new())
    };
    let mut edits = vec![];
    let plugins = plugins_plan(value, &previous_plugins, requests, &mut parents, &mut edits)?;
    let pstack = pstack_plan(
        value,
        previous_pstack.as_ref(),
        requests,
        &mut parents,
        &mut edits,
    )?;
    let after = json_fields::apply(raw, &edits)?;
    let after = prune(after, &parents, !plugins.is_empty(), pstack.is_some())?;
    if plugins.is_empty() {
        parents.remove(&Parent::EnabledPlugins);
    }
    if pstack.is_none() {
        parents.remove(&Parent::Permissions);
        parents.remove(&Parent::PermissionsDeny);
    }
    let record = if plugins.is_empty() && pstack.is_none() {
        None
    } else {
        Some(profile_registry::File {
            target: target.into(),
            parents,
            fields: Fields::Claude { plugins, pstack },
        })
    };
    if let Some(record) = &record {
        record.validate()?;
    }
    Ok(Proposal { after, record })
}
#[cfg(test)]
#[path = "../../../tests/unit/claude_fields_policy.rs"]
mod tests;
