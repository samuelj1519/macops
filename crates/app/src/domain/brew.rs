use super::{Action, CommandSpec, FileId};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewFormulaKey {
    pub canonical_prefix: PathBuf,
    pub full_name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewSelection {
    pub tap: String,
    pub name: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewNodeStamp {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    pub mode: u32,
    pub size: u64,
    pub modified_ns: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewFileStamp {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub node: BrewNodeStamp,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrewConfigStamp {
    Absent {
        path: PathBuf,
        anchor: PathBuf,
        #[serde(deserialize_with = "crate::policy::json::object")]
        parent: BrewNodeStamp,
    },
    Present {
        path: PathBuf,
        #[serde(deserialize_with = "crate::policy::json::object")]
        stamp: BrewFileStamp,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewMetadataScope {
    pub home: PathBuf,
    pub xdg_config_home: Option<PathBuf>,
    pub homebrew_xdg_config_home: Option<PathBuf>,
    pub actual_user_config_home: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub config_files: Vec<BrewConfigStamp>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrewCompatibility {
    Homebrew7_0_15 {
        source_commit: String,
        acceptance_sha256: String,
        ruby_version: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewContextBinding {
    pub context_id: String,
    pub invocation: PathBuf,
    pub canonical_program: PathBuf,
    pub prefix: PathBuf,
    pub cellar: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub program_stamp: BrewFileStamp,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub prefix_id: FileId,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub cellar_id: FileId,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub metadata_scope: BrewMetadataScope,
    pub compatibility: BrewCompatibility,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BrewScriptOrigin {
    MacopsBuiltinV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewQueryScript {
    pub origin: BrewScriptOrigin,
    pub absolute_path: PathBuf,
    pub canonical_path: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub file_id: FileId,
    pub sha256: String,
    pub size: u64,
    pub product_build_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BrewQueryKind {
    BrewFormulaNativeV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewQuerySpec {
    pub kind: BrewQueryKind,
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub key: BrewFormulaKey,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub selection: BrewSelection,
    pub context_epoch: u64,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub binding: BrewContextBinding,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub script: BrewQueryScript,
    pub env_capture_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewEvidenceStamp {
    pub context_id: String,
    pub context_epoch: u64,
    pub binding_fingerprint: String,
    pub env_capture_id: String,
    pub query_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewEffectivePolicies {
    pub no_auto_update: bool,
    pub no_install_cleanup: bool,
    pub no_autoremove: bool,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub runtime: BrewRuntimePolicies,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewRuntimePolicies {
    pub no_bootsnap: bool,
    pub installed_dependents_check: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BrewEntryKind {
    File,
    Directory,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewRackEntry {
    pub name: String,
    pub kind: BrewEntryKind,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub stamp: BrewNodeStamp,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewKegIdentity {
    pub path: PathBuf,
    pub name: String,
    pub tap: String,
    pub full_name: String,
    pub spec: String,
    pub version: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub receipt: BrewFileStamp,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewRackProof {
    pub path: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub cellar_before: BrewNodeStamp,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub cellar_after: BrewNodeStamp,
    #[serde(deserialize_with = "crate::policy::json::optional_object")]
    pub rack_before: Option<BrewNodeStamp>,
    #[serde(deserialize_with = "crate::policy::json::optional_object")]
    pub rack_after: Option<BrewNodeStamp>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub entries: Vec<BrewRackEntry>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub kegs: Vec<BrewKegIdentity>,
    pub native_selected: Vec<PathBuf>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewNativeInfo {
    pub name: String,
    pub tap: String,
    pub full_name: String,
    pub active_spec: String,
    pub pkg_version: String,
    pub pinned: bool,
    pub outdated: bool,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub resolution: BrewResolution,
    pub latest_full_name: String,
    pub installed_paths: Vec<PathBuf>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewResolution {
    pub alias_changed: bool,
    pub migration_needed: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewNativeReport {
    pub schema_version: u32,
    pub query_id: String,
    pub homebrew_version: String,
    pub selected_version: Option<String>,
    pub prefix: PathBuf,
    pub cellar: PathBuf,
    pub actual_user_config_home: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub policies: BrewEffectivePolicies,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub rack: BrewRackProof,
    #[serde(deserialize_with = "crate::policy::json::optional_object")]
    pub formula: Option<BrewNativeInfo>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewInstalledEvidence {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub stamp: BrewEvidenceStamp,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub report: BrewNativeReport,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewAbsenceEvidence {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub stamp: BrewEvidenceStamp,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub report: BrewNativeReport,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewOwnerSnapshot {
    pub logical_owner_id: String,
    pub selected_package_id: String,
    pub entries: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewContextAuthority {
    pub context_id: String,
    pub epoch: u64,
    pub binding_fingerprint: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "goal", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrewGoal {
    AdoptBaseline,
    InstallAvailable { pkg_version: String },
    UpgradeAvailable { pkg_version: String },
    UninstallAllInstalledVersions,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BrewExecution {
    NativeMutator,
    VerifiedNoop,
    BaselineOnly,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BrewConsent {
    ExistingContext,
    Interactive,
    Yes,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewScopeConfirmation {
    pub context_id: String,
    pub epoch: u64,
    pub logical_owner_id: String,
    pub action: Action,
    pub effect_contract: String,
    pub consent: BrewConsent,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewPlanData {
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub owner: BrewOwnerSnapshot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub context: BrewContextAuthority,
    pub goal: BrewGoal,
    pub execution: BrewExecution,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub confirmation: BrewScopeConfirmation,
    pub env_capture_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewContextRecord {
    pub epoch: u64,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub binding: BrewContextBinding,
    pub observation_valid: bool,
    #[serde(default)]
    pub writer_scope_confirmed: bool,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub declaration_keys: BTreeMap<String, BrewFormulaKey>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewContextRegistry {
    pub schema_version: u32,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub contexts: BTreeMap<String, BrewContextRecord>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrewInvocationProjection {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub command: CommandSpec,
    pub cwd: PathBuf,
    pub timeout_secs: u64,
}
