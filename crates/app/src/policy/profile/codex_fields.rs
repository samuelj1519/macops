use crate::{
    error::{Error, Result},
    policy::profile::profile_fields::{ParentState, Presence},
};
use std::collections::{BTreeMap, BTreeSet};
use toml_edit::{DocumentMut, Item, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub enabled: Presence<bool>,
    pub other_fields: bool,
}
#[derive(Debug)]
pub struct Observed {
    pub skills_parent: ParentState,
    pub config_parent: ParentState,
    pub entries: BTreeMap<String, Skill>,
}
#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub enabled: Presence<bool>,
    pub remove_created_table: bool,
}
fn invalid() -> Error {
    Error::attention(
        "unsupported_codex_profile_config",
        "Codex skills 配置结构或修改范围无法验证",
    )
}
fn parse(raw: &[u8]) -> Result<DocumentMut> {
    if raw.len() > 1024 * 1024 {
        return Err(invalid());
    }
    std::str::from_utf8(raw)
        .map_err(|_| invalid())?
        .parse()
        .map_err(|_| invalid())
}
fn enabled(item: Option<&Item>) -> Result<Presence<bool>> {
    item.map_or(Ok(Presence::Missing), |i| {
        i.as_bool().map(Presence::Present).ok_or_else(invalid)
    })
}
pub fn observe(raw: &[u8]) -> Result<Observed> {
    observe_doc(&parse(raw)?)
}
fn observe_doc(doc: &DocumentMut) -> Result<Observed> {
    let Some(skills) = doc.get("skills") else {
        return Ok(Observed {
            skills_parent: ParentState::Missing,
            config_parent: ParentState::Missing,
            entries: BTreeMap::new(),
        });
    };
    let skills = skills.as_table().ok_or_else(invalid)?;
    let parent = if skills.is_empty() {
        ParentState::PresentEmpty
    } else {
        ParentState::PresentNonEmpty
    };
    let Some(config) = skills.get("config") else {
        return Ok(Observed {
            skills_parent: parent,
            config_parent: ParentState::Missing,
            entries: BTreeMap::new(),
        });
    };
    if config.as_array().is_some_and(toml_edit::Array::is_empty) {
        return Ok(Observed {
            skills_parent: parent,
            config_parent: ParentState::PresentEmpty,
            entries: BTreeMap::new(),
        });
    }
    let tables = config.as_array_of_tables().ok_or_else(invalid)?;
    let mut entries = BTreeMap::new();
    for table in tables {
        let path = table
            .get("path")
            .and_then(Item::as_str)
            .ok_or_else(invalid)?
            .to_owned();
        if entries
            .insert(
                path,
                Skill {
                    enabled: enabled(table.get("enabled"))?,
                    other_fields: table
                        .iter()
                        .any(|(key, _)| !matches!(key, "path" | "enabled")),
                },
            )
            .is_some()
        {
            return Err(invalid());
        }
    }
    Ok(Observed {
        skills_parent: parent,
        config_parent: if tables.is_empty() {
            ParentState::PresentEmpty
        } else {
            ParentState::PresentNonEmpty
        },
        entries,
    })
}
fn decor_has_comments(decor: &toml_edit::Decor) -> bool {
    [decor.prefix(), decor.suffix()]
        .into_iter()
        .flatten()
        .filter_map(toml_edit::RawString::as_str)
        .any(|s| s.contains('#'))
}
fn value_has_comments(value: &Value) -> bool {
    decor_has_comments(value.decor())
        || value.as_array().is_some_and(|array| {
            array
                .trailing()
                .as_str()
                .is_some_and(|text| text.contains('#'))
        })
}
fn removable(table: &toml_edit::Table) -> bool {
    !decor_has_comments(table.decor())
        && table.iter().all(|(key, item)| {
            matches!(key, "path" | "enabled")
                && table.key(key).is_some_and(|k| {
                    !decor_has_comments(k.leaf_decor()) && !decor_has_comments(k.dotted_decor())
                })
                && item
                    .as_value()
                    .is_some_and(|v| !decor_has_comments(v.decor()))
        })
}
fn set_bool(table: &mut toml_edit::Table, desired: &Presence<bool>) -> Result<()> {
    let decor = table
        .get("enabled")
        .and_then(Item::as_value)
        .map(|v| v.decor().clone());
    match desired {
        Presence::Missing => {
            if decor.as_ref().is_some_and(decor_has_comments)
                || table.key("enabled").is_some_and(|k| {
                    decor_has_comments(k.leaf_decor()) || decor_has_comments(k.dotted_decor())
                })
            {
                return Err(invalid());
            }
            table.remove("enabled");
        }
        Presence::Present(value) => {
            let mut new = Value::from(*value);
            if let Some(decor) = decor {
                *new.decor_mut() = decor;
            }
            if let Some(existing) = table.get_mut("enabled") {
                *existing = Item::Value(new);
            } else {
                table.insert("enabled", Item::Value(new));
            }
        }
    }
    Ok(())
}
fn strip_owned(doc: &mut DocumentMut, changes: &[Change], before: &Observed) {
    if let Some(skills) = doc.get_mut("skills").and_then(Item::as_table_mut) {
        if let Some(tables) = skills
            .get_mut("config")
            .and_then(Item::as_array_of_tables_mut)
        {
            tables.retain(|table| {
                let Some(path) = table.get("path").and_then(Item::as_str) else {
                    return true;
                };
                let Some(change) = changes.iter().find(|c| c.path == path) else {
                    return true;
                };
                before.entries.contains_key(path)
                    && !(change.remove_created_table && removable(table))
            });
            for table in tables.iter_mut() {
                if table
                    .get("path")
                    .and_then(Item::as_str)
                    .is_some_and(|path| changes.iter().any(|c| c.path == path))
                {
                    table.remove("enabled");
                }
            }
        }
        let empty = skills.get("config").is_some_and(|i| {
            i.as_array_of_tables()
                .is_some_and(toml_edit::ArrayOfTables::is_empty)
                || i.as_array().is_some_and(toml_edit::Array::is_empty)
        });
        if empty {
            skills.remove("config");
        }
        if skills.is_empty() {
            doc.remove("skills");
        }
    }
}
fn apply_change(doc: &mut DocumentMut, change: &Change) -> Result<()> {
    if !doc.contains_key("skills") {
        if change.enabled == Presence::Missing {
            return Ok(());
        }
        doc.insert("skills", Item::Table(toml_edit::Table::new()));
    }
    let skills = doc
        .get_mut("skills")
        .and_then(Item::as_table_mut)
        .ok_or_else(invalid)?;
    if !skills.contains_key("config")
        || skills
            .get("config")
            .is_some_and(|i| i.as_array().is_some_and(toml_edit::Array::is_empty))
    {
        if change.enabled == Presence::Missing {
            return Ok(());
        }
        if skills
            .get("config")
            .and_then(Item::as_value)
            .is_some_and(value_has_comments)
            || skills.key("config").is_some_and(|k| {
                decor_has_comments(k.leaf_decor()) || decor_has_comments(k.dotted_decor())
            })
        {
            return Err(invalid());
        }
        skills.insert(
            "config",
            Item::ArrayOfTables(toml_edit::ArrayOfTables::new()),
        );
    }
    let tables = skills
        .get_mut("config")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(invalid)?;
    let found = tables
        .iter_mut()
        .enumerate()
        .find(|(_, t)| t.get("path").and_then(Item::as_str) == Some(change.path.as_str()));
    let mut remove = None;
    if let Some((index, table)) = found {
        if change.enabled == Presence::Missing && change.remove_created_table && removable(table) {
            remove = Some(index);
        } else {
            set_bool(table, &change.enabled)?;
        }
    } else if let Presence::Present(value) = change.enabled {
        let mut table = toml_edit::Table::new();
        table.insert("path", toml_edit::value(&change.path));
        table.insert("enabled", toml_edit::value(value));
        tables.push(table);
    }
    if let Some(index) = remove {
        tables.remove(index);
    }
    if tables.is_empty() {
        skills.insert("config", toml_edit::value(toml_edit::Array::new()));
    }
    Ok(())
}
fn line_endings(text: String, raw: &[u8]) -> Result<Vec<u8>> {
    let crlf = raw.windows(2).any(|w| w == b"\r\n");
    let lf = raw
        .iter()
        .enumerate()
        .any(|(i, b)| *b == b'\n' && (i == 0 || raw[i - 1] != b'\r'));
    if crlf && lf {
        return Err(invalid());
    }
    let mut output = if crlf {
        text.replace("\r\n", "\n")
            .replace('\n', "\r\n")
            .into_bytes()
    } else {
        text.into_bytes()
    };
    if !raw.ends_with(b"\n") {
        if output.ends_with(b"\r\n") {
            output.truncate(output.len() - 2);
        } else if output.ends_with(b"\n") {
            output.pop();
        }
    }
    Ok(output)
}
pub fn patch(raw: &[u8], changes: &[Change]) -> Result<Vec<u8>> {
    let mut doc = parse(raw)?;
    let before = observe_doc(&doc)?;
    let targets: BTreeSet<_> = changes.iter().map(|c| c.path.clone()).collect();
    if targets.len() != changes.len() || changes.len() > 5000 {
        return Err(invalid());
    }
    if changes.iter().all(|c| {
        before
            .entries
            .get(&c.path)
            .map_or(&Presence::Missing, |s| &s.enabled)
            == &c.enabled
            && !(c.remove_created_table && before.entries.contains_key(&c.path))
    }) {
        return Ok(raw.into());
    }
    let mut proof = doc.clone();
    strip_owned(&mut proof, changes, &before);
    for change in changes {
        apply_change(&mut doc, change)?;
    }
    let mut after_proof = doc.clone();
    strip_owned(&mut after_proof, changes, &before);
    let output = line_endings(doc.to_string(), raw)?;
    verify_patch(&proof, &after_proof, &output, changes)?;
    Ok(output)
}

fn verify_patch(
    proof: &DocumentMut,
    after_proof: &DocumentMut,
    output: &[u8],
    changes: &[Change],
) -> Result<()> {
    if proof.to_string() != after_proof.to_string() {
        return Err(invalid());
    }
    let after = observe(output)?;
    if changes.iter().any(|c| {
        after
            .entries
            .get(&c.path)
            .map_or(&Presence::Missing, |s| &s.enabled)
            != &c.enabled
    }) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/codex_fields_policy.rs"]
mod tests;

pub fn prune_parents(
    raw: &[u8],
    parents: &BTreeMap<
        crate::policy::profile::profile_registry::Parent,
        crate::policy::profile::profile_fields::ParentRecord,
    >,
    active: bool,
) -> Result<Vec<u8>> {
    use crate::policy::profile::{profile_fields::ParentAction, profile_registry::Parent};
    let mut doc = parse(raw)?;
    let observed = observe_doc(&doc)?;
    let mut changed = false;
    if let Some(parent) = parents.get(&Parent::SkillsConfig)
        && parent.after(observed.config_parent, active)? == ParentAction::Remove
    {
        changed = remove_empty_config(&mut doc);
    }
    let skills_parent =
        doc.get("skills")
            .and_then(Item::as_table)
            .map_or(ParentState::Missing, |table| {
                if table.is_empty() {
                    ParentState::PresentEmpty
                } else {
                    ParentState::PresentNonEmpty
                }
            });
    if let Some(parent) = parents.get(&Parent::Skills)
        && parent.after(skills_parent, active)? == ParentAction::Remove
        && doc
            .get("skills")
            .and_then(Item::as_table)
            .is_some_and(|t| !decor_has_comments(t.decor()))
    {
        doc.remove("skills");
        changed = true;
    }
    if changed {
        line_endings(doc.to_string(), raw)
    } else {
        Ok(raw.into())
    }
}

fn remove_empty_config(doc: &mut DocumentMut) -> bool {
    let Some(skills) = doc.get_mut("skills").and_then(Item::as_table_mut) else {
        return false;
    };
    let safe = skills.key("config").is_some_and(|k| {
        !decor_has_comments(k.leaf_decor()) && !decor_has_comments(k.dotted_decor())
    }) && skills
        .get("config")
        .and_then(Item::as_value)
        .is_some_and(|v| {
            v.as_array().is_some_and(toml_edit::Array::is_empty) && !value_has_comments(v)
        });
    if safe {
        skills.remove("config");
    }
    safe
}
