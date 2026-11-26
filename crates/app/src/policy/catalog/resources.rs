use crate::{
    domain::resource::Model,
    error::{Error, Result},
};
use serde::Deserialize;
use std::collections::BTreeMap;

fn strict_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_value::<crate::policy::json::Object<T>>(crate::policy::json::parse(bytes)?)
        .map(|object| object.0)
        .map_err(Into::into)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    All,
    Applications,
    Agents,
    Capabilities,
    Configs,
}
impl Filter {
    pub fn parse(value: Option<&str>) -> Result<Self> {
        match value {
            None | Some("all") => Ok(Self::All),
            Some("app" | "apps" | "applications" | "cli" | "runtime" | "manager") => {
                Ok(Self::Applications)
            }
            Some("agent" | "agents" | "harness" | "harnesses") => Ok(Self::Agents),
            Some(
                "skill" | "skills" | "plugin" | "plugins" | "capability" | "capabilities" | "mcp",
            ) => Ok(Self::Capabilities),
            Some("config" | "configs") => Ok(Self::Configs),
            _ => Err(Error::new(
                "invalid_kind",
                "kind 支持 applications、agents、capabilities、configs",
            )),
        }
    }
    pub fn includes(self, other: Self) -> bool {
        self == Self::All || self == other
    }
}

#[derive(Deserialize, Default)]
struct ClaudeSettings {
    model: Option<String>,
    #[serde(default, deserialize_with = "crate::policy::json::object")]
    env: ClaudeEnv,
}
#[derive(Deserialize, Default)]
struct ClaudeEnv {
    #[serde(rename = "ANTHROPIC_MODEL")]
    model: Option<String>,
    #[serde(rename = "ANTHROPIC_DEFAULT_OPUS_MODEL")]
    opus: Option<String>,
    #[serde(rename = "ANTHROPIC_DEFAULT_SONNET_MODEL")]
    sonnet: Option<String>,
    #[serde(rename = "ANTHROPIC_DEFAULT_HAIKU_MODEL")]
    haiku: Option<String>,
    #[serde(rename = "ANTHROPIC_BASE_URL")]
    base_url: Option<String>,
}

pub fn claude_model(bytes: &[u8]) -> Result<Model> {
    let settings: ClaudeSettings = strict_json(bytes)
        .map_err(|_| Error::new("invalid_profile_metadata", "profile 模型字段格式错误"))?;
    let mut aliases = BTreeMap::new();
    for (name, value) in [
        ("opus", settings.env.opus),
        ("sonnet", settings.env.sonnet),
        ("haiku", settings.env.haiku),
    ] {
        if let Some(value) = value {
            aliases.insert(name.into(), value);
        }
    }
    Ok(Model {
        selected: settings.model.or(settings.env.model),
        aliases,
        custom_provider_configured: settings.env.base_url.is_some_and(|value| !value.is_empty()),
    })
}

pub fn component(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && !value.contains(['/', '\\', '\0', '\n', '\r'])
}

#[cfg(test)]
#[path = "../../../tests/unit/resource_policy.rs"]
mod tests;

#[derive(Deserialize)]
pub struct MiseRelease {
    pub version: String,
    #[serde(default)]
    pub install_path: Option<std::path::PathBuf>,
    #[serde(default)]
    pub active: bool,
}

pub fn mise_releases(bytes: &[u8]) -> Result<BTreeMap<String, Vec<MiseRelease>>> {
    let value = crate::policy::json::parse(bytes)
        .map_err(|_| Error::new("invalid_mise_inventory", "mise安装清单格式不兼容"))?;
    crate::policy::json::object_vec_map(value)
        .map_err(|_| Error::new("invalid_mise_inventory", "mise安装清单格式不兼容"))
}

pub fn source_label(raw: &str) -> String {
    if let Some((scheme, rest)) = raw.split_once("://") {
        let host = rest
            .split('/')
            .next()
            .unwrap_or_default()
            .rsplit('@')
            .next()
            .unwrap_or_default()
            .split(['?', '#'])
            .next()
            .unwrap_or_default();
        format!("{scheme}://{host}")
    } else if raw.contains(['?', '\n', '\r'])
        || (raw.contains('@')
            && raw
                .split('@')
                .next()
                .is_some_and(|prefix| prefix.contains(':') && prefix != "npm:"))
    {
        "private-source".into()
    } else {
        raw.into()
    }
}

#[derive(Deserialize)]
pub struct SkillLock {
    pub version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub skills: BTreeMap<String, SkillRecord>,
}
#[derive(Deserialize)]
pub struct SkillRecord {
    pub source: String,
    #[serde(rename = "sourceType")]
    pub source_type: String,
    #[serde(rename = "sourceUrl", default)]
    pub source_url: Option<String>,
    #[serde(rename = "ref", default)]
    pub revision: Option<String>,
    #[serde(rename = "installMode", default)]
    pub install_mode: Option<String>,
    #[serde(rename = "skillFolderHash", default)]
    pub folder_hash: Option<String>,
}
pub fn skill_lock(bytes: &[u8], project: bool) -> Result<SkillLock> {
    let lock: SkillLock = strict_json(bytes)
        .map_err(|_| Error::new("invalid_skill_lock", "skills lock 格式不兼容"))?;
    if lock.version != if project { 2 } else { 3 } {
        return Err(Error::new(
            "unsupported_skill_lock",
            "skills lock版本不兼容，未当成空清单",
        ));
    }
    if lock.skills.keys().any(|name| !component(name)) {
        return Err(Error::new(
            "invalid_skill_key",
            "skill名称不是安全的路径组件",
        ));
    }
    Ok(lock)
}

#[derive(Deserialize)]
pub struct PluginBook {
    pub version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_vec_map")]
    pub plugins: BTreeMap<String, Vec<PluginRecord>>,
}
#[derive(Deserialize)]
pub struct PluginRecord {
    pub scope: String,
    #[serde(rename = "installPath")]
    pub path: std::path::PathBuf,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(rename = "gitCommitSha", default)]
    pub revision: Option<String>,
    #[serde(rename = "projectPath", default)]
    pub project: Option<std::path::PathBuf>,
}
pub fn plugin_book(bytes: &[u8]) -> Result<PluginBook> {
    let book: PluginBook =
        strict_json(bytes).map_err(|_| Error::new("invalid_plugin_book", "插件登记格式不兼容"))?;
    if book.version != 2 {
        return Err(Error::new("unsupported_plugin_book", "插件登记版本不兼容"));
    }
    for (id, records) in &book.plugins {
        let Some((name, market)) = id.rsplit_once('@') else {
            return Err(Error::new("invalid_plugin_id", "插件缺少marketplace身份"));
        };
        if !component(name) || !component(market) {
            return Err(Error::new("invalid_plugin_id", "插件身份不安全"));
        }
        for record in records {
            crate::policy::paths::lexical(&record.path)?;
            if !matches!(record.scope.as_str(), "user" | "project" | "local")
                || (record.scope != "user" && record.project.is_none())
            {
                return Err(Error::new(
                    "invalid_plugin_scope",
                    "插件缺少明确scope/project",
                ));
            }
            if let Some(project) = &record.project {
                crate::policy::paths::lexical(project)?;
            }
        }
    }
    Ok(book)
}

#[derive(Deserialize, Default)]
struct PluginSettings {
    #[serde(rename = "enabledPlugins", default)]
    plugins: BTreeMap<String, bool>,
}
pub fn plugin_enabled(bytes: &[u8]) -> Result<BTreeMap<String, bool>> {
    let settings: PluginSettings = strict_json(bytes)
        .map_err(|_| Error::new("invalid_plugin_settings", "插件启用声明格式不兼容"))?;
    Ok(settings.plugins)
}

pub fn skill_target_matches(
    resource: &crate::domain::resource::Resource,
    name: &str,
    logical: &std::path::Path,
    real: Option<&std::path::Path>,
    scope: &str,
    project: Option<&std::path::Path>,
) -> bool {
    match &resource.data {
        crate::domain::resource::Data::Capability {
            native_id,
            origin,
            contents,
            ..
        } => {
            native_id == name
                && contents.iter().any(|v| v.realpath.as_deref() == real)
                && (contents.iter().any(|v| v.path == logical)
                    || (scope != "system"
                        && origin.manager.starts_with("skills-cli:")
                        && project.map_or(
                            origin
                                .record
                                .file_name()
                                .is_some_and(|n| n == ".skill-lock.json"),
                            |p| origin.record == p.join("skills-lock.json"),
                        )))
        }
        _ => false,
    }
}

pub fn legacy_mise_resolver(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let marker = "claude_resolved_bin() {";
    if text.matches(marker).count() != 1 {
        return false;
    }
    let Some(body) = text
        .split_once(marker)
        .and_then(|(_, tail)| tail.split_once("\n}"))
    else {
        return false;
    };
    body.0.trim() == "mise which claude"
}

pub fn legacy_claude_wrapper(bytes: &[u8], helper: &std::path::Path, profile: &str) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let expected = format!("exec {} run {profile} \"$@\"", helper.display());
    let commands: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && *l != "set -eu")
        .collect();
    commands == [expected.as_str()]
}

pub fn bind_runtime(
    binary: &std::path::Path,
    applications: &[crate::domain::resource::Resource],
) -> Option<String> {
    let matches:Vec<_>=applications.iter().filter(|r|matches!(&r.data,crate::domain::resource::Data::Application{releases,..} if releases.iter().any(|v|v.path.as_ref().is_some_and(|p|binary.starts_with(p))))).collect();
    if matches.len() == 1 {
        Some(matches[0].id.clone())
    } else {
        None
    }
}

#[derive(Debug)]
pub struct McpRecord {
    pub name: String,
    pub runner: Option<String>,
    pub enabled: Option<bool>,
}
#[derive(Deserialize)]
struct JsonMcp {
    command: Option<String>,
    url: Option<String>,
    disabled: Option<bool>,
}
#[derive(Deserialize, Default)]
struct McpJson {
    #[serde(
        rename = "mcpServers",
        default,
        deserialize_with = "crate::policy::json::object_map"
    )]
    servers: BTreeMap<String, JsonMcp>,
}

pub fn mcp_json(bytes: &[u8]) -> Result<Vec<McpRecord>> {
    let data: McpJson =
        strict_json(bytes).map_err(|_| Error::new("invalid_mcp_config", "MCP登记格式不兼容"))?;
    let mut records = vec![];
    for (name, record) in data.servers {
        if !component(&name) {
            return Err(Error::new("invalid_mcp_id", "MCP名称不安全"));
        }
        records.push(McpRecord {
            name,
            runner: record
                .command
                .as_deref()
                .map(runner_label)
                .or(record.url.map(|_| "remote".into())),
            enabled: record.disabled.map(|v| !v),
        });
    }
    Ok(records)
}

fn runner_label(command: &str) -> String {
    if command.contains(char::is_whitespace) || command.contains("://") {
        return "custom-command".into();
    }
    std::path::Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| component(n))
        .unwrap_or("custom-command")
        .into()
}

pub fn codex_plugins(bytes: &[u8]) -> Result<BTreeMap<String, Option<bool>>> {
    codex_plugins_from(&codex_config_document(bytes)?)
}

pub(crate) fn codex_config_document(bytes: &[u8]) -> Result<toml_edit::DocumentMut> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_codex_config", "Codex配置不是UTF-8"))?;
    text.parse::<toml_edit::DocumentMut>()
        .map_err(|_| Error::new("invalid_codex_config", "Codex配置格式不兼容"))
}

pub(crate) fn codex_plugins_from(
    doc: &toml_edit::DocumentMut,
) -> Result<BTreeMap<String, Option<bool>>> {
    let Some(item) = doc.get("plugins") else {
        return Ok(BTreeMap::new());
    };
    let table = item
        .as_table_like()
        .ok_or_else(|| Error::new("invalid_codex_config", "plugins不是表"))?;
    let mut records = BTreeMap::new();
    for (name, item) in table.iter() {
        let Some((plugin, market)) = name.rsplit_once('@') else {
            continue;
        };
        if !component(plugin) || !component(market) {
            return Err(Error::new("invalid_plugin_id", "Codex插件身份不安全"));
        }
        let item = item
            .as_table_like()
            .ok_or_else(|| Error::new("invalid_codex_config", "插件登记不是表"))?;
        let enabled = item
            .get("enabled")
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| Error::new("invalid_codex_config", "插件启用值不是布尔"))
            })
            .transpose()?;
        records.insert(name.into(), enabled);
    }
    Ok(records)
}

pub fn codex_mcp(bytes: &[u8]) -> Result<Vec<McpRecord>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_codex_config", "Codex配置不是UTF-8"))?;
    let doc = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| Error::new("invalid_codex_config", "Codex配置格式不兼容"))?;
    let Some(item) = doc.get("mcp_servers") else {
        return Ok(vec![]);
    };
    let table = item
        .as_table_like()
        .ok_or_else(|| Error::new("invalid_codex_config", "mcp_servers不是表"))?;
    let mut records = vec![];
    for (name, item) in table.iter() {
        if !component(name) {
            return Err(Error::new("invalid_mcp_id", "MCP名称不安全"));
        }
        let item = item
            .as_table_like()
            .ok_or_else(|| Error::new("invalid_codex_config", "MCP登记不是表"))?;
        let enabled = item
            .get("enabled")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| Error::new("invalid_codex_config", "MCP启用值不是布尔"))
            })
            .transpose()?;
        let runner = item
            .get("command")
            .map(|v| {
                v.as_str()
                    .map(runner_label)
                    .ok_or_else(|| Error::new("invalid_codex_config", "MCP执行器不是字符串"))
            })
            .transpose()?;
        records.push(McpRecord {
            name: name.into(),
            runner: runner.or(item
                .get("url")
                .map(|value| {
                    value
                        .as_str()
                        .map(|_| "remote".into())
                        .ok_or_else(|| Error::new("invalid_codex_config", "MCP URL不是字符串"))
                })
                .transpose()?),
            enabled,
        });
    }
    Ok(records)
}

#[derive(Deserialize)]
pub struct BrewBook {
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub formulae: Vec<BrewFormula>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub casks: Vec<BrewCask>,
}
#[derive(Deserialize)]
pub struct BrewFormula {
    pub name: String,
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub installed: Vec<BrewRelease>,
    #[serde(default)]
    pub linked_keg: Option<String>,
}
#[derive(Deserialize)]
pub struct BrewRelease {
    pub version: String,
}
#[derive(Deserialize)]
pub struct BrewCask {
    pub token: String,
    #[serde(default)]
    pub full_token: Option<String>,
    pub installed: Option<String>,
}
pub fn brew_book(bytes: &[u8]) -> Result<BrewBook> {
    strict_json(bytes).map_err(|_| Error::new("invalid_brew_inventory", "brew安装清单格式不兼容"))
}

pub fn text_packages(text: &str, cargo: bool) -> Result<BTreeMap<String, String>> {
    let mut packages = BTreeMap::new();
    let mut has_header = false;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        if line.starts_with(char::is_whitespace) || (!cargo && line.starts_with("- ")) {
            if !has_header {
                return Err(Error::new(
                    "invalid_tool_inventory",
                    "原生可执行项缺少所属包",
                ));
            }
            continue;
        }
        let words: Vec<_> = line.split_whitespace().collect();
        if words.len() < 2
            || !words[1].starts_with('v')
            || (cargo && !line.ends_with(':'))
            || !crate::policy::operation::parsers::registry_name(words[0])
        {
            return Err(Error::new("invalid_tool_inventory", "原生包清单格式不兼容"));
        }
        let version = crate::policy::operation::parsers::exact_version(
            words[1].trim_start_matches('v').trim_end_matches(':'),
        )
        .map_err(|_| Error::new("invalid_tool_inventory", "包版本格式不兼容"))?;
        if packages.insert(words[0].into(), version).is_some() {
            return Err(Error::new("invalid_tool_inventory", "原生包重复"));
        }
        has_header = true;
    }
    Ok(packages)
}

pub fn bun_packages(text: &str) -> Result<(std::path::PathBuf, BTreeMap<String, String>)> {
    crate::policy::operation::parsers::bun_inventory(text)
        .map_err(|_| Error::new("invalid_bun_inventory", "Bun清单格式不兼容"))
}

pub fn application(
    manager: &str,
    native: &str,
    name: &str,
    root: &std::path::Path,
    releases: Vec<crate::domain::resource::Release>,
) -> Result<crate::domain::resource::Resource> {
    use crate::domain::resource::{self, Data, Management, Resource, State};
    Ok(Resource {
        id: resource::id("application", &(manager, root, native))?,
        name: source_label(name),
        aliases: vec![],
        management: Management::Native,
        status: State::Present,
        data: Data::Application {
            manager: manager.into(),
            native_id: source_label(native),
            scope: root.display().to_string(),
            releases,
            baseline: None,
            issues: vec![],
            recipe_ref: None,
        },
    })
}

#[derive(Deserialize, Default)]
struct ClaudeProjects {
    #[serde(default, deserialize_with = "crate::policy::json::object_map")]
    projects: BTreeMap<String, McpJson>,
}
pub fn mcp_local(bytes: &[u8], project: &std::path::Path) -> Result<Vec<McpRecord>> {
    let data: ClaudeProjects = strict_json(bytes)
        .map_err(|_| Error::new("invalid_mcp_config", "MCP项目登记格式不兼容"))?;
    let Some(data) = data.projects.get(&project.display().to_string()) else {
        return Ok(vec![]);
    };
    let mut records = vec![];
    for (name, record) in &data.servers {
        if !component(name) {
            return Err(Error::new("invalid_mcp_id", "MCP名称不安全"));
        }
        records.push(McpRecord {
            name: name.clone(),
            runner: record
                .command
                .as_deref()
                .map(runner_label)
                .or(record.url.as_ref().map(|_| "remote".into())),
            enabled: record.disabled.map(|v| !v),
        });
    }
    Ok(records)
}

pub fn insert_capability(
    resources: &mut BTreeMap<String, crate::domain::resource::Resource>,
    incoming: crate::domain::resource::Resource,
) -> bool {
    use crate::domain::resource::{Data, State};
    use std::collections::btree_map::Entry;
    match resources.entry(incoming.id.clone()) {
        Entry::Vacant(slot) => {
            slot.insert(incoming);
            false
        }
        Entry::Occupied(mut slot) => {
            let current = slot.get_mut();
            current.status = State::Unknown;
            if let (
                Data::Capability {
                    contents, revision, ..
                },
                Data::Capability {
                    contents: extra,
                    revision: other,
                    ..
                },
            ) = (&mut current.data, incoming.data)
            {
                for item in extra {
                    if !contents
                        .iter()
                        .any(|c| c.path == item.path && c.realpath == item.realpath)
                    {
                        contents.push(item);
                    }
                }
                if revision.recorded != other.recorded {
                    revision.recorded = None;
                }
                if revision.source != other.source {
                    revision.source = None;
                }
            }
            true
        }
    }
}

pub fn plugin_settings_path(
    root: &std::path::Path,
    record: &PluginRecord,
) -> Option<std::path::PathBuf> {
    match record.scope.as_str() {
        "user" => Some(root.join("settings.json")),
        "project" => record
            .project
            .as_ref()
            .map(|p| p.join(".claude/settings.json")),
        "local" => record
            .project
            .as_ref()
            .map(|p| p.join(".claude/settings.local.json")),
        _ => None,
    }
}
