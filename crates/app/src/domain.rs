pub mod brew;
use crate::error::{Error, Result};
pub use brew::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub home: PathBuf,
    pub paths: BTreeMap<String, Option<PathBuf>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub service_identity: Option<ServiceIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceIdentity {
    pub name: String,
    pub uid: u32,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Cli,
    App,
    Skill,
    Plugin,
    Runtime,
    Manager,
    Service,
    Config,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum QuerySpec {
    BrewFormula(#[serde(deserialize_with = "crate::policy::json::object")] Box<BrewQuerySpec>),
    Command(#[serde(deserialize_with = "crate::policy::json::object")] CommandSpec),
    LocalUvTool(#[serde(deserialize_with = "crate::policy::json::object")] LocalUvToolQuery),
    LocalNpm(#[serde(deserialize_with = "crate::policy::json::object")] LocalNpmQuery),
    LocalConfig(#[serde(deserialize_with = "crate::policy::json::object")] LocalConfigQuery),
    NativeConfig(
        #[serde(deserialize_with = "crate::policy::json::object")]
        crate::policy::chezmoi_native::Query,
    ),
    LocalCapability(
        #[serde(deserialize_with = "crate::policy::json::object")] LocalCapabilityQuery,
    ),
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UvToolBinding {
    pub tool_dir: PathBuf,
    pub bin_dir: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UvDirectoryAnchor {
    pub path: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExistingUvNamespace {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub tool_dir: UvDirectoryAnchor,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub bin_dir: UvDirectoryAnchor,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum UvQueryKind {
    #[serde(rename = "uv-tool-local-v1")]
    LocalV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalUvToolQuery {
    pub kind: UvQueryKind,
    pub name: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub namespace: ExistingUvNamespace,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UvToolEvidence {
    pub namespace_identity: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub payload_identity: FileId,
    pub receipt_sha256: String,
    pub metadata_sha256: String,
    pub pyvenv_sha256: String,
    pub entrypoints: Vec<String>,
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityKind {
    SkillsCli,
    ClaudePlugin,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum CapabilityQueryKind {
    #[serde(rename = "capability-local-v1")]
    LocalV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalCapabilityQuery {
    pub kind: CapabilityQueryKind,
    pub capability: CapabilityKind,
    pub native_id: String,
    pub manager: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub record: ConfigAnchor,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub payload: ConfigAnchor,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub settings: Option<ConfigAnchor>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityStamp {
    pub record_digest: String,
    pub payload_digest: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub payload_identity: FileId,
    pub file_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub configured_enabled: Option<bool>,
    pub fingerprint: String,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigAuthority {
    ChezmoiNative,
    ChezmoiCandidate,
    Untracked,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum ConfigQueryKind {
    #[serde(rename = "config-local-v1")]
    LocalV1,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigModePolicy {
    pub private: bool,
    pub readonly: bool,
    pub executable: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigAnchor {
    pub requested: PathBuf,
    pub canonical_parent: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub parent_identity: FileId,
    pub name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalConfigQuery {
    pub kind: ConfigQueryKind,
    pub authority: ConfigAuthority,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub target: ConfigAnchor,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub source: Option<ConfigAnchor>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub expected_mode: Option<ConfigModePolicy>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConfigFileStamp {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    pub mode: u32,
    pub content_digest: String,
    pub fingerprint: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NpmRegistryAuthority {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_url: Option<String>,
    #[serde(default)]
    pub allow_scripts: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NpmWriteAuthority {
    pub allow_remove: bool,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub registry: Option<NpmRegistryAuthority>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NpmConfigBindings {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub userconfig: Option<ConfigFileStamp>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub globalconfig: Option<ConfigFileStamp>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalNpmQuery {
    pub kind: NpmQueryKind,
    pub prefix: PathBuf,
    pub name: String,
    pub contract: NpmContract,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub namespace: NpmNamespace,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub config_bindings: Option<NpmConfigBindings>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum NpmQueryKind {
    #[serde(rename = "npm-local-v1")]
    LocalV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum NpmContract {
    DeclaredV1,
    NativeV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NpmNamespace {
    pub canonical_prefix: PathBuf,
    pub anchor: NpmAnchor,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum NpmAnchor {
    ExistingPrefix {
        path: PathBuf,
        #[serde(deserialize_with = "crate::policy::json::object")]
        identity: FileId,
    },
    CreatePrefix {
        path: PathBuf,
        #[serde(deserialize_with = "crate::policy::json::object")]
        identity: FileId,
        missing_tail: Vec<String>,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NpmEvidence {
    pub kind: NpmQueryKind,
    pub namespace: String,
    pub canonical_prefix: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::optional_object")]
    pub prefix_identity: Option<FileId>,
    pub fingerprint: String,
}

impl From<CommandSpec> for QuerySpec {
    fn from(command: CommandSpec) -> Self {
        Self::Command(command)
    }
}
impl QuerySpec {
    pub fn command(&self) -> Option<&CommandSpec> {
        if let Self::Command(command) = self {
            Some(command)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GovernanceCategory {
    Policy,
    Links,
    Source,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Provider {
    Governance {
        category: GovernanceCategory,
        manifest: PathBuf,
        manifest_sha256: String,
    },
    BrewFormula {
        #[serde(deserialize_with = "crate::policy::json::object")]
        query: Box<BrewQuerySpec>,
        recipe_ref: Option<String>,
    },
    Brew {
        package: String,
        #[serde(default)]
        cask: bool,
    },
    Mise {
        package: String,
    },
    Uv {
        package: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::uv_local::optional_binding"
        )]
        binding: Option<UvToolBinding>,
    },
    UvObserved {
        package: String,
        #[serde(deserialize_with = "crate::policy::json::object")]
        binding: UvToolBinding,
    },
    Npm {
        package: String,
        prefix: PathBuf,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::json::optional_object"
        )]
        authority: Option<NpmWriteAuthority>,
    },
    NpmObserved {
        prefix: PathBuf,
        name: String,
    },
    Bun {
        package: String,
    },
    Cargo {
        package: String,
        binary: String,
    },
    Command {
        query: CommandSpec,
        #[serde(default)]
        available: Option<CommandSpec>,
        #[serde(default)]
        install: Option<CommandSpec>,
        #[serde(default)]
        upgrade: Option<CommandSpec>,
        #[serde(default)]
        uninstall: Option<CommandSpec>,
        #[serde(default)]
        synchronous: bool,
    },
    Legacy {
        category: String,
        selector: String,
    },
    ObservedApp {
        path: PathBuf,
        bundle_id: String,
        historical_version: String,
        #[serde(default)]
        owner: String,
        #[serde(default)]
        source: String,
        #[serde(default)]
        uninstaller: Option<String>,
    },
    Manager {
        executable: String,
    },
    Service {
        formula: String,
    },
    ConfigObserved {
        target: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<PathBuf>,
        authority: ConfigAuthority,
    },
    CapabilityObserved {
        capability: CapabilityKind,
        native_id: String,
        manager: String,
        record: PathBuf,
        payload: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        settings: Option<PathBuf>,
    },
    Macops,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PathKind {
    Payload,
    Config,
    Data,
    Cache,
    Temp,
    Backup,
    Log,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedPath {
    pub path: PathBuf,
    pub kind: PathKind,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub official_url: String,
    pub provider: Provider,
    #[serde(default, deserialize_with = "crate::policy::json::object_vec")]
    pub paths: Vec<OwnedPath>,
    #[serde(default)]
    pub exact_version_required: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema_version: u32,
    pub packages: Vec<Package>,
}

impl Catalog {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(Error::new(
                "catalog_version",
                "不支持的 catalog schema_version",
            ));
        }
        let mut ids = std::collections::BTreeSet::new();
        for p in &self.packages {
            if p.id.is_empty()
                || !p
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b":/._-@".contains(&c))
            {
                return Err(Error::new("invalid_id", &p.id));
            }
            if !ids.insert(&p.id) {
                return Err(Error::new("duplicate_id", &p.id));
            }
            if matches!(p.kind, Kind::Service) != matches!(p.provider, Provider::Service { .. }) {
                return Err(Error::new("invalid_service_kind", &p.id));
            }
            crate::policy::brew::validate_package(p)?;
            crate::policy::uv_local::validate_package(p)?;
            crate::policy::config_local::validate_package(p)?;
            crate::policy::capability_local::validate_package(p)?;
            if let Provider::Npm {
                package,
                authority: Some(authority),
                ..
            } = &p.provider
            {
                crate::policy::npm_local::validate_authority(package, authority)?;
            }
            if let Provider::Service { formula } = &p.provider
                && formula != "postgresql@16"
            {
                return Err(Error::new("undeclared_service", &p.id));
            }
            let native_name = match &p.provider {
                Provider::Service { formula: package }
                | Provider::Brew { package, .. }
                | Provider::Mise { package }
                | Provider::Uv { package, .. }
                | Provider::Npm { package, .. }
                | Provider::Bun { package }
                | Provider::Cargo { package, .. } => Some(package),
                _ => None,
            };
            if native_name.is_some_and(|name| {
                name.is_empty()
                    || name.starts_with('-')
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"@._:/+-".contains(&b))
            }) {
                return Err(Error::new("invalid_native_package", &p.id));
            }
            let source_required = !matches!(
                p.provider,
                Provider::Legacy { .. }
                    | Provider::ObservedApp { .. }
                    | Provider::Manager { .. }
                    | Provider::ConfigObserved { .. }
                    | Provider::UvObserved { .. }
                    | Provider::CapabilityObserved { .. }
                    | Provider::Macops
            );
            if (source_required || !p.official_url.is_empty())
                && (!p.official_url.starts_with("https://") || p.official_url.len() <= 8)
            {
                return Err(Error::new("official_source_required", &p.id));
            }
            if let Provider::Command {
                query,
                install,
                upgrade,
                uninstall,
                ..
            } = &p.provider
            {
                for c in std::iter::once(query)
                    .chain(install)
                    .chain(upgrade)
                    .chain(uninstall)
                {
                    if !c.program.is_absolute() {
                        return Err(Error::new(
                            "absolute_program_required",
                            c.program.display().to_string(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn select(&self, name: &str) -> Result<&Package> {
        if let Some(p) = self.packages.iter().find(|p| p.id == name) {
            return Ok(p);
        }
        let found: Vec<_> = self.packages.iter().filter(|p| p.name == name || matches!(&p.provider,Provider::ObservedApp {bundle_id,path,..} if bundle_id==name||path.file_name().is_some_and(|n|n==name))).collect();
        match found.as_slice() {
            [p] => Ok(p),
            [] => Err(Error::attention("not_found", format!("没有登记：{name}"))),
            _ => Err(Error::attention(
                "ambiguous",
                format!("名称有多个来源，请使用完整 ID：{name}"),
            )),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum Observation {
    BrewInstalled {
        version: String,
        #[serde(deserialize_with = "crate::policy::json::object")]
        evidence: Box<BrewInstalledEvidence>,
    },
    BrewMissing {
        #[serde(deserialize_with = "crate::policy::json::object")]
        evidence: Box<BrewAbsenceEvidence>,
    },
    UvTool {
        version: String,
        #[serde(deserialize_with = "crate::policy::json::object")]
        evidence: UvToolEvidence,
    },
    Governance {
        fingerprint: String,
    },
    Present {
        version: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::json::optional_object"
        )]
        evidence: Option<NpmEvidence>,
    },
    Missing {
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::json::optional_object"
        )]
        evidence: Option<NpmEvidence>,
    },
    NativePackage {
        version: String,
        source_fingerprint: String,
        linked: bool,
    },
    Unknown {
        reason: String,
    },
    Service {
        running: bool,
        loaded: bool,
        registered: bool,
    },
    ConfigNative {
        binding_sha256: String,
        matched: bool,
    },
    ConfigFile {
        authority: ConfigAuthority,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::json::optional_object"
        )]
        target: Option<ConfigFileStamp>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::json::optional_object"
        )]
        source: Option<ConfigFileStamp>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode_matches_source: Option<bool>,
    },
    CapabilityPack {
        capability: CapabilityKind,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "crate::policy::json::optional_object"
        )]
        pack: Option<CapabilityStamp>,
    },
}

impl Observation {
    pub fn version(&self) -> Option<&str> {
        if let Self::BrewInstalled { version, .. }
        | Self::Present { version, .. }
        | Self::NativePackage { version, .. }
        | Self::UvTool { version, .. } = self
        {
            Some(version)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Maintain,
    Install,
    Upgrade,
    Uninstall,
    Update,
    Adopt,
    Cleanup,
    Start,
    Stop,
    Apply,
    Restore,
    Reconcile,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UvOwnerSnapshot {
    pub selected_owner: String,
    pub entries: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NpmBindingProof {
    pub old_receipt_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Plan {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub brew: Option<Box<BrewPlanData>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub uv_owner: Option<UvOwnerSnapshot>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub npm_binding: Option<NpmBindingProof>,
    pub timeout_secs: u64,
    pub package: Package,
    pub action: Action,
    pub before: Observation,
    pub target: Option<String>,
    pub command: Option<CommandSpec>,
    pub query: Option<QuerySpec>,
    pub recipe_sha256: String,
    pub warnings: Vec<String>,
    pub synchronous: bool,
    #[serde(default)]
    pub delegated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Receipt {
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub scope: Option<Scope>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub query: Option<QuerySpec>,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub package: Package,
    pub observation: Observation,
    pub operation_id: String,
    pub updated_at: u64,
}

#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipts {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default, deserialize_with = "crate::policy::json::object_map")]
    pub installations: BTreeMap<String, Receipt>,
}

pub fn fingerprint<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

#[derive(Debug)]
pub struct ProcessOutput {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub uncertain: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Prepared,
    Running,
    Unknown,
    Failed,
    Succeeded,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    pub scope_sha256: String,
    pub scope: crate::domain::Scope,
    pub schema_version: u32,
    pub id: String,
    pub phase: Phase,
    pub plan: Plan,
    pub plan_sha256: String,
    pub home: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub settings: PathBuf,
    pub process_group: Option<u32>,
    pub started_at: u64,
    pub message: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Available {
    pub version: String,
    pub source_sha256: String,
    pub scope_sha256: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub schema_version: u32,
    pub format: String,
    pub entries: BTreeMap<String, Available>,
}
#[derive(Deserialize)]
pub struct Version {
    pub version: String,
}

impl Index {
    pub fn empty() -> Self {
        Self {
            schema_version: 1,
            format: "macops-version-index-v1".into(),
            entries: BTreeMap::new(),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || self.format != "macops-version-index-v1" {
            return Err(Error::new("version_index_schema", "版本索引格式错误"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileId {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Identity {
    pub root: FileId,
    pub indexes: FileId,
    pub scope_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Book {
    pub path: PathBuf,
    pub identity: Identity,
    pub current: Option<String>,
    pub owned: BTreeSet<String>,
    pub queried_at: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: u32,
    pub format: String,
    pub scopes: BTreeMap<String, Book>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub format: String,
    pub scope_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub device: u64,
    pub inode: u64,
    pub sha256: String,
}

#[cfg(test)]
#[path = "../tests/unit/domain.rs"]
mod tests;
