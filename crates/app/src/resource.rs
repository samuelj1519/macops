use crate::{
    domain::fingerprint,
    error::{Error, Result},
};
use serde::Serialize;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Declared,
    Present,
    Missing,
    Unknown,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Management {
    Declared,
    Native,
    Untracked,
    Adopted,
}

#[derive(Clone, Debug, Serialize)]
pub struct Resource {
    pub id: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub management: Management,
    pub status: State,
    #[serde(flatten)]
    pub data: Data,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Data {
    Application {
        manager: String,
        native_id: String,
        scope: String,
        releases: Vec<Release>,
        recipe_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        baseline: Option<Baseline>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        issues: Vec<String>,
    },
    Agent {
        client: String,
        profile: String,
        config_dir: PathBuf,
        model: Model,
        runtime: RuntimeBinding,
    },
    Capability {
        delivery_kind: String,
        native_id: String,
        origin: Origin,
        revision: Revision,
        contents: Vec<Content>,
        deployments: Vec<Deployment>,
        configured_enabled: Option<bool>,
        loaded: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        baseline: Option<Baseline>,
    },
    Config {
        target: PathBuf,
        source_candidates: Vec<SourceRef>,
        owner: String,
        consumers: Vec<String>,
        observed_type: String,
        mode: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        baseline: Option<Baseline>,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Same,
    Different,
    Unknown,
}
#[derive(Clone, Debug, Serialize)]
pub struct Baseline {
    pub receipt: crate::domain::Observation,
    pub current: crate::domain::Observation,
    pub comparison: Comparison,
}

#[derive(Clone, Debug, Serialize)]
pub struct Release {
    pub version: String,
    pub path: Option<PathBuf>,
    pub active: bool,
}
#[derive(Clone, Debug, Serialize, Default)]
pub struct Model {
    pub selected: Option<String>,
    pub aliases: BTreeMap<String, String>,
    pub custom_provider_configured: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeBinding {
    pub selector: String,
    pub context: PathBuf,
    pub binary: Option<PathBuf>,
    pub install_ref: Option<String>,
    pub evidence: Option<PathBuf>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Origin {
    pub manager: String,
    pub label: Option<String>,
    pub record: PathBuf,
    pub native_key: String,
}
#[derive(Clone, Debug, Serialize, Default)]
pub struct Revision {
    pub declared: Option<String>,
    pub recorded: Option<String>,
    pub source: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Content {
    pub id: String,
    pub path: PathBuf,
    pub realpath: Option<PathBuf>,
    pub status: State,
}
#[derive(Clone, Debug, Serialize)]
pub struct Deployment {
    pub harness: String,
    pub scope: String,
    pub project: Option<PathBuf>,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SourceRef {
    pub path: PathBuf,
    pub identity: crate::domain::FileId,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Complete,
    Absent,
    Unavailable,
    Invalid,
    Truncated,
    Unsupported,
}
#[derive(Clone, Debug, Serialize)]
pub struct Coverage {
    pub source: String,
    pub state: CoverageState,
    pub count: usize,
    pub roots: Vec<PathBuf>,
    pub issues: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub home: PathBuf,
    pub project: Option<PathBuf>,
    pub resources: Vec<Resource>,
    pub coverage: Vec<Coverage>,
    pub incomplete: bool,
}

impl Resource {
    pub(crate) fn matches_name(&self, key: &str) -> bool {
        self.name == key || self.aliases.iter().any(|alias| alias == key)
    }

    pub fn category(&self) -> &'static str {
        match self.data {
            Data::Application { .. } => "applications",
            Data::Agent { .. } => "agents",
            Data::Capability { .. } => "capabilities",
            Data::Config { .. } => "configs",
        }
    }
}

impl Snapshot {
    pub fn finish(&mut self) {
        self.resources
            .sort_by(|a, b| a.category().cmp(b.category()).then(a.id.cmp(&b.id)));
        self.coverage.sort_by(|a, b| a.source.cmp(&b.source));
        self.incomplete = self
            .coverage
            .iter()
            .any(|c| !matches!(c.state, CoverageState::Complete | CoverageState::Absent));
    }
    pub fn select(&self, key: &str) -> Result<&Resource> {
        if let Some(resource) = self.resources.iter().find(|r| r.id == key) {
            return Ok(resource);
        }
        self.by_name(key)?
            .ok_or_else(|| Error::attention("not_found", "没有发现对应对象"))
    }

    pub(crate) fn by_name(&self, key: &str) -> Result<Option<&Resource>> {
        let matches: Vec<_> = self
            .resources
            .iter()
            .filter(|r| r.matches_name(key))
            .collect();
        match matches.as_slice() {
            [resource] => Ok(Some(*resource)),
            [] => Ok(None),
            _ => Err(Error::attention(
                "ambiguous",
                format!(
                    "请选择明确实例：{}",
                    matches
                        .iter()
                        .map(|r| r.id.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }
}

pub fn id<T: Serialize>(kind: &str, identity: &T) -> Result<String> {
    Ok(format!("{kind}:{}", fingerprint(identity)?))
}

#[cfg(test)]
#[path = "../tests/unit/resources.rs"]
mod tests;
