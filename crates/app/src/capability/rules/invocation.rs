use crate::{
    error::{Error, Result},
    policy::yaml,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
type Mapping = Map<String, Value>;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum MetadataKind {
    Frontmatter,
    CodexPolicy,
}

fn unsupported() -> Error {
    Error::new(
        "unsupported_invocation_metadata",
        "元数据格式无法在保留其余字段的前提下修改",
    )
}

fn line_parts(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

struct Parts<'a> {
    prefix: &'a str,
    yaml: &'a str,
    suffix: &'a str,
}

fn parts(kind: MetadataKind, text: &str) -> Result<Parts<'_>> {
    if kind == MetadataKind::CodexPolicy {
        return Ok(Parts {
            prefix: "",
            yaml: text,
            suffix: "",
        });
    }
    let first = text.split_inclusive('\n').next().ok_or_else(unsupported)?;
    if line_parts(first).0 != "---" || !first.ends_with('\n') {
        return Err(unsupported());
    }
    let mut end = first.len();
    for line in text[first.len()..].split_inclusive('\n') {
        if line_parts(line).0 == "---" {
            return Ok(Parts {
                prefix: first,
                yaml: &text[first.len()..end],
                suffix: &text[end..],
            });
        }
        end += line.len();
    }
    Err(unsupported())
}

fn mapping(value: Value) -> Result<Mapping> {
    match value {
        Value::Object(fields) => Ok(fields),
        Value::Null => Ok(Mapping::new()),
        _ => Err(unsupported()),
    }
}

fn valid_flag(value: &Value) -> bool {
    match value {
        Value::Bool(_) => true,
        Value::Number(n) => matches!(n.as_u64(), Some(0 | 1)),
        Value::String(s) => matches!(s.to_ascii_lowercase().as_str(), "yes" | "no" | "on" | "off"),
        _ => false,
    }
}

fn desired(kind: MetadataKind, before: &Mapping) -> Result<Mapping> {
    let mut after = before.clone();
    match kind {
        MetadataKind::Frontmatter => {
            let key = "disable-model-invocation".to_owned();
            if before.get(&key).is_some_and(|value| !valid_flag(value)) {
                return Err(unsupported());
            }
            after.insert(key, Value::Bool(true));
        }
        MetadataKind::CodexPolicy => {
            let key = "policy".to_owned();
            let mut policy = match before.get(&key) {
                Some(Value::Object(fields)) => fields.clone(),
                None => Mapping::new(),
                _ => return Err(unsupported()),
            };
            let implicit = "allow_implicit_invocation".to_owned();
            if policy
                .get(&implicit)
                .is_some_and(|value| !valid_flag(value))
            {
                return Err(unsupported());
            }
            policy.insert(implicit, Value::Bool(false));
            after.insert(key, Value::Object(policy));
        }
    }
    Ok(after)
}

fn key_prefix<'a>(line: &'a str, key: &str, indent: &str) -> Option<(&'a str, &'a str)> {
    let body = line_parts(line).0;
    let rest = body.strip_prefix(indent)?;
    let (name, value) = rest.split_once(':')?;
    let name = name.trim_end_matches([' ', '\t']);
    let unquoted = name
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| name.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .unwrap_or(name);
    if unquoted != key {
        return None;
    }
    let split = body.len() - value.len();
    Some((&body[..split], &body[split..]))
}

fn replace_flag(line: &str, key: &str, indent: &str, flag: &str) -> Result<Option<String>> {
    let Some((prefix, rest)) = key_prefix(line, key, indent) else {
        return Ok(None);
    };
    let content = rest.trim_start_matches([' ', '\t']);
    let end = content.find(char::is_whitespace).unwrap_or(content.len());
    let token = &content[..end];
    if !matches!(
        token.to_ascii_lowercase().as_str(),
        "true" | "false" | "yes" | "no" | "on" | "off" | "0" | "1"
    ) {
        return Err(unsupported());
    }
    let suffix = &content[end..];
    if !suffix.trim_start().is_empty() && !suffix.trim_start().starts_with('#') {
        return Err(unsupported());
    }
    let spaces = &rest[..rest.len() - content.len()];
    Ok(Some(format!(
        "{prefix}{spaces}{flag}{suffix}{}",
        line_parts(line).1
    )))
}

fn frontmatter(text: &str, before: &Mapping, nl: &str) -> Result<String> {
    let mut output = String::new();
    let mut found = false;
    for line in text.split_inclusive('\n') {
        if let Some(replaced) = replace_flag(line, "disable-model-invocation", "", "true")? {
            if found {
                return Err(unsupported());
            }
            output.push_str(&replaced);
            found = true;
        } else {
            output.push_str(line);
        }
    }
    if !found {
        if before.contains_key("disable-model-invocation") {
            return Err(unsupported());
        }
        output.push_str("disable-model-invocation: true");
        output.push_str(nl);
    }
    Ok(output)
}

fn policy_indent(lines: &[&str]) -> Result<String> {
    let mut width = None;
    for line in lines {
        let body = line_parts(line).0;
        if body.trim().is_empty() || body.trim_start().starts_with('#') {
            continue;
        }
        let prefix = &body[..body.len() - body.trim_start().len()];
        if prefix.is_empty() || prefix.contains('\t') {
            return Err(unsupported());
        }
        width = Some(width.map_or(prefix.len(), |old: usize| old.min(prefix.len())));
    }
    Ok(" ".repeat(width.unwrap_or(2)))
}

fn codex(text: &str, before: &Mapping, nl: &str) -> Result<String> {
    if !before.contains_key("policy") {
        let separator = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            nl
        };
        return Ok(format!(
            "{text}{separator}policy:{nl}  allow_implicit_invocation: false{nl}"
        ));
    }
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let (start, rest) = lines
        .iter()
        .enumerate()
        .find_map(|(index, line)| key_prefix(line, "policy", "").map(|(_, rest)| (index, rest)))
        .ok_or_else(unsupported)?;
    if !rest.trim().is_empty() && !rest.trim_start().starts_with('#') {
        return Err(unsupported());
    }
    let end = (start + 1..lines.len())
        .find(|&i| {
            let b = line_parts(lines[i]).0;
            !b.trim().is_empty() && !b.starts_with([' ', '\t', '#'])
        })
        .unwrap_or(lines.len());
    let indent = policy_indent(&lines[start + 1..end])?;
    let mut output = lines[..=start].concat();
    let mut found = false;
    for line in &lines[start + 1..end] {
        if let Some(replaced) = replace_flag(line, "allow_implicit_invocation", &indent, "false")? {
            if found {
                return Err(unsupported());
            }
            output.push_str(&replaced);
            found = true;
        } else {
            output.push_str(line);
        }
    }
    if !found {
        if before
            .get("policy")
            .and_then(Value::as_object)
            .is_some_and(|policy| policy.contains_key("allow_implicit_invocation"))
        {
            return Err(unsupported());
        }
        if !output.ends_with('\n') {
            output.push_str(nl);
        }
        output.push_str(&indent);
        output.push_str("allow_implicit_invocation: false");
        output.push_str(nl);
    }
    output.push_str(&lines[end..].concat());
    Ok(output)
}

pub fn transform(kind: MetadataKind, original: Option<&[u8]>) -> Result<Vec<u8>> {
    let bytes = match (kind, original) {
        (MetadataKind::Frontmatter, None) => return Err(unsupported()),
        (_, data) => data.unwrap_or_default(),
    };
    if bytes.len() > 1024 * 1024 {
        return Err(Error::new("metadata_budget_limit", "元数据文件超过限制"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| unsupported())?;
    let parts = parts(kind, text)?;
    let before = mapping(yaml::parse(parts.yaml.as_bytes())?)?;
    let expected = desired(kind, &before)?;
    if expected == before {
        return Ok(bytes.to_vec());
    }
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let patched = match kind {
        MetadataKind::Frontmatter => frontmatter(parts.yaml, &before, nl)?,
        MetadataKind::CodexPolicy => codex(parts.yaml, &before, nl)?,
    };
    if yaml::parse(patched.as_bytes())? != Value::Object(expected) {
        return Err(Error::new(
            "metadata_transform_unproven",
            "修改会影响其他元数据，未执行",
        ));
    }
    Ok(format!("{}{patched}{}", parts.prefix, parts.suffix).into_bytes())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct PluginRule {
    #[serde(default)]
    pub mirror_upstream_manual: bool,
    #[serde(default)]
    pub manual_only: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub schema_version: u32,
    pub standalone_manual_only: Vec<String>,
    #[serde(default, deserialize_with = "crate::policy::json::object_map")]
    pub plugins: BTreeMap<String, PluginRule>,
    #[serde(default)]
    pub mirror_standalone_upstream_manual: bool,
    #[serde(default)]
    pub notes: Vec<String>,
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.to_ascii_lowercase().starts_with("devnorm")
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

fn names(values: &[String]) -> bool {
    let unique: BTreeSet<_> = values.iter().collect();
    values.len() <= 5000 && unique.len() == values.len() && values.iter().all(|n| name(n))
}

pub fn declaration(bytes: &[u8]) -> Result<Declaration> {
    let value = crate::policy::json::parse(bytes)?;
    let crate::policy::json::Object(value): crate::policy::json::Object<Declaration> =
        serde_json::from_value(value)?;
    if value.schema_version != 1
        || !names(&value.standalone_manual_only)
        || value.plugins.len() > 256
        || value
            .plugins
            .iter()
            .any(|(key, rule)| !name(key) || !names(&rule.manual_only))
    {
        return Err(Error::new(
            "invalid_invocation_declaration",
            "调用策略声明无效或包含受保护的 DevNorm 目标",
        ));
    }
    Ok(value)
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MetadataAccess {
    Managed,
    ReadOnly,
}

pub fn metadata_access(home: &Path, path: &Path, kind: MetadataKind) -> Result<MetadataAccess> {
    crate::policy::paths::lexical(path)?;
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(unsupported());
    }
    let relative = path.strip_prefix(home).map_err(|_| unsupported())?;
    let parts: Vec<_> = relative
        .components()
        .map(|part| part.as_os_str().to_str().ok_or_else(unsupported))
        .collect::<Result<_>>()?;
    if parts
        .iter()
        .any(|part| part.to_ascii_lowercase().starts_with("devnorm"))
    {
        return Err(unsupported());
    }
    let standalone =
        parts.len() >= 3 && matches!(parts[0], ".agents" | ".cursor") && parts[1] == "skills";
    let plugin = parts.len() >= 6
        && (parts[0] == ".claude"
            || parts[0].starts_with(".claude-")
            || matches!(parts[0], ".codex" | ".cursor"))
        && parts[1] == "plugins"
        && parts[2] == "cache";
    let tail = if standalone {
        &parts[3..]
    } else if plugin {
        &parts[6..]
    } else {
        return Err(unsupported());
    };
    let valid = match (standalone, kind) {
        (true, MetadataKind::Frontmatter) => tail == ["SKILL.md"],
        (true, MetadataKind::CodexPolicy) => tail == ["agents", "openai.yaml"],
        (false, MetadataKind::Frontmatter) => {
            (tail.len() == 3 && tail[0] == "skills" && tail[2] == "SKILL.md")
                || (tail.len() == 2
                    && tail[0] == "commands"
                    && Path::new(tail[1])
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md")))
        }
        (false, MetadataKind::CodexPolicy) => {
            tail.len() == 4 && tail[0] == "skills" && tail[2..] == ["agents", "openai.yaml"]
        }
    };
    if !valid {
        return Err(unsupported());
    }
    Ok(if parts[0] == ".cursor" {
        MetadataAccess::ReadOnly
    } else {
        MetadataAccess::Managed
    })
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OverlayRow {
    pub kind: MetadataKind,
    pub label: String,
    pub owner: String,
    pub original: Option<String>,
    pub original_hash: Option<String>,
    pub applied_hash: String,
    pub original_mode: Option<u32>,
    pub applied_mode: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OverlayRecord {
    pub schema_version: u32,
    #[serde(default)]
    pub policy_hash: Option<String>,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub files: BTreeMap<PathBuf, OverlayRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_policy_hash: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::policy::json::object_map"
    )]
    pub profiles: BTreeMap<PathBuf, crate::policy::profile::profile_registry::File>,
}

pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl OverlayRow {
    pub fn bytes(&self) -> Result<(Option<Vec<u8>>, Vec<u8>)> {
        let original = self
            .original
            .as_deref()
            .map(|raw| {
                STANDARD
                    .decode(raw)
                    .map_err(|_| Error::new("invalid_invocation_overlay", "原始元数据编码无效"))
            })
            .transpose()?;
        if original.is_some() != self.original_mode.is_some()
            || self.original_mode.is_some_and(|mode| mode > 0o7777)
            || self.applied_mode > 0o7777
            || original.as_ref().map(|v| hash(v)) != self.original_hash
        {
            return Err(Error::new(
                "invalid_invocation_overlay",
                "原始元数据摘要或模式无效",
            ));
        }
        let applied = transform(self.kind, original.as_deref())?;
        if hash(&applied) != self.applied_hash {
            return Err(Error::new(
                "invalid_invocation_overlay",
                "已应用元数据摘要无效",
            ));
        }
        Ok((original, applied))
    }
}

pub fn record(home: &Path, bytes: &[u8]) -> Result<OverlayRecord> {
    let value = crate::policy::json::parse(bytes)?;
    let crate::policy::json::Object(value): crate::policy::json::Object<OverlayRecord> =
        serde_json::from_value(value)?;
    if !matches!(value.schema_version, 1 | 2)
        || (value.schema_version == 1
            && (!value.profiles.is_empty() || value.profile_policy_hash.is_some()))
        || value.files.len() > 5000
        || value
            .profile_policy_hash
            .as_ref()
            .is_some_and(|s| !crate::policy::config_local::valid_digest(s))
        || value
            .policy_hash
            .as_ref()
            .is_some_and(|s| !crate::policy::config_local::valid_digest(s))
    {
        return Err(Error::new(
            "invalid_invocation_overlay",
            "调用策略账本版本或范围无效",
        ));
    }
    for (path, row) in &value.files {
        metadata_access(home, path, row.kind)?;
        row.bytes()?;
        if row.label.is_empty()
            || row.owner.is_empty()
            || row.label.len() > 256
            || row.owner.len() > 256
            || row.label.chars().any(char::is_control)
            || row.owner.chars().any(char::is_control)
        {
            return Err(Error::new("invalid_invocation_overlay", "调用策略标签无效"));
        }
    }
    crate::policy::profile::profile_registry::validate(&value.profiles)?;
    Ok(value)
}

pub struct Proposal {
    pub after: Option<Vec<u8>>,
    pub after_mode: Option<u32>,
    pub row: Option<OverlayRow>,
    pub changed: bool,
}

fn current_valid(current: Option<&[u8]>, mode: Option<u32>) -> Result<()> {
    if current.is_some() != mode.is_some() || mode.is_some_and(|m| m > 0o7777) {
        return Err(Error::new(
            "invalid_metadata_observation",
            "元数据存在性与权限观察不一致",
        ));
    }
    Ok(())
}

fn known_current(
    previous: &OverlayRow,
    current: Option<&[u8]>,
    mode: Option<u32>,
) -> Result<Option<Vec<u8>>> {
    let (original, applied) = previous.bytes()?;
    if (current == Some(applied.as_slice()) && mode == Some(previous.applied_mode))
        || (current == original.as_deref() && mode == previous.original_mode)
    {
        return Ok(original);
    }
    Err(Error::attention(
        "invocation_overlay_drift",
        "当前元数据与原始或已应用基线不符，保留外部修改",
    ))
}

pub fn propose(
    kind: MetadataKind,
    label: &str,
    owner: &str,
    current: Option<&[u8]>,
    mode: Option<u32>,
    previous: Option<&OverlayRow>,
) -> Result<Proposal> {
    current_valid(current, mode)?;
    let original = if let Some(previous) = previous {
        if previous.kind != kind {
            return Err(Error::new("invocation_owner_conflict", "调用策略种类改变"));
        }
        known_current(previous, current, mode)?
    } else {
        current.map(<[u8]>::to_vec)
    };
    let original_mode = previous.map_or(mode, |row| row.original_mode);
    let after = transform(kind, original.as_deref())?;
    let mode_after = previous.map_or(mode.unwrap_or(0o600), |row| row.applied_mode);
    let after_mode = Some(mode_after);
    let changed = current != Some(after.as_slice()) || mode != after_mode;
    let row = if original.as_deref() == Some(after.as_slice()) && original_mode == after_mode {
        None
    } else {
        Some(OverlayRow {
            kind,
            label: label.into(),
            owner: owner.into(),
            original: previous.map_or_else(
                || original.as_ref().map(|b| STANDARD.encode(b)),
                |row| row.original.clone(),
            ),
            original_hash: original.as_ref().map(|b| hash(b)),
            applied_hash: hash(&after),
            original_mode,
            applied_mode: mode_after,
        })
    };
    Ok(Proposal {
        after: Some(after),
        after_mode,
        row,
        changed,
    })
}

pub fn retire(
    previous: &OverlayRow,
    current: Option<&[u8]>,
    mode: Option<u32>,
) -> Result<Proposal> {
    current_valid(current, mode)?;
    if current.is_none() {
        return Ok(Proposal {
            after: None,
            after_mode: None,
            row: None,
            changed: false,
        });
    }
    let original = known_current(previous, current, mode)?;
    let changed = current != original.as_deref() || mode != previous.original_mode;
    Ok(Proposal {
        after: original,
        after_mode: previous.original_mode,
        row: None,
        changed,
    })
}

pub(crate) fn skill_name(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| unsupported())?;
    let frontmatter = parts(MetadataKind::Frontmatter, text)?;
    mapping(yaml::parse(frontmatter.yaml.as_bytes())?)?
        .remove("name")
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(unsupported)
}

pub fn manual_frontmatter(bytes: &[u8]) -> Result<bool> {
    let text = std::str::from_utf8(bytes).map_err(|_| unsupported())?;
    let parts = parts(MetadataKind::Frontmatter, text)?;
    let fields = mapping(yaml::parse(parts.yaml.as_bytes())?)?;
    let Some(value) = fields.get("disable-model-invocation") else {
        return Ok(false);
    };
    match value {
        Value::Bool(value) => Ok(*value),
        Value::Number(value) if matches!(value.as_u64(), Some(0 | 1)) => {
            Ok(value.as_u64() == Some(1))
        }
        Value::String(value)
            if matches!(
                value.to_ascii_lowercase().as_str(),
                "yes" | "no" | "on" | "off"
            ) =>
        {
            Ok(matches!(value.to_ascii_lowercase().as_str(), "yes" | "on"))
        }
        _ => Err(unsupported()),
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/invocation_policy.rs"]
mod tests;
