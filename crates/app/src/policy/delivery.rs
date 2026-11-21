use crate::error::{Error, Result};
use serde::{Deserialize, Deserializer, Serialize};

fn invalid() -> Error {
    Error::attention("invalid_link_text", "链接文字或技能组件无法安全验证")
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct LinkText(String);
impl LinkText {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 4096 || bytes.contains(&0) {
            return Err(invalid());
        }
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        Ok(Self(text.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn managed(name: &str) -> Result<Self> {
        if name.len() > 255
            || !crate::policy::resources::component(name)
            || name.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        Self::from_bytes(format!("../../.agents/skills/{name}").as_bytes())
    }
    pub fn owned_by(&self, name: &str) -> bool {
        Self::managed(name).is_ok_and(|expected| *self == expected)
    }
}
impl<'de> Deserialize<'de> for LinkText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::from_bytes(value.as_bytes())
            .map_err(|_| serde::de::Error::custom("invalid literal link text"))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/delivery_policy.rs"]
mod tests;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    Mi,
    Kimi,
    Glm,
    Zen,
}
impl Profile {
    pub const ALL: [Self; 4] = [Self::Mi, Self::Kimi, Self::Glm, Self::Zen];
    pub fn parse(key: &str) -> Result<Self> {
        match key {
            "mi" => Ok(Self::Mi),
            "kimi" => Ok(Self::Kimi),
            "glm" => Ok(Self::Glm),
            "zen" => Ok(Self::Zen),
            _ => Err(declaration_error()),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Mi => "mi",
            Self::Kimi => "kimi",
            Self::Glm => "glm",
            Self::Zen => "zen",
        }
    }
    pub fn root(self, home: &Path) -> Result<PathBuf> {
        Ok(crate::policy::paths::lexical(home)?.join(format!(".claude-{}", self.name())))
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkRule {
    All,
    Selected(BTreeSet<String>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileRule {
    pub links: LinkRule,
    pub plugins_off: Option<BTreeSet<String>>,
    pub pstack_allow: Option<BTreeSet<String>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profiles {
    pub rules: BTreeMap<Profile, ProfileRule>,
    pub declared: BTreeSet<Profile>,
    pub codex_disabled: Option<BTreeSet<String>>,
}
impl Default for Profiles {
    fn default() -> Self {
        Self {
            rules: Profile::ALL
                .into_iter()
                .map(|p| {
                    (
                        p,
                        ProfileRule {
                            links: LinkRule::All,
                            plugins_off: None,
                            pstack_allow: None,
                        },
                    )
                })
                .collect(),
            declared: BTreeSet::new(),
            codex_disabled: None,
        }
    }
}
fn declaration_error() -> Error {
    Error::attention(
        "invalid_skill_profiles",
        "能力投放声明的形状、profile 或名称无法验证",
    )
}

fn present_object<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    crate::policy::json::object(d).map(Some)
}
fn present_names<'de, D: Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Vec<String>>, D::Error> {
    Vec::<String>::deserialize(d).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfilesInput {
    #[serde(default)]
    _comment: Option<String>,
    #[serde(default, deserialize_with = "present_object")]
    claude: Option<BTreeMap<String, RuleInput>>,
    #[serde(default, deserialize_with = "present_object")]
    codex: Option<CodexInput>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum RuleInput {
    All(String),
    Specific(crate::policy::json::Object<Specific>),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Specific {
    #[serde(default)]
    _comment: Option<String>,
    #[serde(default, deserialize_with = "present_names")]
    links: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present_names")]
    plugins_off: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present_names")]
    pstack_allow: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexInput {
    #[serde(default)]
    _comment: Option<String>,
    #[serde(default, deserialize_with = "present_names")]
    disable_agents_skills: Option<Vec<String>>,
}
fn names(values: Vec<String>, plugins: bool) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    if values.len() > 5000 {
        return Err(declaration_error());
    }
    for value in values {
        let valid = if plugins {
            value.len() <= 512
                && !value.chars().any(char::is_control)
                && crate::policy::capability_local::valid_plugin_id(&value)
        } else {
            LinkText::managed(&value).is_ok()
        };
        if !valid || !names.insert(value) {
            return Err(declaration_error());
        }
    }
    Ok(names)
}
fn rule(input: RuleInput) -> Result<ProfileRule> {
    match input {
        RuleInput::All(word) if word == "all" => Ok(ProfileRule {
            links: LinkRule::All,
            plugins_off: None,
            pstack_allow: None,
        }),
        RuleInput::All(_) => Err(declaration_error()),
        RuleInput::Specific(crate::policy::json::Object(spec)) => Ok(ProfileRule {
            links: spec
                .links
                .map(|v| names(v, false))
                .transpose()?
                .map_or(LinkRule::All, LinkRule::Selected),
            plugins_off: spec.plugins_off.map(|v| names(v, true)).transpose()?,
            pstack_allow: spec.pstack_allow.map(|v| names(v, false)).transpose()?,
        }),
    }
}
pub fn profiles(bytes: &[u8]) -> Result<Profiles> {
    if bytes.len() > 1024 * 1024 {
        return Err(declaration_error());
    }
    let value = crate::policy::json::parse(bytes).map_err(|_| declaration_error())?;
    let crate::policy::json::Object(input) =
        serde_json::from_value::<crate::policy::json::Object<ProfilesInput>>(value)
            .map_err(|_| declaration_error())?;
    let mut result = Profiles::default();
    for (key, input) in input.claude.unwrap_or_default() {
        if key == "_comment" {
            if !matches!(input, RuleInput::All(_)) {
                return Err(declaration_error());
            }
            continue;
        }
        let profile = Profile::parse(&key)?;
        result.rules.insert(profile, rule(input)?);
        result.declared.insert(profile);
    }
    result.codex_disabled = input
        .codex
        .and_then(|v| v.disable_agents_skills)
        .map(|v| names(v, false))
        .transpose()?;
    Ok(result)
}
impl Profiles {
    pub fn pending_config_policies(&self) -> Vec<String> {
        let mut rows = vec![];
        for (profile, rule) in &self.rules {
            if rule.plugins_off.is_some() {
                rows.push(format!("claude:{}:plugins_off", profile.name()));
            }
            if rule.pstack_allow.is_some() {
                rows.push(format!("claude:{}:pstack_allow", profile.name()));
            }
        }
        if self.codex_disabled.is_some() {
            rows.push("codex:disable_agents_skills".into());
        }
        rows
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(
    tag = "type",
    content = "selectors",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Selection {
    All,
    Explicit(Vec<String>),
}
pub fn selection(
    name: Option<&str>,
    only: &[String],
    all: bool,
    revision: Option<&str>,
    change: Option<&str>,
) -> Result<Selection> {
    if let Some(change) = change {
        crate::policy::skills::parse_change_id(change)
            .map_err(|_| Error::new("invalid_change_id", "change-id 无效"))?;
    }
    if let Some(revision) = revision {
        crate::policy::skills::revision(revision)?;
        return Err(Error::attention(
            "revision_requires_upgrade",
            "链接同步不修改版本；使用 skills upgrade --revision",
        ));
    }
    if (name.is_some() && !only.is_empty()) || (all && (name.is_some() || !only.is_empty())) {
        return Err(Error::new(
            "selection_conflict",
            "name、--only 与 --all 不能混用",
        ));
    }
    if all {
        return Ok(Selection::All);
    }
    let keys: Vec<_> = name
        .into_iter()
        .map(str::to_owned)
        .chain(only.iter().cloned())
        .collect();
    if keys.is_empty() {
        return Err(Error::new("selection_required", "请明确选择或使用 --all"));
    }
    let mut seen = BTreeSet::new();
    if keys.len() > 5000
        || keys.iter().any(|key| {
            key.is_empty()
                || key.len() > 512
                || key.chars().any(char::is_control)
                || !seen.insert(key)
        })
    {
        return Err(Error::new(
            "invalid_link_selection",
            "选择器包含空值、重复项或无法验证的文字",
        ));
    }
    Ok(Selection::Explicit(keys))
}

#[derive(Clone, Debug)]
pub struct LinkUnit {
    pub id: String,
    pub name: String,
    pub aliases: BTreeSet<String>,
}
#[derive(Clone, Debug)]
pub struct SourceGroup {
    pub id: String,
    pub aliases: BTreeSet<String>,
    pub members: BTreeSet<String>,
}

#[derive(Clone, Copy)]
enum Match {
    Unit(usize),
    Group(usize),
}
fn selector_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 512 && !key.chars().any(char::is_control)
}
fn source_index<'a>(
    units: &'a [LinkUnit],
    groups: &'a [SourceGroup],
) -> Result<BTreeMap<&'a str, Vec<Match>>> {
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut group_ids = BTreeSet::new();
    let mut index: BTreeMap<&str, Vec<Match>> = BTreeMap::new();
    if units.len() > 5000 || groups.len() > 5000 {
        return Err(declaration_error());
    }
    for (position, unit) in units.iter().enumerate() {
        if !selector_key(&unit.id)
            || LinkText::managed(&unit.name).is_err()
            || !ids.insert(unit.id.as_str())
            || !names.insert(unit.name.as_str())
        {
            return Err(declaration_error());
        }
        let keys: BTreeSet<_> = std::iter::once(unit.id.as_str())
            .chain(std::iter::once(unit.name.as_str()))
            .chain(unit.aliases.iter().map(String::as_str))
            .collect();
        for key in keys {
            if !selector_key(key) {
                return Err(declaration_error());
            }
            index.entry(key).or_default().push(Match::Unit(position));
        }
    }
    let mut members = 0;
    for (position, group) in groups.iter().enumerate() {
        members += group.members.len();
        if !selector_key(&group.id)
            || !group_ids.insert(group.id.as_str())
            || members > 20000
            || group.members.iter().any(|id| !ids.contains(id.as_str()))
        {
            return Err(declaration_error());
        }
        let keys: BTreeSet<_> = std::iter::once(group.id.as_str())
            .chain(group.aliases.iter().map(String::as_str))
            .collect();
        for key in keys {
            if !selector_key(key) {
                return Err(declaration_error());
            }
            index.entry(key).or_default().push(Match::Group(position));
        }
    }
    if index.len() > 20000 {
        return Err(declaration_error());
    }
    Ok(index)
}
pub fn resolve(
    request: &Selection,
    units: &[LinkUnit],
    groups: &[SourceGroup],
) -> Result<BTreeSet<String>> {
    let index = source_index(units, groups)?;
    let Selection::Explicit(keys) = request else {
        return Ok(units.iter().map(|u| u.id.clone()).collect());
    };
    let mut selected = BTreeSet::new();
    for key in keys {
        let matches = index.get(key.as_str()).ok_or_else(|| {
            Error::attention(
                "link_selection_not_found",
                "未找到所选来源或 canonical 技能",
            )
        })?;
        let [one] = matches.as_slice() else {
            return Err(Error::attention(
                "ambiguous_link_selection",
                "名称指向多个来源；需要明确安装实例或来源 ID",
            ));
        };
        let members = match one {
            Match::Unit(position) => BTreeSet::from([units[*position].id.clone()]),
            Match::Group(position) => groups[*position].members.clone(),
        };
        if members.is_empty() {
            return Err(Error::attention(
                "no_linkable_members",
                "所选来源没有可验证的 canonical 技能，不猜测插件的投放",
            ));
        }
        for member in members {
            if !selected.insert(member) {
                return Err(Error::attention(
                    "link_selection_overlap",
                    "选择重复或来源与其成员重叠",
                ));
            }
        }
    }
    Ok(selected)
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum ChangeKind {
    #[serde(rename = "frontmatter")]
    Frontmatter,
    #[serde(rename = "codex-policy")]
    CodexPolicy,
    #[serde(rename = "managed-link")]
    ManagedLink,
    #[serde(rename = "profile-json")]
    ProfileJson,
    #[serde(rename = "profile-toml")]
    ProfileToml,
}
impl From<crate::policy::invocation::MetadataKind> for ChangeKind {
    fn from(kind: crate::policy::invocation::MetadataKind) -> Self {
        match kind {
            crate::policy::invocation::MetadataKind::Frontmatter => Self::Frontmatter,
            crate::policy::invocation::MetadataKind::CodexPolicy => Self::CodexPolicy,
        }
    }
}
impl ChangeKind {
    pub fn metadata(self) -> Result<crate::policy::invocation::MetadataKind> {
        match self {
            Self::Frontmatter => Ok(crate::policy::invocation::MetadataKind::Frontmatter),
            Self::CodexPolicy => Ok(crate::policy::invocation::MetadataKind::CodexPolicy),
            Self::ManagedLink | Self::ProfileJson | Self::ProfileToml => Err(Error::attention(
                "invalid_metadata_change_kind",
                "链接变更不是 metadata 写权限",
            )),
        }
    }
}

pub fn link_target(home: &Path, profile: Profile, name: &str) -> Result<PathBuf> {
    LinkText::managed(name)?;
    if name.to_ascii_lowercase().starts_with("devnorm") {
        return Err(Error::attention(
            "protected_delivery_target",
            "该能力交由原生交付维护",
        ));
    }
    Ok(profile.root(home)?.join("skills").join(name))
}
pub fn link_target_parts(home: &Path, path: &Path) -> Result<(Profile, String)> {
    let path = crate::policy::paths::lexical(path)?;
    let relative = path.strip_prefix(home).map_err(|_| declaration_error())?;
    let parts: Vec<_> = relative
        .components()
        .map(|p| p.as_os_str().to_str().ok_or_else(declaration_error))
        .collect::<Result<_>>()?;
    let [folder, "skills", name] = parts.as_slice() else {
        return Err(declaration_error());
    };
    let profile = Profile::parse(
        folder
            .strip_prefix(".claude-")
            .ok_or_else(declaration_error)?,
    )?;
    link_target(home, profile, name)?;
    Ok((profile, (*name).into()))
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetFacts {
    pub standalone: BTreeSet<String>,
    pub repositories: BTreeMap<String, String>,
    pub copies: BTreeMap<Profile, BTreeSet<String>>,
}
pub fn asset_facts(bytes: &[u8]) -> Result<AssetFacts> {
    let value = crate::policy::json::parse(bytes).map_err(|_| declaration_error())?;
    let root = value.as_object().ok_or_else(declaration_error)?;
    if root
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(declaration_error());
    }
    let assets = root
        .get("assets")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(declaration_error)?;
    if assets.len() > 5000 {
        return Err(declaration_error());
    }
    let mut facts = AssetFacts::default();
    for (name, spec) in assets {
        LinkText::managed(name)?;
        let spec = spec.as_object().ok_or_else(declaration_error)?;
        let kind = spec
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(declaration_error)?;
        if kind == "github-skill" {
            let repo = spec
                .get("repository")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(declaration_error)?;
            facts
                .repositories
                .insert(name.clone(), github_repository(repo)?);
            facts.standalone.insert(name.clone());
        }
        if kind != "local-release-plugin" {
            continue;
        }
        let copied = if let Some(names) = spec.get("copy_skills") {
            serde_json::from_value::<Vec<String>>(names.clone()).map_err(|_| declaration_error())?
        } else {
            vec![
                spec.get("name")
                    .map_or(Ok(name.as_str()), |v| {
                        v.as_str().ok_or_else(declaration_error)
                    })?
                    .into(),
            ]
        };
        let copied = names(copied, false)?;
        let profiles = spec
            .get("claude_profiles")
            .map(|v| {
                serde_json::from_value::<Vec<String>>(v.clone()).map_err(|_| declaration_error())
            })
            .transpose()?
            .unwrap_or_default();
        for key in profiles {
            if key == "default" {
                continue;
            }
            facts
                .copies
                .entry(Profile::parse(&key)?)
                .or_default()
                .extend(copied.iter().cloned());
        }
    }
    Ok(facts)
}

pub fn github_repository(value: &str) -> Result<String> {
    let value = value.strip_prefix("https://github.com/").unwrap_or(value);
    let parts: Vec<_> = value.split('/').collect();
    let [owner, repo] = parts.as_slice() else {
        return Err(declaration_error());
    };
    let component = |s: &str| {
        !s.is_empty()
            && s.len() <= 255
            && !matches!(s, "." | "..")
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    if !component(owner) || !component(repo) {
        return Err(declaration_error());
    }
    Ok(format!("{owner}/{repo}"))
}

pub fn native_lock_path(home: &Path, scope: &crate::domain::Scope) -> PathBuf {
    scope
        .paths
        .get("XDG_STATE_HOME")
        .and_then(Option::as_ref)
        .map_or_else(
            || home.join(".agents/.skill-lock.json"),
            |p| p.join("skills/.skill-lock.json"),
        )
}
