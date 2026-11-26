use crate::{
    domain::{Action, GovernanceCategory, Journal, Kind, Observation, Plan, Provider},
    error::{Error, Result},
    policy::{config_local::valid_digest, paths::lexical},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

fn invalid() -> Error {
    Error::new(
        "invalid_capability_transaction",
        "能力包事务身份、作用域或协议字段不一致",
    )
}

pub fn category_name(category: GovernanceCategory) -> &'static str {
    match category {
        GovernanceCategory::Policy => "policy",
        GovernanceCategory::Links => "links",
        GovernanceCategory::Source => "source",
    }
}

pub fn validate_plan(plan: &Plan) -> Result<()> {
    let Provider::Governance {
        category,
        manifest,
        manifest_sha256,
    } = &plan.package.provider
    else {
        return if plan.action == Action::Maintain
            || matches!(plan.before, Observation::Governance { .. })
        {
            Err(invalid())
        } else {
            Ok(())
        };
    };
    lexical(manifest)?;
    let Observation::Governance {
        fingerprint: before,
    } = &plan.before
    else {
        return Err(invalid());
    };
    if plan.package.kind != Kind::Skill
        || plan.package.id != format!("governance:{}", category_name(*category))
        || plan.action != Action::Maintain
        || plan.command.is_some()
        || plan.query.is_some()
        || !plan.synchronous
        || plan.delegated
        || !valid_digest(manifest_sha256)
        || !valid_digest(before)
        || plan.target.as_deref() != Some(manifest_sha256)
        || manifest.file_name() != Some(std::ffi::OsStr::new("capability-plan.json"))
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn validate_journal(journal: &Journal) -> Result<()> {
    validate_plan(&journal.plan)?;
    let Provider::Governance { manifest, .. } = &journal.plan.package.provider else {
        return Ok(());
    };
    let mut directory = manifest.clone();
    directory.pop();
    let label = directory
        .file_name()
        .and_then(|v| v.to_str())
        .ok_or_else(invalid)?;
    crate::skills::rules::skills::parse_change_id(label).map_err(|_| invalid())?;
    let mut parent = directory.clone();
    parent.pop();
    if parent != journal.state.join("backups")
        && parent != journal.scope.home.join(".agents/backups")
    {
        return Err(invalid());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Blob {
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum State {
    Missing,
    File {
        #[serde(deserialize_with = "crate::policy::json::object")]
        identity: crate::domain::FileId,
        sha256: String,
        mode: u32,
    },
    Link {
        #[serde(deserialize_with = "crate::policy::json::object")]
        identity: crate::domain::FileId,
        text: crate::capability::rules::delivery::LinkText,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mode: Option<u32>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Namespace {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub ancestor: crate::policy::governance::DirectoryBinding,
    pub remaining: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Witness {
    pub path: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub namespace: Namespace,
    pub state: State,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: crate::capability::rules::delivery::ChangeKind,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub namespace: Namespace,
    pub before: State,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub before_blob: Option<Blob>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub after_blob: Option<Blob>,
    pub after_mode: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LedgerSlot {
    Native,
    Managed,
    Overlays,
    Receipts,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LedgerChange {
    pub slot: LedgerSlot,
    pub path: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub namespace: Namespace,
    pub before: State,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub before_blob: Option<Blob>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub after_blob: Option<Blob>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectoryWitness {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub binding: crate::policy::governance::DirectoryBinding,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub entries: BTreeMap<String, DirectoryEntry>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectoryEntry {
    pub kind: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: crate::domain::FileId,
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LinkAuthority {
    pub requested: crate::capability::rules::delivery::Selection,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub canonical: crate::policy::governance::DirectoryBinding,
    pub selected: BTreeMap<String, String>,
    #[serde(deserialize_with = "crate::policy::json::object_map")]
    pub profiles: BTreeMap<
        crate::capability::rules::delivery::Profile,
        crate::policy::governance::DirectoryBinding,
    >,
    pub pending_config_policies: Vec<String>,
    #[serde(default)]
    pub protected:
        BTreeMap<crate::capability::rules::delivery::Profile, std::collections::BTreeSet<String>>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub layout: Option<crate::policy::governance::Layout>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub links: Option<LinkAuthority>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub configuration: Option<crate::policy::profile::profile_authority::Authority>,
    pub operation_id: String,
    pub change_id: String,
    pub category: GovernanceCategory,
    pub home: PathBuf,
    pub state: PathBuf,
    pub settings: PathBuf,
    pub pending: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub inputs: Vec<Witness>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub directories: Vec<DirectoryWitness>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub files: Vec<FileChange>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub ledgers: Vec<LedgerChange>,
}

pub const POLICY_MANIFEST_VERSION: u32 = 3;

pub fn recipe_book_witness(manifest: &Manifest) -> Result<&Witness> {
    let name = std::ffi::OsStr::new(crate::skills::rules::native_skills::RECIPE_BOOK_NAME);
    let mut matches = manifest
        .inputs
        .iter()
        .filter(|witness| witness.path.file_name() == Some(name));
    let witness = matches.next().ok_or_else(invalid)?;
    if matches.next().is_some() || !matches!(witness.state, State::File { .. } | State::Missing) {
        return Err(invalid());
    }
    Ok(witness)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub target: PathBuf,
    pub staged: Option<PathBuf>,
    pub after: State,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ParentCandidate {
    pub target: PathBuf,
    pub staged: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub binding: crate::policy::governance::DirectoryBinding,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub schema_version: u32,
    pub operation_id: String,
    pub manifest_sha256: String,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub parents: Vec<ParentCandidate>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub files: Vec<Candidate>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub ledgers: Vec<Candidate>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub rollbacks: Vec<Candidate>,
    pub retired_parents: Vec<PathBuf>,
}

pub fn blob_valid(blob: &Blob) -> bool {
    valid_digest(&blob.sha256) && blob.size <= 64 * 1024 * 1024
}

pub(crate) fn namespace_valid(path: &std::path::Path, namespace: &Namespace) -> Result<()> {
    lexical(path)?;
    lexical(&namespace.ancestor.requested)?;
    lexical(&namespace.ancestor.canonical)?;
    if namespace.ancestor.lineage.last() != Some(&namespace.ancestor.identity)
        || namespace.ancestor.lineage.len() > 64
        || namespace.ancestor.identity.device == 0
        || namespace.ancestor.identity.inode == 0
        || namespace.remaining.is_empty()
        || namespace.remaining.len() > 64
    {
        return Err(invalid());
    }
    let mut expected = namespace.ancestor.requested.clone();
    for name in &namespace.remaining {
        if !crate::policy::catalog::resources::component(name) {
            return Err(invalid());
        }
        expected.push(name);
    }
    if expected != path {
        return Err(invalid());
    }
    Ok(())
}

pub fn state_valid(state: &State) -> bool {
    match state {
        State::Missing => true,
        State::File {
            identity,
            sha256,
            mode,
        } => identity.device > 0 && identity.inode > 0 && valid_digest(sha256) && *mode <= 0o7777,
        State::Link { identity, mode, .. } => {
            identity.device > 0 && identity.inode > 0 && mode.is_none_or(|mode| mode <= 0o7777)
        }
    }
}

fn before_valid(state: &State, blob: Option<&Blob>) -> bool {
    if !state_valid(state) {
        return false;
    }
    match (state, blob) {
        (State::Missing, None) => true,
        (State::File { sha256, .. }, Some(blob)) => blob_valid(blob) && sha256 == &blob.sha256,
        _ => false,
    }
}

fn validate_plan_kind(manifest: &Manifest) -> Result<()> {
    match (manifest.schema_version, manifest.category) {
        (2 | POLICY_MANIFEST_VERSION, GovernanceCategory::Policy)
            if manifest.layout.is_none()
                && manifest.links.is_none()
                && manifest.configuration.is_none() =>
        {
            if manifest.schema_version == POLICY_MANIFEST_VERSION {
                recipe_book_witness(manifest)?;
            }
        }
        (3, GovernanceCategory::Links) if manifest.configuration.is_none() => {
            validate_link_authority(manifest)?;
        }
        (4, GovernanceCategory::Links) => {
            validate_layout(manifest)?;
            manifest
                .configuration
                .as_ref()
                .ok_or_else(invalid)?
                .validate(&manifest.home)?;
            if manifest.links.is_some() {
                validate_link_authority(manifest)?;
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if !matches!(manifest.schema_version, 2..=4)
        || !crate::policy::operation::state::valid_id(&manifest.operation_id)
        || manifest.files.len() > 5000
        || manifest.inputs.len() > 10000
        || manifest.directories.len() > 512
        || manifest.ledgers.len() > 4
    {
        return Err(invalid());
    }
    crate::skills::rules::skills::parse_change_id(&manifest.change_id).map_err(|_| invalid())?;
    lexical(&manifest.home)?;
    lexical(&manifest.state)?;
    lexical(&manifest.settings)?;
    lexical(&manifest.pending)?;
    validate_plan_kind(manifest)?;
    let base = if manifest.pending == manifest.home.join(".agents/invocation-policy.pending.json") {
        manifest.home.join(".agents")
    } else if manifest.pending == manifest.state.join("invocation-policy.pending.json") {
        manifest.state.clone()
    } else {
        return Err(invalid());
    };
    let expected = [
        (LedgerSlot::Managed, base.join("managed-assets.json")),
        (LedgerSlot::Overlays, base.join("invocation-overlays.json")),
        (LedgerSlot::Receipts, manifest.state.join("receipts.json")),
    ];
    if manifest.ledgers.len() != expected.len()
        || manifest
            .ledgers
            .iter()
            .zip(&expected)
            .any(|(actual, (slot, path))| actual.slot != *slot || actual.path != *path)
    {
        return Err(invalid());
    }
    validate_directories(manifest)?;
    let mut paths = std::collections::BTreeSet::new();
    for input in &manifest.inputs {
        namespace_valid(&input.path, &input.namespace)?;
        if !state_valid(&input.state)
            || (manifest.schema_version >= 3
                && matches!(input.state, State::Link { mode: None, .. }))
        {
            return Err(invalid());
        }
    }
    for file in &manifest.files {
        namespace_valid(&file.path, &file.namespace)?;
        if file.namespace.ancestor.requested != file.namespace.ancestor.canonical
            || !paths.insert(&file.path)
        {
            return Err(invalid());
        }
        if manifest.category == GovernanceCategory::Policy {
            validate_metadata_file(manifest, file)?;
        } else if file.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink {
            validate_link_file(manifest, file)?;
        } else {
            validate_profile_file(manifest, file)?;
        }
    }
    for ledger in &manifest.ledgers {
        namespace_valid(&ledger.path, &ledger.namespace)?;
        if ledger.namespace.ancestor.requested != ledger.namespace.ancestor.canonical {
            return Err(invalid());
        }
        if !paths.insert(&ledger.path)
            || !before_valid(&ledger.before, ledger.before_blob.as_ref())
            || ledger
                .after_blob
                .as_ref()
                .is_some_and(|blob| !blob_valid(blob))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/unit/capability_tx_policy.rs"]
mod tests;

fn staged_name(path: &std::path::Path, suffix: &str) -> bool {
    path.file_name()
        .and_then(|v| v.to_str())
        .is_some_and(|name| {
            name.strip_prefix('.')
                .and_then(|v| v.strip_suffix(suffix))
                .is_some_and(crate::policy::operation::state::valid_id)
        })
}
fn candidate_valid(
    candidate: &Candidate,
    path: &std::path::Path,
    namespace: &Namespace,
    blob: Option<&Blob>,
    mode: Option<u32>,
    pending: &Pending,
) -> Result<()> {
    if candidate.target != path || !state_valid(&candidate.after) {
        return Err(invalid());
    }
    match (blob, &candidate.after, &candidate.staged) {
        (None, State::Missing, None) => Ok(()),
        (
            Some(blob),
            State::File {
                sha256,
                mode: actual,
                ..
            },
            Some(staged),
        ) if mode == Some(*actual) && blob.sha256 == *sha256 => {
            candidate_path(staged, path, namespace, pending)
        }
        (
            Some(blob),
            State::Link {
                text,
                mode: Some(_),
                ..
            },
            Some(staged),
        ) if mode.is_none() && link_blob(blob, text) => {
            candidate_path(staged, path, namespace, pending)
        }
        _ => Err(invalid()),
    }
}
fn candidate_path(
    staged: &std::path::Path,
    path: &std::path::Path,
    namespace: &Namespace,
    pending: &Pending,
) -> Result<()> {
    lexical(staged)?;
    let mut parent = namespace.ancestor.canonical.clone();
    for part in namespace
        .remaining
        .iter()
        .take(namespace.remaining.len().saturating_sub(1))
    {
        parent.push(part);
    }
    let alternate = pending
        .parents
        .iter()
        .find(|p| Some(p.target.as_path()) == path.parent())
        .map(|p| p.staged.as_path());
    if !staged_name(staged, ".cap.tmp")
        || (staged.parent() != Some(parent.as_path()) && staged.parent() != alternate)
    {
        return Err(invalid());
    }
    Ok(())
}

pub fn validate_pending(manifest: &Manifest, pending: &Pending, digest: &str) -> Result<()> {
    if pending.schema_version != manifest.schema_version
        || pending.operation_id != manifest.operation_id
        || pending.manifest_sha256 != digest
        || !valid_digest(digest)
        || pending.files.len() > manifest.files.len()
        || pending.ledgers.len() > manifest.ledgers.len()
        || pending.parents.len() > manifest.files.len()
        || pending.rollbacks.len() > 20000
    {
        return Err(invalid());
    }
    let parents = validate_parents(manifest, pending)?;
    for (candidate, file) in pending.files.iter().zip(&manifest.files) {
        candidate_valid(
            candidate,
            &file.path,
            &file.namespace,
            file.after_blob.as_ref(),
            if file.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink {
                None
            } else {
                Some(file.after_mode.unwrap_or(0o600))
            },
            pending,
        )?;
    }
    for (candidate, ledger) in pending.ledgers.iter().zip(
        manifest
            .ledgers
            .iter()
            .filter(|l| l.before_blob != l.after_blob),
    ) {
        candidate_valid(
            candidate,
            &ledger.path,
            &ledger.namespace,
            ledger.after_blob.as_ref(),
            Some(0o600),
            pending,
        )?;
    }
    if pending.ledgers.len()
        > manifest
            .ledgers
            .iter()
            .filter(|l| l.before_blob != l.after_blob)
            .count()
    {
        return Err(invalid());
    }
    for candidate in &pending.rollbacks {
        let (namespace, state, blob) = if let Some(file) =
            manifest.files.iter().find(|f| f.path == candidate.target)
        {
            (&file.namespace, &file.before, file.before_blob.as_ref())
        } else if let Some(ledger) = manifest.ledgers.iter().find(|l| l.path == candidate.target) {
            (
                &ledger.namespace,
                &ledger.before,
                ledger.before_blob.as_ref(),
            )
        } else {
            return Err(invalid());
        };
        let mode = if manifest.files.iter().any(|f| {
            f.path == candidate.target
                && f.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink
        }) {
            None
        } else if let State::File { mode, .. } = state {
            Some(*mode)
        } else {
            Some(0o600)
        };
        candidate_valid(candidate, &candidate.target, namespace, blob, mode, pending)?;
    }
    let mut retired = std::collections::BTreeSet::new();
    if pending
        .retired_parents
        .iter()
        .any(|path| !parents.contains(path) || !retired.insert(path))
    {
        return Err(invalid());
    }
    Ok(())
}

fn validate_directories(manifest: &Manifest) -> Result<()> {
    let mut seen = std::collections::BTreeSet::new();
    for directory in &manifest.directories {
        namespace_valid(
            &directory.binding.requested.join(".probe"),
            &Namespace {
                ancestor: directory.binding.clone(),
                remaining: vec![".probe".into()],
            },
        )?;
        if !seen.insert(&directory.binding.requested)
            || directory.entries.len() > 5000
            || directory.entries.iter().any(|(name, entry)| {
                !crate::policy::catalog::resources::component(name)
                    || !matches!(
                        entry.kind.as_str(),
                        "file" | "directory" | "symlink" | "other"
                    )
                    || entry.mode > 0o7777
                    || entry.identity.device == 0
                    || entry.identity.inode == 0
            })
        {
            return Err(invalid());
        }
    }

    Ok(())
}

fn validate_parents<'a>(
    manifest: &Manifest,
    pending: &'a Pending,
) -> Result<std::collections::BTreeSet<&'a PathBuf>> {
    let mut parents = std::collections::BTreeSet::new();
    for parent in &pending.parents {
        let file = manifest
            .files
            .iter()
            .find(|f| f.path.parent() == Some(parent.target.as_path()))
            .ok_or_else(invalid)?;
        if file.namespace.remaining.len() != 2
            || file.before != State::Missing
            || !match manifest.category {
                GovernanceCategory::Policy => {
                    parent.target.file_name() == Some(std::ffi::OsStr::new("agents"))
                        && file.kind == crate::capability::rules::delivery::ChangeKind::CodexPolicy
                }
                GovernanceCategory::Links => {
                    parent.target.file_name() == Some(std::ffi::OsStr::new("skills"))
                        && file.kind == crate::capability::rules::delivery::ChangeKind::ManagedLink
                }
                GovernanceCategory::Source => false,
            }
            || parent.staged.parent() != Some(file.namespace.ancestor.canonical.as_path())
            || !staged_name(&parent.staged, ".cap-dir")
            || parent.binding.requested != parent.staged
            || parent.binding.canonical != parent.staged
            || parent.binding.lineage.last() != Some(&parent.binding.identity)
            || parent
                .binding
                .lineage
                .get(..parent.binding.lineage.len().saturating_sub(1))
                != Some(file.namespace.ancestor.lineage.as_slice())
            || parent.binding.mode != 0o700
            || parent.binding.uid != file.namespace.ancestor.uid
            || !parents.insert(&parent.target)
        {
            return Err(invalid());
        }
    }
    Ok(parents)
}

fn validate_metadata_file(manifest: &Manifest, file: &FileChange) -> Result<()> {
    if crate::capability::rules::invocation::metadata_access(
        &manifest.home,
        &file.path,
        file.kind.metadata()?,
    )? != crate::capability::rules::invocation::MetadataAccess::Managed
        || !before_valid(&file.before, file.before_blob.as_ref())
        || file.after_blob.is_some() != file.after_mode.is_some()
        || file.after_mode.is_some_and(|mode| mode > 0o7777)
        || file.after_blob.as_ref().is_some_and(|b| !blob_valid(b))
    {
        return Err(invalid());
    }
    Ok(())
}
fn binding_shape(binding: &crate::policy::governance::DirectoryBinding) -> Result<()> {
    namespace_valid(
        &binding.requested.join(".probe"),
        &Namespace {
            ancestor: binding.clone(),
            remaining: vec![".probe".into()],
        },
    )?;
    if binding.requested != binding.canonical || binding.mode > 0o7777 {
        return Err(invalid());
    }
    Ok(())
}
fn validate_layout(manifest: &Manifest) -> Result<()> {
    let layout = manifest.layout.as_ref().ok_or_else(invalid)?;
    for root in [&layout.config, &layout.data, &layout.state] {
        lexical(root)?;
    }
    if layout.state != manifest.state
        || [&layout.config, &layout.data, &layout.state]
            .iter()
            .enumerate()
            .any(|(i, a)| {
                [&layout.config, &layout.data, &layout.state]
                    .iter()
                    .skip(i + 1)
                    .any(|b| a.starts_with(b) || b.starts_with(a))
            })
    {
        return Err(invalid());
    }
    Ok(())
}
fn validate_profile_file(manifest: &Manifest, file: &FileChange) -> Result<()> {
    if manifest.schema_version != 4
        || manifest
            .configuration
            .as_ref()
            .ok_or_else(invalid)?
            .target_kind(&file.path)?
            != file.kind
    {
        return Err(invalid());
    }
    let State::File { mode, .. } = file.before else {
        return Err(invalid());
    };
    if !before_valid(&file.before, file.before_blob.as_ref())
        || file.after_mode != Some(mode)
        || file.after_blob.as_ref().is_none_or(|b| !blob_valid(b))
    {
        return Err(invalid());
    }
    Ok(())
}
fn validate_link_authority(manifest: &Manifest) -> Result<()> {
    use crate::capability::rules::delivery::{LinkText, Selection};
    validate_layout(manifest)?;
    let authority = manifest.links.as_ref().ok_or_else(invalid)?;
    binding_shape(&authority.canonical)?;
    if authority.canonical.requested != manifest.home.join(".agents/skills")
        || authority.selected.len() > 5000
        || authority.profiles.len() > 4
        || authority.pending_config_policies.len() > 32
    {
        return Err(invalid());
    }
    if let Selection::Explicit(keys) = &authority.requested
        && (keys.is_empty()
            || keys.len() > 5000
            || keys
                .iter()
                .any(|k| k.is_empty() || k.len() > 512 || k.chars().any(char::is_control)))
    {
        return Err(invalid());
    }
    let mut ids = std::collections::BTreeSet::new();
    for (name, id) in &authority.selected {
        LinkText::managed(name)?;
        if id.is_empty() || id.len() > 512 || id.chars().any(char::is_control) || !ids.insert(id) {
            return Err(invalid());
        }
    }
    for (profile, binding) in &authority.profiles {
        binding_shape(binding)?;
        if binding.requested != manifest.home.join(format!(".claude-{}", profile.name())) {
            return Err(invalid());
        }
    }
    for (profile, names) in &authority.protected {
        if !authority.profiles.contains_key(profile) || names.len() > 5000 {
            return Err(invalid());
        }
        for name in names {
            LinkText::managed(name)?;
        }
    }
    if authority.pending_config_policies.iter().any(|p| {
        !matches!(p.as_str(), "codex:disable_agents_skills")
            && !crate::capability::rules::delivery::Profile::ALL
                .into_iter()
                .any(|profile| {
                    p == &format!("claude:{}:plugins_off", profile.name())
                        || p == &format!("claude:{}:pstack_allow", profile.name())
                })
    }) {
        return Err(invalid());
    }
    Ok(())
}
fn link_location<'a>(manifest: &'a Manifest, file: &FileChange) -> Result<&'a str> {
    let authority = manifest.links.as_ref().ok_or_else(invalid)?;
    let relative = file
        .path
        .strip_prefix(&manifest.home)
        .map_err(|_| invalid())?;
    let parts: Vec<_> = relative
        .components()
        .map(|p| p.as_os_str().to_str().ok_or_else(invalid))
        .collect::<Result<_>>()?;
    let [folder, "skills", name] = parts.as_slice() else {
        return Err(invalid());
    };
    let profile = crate::capability::rules::delivery::Profile::parse(
        folder.strip_prefix(".claude-").ok_or_else(invalid)?,
    )?;
    if name.to_ascii_lowercase().starts_with("devnorm")
        || authority
            .protected
            .get(&profile)
            .is_some_and(|v| v.contains(*name))
    {
        return Err(invalid());
    }
    let (selected, _) = authority
        .selected
        .get_key_value(*name)
        .ok_or_else(invalid)?;
    let root = authority.profiles.get(&profile).ok_or_else(invalid)?;
    let ancestor = &file.namespace.ancestor;
    if ancestor.requested == root.requested {
        if ancestor != root || file.namespace.remaining != ["skills".to_owned(), selected.clone()] {
            return Err(invalid());
        }
    } else if ancestor.requested == root.requested.join("skills") {
        if ancestor.lineage.len() != root.lineage.len() + 1
            || !ancestor.lineage.starts_with(&root.lineage)
            || ancestor.uid != root.uid
        {
            return Err(invalid());
        }
    } else {
        return Err(invalid());
    }
    Ok(selected)
}
fn link_blob(blob: &Blob, text: &crate::capability::rules::delivery::LinkText) -> bool {
    blob_valid(blob)
        && blob.sha256 == crate::capability::rules::invocation::hash(text.as_str().as_bytes())
        && blob.size == text.as_str().len() as u64
}
fn validate_link_file(manifest: &Manifest, file: &FileChange) -> Result<()> {
    if file.kind != crate::capability::rules::delivery::ChangeKind::ManagedLink
        || file.after_mode.is_some()
    {
        return Err(invalid());
    }
    let name = link_location(manifest, file)?;
    let text = crate::capability::rules::delivery::LinkText::managed(name)?;
    match (&file.before, &file.before_blob) {
        (State::Missing, None) => {
            if file.after_blob.is_none() {
                return Err(invalid());
            }
        }
        (
            State::Link {
                text: before,
                mode: Some(_),
                ..
            },
            Some(blob),
        ) if state_valid(&file.before) && before == &text && link_blob(blob, before) => (),
        _ => return Err(invalid()),
    }
    if file
        .after_blob
        .as_ref()
        .is_some_and(|blob| !link_blob(blob, &text))
    {
        return Err(invalid());
    }
    Ok(())
}
