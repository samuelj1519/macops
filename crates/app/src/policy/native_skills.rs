use crate::{
    domain::fingerprint,
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RECIPE_BOOK_NAME: &str = "native-skills-recipes.json";
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Runner {
    pub node: PathBuf,
    pub entry: PathBuf,
    pub sha256: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Source {
    pub repository: String,
    pub commit: String,
    pub skill_directory: String,
    pub member_name: String,
    pub tracking_ref: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recipe {
    pub schema_version: u32,
    pub id: String,
    pub owner: String,
    pub cli_version: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub source: Source,
    pub install_name: String,
    pub scope: String,
    pub client_scope: Vec<String>,
    pub copy: bool,
    pub manual_only: bool,
    pub license_files: Vec<String>,
    pub derived: Option<Value>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub runner: Runner,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Book {
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub recipes: Vec<Recipe>,
}
fn invalid() -> Error {
    Error::attention(
        "invalid_native_skills_recipe",
        "固定 Native Skills 配方、作用域或原生记录无法验证",
    )
}
fn native_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with(['.', '-'])
        && !value.ends_with(['.', '-'])
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
}
pub fn decode<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T> {
    let value = crate::policy::json::parse(raw)?;
    serde_json::from_value::<crate::policy::json::Object<T>>(value)
        .map(|v| v.0)
        .map_err(Into::into)
}
pub fn installation_id(payload: &Path) -> Result<String> {
    let path = crate::policy::paths::lexical(payload)?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(invalid)?;
    crate::policy::source_git::Component::parse(name)?;
    let mut parent = path.clone();
    parent.pop();
    crate::resource::id(
        "capability",
        &("skills-cli-install-v1", "global", &parent, name),
    )
}
pub fn skill_directory(raw: &str) -> Result<String> {
    let directory = raw.strip_suffix("/SKILL.md").unwrap_or(raw);
    let directory = if directory == "SKILL.md" {
        "."
    } else {
        directory
    };
    crate::policy::source_git::RelativePath::parse(directory)?;
    Ok(directory.into())
}
struct ParsedRecipe {
    rules: crate::policy::source_payload::Rules,
    reference: crate::policy::source_git::Reference,
}
pub fn validate(recipe: &Recipe) -> Result<()> {
    parse_recipe(recipe).map(|_| ())
}
fn parse_recipe(recipe: &Recipe) -> Result<ParsedRecipe> {
    if recipe.schema_version != 1
        || recipe.owner != "skills-cli"
        || recipe.cli_version != "1.9.0"
        || recipe.scope != "global"
        || recipe.client_scope != ["codex"]
        || !recipe.copy
        || !crate::policy::resources::component(&recipe.id)
        || !native_name(&recipe.install_name)
        || recipe.license_files.len() > 2
        || !recipe.runner.node.is_absolute()
        || !recipe.runner.entry.is_absolute()
        || recipe
            .runner
            .sha256
            .as_deref()
            .is_some_and(|s| !crate::policy::config_local::valid_digest(s))
    {
        return Err(invalid());
    }
    let repository = crate::policy::source_git::Repository::parse(&recipe.source.repository)?;
    crate::policy::source_git::ObjectId::parse(&recipe.source.commit)?;
    let directory = crate::policy::source_git::RelativePath::parse(&recipe.source.skill_directory)?;
    crate::policy::source_git::Component::parse(&recipe.source.member_name)?;
    if recipe.source.member_name.starts_with('-') || recipe.source.member_name == "*" {
        return Err(invalid());
    }
    let reference = crate::policy::source_git::Reference::parse(
        recipe
            .source
            .tracking_ref
            .as_deref()
            .or(Some(recipe.source.commit.as_str())),
    )?;
    let mut names = BTreeSet::new();
    let mut licenses = vec![];
    for path in &recipe.license_files {
        let parts = crate::policy::source_git::RelativePath::parse(path)?;
        let name = parts.parts().last().ok_or_else(invalid)?.as_str();
        if !names.insert(name.to_owned())
            || !matches!(name, "LICENSE" | "ASSET_LICENSES.md" | "LICENSE-ASSETS.md")
        {
            return Err(invalid());
        }
        licenses.push(parts);
    }
    let mut rules = if let Some(value) = &recipe.derived {
        if !recipe
            .source
            .repository
            .eq_ignore_ascii_case("larashero3-dotcom/writing-dna-skill")
            || recipe.source.skill_directory != "."
            || !recipe.manual_only
        {
            return Err(invalid());
        }
        let rules = crate::policy::source_payload::Rules::declared(value)?;
        if rules.repository != repository
            || rules.path != directory
            || rules.exclude != ["skills/lieflat-less-ai-tone"]
            || rules.edits.len() != 1
            || rules.edits[0].path != "SKILL.md"
            || !rules.manual
        {
            return Err(invalid());
        }
        rules
    } else {
        crate::policy::source_payload::Rules::native(repository, directory)
    };
    rules.manual = recipe.manual_only;
    rules.licenses = licenses;
    Ok(ParsedRecipe { rules, reference })
}

pub fn book(raw: Option<&[u8]>) -> Result<Book> {
    let book = raw.map(decode).transpose()?.unwrap_or(Book {
        schema_version: 1,
        recipes: vec![],
    });
    if book.schema_version != 1 || book.recipes.len() > 5000 {
        return Err(invalid());
    }
    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for recipe in &book.recipes {
        validate(recipe)?;
        if recipe.runner.sha256.is_none() {
            return Err(invalid());
        }
        if !names.insert(recipe.install_name.to_ascii_lowercase()) || !ids.insert(&recipe.id) {
            return Err(invalid());
        }
    }
    Ok(book)
}
pub fn add_argv(recipe: &Recipe, local: Option<&Path>) -> Result<Vec<String>> {
    validate(recipe)?;
    let source = if let Some(local) = local {
        crate::policy::paths::lexical(local)?
            .to_str()
            .ok_or_else(invalid)?
            .into()
    } else {
        format!(
            "https://github.com/{}/tree/{}{}",
            recipe.source.repository,
            recipe.source.commit,
            if recipe.source.skill_directory == "." {
                String::new()
            } else {
                format!("/{}", recipe.source.skill_directory)
            }
        )
    };
    let mut args = vec![
        recipe.runner.entry.to_str().ok_or_else(invalid)?.into(),
        "add".into(),
        source,
        "--global".into(),
        "--yes".into(),
        "--agent".into(),
        "codex".into(),
        "--copy".into(),
        "--skill".into(),
        recipe.source.member_name.clone(),
        "--json".into(),
    ];
    if recipe.install_name != recipe.source.member_name {
        args.extend(["--rename".into(), recipe.install_name.clone()]);
    }
    Ok(args)
}
pub fn registry(raw: Option<&[u8]>) -> Result<BTreeMap<String, Value>> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    let value = crate::policy::json::parse(raw)?;
    let root = value.as_object().ok_or_else(invalid)?;
    if root.get("version").and_then(Value::as_u64) != Some(3)
        || root.keys().any(|k| {
            !["version", "skills", "dismissed", "lastSelectedAgents"].contains(&k.as_str())
        })
    {
        return Err(invalid());
    }
    if let Some(value) = root.get("dismissed")
        && !value.as_object().is_some_and(|m| {
            m.iter()
                .all(|(k, v)| k == "findSkillsPrompt" && v.is_boolean())
        })
    {
        return Err(invalid());
    }
    if let Some(value) = root.get("lastSelectedAgents") {
        let agents = value.as_array().ok_or_else(invalid)?;
        let mut seen = BTreeSet::new();
        if agents
            .iter()
            .any(|v| !v.as_str().is_some_and(|s| !s.is_empty() && seen.insert(s)))
        {
            return Err(invalid());
        }
    }
    let rows = root
        .get("skills")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    if rows.len() > 5000 {
        return Err(invalid());
    }
    let mut seen = BTreeSet::new();
    for (name, row) in rows {
        if !native_name(name) || !seen.insert(name.to_ascii_lowercase()) {
            return Err(invalid());
        }
        registry_row(row)?;
    }
    Ok(rows.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
}

fn registry_row(row: &Value) -> Result<()> {
    let row = row.as_object().ok_or_else(invalid)?;
    let required = [
        "source",
        "sourceType",
        "sourceUrl",
        "skillFolderHash",
        "installedAt",
        "updatedAt",
    ];
    if required
        .iter()
        .any(|key| !row.get(*key).is_some_and(Value::is_string))
    {
        return Err(invalid());
    }
    let allowed = [
        "source",
        "sourceType",
        "sourceUrl",
        "ref",
        "commit",
        "skillPath",
        "skillName",
        "skillFolderHash",
        "installedAt",
        "updatedAt",
        "pluginName",
        "sourceBaseUrl",
        "wellKnownDigest",
        "installMode",
    ];
    if row.iter().any(|(key, value)| {
        !allowed.contains(&key.as_str())
            || !value
                .as_str()
                .is_some_and(|text| text.len() <= 4096 && !text.chars().any(char::is_control))
    }) {
        return Err(invalid());
    }
    if row.get("installMode").is_some_and(|v| v != "copy") {
        return Err(invalid());
    }
    if row
        .get("source")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
        || row
            .get("sourceType")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
    {
        return Err(invalid());
    }
    if let Some(commit) = row.get("commit").and_then(Value::as_str) {
        crate::policy::source_git::ObjectId::parse(commit)?;
    }
    if let Some(path) = row.get("skillPath").and_then(Value::as_str) {
        skill_directory(path)?;
    }
    Ok(())
}
pub fn selected_after(recipe: &Recipe, row: &Value, local: Option<&Path>) -> Result<()> {
    let parsed = parse_recipe(recipe)?;
    if row["installMode"] != "copy" {
        return Err(invalid());
    }
    if (recipe.install_name != recipe.source.member_name || row.get("skillName").is_some())
        && row["skillName"] != recipe.source.member_name
    {
        return Err(invalid());
    }
    if let Some(local) = local {
        if row["sourceType"] != "local"
            || row["source"] != local.to_str().ok_or_else(invalid)?
            || row["sourceUrl"] != row["source"]
        {
            return Err(invalid());
        }
    } else {
        if row["sourceType"] != "github"
            || row["source"].as_str().map(str::to_ascii_lowercase)
                != Some(recipe.source.repository.to_ascii_lowercase())
            || row["commit"] != recipe.source.commit
            || row["ref"] != recipe.source.commit
            || skill_directory(row["skillPath"].as_str().ok_or_else(invalid)?)?
                != recipe.source.skill_directory
        {
            return Err(invalid());
        }
        parsed
            .rules
            .repository
            .verify_url(row["sourceUrl"].as_str().ok_or_else(invalid)?)?;
    }
    let hash = row["skillFolderHash"].as_str().ok_or_else(invalid)?;
    if !matches!(hash.len(), 40 | 64) || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    Ok(())
}
pub fn recipe_hash(recipe: &Recipe) -> Result<String> {
    validate(recipe)?;
    fingerprint(recipe)
}
pub fn merge(mut book: Book, recipe: &Recipe) -> Result<Book> {
    validate(recipe)?;
    if book.recipes.iter().any(|r| {
        (r.id == recipe.id && r.install_name != recipe.install_name)
            || (r.id != recipe.id && r.install_name == recipe.install_name)
    }) {
        return Err(invalid());
    }
    book.recipes.retain(|r| r.id != recipe.id);
    book.recipes.push(recipe.clone());
    self::book(Some(&serde_json::to_vec(&book)?))
}
pub fn locator(recipe: &Recipe, installation: &str) -> Result<String> {
    parse_recipe(recipe).and_then(|parsed| {
        parsed
            .rules
            .locator(installation, &parsed.reference)
            .and_then(|locator| {
                fingerprint(&(
                    locator,
                    &recipe.source.member_name,
                    &recipe.install_name,
                    &recipe.cli_version,
                    &recipe.scope,
                    &recipe.client_scope,
                    &recipe.runner,
                    &recipe.derived,
                ))
            })
    })
}

pub fn native_scope(scope: &crate::domain::Scope) -> Result<String> {
    fingerprint(&(
        &scope.home,
        [
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
        ]
        .map(|key| scope.paths.get(key)),
    ))
}
pub fn node_version(raw: &str) -> Result<()> {
    let values = raw
        .trim()
        .strip_prefix('v')
        .ok_or_else(invalid)?
        .split('.')
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid())?;
    if values.len() != 3 || (values[0] < 22 || values[0] == 22 && values[1] < 20) {
        return Err(Error::attention(
            "native_node_version_unsupported",
            "固定 Skills 1.9.0 需要现有 Node >=22.20.0，不隐式安装 runner",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/native_skills_policy.rs"]
mod tests;
