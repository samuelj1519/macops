use crate::{
    agent::ai_payload::{native_binary_digest, native_plugin_tree_digest},
    agent::native_plugins::{Instance, Recipe, Source, is_claude, refusal, valid_name},
    capability::capability_store::hash,
    domain::FileId,
    error::Result,
    platform::fs::{Bindings, BoundTarget, Directory, EntryKind, ObservedRegular},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

const FILE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Stamp {
    Missing,
    File(FileId, u32, String),
}

pub struct Passive {
    bindings: Bindings,
    root: Directory,
    files: Vec<(BoundTarget, Stamp)>,
    directories: Vec<(BoundTarget, Option<FileId>)>,
    binary: Option<(BoundTarget, PathBuf, Stamp)>,
    trees: Vec<(Directory, String, Option<PathBuf>)>,
}

fn file_stamp(observation: ObservedRegular) -> Result<(Stamp, Option<Vec<u8>>)> {
    match observation {
        ObservedRegular::Missing => Ok((Stamp::Missing, None)),
        ObservedRegular::Regular { bytes, metadata } => Ok((
            Stamp::File(metadata.identity, metadata.mode, hash(&bytes)),
            Some(bytes),
        )),
        ObservedRegular::NonRegular(_) => Err(refusal("unsafe_native_plugin_evidence")),
    }
}

impl Passive {
    pub fn new() -> Result<Self> {
        let root = Directory::open(Path::new("/"))?;
        Ok(Self {
            bindings: Bindings::new(128),
            root,
            files: vec![],
            directories: vec![],
            binary: None,
            trees: vec![],
        })
    }

    fn target(&mut self, path: &Path) -> Result<BoundTarget> {
        let path = crate::policy::paths::lexical(path)?;
        let relative = path
            .strip_prefix("/")
            .map_err(|_| refusal("unsafe_native_plugin_evidence"))?;
        self.bindings
            .relative(&self.root, relative)
            .map_err(|_| refusal("unsafe_native_plugin_evidence"))
    }

    pub fn file(&mut self, path: &Path) -> Result<Option<Vec<u8>>> {
        self.stamped_file(path).map(|(bytes, _)| bytes)
    }

    pub fn stamped_file(&mut self, path: &Path) -> Result<(Option<Vec<u8>>, &Stamp)> {
        let target = self.target(path)?;
        let (stamp, bytes) = file_stamp(target.observe_regular(FILE_LIMIT)?)?;
        self.files.push((target, stamp));
        Ok((
            bytes,
            &self
                .files
                .last()
                .ok_or_else(|| refusal("native_plugin_evidence_changed"))?
                .1,
        ))
    }

    pub fn directory(&mut self, path: &Path) -> Result<Option<Directory>> {
        let target = self.target(path)?;
        let directory = target.bound_directory()?;
        self.directories
            .push((target, directory.as_ref().map(|d| d.identity().clone())));
        Ok(directory)
    }

    pub fn binary(&mut self, path: &Path) -> Result<String> {
        match self.executable_stamp(path)? {
            Stamp::File(_, _, sha256) => Ok(sha256),
            Stamp::Missing => Err(refusal("native_binary_missing")),
        }
    }

    pub fn executable_stamp(&mut self, path: &Path) -> Result<Stamp> {
        let target = self.target(path)?;
        let stamp = binary(&target, path)?;
        self.binary = Some((target, path.to_path_buf(), stamp.clone()));
        Ok(stamp)
    }

    fn tree(&mut self, path: &Path) -> Result<String> {
        self.projected_tree(path, None)
    }

    pub fn projected_tree(&mut self, path: &Path, skip: Option<&Path>) -> Result<String> {
        let directory = self
            .directory(path)?
            .ok_or_else(|| refusal("native_plugin_payload_missing"))?;
        let sha256 = native_plugin_tree_digest(&directory, skip)?;
        self.trees
            .push((directory, sha256.clone(), skip.map(Path::to_path_buf)));
        Ok(sha256)
    }

    pub fn verify(&self) -> Result<()> {
        let changed = || refusal("native_plugin_evidence_changed");
        for (target, before) in &self.files {
            if file_stamp(target.observe_regular(FILE_LIMIT)?)?.0 != *before {
                return Err(changed());
            }
        }
        for (target, before) in &self.directories {
            if target.bound_directory()?.as_ref().map(Directory::identity) != before.as_ref() {
                return Err(changed());
            }
        }
        if let Some((target, path, digest)) = &self.binary
            && binary(target, path)? != *digest
        {
            return Err(changed());
        }
        for (root, digest, skip) in &self.trees {
            if native_plugin_tree_digest(root, skip.as_deref())? != *digest {
                return Err(changed());
            }
        }
        Ok(())
    }
}

fn binary(target: &BoundTarget, path: &Path) -> Result<Stamp> {
    let name = path
        .file_name()
        .ok_or_else(|| refusal("native_binary_unsupported"))?;
    let before = target
        .metadata()?
        .ok_or_else(|| refusal("native_binary_missing"))?;
    if before.kind != EntryKind::File {
        return Err(refusal("native_binary_unsupported"));
    }
    let digest = native_binary_digest(target.parent_directory()?, name)?;
    if target
        .metadata()?
        .is_none_or(|after| after.identity != before.identity || after.mode != before.mode)
    {
        return Err(refusal("native_plugin_binary_changed"));
    }
    Ok(Stamp::File(before.identity, before.mode, digest))
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Registration {
    Present,
    Absent,
    #[default]
    Unknown,
}

#[derive(Debug, Default, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Payload {
    LocalSource {
        sha256: String,
    },
    Copied {
        source_sha256: String,
        installed_sha256: String,
    },
    #[default]
    Unknown,
}

#[derive(Debug, Default)]
pub struct PluginFacts {
    pub marketplace_registered: Option<bool>,
    pub source_changed: bool,
    pub registration: Registration,
    pub configured_enabled: Option<bool>,
    pub version: Option<String>,
    pub payload: Payload,
    limitation: Option<&'static str>,
}
impl PluginFacts {
    pub fn reason(&self) -> Option<&'static str> {
        self.limitation
            .or_else(|| match (&self.registration, &self.payload) {
                (Registration::Unknown, _) => Some("native_plugin_registration_unknown"),
                (Registration::Absent, _) => Some("native_plugin_not_registered"),
                (_, Payload::Unknown) => Some("native_plugin_payload_unknown"),
                (
                    _,
                    Payload::Copied {
                        source_sha256,
                        installed_sha256,
                    },
                ) if source_sha256 != installed_sha256 => Some("native_plugin_payload_differs"),
                _ => None,
            })
    }
}

pub fn object_file(reader: &mut Passive, path: &Path) -> Result<Option<Value>> {
    let invalid = || refusal("invalid_native_plugin_metadata");
    reader
        .file(path)?
        .map(|bytes| {
            let data = crate::policy::json::parse(&bytes).map_err(|_| invalid())?;
            if !data.is_object() {
                return Err(invalid());
            }
            Ok(data)
        })
        .transpose()
}

fn child_object<'a>(
    value: &'a Value,
    name: &str,
) -> Result<Option<&'a serde_json::Map<String, Value>>> {
    value
        .get(name)
        .map(|child| {
            child
                .as_object()
                .ok_or_else(|| refusal("invalid_native_plugin_metadata"))
        })
        .transpose()
}

fn settings(reader: &mut Passive, path: &Path, id: &str) -> Result<Option<bool>> {
    let Some(data) = object_file(reader, path)? else {
        return Ok(None);
    };
    child_object(&data, "enabledPlugins")?
        .and_then(|plugins| plugins.get(id))
        .map(|entry| {
            entry
                .as_bool()
                .ok_or_else(|| refusal("invalid_native_plugin_metadata"))
        })
        .transpose()
}

fn match_claude_market(entry: &Value, recipe: &Recipe) -> Result<bool> {
    let invalid = || refusal("invalid_native_plugin_metadata");
    let source = child_object(entry, "source")?.ok_or_else(invalid)?;
    let origin = source
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    match (&recipe.source, origin) {
        (
            Source::LocalDirectory { root, .. } | Source::AcceptedRelease { root, .. },
            "directory",
        ) => {
            let path = source
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            let install = entry
                .get("installLocation")
                .and_then(Value::as_str)
                .ok_or_else(invalid)?;
            Ok(Path::new(path) == root && Path::new(install) == root)
        }
        (Source::NativeGit { repo }, "github") => Ok(source
            .get("repo")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?
            == repo),
        (_, "directory" | "file" | "github" | "git" | "url" | "npm") => Ok(false),
        _ => Err(refusal("native_plugin_marketplace_type_unsupported")),
    }
}

fn claude(reader: &mut Passive, instance: &Instance, recipe: &Recipe) -> Result<PluginFacts> {
    let mut result = PluginFacts::default();
    let id = recipe.native_id();
    let plugins = instance.plugin_root();
    let known = object_file(reader, &plugins.join("known_marketplaces.json"))?;
    if let Some(book) = known {
        result.marketplace_registered = Some(book.get(&recipe.marketplace).is_some());
        if let Some(entry) = book.get(&recipe.marketplace) {
            result.source_changed = !match_claude_market(entry, recipe)?;
        }
    }
    if result.source_changed {
        return Ok(result);
    }
    result.configured_enabled = settings(reader, &instance.config_root.join("settings.json"), &id)?;
    let Some(bytes) = reader.file(&plugins.join("installed_plugins.json"))? else {
        return Ok(result);
    };
    let book = crate::policy::resources::plugin_book(&bytes)?;
    let Some(records) = book.plugins.get(&id) else {
        result.registration = Registration::Absent;
        return Ok(result);
    };
    let mut user = records
        .iter()
        .filter(|record| record.scope == "user" && record.project.is_none());
    let Some(record) = user.next() else {
        result.limitation = Some("native_plugin_scope_unsupported");
        return Ok(result);
    };
    if user.next().is_some() {
        return Err(refusal("native_plugin_registration_ambiguous"));
    }
    result.registration = Registration::Present;
    result.version = record.version.as_ref().filter(|v| valid_name(v)).cloned();
    if result.marketplace_registered != Some(true) {
        result.limitation = Some("native_plugin_marketplace_unproven");
        return Ok(result);
    }
    if let Some((root, member, expected)) = local_source(&recipe.source) {
        let member_root = root.join(member);
        if record.path != member_root {
            result.limitation = Some("claude_inplace_payload_unproven");
            return Ok(result);
        }
        let observed = source_payload(reader, recipe, root, member, true)?;
        result.limitation =
            source_drift(expected, &observed).or(Some("claude_inplace_query_unverified"));
        result.payload = Payload::LocalSource { sha256: observed };
    } else {
        result.limitation = source_limitation(&recipe.source);
    }
    Ok(result)
}

fn local_source(source: &Source) -> Option<(&Path, &Path, Option<&str>)> {
    match source {
        Source::LocalDirectory {
            root,
            member,
            sha256,
        } => Some((root, member, sha256.as_deref())),
        Source::NativeGit { .. } | Source::AcceptedRelease { .. } => None,
    }
}

fn source_limitation(source: &Source) -> Option<&'static str> {
    match source {
        Source::LocalDirectory { .. } => None,
        Source::NativeGit { .. } => Some("native_git_payload_unproven"),
        Source::AcceptedRelease { .. } => Some("accepted_release_authority_required"),
    }
}

fn source_drift(expected: Option<&str>, observed: &str) -> Option<&'static str> {
    expected
        .filter(|expected| *expected != observed)
        .map(|_| "native_plugin_source_differs")
}

fn source_payload(
    reader: &mut Passive,
    recipe: &Recipe,
    root: &Path,
    member: &Path,
    claude: bool,
) -> Result<String> {
    let manifest = if claude {
        object_file(reader, &root.join(".claude-plugin/marketplace.json"))?
    } else {
        let mut manifest = None;
        // Codex 0.169.0 searches these layouts in order.
        for location in [
            ".agents/plugins/marketplace.json",
            ".agents/plugins/api_marketplace.json",
            ".claude-plugin/marketplace.json",
            ".cursor-plugin/marketplace.json",
        ] {
            manifest = object_file(reader, &root.join(location))?;
            if manifest.is_some() {
                break;
            }
        }
        manifest
    };
    let mismatch = || refusal("native_plugin_source_manifest_mismatch");
    let manifest = manifest.ok_or_else(mismatch)?;
    let plugins = manifest
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(mismatch)?;
    if plugins.len() > 512
        || manifest.get("name").and_then(Value::as_str) != Some(recipe.marketplace.as_str())
    {
        return Err(mismatch());
    }
    let mut matched = plugins
        .iter()
        .filter(|entry| entry.get("name").and_then(Value::as_str) == Some(recipe.plugin.as_str()));
    let source = matched
        .next()
        .and_then(|entry| entry.get("source"))
        .ok_or_else(mismatch)?;
    if matched.next().is_some() {
        return Err(mismatch());
    }
    let actual = if claude {
        source.as_str()
    } else {
        source
            .as_object()
            .and_then(|source| {
                (source.get("source").and_then(Value::as_str) == Some("local"))
                    .then(|| source.get("path").and_then(Value::as_str))
                    .flatten()
            })
            .or_else(|| source.as_str())
    };
    let actual = actual
        .and_then(|s| s.strip_prefix("./"))
        .ok_or_else(mismatch)?;
    if member.to_str() != Some(actual) {
        return Err(mismatch());
    }
    reader.tree(&root.join(member))
}

fn table(item: Option<&toml_edit::Item>) -> Result<Option<&dyn toml_edit::TableLike>> {
    item.map(|v| {
        v.as_table_like()
            .ok_or_else(|| refusal("invalid_native_plugin_metadata"))
    })
    .transpose()
}

pub fn codex(reader: &mut Passive, instance: &Instance, recipe: &Recipe) -> Result<PluginFacts> {
    let mut result = PluginFacts::default();
    let Some(raw) = reader.file(&instance.config_root.join("config.toml"))? else {
        return Ok(result);
    };
    let invalid = || refusal("invalid_native_plugin_metadata");
    let config = std::str::from_utf8(&raw)
        .map_err(|_| invalid())?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| invalid())?;
    let market =
        table(table(config.get("marketplaces"))?.and_then(|all| all.get(&recipe.marketplace)))?;
    result.marketplace_registered = Some(market.is_some());
    if let Some(registered) = market {
        let tag = registered
            .get("source_type")
            .and_then(toml_edit::Item::as_str)
            .ok_or_else(invalid)?;
        let value = registered
            .get("source")
            .and_then(toml_edit::Item::as_str)
            .ok_or_else(invalid)?;
        result.source_changed = match (&recipe.source, tag) {
            (
                Source::LocalDirectory { root, .. } | Source::AcceptedRelease { root, .. },
                "local",
            ) => Path::new(value) != root,
            (Source::NativeGit { repo }, "git") => {
                value != format!("https://github.com/{repo}.git")
            }
            (_, "local" | "git") => true,
            _ => return Err(refusal("native_plugin_marketplace_type_unsupported")),
        };
    }
    if result.source_changed {
        return Ok(result);
    }
    let id = recipe.native_id();
    let registration = table(table(config.get("plugins"))?.and_then(|all| all.get(&id)))?;
    result.configured_enabled = registration
        .and_then(|record| record.get("enabled"))
        .map(|field| field.as_bool().ok_or_else(invalid))
        .transpose()?;
    let base_path = instance
        .plugin_root()
        .join("cache")
        .join(&recipe.marketplace)
        .join(&recipe.plugin);
    let Some(cache) = reader.directory(&base_path)? else {
        if registration.is_none() {
            result.registration = Registration::Absent;
        }
        return Ok(result);
    };
    if cache
        .metadata(OsStr::new(".codex-remote-plugin-install.json"))?
        .is_some()
    {
        result.limitation = Some("codex_remote_plugin_provenance");
        return Ok(result);
    }
    let version = cache_version(&cache)?;
    let Some(version) = version else {
        return Ok(result);
    };
    if registration.is_none() || result.marketplace_registered != Some(true) {
        return Ok(result);
    }
    result.registration = Registration::Present;
    result.version = Some(version.clone());
    if let Some((root, member, expected)) = local_source(&recipe.source) {
        let source = source_payload(reader, recipe, root, member, false)?;
        let cached = reader.tree(&base_path.join(&version))?;
        if cache_version(&cache)?.as_deref() != Some(version.as_str())
            || cache
                .metadata(OsStr::new(".codex-remote-plugin-install.json"))?
                .is_some()
        {
            return Err(refusal("native_plugin_evidence_changed"));
        }
        result.limitation = source_drift(expected, &source);
        result.payload = Payload::Copied {
            source_sha256: source,
            installed_sha256: cached,
        };
    } else {
        result.limitation = source_limitation(&recipe.source);
    }
    Ok(result)
}

fn cache_version(cache: &Directory) -> Result<Option<String>> {
    let mut version = None;
    for entry in cache.names()? {
        let entry = entry.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let name = entry
            .file_name()
            .to_str()
            .map_err(|_| refusal("codex_plugin_cache_unproven"))?;
        if name == "." || name == ".." {
            continue;
        }
        if version.is_some()
            || !valid_name(name)
            || cache
                .metadata(OsStr::new(name))?
                .is_none_or(|meta| meta.kind != EntryKind::Directory)
        {
            return Err(refusal("codex_plugin_cache_unproven"));
        }
        version = Some(name.to_string());
    }
    cache.verify()?;
    Ok(version)
}

pub fn observe(id: &str, instance: &Instance, recipe: &Recipe) -> Result<PluginFacts> {
    let mut reader = Passive::new()?;
    let facts = if is_claude(id) {
        claude(&mut reader, instance, recipe)?
    } else {
        codex(&mut reader, instance, recipe)?
    };
    reader.verify()?;
    Ok(facts)
}

#[cfg(test)]
#[path = "../../tests/unit/native_plugins_files.rs"]
mod tests;
