use crate::{
    error::{Error, Result},
    policy::governance::{self, ArchiveKind, DATASETS, Dataset, Locations, RetainKind},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "action", content = "detail", rename_all = "snake_case")]
pub enum Classification {
    Convert(Dataset),
    ArchiveIndexed(ArchiveKind),
    RetainInPlace(RetainKind),
    ProfilesLink,
    LegacyWriter,
    Container,
    Runtime,
    Block,
}

pub fn classify(p: &Locations, path: &Path) -> Classification {
    if let Some(dataset) = DATASETS.into_iter().find(|d| d.source(p) == path) {
        return Classification::Convert(dataset);
    }
    for kind in [
        ArchiveKind::Settings,
        ArchiveKind::Backups,
        ArchiveKind::RebuildState,
        ArchiveKind::UnifyState,
    ] {
        if governance::archive_root(p, kind) == path {
            return Classification::ArchiveIndexed(kind);
        }
    }
    if let Some((owner, _)) = governance::retained_rule(p, path) {
        return Classification::RetainInPlace(owner);
    }
    if path == p.home.join(".agents/skill-profiles.json") {
        return Classification::ProfilesLink;
    }
    if path == p.home.join(".agents/bin/skillctl") {
        return Classification::LegacyWriter;
    }
    if matches!(
        path.strip_prefix(&p.home).ok().and_then(Path::to_str),
        Some(
            ".agents/cache"
                | ".agents/bin"
                | ".agents/cache/upstream"
                | ".agents/out"
                | ".agents/bin/tests"
                | ".agents/bin/tests/__pycache__"
                | ".agents/bin/__pycache__"
        )
    ) {
        return Classification::Container;
    }
    if path == p.state || path == p.data || path == p.config || path == p.cache {
        return Classification::Runtime;
    }
    Classification::Block
}

pub fn known_manifest(name: &str) -> bool {
    [
        Dataset::SkillProfiles,
        Dataset::AssetDefinitions,
        Dataset::InvocationRules,
        Dataset::SourceChannels,
        Dataset::MacApps,
    ]
    .into_iter()
    .any(|d| d.filename() == name)
        || matches!(
            name,
            "agent-media.json"
                | "agent-plugins.tsv"
                | "agent-skills.tsv"
                | "grok-bot-skills.tsv"
                | "home-top.tsv"
                | "maintenance.tsv"
        )
}

pub fn validate_dataset(home: &Path, dataset: Dataset, bytes: &[u8], owner: u32) -> Result<()> {
    match dataset {
        Dataset::InvocationOverlays => {
            crate::policy::invocation::record(home, bytes)?;
        }
        Dataset::SkillProfiles => {
            crate::policy::delivery::profiles(bytes)?;
        }
        Dataset::AssetDefinitions => {
            crate::policy::delivery::asset_facts(bytes)?;
        }
        Dataset::InvocationRules => {
            crate::policy::invocation::declaration(bytes)?;
        }
        Dataset::MacApps => {
            let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
            crate::policy::parsers::tsv(
                text,
                &["path", "bundle_id", "version", "owner", "source"],
            )?;
        }
        Dataset::SourceChannels => {
            let value = crate::policy::json::parse(bytes)?;
            let root = value.as_object().ok_or_else(invalid)?;
            if root
                .get("schema_version")
                .and_then(serde_json::Value::as_u64)
                != Some(1)
            {
                return Err(invalid());
            }
            if let Some(ids) = root.get("source_ids") {
                let map = ids.as_object().ok_or_else(invalid)?;
                if map.values().any(|v| {
                    !v.as_str()
                        .is_some_and(|s| crate::policy::resources::component(s) && s.len() <= 255)
                }) {
                    return Err(invalid());
                }
            }
        }
        Dataset::ManagedAssets => {
            let value = crate::policy::json::parse(bytes)?;
            if !value.is_object() {
                return Err(invalid());
            }
            crate::policy::source_baseline::validate_embedded(&value, home, owner)?;
        }
        Dataset::ManagedUpdates => {
            if !crate::policy::json::parse(bytes)?.is_object() {
                return Err(invalid());
            }
        }
    }
    Ok(())
}

fn invalid() -> Error {
    Error::attention("migration_schema_invalid", "治理数据格式或迁移记录无效")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    pub dataset: Dataset,
    pub target: PathBuf,
    pub slot: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: crate::domain::FileId,
    pub sha256: String,
    pub size: u64,
}

pub fn validate_placements(p: &Locations, items: &[Placement]) -> Result<()> {
    let layout = governance::Layout {
        config: p.config.clone(),
        data: p.data.clone(),
        state: p.state.clone(),
        cache: p.cache.clone(),
    };
    let mut datasets = std::collections::BTreeSet::new();
    for item in items {
        let expected_slot = format!("{:?}.candidate", item.dataset);
        if !datasets.insert(item.dataset)
            || item.target != item.dataset.target(&layout)
            || item.slot != expected_slot
            || item.identity.device == 0
            || item.identity.inode == 0
            || item.sha256.len() != 64
            || !item
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || item.size > 16 * 1024 * 1024
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/migration_policy.rs"]
mod tests;
