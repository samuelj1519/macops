use crate::{
    config_local,
    context::Context,
    domain::{ConfigAnchor, FileId, Journal, fingerprint},
    error::{Error, Result},
    paths::Directory,
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::Path};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub anchor: ConfigAnchor,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub lineage: Vec<FileId>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    pub mode: u32,
    pub text: String,
}
impl Snapshot {
    pub fn capture(path: &Path) -> Result<Self> {
        let anchor = config_local::capture_anchor(path)?;
        let directory = Directory::open(&anchor.canonical_parent)?;
        let (bytes, meta) =
            directory.regular_config_snapshot(OsStr::new(&anchor.name), 1024 * 1024)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::attention("invalid_ai_config", "配置不是有效 UTF-8"))?;
        directory.verify()?;
        Ok(Self {
            anchor,
            lineage: directory.lineage()?,
            identity: meta.identity,
            mode: meta.mode,
            text,
        })
    }
    pub fn current(&self) -> Result<Self> {
        let current = Self::capture(&self.anchor.requested)?;
        if current.anchor != self.anchor || current.lineage != self.lineage {
            return Err(Error::attention(
                "ai_namespace_changed",
                "配置目录或路径绑定发生变化",
            ));
        }
        Ok(current)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mise {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub live: Snapshot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub source: Snapshot,
    pub prev_live_pin: String,
    pub expected_live: String,
    pub expected_source: String,
    pub root_digest: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub roots: crate::config_inventory::Inputs,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileFile {
    pub path: std::path::PathBuf,
    pub digest: Option<String>,
}

pub fn profiles(ctx: &Context) -> Result<Vec<ProfileFile>> {
    let mut files = Vec::new();
    for name in [
        ".claude",
        ".claude-mi",
        ".claude-kimi",
        ".claude-glm",
        ".claude-zen",
    ] {
        for leaf in [
            "settings.json",
            "settings.local.json",
            "config.json",
            "models.json",
        ] {
            let path = ctx.home.join(name).join(leaf);
            files.push(ProfileFile {
                digest: file_digest(&path)?,
                path,
            });
        }
    }
    let path = ctx.home.join(".claude.json");
    files.push(ProfileFile {
        digest: file_digest(&path)?,
        path,
    });
    Ok(files)
}
fn file_digest(path: &Path) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => fingerprint(&crate::paths::regular_bytes(path)?).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn verify_profiles(files: &[ProfileFile]) -> Result<()> {
    for file in files {
        if file_digest(&file.path)? != file.digest {
            return Err(Error::attention(
                "claude_profile_changed",
                "Claude profile 配置或模型发生变化，保留阻断",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Upgrade {
    pub isolated: bool,
    pub tool: String,
    pub version: Option<String>,
    pub force_live: bool,
    #[serde(deserialize_with = "crate::policy::json::optional_object")]
    pub mise: Option<Mise>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub profiles: Vec<ProfileFile>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Step {
    Inspect,
    Rollback,
    Reshim,
    Reconcile,
}

pub fn save(store: &Store, journal: &Journal, upgrade: &Upgrade) -> Result<()> {
    Store::atomic_new(&store.root, &format!("ai-{}.json", journal.id), upgrade)
}
pub fn read(store: &Store, journal: &Journal) -> Result<Upgrade> {
    let crate::policy::json::Object(upgrade) = Store::read::<crate::policy::json::Object<Upgrade>>(
        &store.root,
        &format!("ai-{}.json", journal.id),
    )?
    .ok_or_else(|| Error::attention("ai_plan_missing", "缺少冻结的 AI 升级输入"))?;
    if journal.plan.warnings.first() != Some(&format!("ai-native-v1:{}", fingerprint(&upgrade)?)) {
        return Err(Error::attention("ai_plan_changed", "AI 升级输入摘要不匹配"));
    }
    Ok(upgrade)
}
pub fn step(store: &Store, journal: &Journal, value: Step) -> Result<()> {
    Store::atomic(&store.root, &format!("ai-{}-step.json", journal.id), &value)
}
pub fn read_step(store: &Store, journal: &Journal) -> Result<Step> {
    Store::read(&store.root, &format!("ai-{}-step.json", journal.id))?
        .ok_or_else(|| Error::attention("ai_progress_missing", "缺少 AI 操作进度，保留阻断"))
}

pub fn snapshot(store: &Store, name: &str) -> Result<Option<Snapshot>> {
    Ok(
        Store::read::<crate::policy::json::Object<Snapshot>>(&store.root, name)?
            .map(|value| value.0),
    )
}
