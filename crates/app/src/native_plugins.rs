use crate::{
    cli::NativePlugins,
    context::Context,
    error::{Error, Result},
    native_plugins_files::{Passive, PluginFacts},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

const INSTANCES: [&str; 7] = [
    "claude:default",
    "claude:mi",
    "claude:kimi",
    "claude:glm",
    "claude:zen",
    "codex",
    "cursor",
];
pub const RECIPE_FILE: &str = "native-plugin-recipes.json";
const CLAUDE_READ_VERSIONS: [&str; 4] = ["2.1.310", "2.1.316", "2.1.323", "2.1.325"];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub instances: BTreeMap<String, Instance>,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub recipes: BTreeMap<String, Recipe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub home: PathBuf,
    pub config_root: PathBuf,
    pub cwd: PathBuf,
    pub binary: PathBuf,
    pub binary_sha256: String,
    pub version: String,
    pub plugin_root: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub revision: String,
    pub instances: Vec<String>,
    pub marketplace: String,
    pub plugin: String,
    pub source: Source,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    LocalDirectory {
        root: PathBuf,
        member: PathBuf,
        sha256: Option<String>,
    },
    NativeGit {
        repo: String,
    },
    AcceptedRelease {
        root: PathBuf,
        member: PathBuf,
        artifact: String,
        candidate: String,
    },
}

pub fn refusal(code: &str) -> Error {
    Error::attention(code, "固定原生插件配方的本地证据不足")
}

pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || matches!(c, b'a'..=b'f'))
}

pub fn is_claude(id: &str) -> bool {
    id.starts_with("claude:")
}

fn physical_path(path: &Path) -> bool {
    crate::policy::paths::lexical(path).is_ok_and(|clean| clean == path)
        && path.parent().is_some()
        && path.components().count() <= 32
}

fn member_path(path: &Path) -> bool {
    path.to_str().is_some_and(|s| {
        !s.is_empty()
            && s.len() <= 512
            && s.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.chars().any(|c| c.is_control() || c == '\\')
            })
            && path.components().count() <= 16
    })
}

impl Instance {
    pub fn plugin_root(&self) -> PathBuf {
        self.plugin_root
            .clone()
            .unwrap_or_else(|| self.config_root.join("plugins"))
    }

    fn valid(&self, id: &str) -> bool {
        [
            &self.home,
            &self.config_root,
            &self.cwd,
            &self.binary,
            &self.plugin_root(),
        ]
        .into_iter()
        .all(|path| physical_path(path))
            && digest(&self.binary_sha256)
            && valid_name(&self.version)
            && (is_claude(id) || self.plugin_root.is_none())
    }
}

impl Recipe {
    pub fn native_id(&self) -> String {
        format!("{}@{}", self.plugin, self.marketplace)
    }

    fn valid(&self) -> bool {
        let selected: BTreeSet<_> = self.instances.iter().collect();
        let valid_source = match &self.source {
            Source::LocalDirectory {
                root,
                member,
                sha256,
            } => physical_path(root) && member_path(member) && sha256.as_deref().is_none_or(digest),
            Source::NativeGit { repo } => repo
                .split_once('/')
                .is_some_and(|(owner, name)| valid_name(owner) && valid_name(name)),
            Source::AcceptedRelease {
                root,
                member,
                artifact,
                candidate,
            } => {
                physical_path(root) && member_path(member) && digest(artifact) && digest(candidate)
            }
        };
        valid_name(&self.revision)
            && valid_name(&self.marketplace)
            && valid_name(&self.plugin)
            && !selected.is_empty()
            && selected.len() == self.instances.len()
            && selected.iter().all(|id| INSTANCES.contains(&id.as_str()))
            && valid_source
    }
}

impl Registry {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let invalid = || refusal("invalid_native_plugin_recipes");
        let value = crate::policy::json::parse(bytes).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        let registry: Self = serde_json::from_value(value).map_err(|_| invalid())?;
        if registry.schema_version != 1
            || registry.recipes.len() > 64
            || registry
                .instances
                .iter()
                .any(|(id, binding)| !INSTANCES.contains(&id.as_str()) || !binding.valid(id))
            || registry
                .recipes
                .iter()
                .any(|(id, recipe)| !valid_name(id) || !recipe.valid())
        {
            return Err(invalid());
        }
        Ok(registry)
    }

    fn read(ctx: &Context) -> Result<Self> {
        let path = ctx.config.join(RECIPE_FILE);
        let mut reader = Passive::new()?;
        let bytes = reader.file(&path)?;
        reader.verify()?;
        bytes
            .as_deref()
            .map_or_else(|| Ok(Self::default()), Self::parse)
    }

    fn scoped_instances(&self, ctx: &Context) -> BTreeMap<String, Result<InstanceFacts>> {
        self.instances
            .iter()
            .map(|(id, instance)| (id.clone(), instance_facts(ctx, id, instance)))
            .collect()
    }
}

#[derive(Debug)]
struct InstanceFacts {
    plugin_root: PathBuf,
    config_root: crate::domain::FileId,
    plugin_identity: Option<crate::domain::FileId>,
}

fn instance_facts(ctx: &Context, id: &str, instance: &Instance) -> Result<InstanceFacts> {
    if instance.home != ctx.home {
        return Err(refusal("native_plugin_home_changed"));
    }
    if (is_claude(id) && !CLAUDE_READ_VERSIONS.contains(&instance.version.as_str()))
        || (id == "codex" && instance.version != "0.169.0")
    {
        return Err(refusal("native_plugin_file_contract_unsupported"));
    }
    if id == "cursor" {
        return Err(refusal("cursor_plugin_interactive_only"));
    }
    let mut reader = Passive::new()?;
    let config_dir = reader
        .directory(&instance.config_root)?
        .ok_or_else(|| refusal("native_plugin_config_root_missing"))?;
    let plugin_root = instance.plugin_root();
    let plugin_identity = reader
        .directory(&plugin_root)?
        .map(|root| root.identity().clone());
    if reader.directory(&instance.cwd)?.is_none() {
        return Err(refusal("native_plugin_cwd_missing"));
    }
    let binary = reader.binary(&instance.binary)?;
    if binary != instance.binary_sha256 {
        return Err(refusal("native_plugin_binary_changed"));
    }
    reader.verify()?;
    Ok(InstanceFacts {
        plugin_root,
        config_root: config_dir.identity().clone(),
        plugin_identity,
    })
}

fn storage_overlap(
    first: &Instance,
    a: &InstanceFacts,
    second: &Instance,
    b: &InstanceFacts,
) -> bool {
    a.config_root == b.config_root
        || a.plugin_identity.as_ref().is_some_and(|identity| {
            Some(identity) == b.plugin_identity.as_ref() || *identity == b.config_root
        })
        || b.plugin_identity
            .as_ref()
            .is_some_and(|identity| *identity == a.config_root)
        || [&first.config_root, &a.plugin_root]
            .into_iter()
            .any(|path| {
                [&second.config_root, &b.plugin_root]
                    .into_iter()
                    .any(|other| path.starts_with(other) || other.starts_with(path))
            })
}

fn reason_row(id: &str, reason: &str) -> Value {
    json!({"instance":id,"status":"unknown","reason":reason,"loaded":"unknown"})
}

fn bindings(ctx: &Context, r: &Registry, requested: Option<&str>) -> Result<Value> {
    if requested.is_some_and(|id| !INSTANCES.contains(&id)) {
        return Err(Error::new(
            "unknown_native_plugin_instance",
            "未登记的客户端实例",
        ));
    }
    let facts = r.scoped_instances(ctx);
    let mut results = Vec::new();
    for id in INSTANCES
        .into_iter()
        .filter(|id| requested.is_none_or(|wanted| *id == wanted))
    {
        let Some(binding) = r.instances.get(id) else {
            results.push(reason_row(id, "instance_not_bound"));
            continue;
        };
        let Some(Ok(bound)) = facts.get(id) else {
            let reason = facts
                .get(id)
                .and_then(|f| f.as_ref().err())
                .map_or("instance_not_bound", |e| e.code.as_str());
            results.push(reason_row(id, reason));
            continue;
        };
        let shared_with = facts
            .iter()
            .filter_map(|(other, result)| {
                let evidence = result.as_ref().ok()?;
                (id != other && storage_overlap(binding, bound, r.instances.get(other)?, evidence))
                    .then_some(other)
            })
            .collect::<Vec<_>>();
        results.push(json!({"instance":id,"status":"bound","config_root":binding.config_root,"plugin_root":bound.plugin_root,
            "binary_sha256":binding.binary_sha256,"declared_version":binding.version,"shared_with":shared_with,"loaded":"unknown"}));
    }
    Ok(json!({"results":results,"persistentWrites":false}))
}

fn assessed(
    ctx: &Context,
    registry: &Registry,
    key: &str,
    recipe: &Recipe,
    upgrading: bool,
    dry_run: bool,
) -> Value {
    let facts = registry.scoped_instances(ctx);
    let mut results = vec![];
    for id in &recipe.instances {
        let Some(instance) = registry.instances.get(id) else {
            results.push(reason_row(id, "instance_not_bound"));
            continue;
        };
        let Some(Ok(bound)) = facts.get(id) else {
            let reason = facts
                .get(id)
                .and_then(|f| f.as_ref().err())
                .map_or("instance_not_bound", |e| e.code.as_str());
            results.push(reason_row(id, reason));
            continue;
        };
        let observed = crate::native_plugins_files::observe(id, instance, recipe);
        let row = match observed {
            Err(error) => reason_row(id, &error.code),
            Ok(evidence) => {
                let shared = facts.iter().any(|(other, result)| {
                    id != other
                        && result.as_ref().map_or_else(
                            |_| {
                                registry.instances.get(other).is_some_and(|other| {
                                    [&instance.config_root, &bound.plugin_root].into_iter().any(
                                        |path| {
                                            [&other.config_root, &other.plugin_root()]
                                                .into_iter()
                                                .any(|other| {
                                                    path.starts_with(other)
                                                        || other.starts_with(path)
                                                })
                                        },
                                    )
                                })
                            },
                            |f| {
                                registry
                                    .instances
                                    .get(other)
                                    .is_some_and(|other| storage_overlap(instance, bound, other, f))
                            },
                        )
                });
                evidence_row(id, &evidence, shared, upgrading)
            }
        };
        results.push(row);
    }
    json!({"recipe":key,"revision":recipe.revision,"native_id":recipe.native_id(),
        "mode":if dry_run {"preview"} else {"passive"},"results":results,"persistentWrites":false})
}

fn evidence_row(id: &str, evidence: &PluginFacts, shared: bool, upgrading: bool) -> Value {
    let reason = if evidence.source_changed {
        Some("marketplace_source_change_required")
    } else if shared {
        Some("shared_native_storage_unsupported")
    } else if upgrading && id == "codex" && evidence.configured_enabled == Some(false) {
        Some("codex_readd_would_enable")
    } else if upgrading {
        Some("native_contract_required")
    } else {
        evidence.reason()
    };
    json!({"instance":id,"status":if reason.is_some() {"unknown"} else {"observed"},"reason":reason,
        "marketplace_registered":evidence.marketplace_registered,"registration":evidence.registration,
        "version":evidence.version,"configured_enabled":evidence.configured_enabled,"effective_enabled":"unknown",
        "payload":evidence.payload,"loaded":"unknown"})
}

pub fn run(ctx: &Context, command: &NativePlugins, dry_run: bool, yes: bool) -> Result<Value> {
    let registry = Registry::read(ctx)?;
    let (NativePlugins::Check { recipe } | NativePlugins::Upgrade { recipe }) = command else {
        if let NativePlugins::List { instance } = command {
            return bindings(ctx, &registry, instance.as_deref());
        }
        return Err(refusal("unsupported_native_plugin_request"));
    };
    let requested = registry
        .recipes
        .get(recipe)
        .ok_or_else(|| refusal("native_plugin_recipe_missing"))?;
    let upgrading = matches!(command, NativePlugins::Upgrade { .. });
    if upgrading
        && requested.instances == ["codex"]
        && registry
            .instances
            .get("codex")
            .is_some_and(crate::native_plugin_tx::certified)
    {
        return crate::native_plugin_tx::upgrade(ctx, recipe, dry_run, yes);
    }
    Ok(assessed(
        ctx, &registry, recipe, requested, upgrading, dry_run,
    ))
}

#[cfg(test)]
#[path = "../tests/unit/native_plugins.rs"]
mod tests;
