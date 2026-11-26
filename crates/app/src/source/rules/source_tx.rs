use crate::{
    capability::rules::capability_tx::{self, Blob, Candidate, State, Witness},
    domain::{
        CapabilityKind, LocalCapabilityQuery, Observation, Package, Provider, QuerySpec, Scope,
    },
    error::{Error, Result},
    policy::{
        governance::{AuthorityPaths, Layout},
        source_tree::{Change, Tree},
    },
    source::rules::source_baseline::Baseline,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSpec {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub package: Package,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub query: LocalCapabilityQuery,
    #[serde(deserialize_with = "expected")]
    pub expected: Observation,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
enum Expected {
    CapabilityPack {
        capability: CapabilityKind,
        #[serde(deserialize_with = "crate::policy::json::object")]
        pack: crate::domain::CapabilityStamp,
    },
}
fn expected<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Observation, D::Error> {
    let Expected::CapabilityPack { capability, pack } = crate::policy::json::object(input)?;
    Ok(Observation::CapabilityPack {
        capability,
        pack: Some(pack),
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedRoot {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub baseline: Baseline,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub tree: Tree,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub native: Option<NativeSpec>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradedRoot {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub baseline: Baseline,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub change: Change,
    pub commit: String,
    pub skill_tree: String,
    pub explicit_pin: bool,
    pub requested_ref: Option<String>,
    pub base_sha256: String,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub base_shape: crate::policy::source_tree::Shape,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub invocation: crate::source::rules::source_invocation::Intent,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub native_before: Option<NativeSpec>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub native_after: Option<NativeSpec>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Adopt {
        #[serde(deserialize_with = "crate::policy::json::object")]
        root: Box<ObservedRoot>,
    },
    Upgrade {
        #[serde(deserialize_with = "crate::policy::json::object_vec")]
        roots: Vec<UpgradedRoot>,
        #[serde(deserialize_with = "crate::policy::json::object")]
        invocation: Box<Witness>,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Slot {
    Native,
    Definitions,
    Managed,
    Overlays,
    Receipts,
}
pub const SLOTS: [Slot; 5] = [
    Slot::Native,
    Slot::Definitions,
    Slot::Managed,
    Slot::Overlays,
    Slot::Receipts,
];
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub slot: Slot,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub witness: Witness,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub before: Option<Blob>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub after: Option<Blob>,
    pub after_mode: Option<u32>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub pair: Option<crate::source::rules::source_files::Pair>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub operation_id: String,
    pub change_id: String,
    pub home: PathBuf,
    pub settings: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub layout: Layout,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub scope: Scope,
    pub pending: PathBuf,
    pub created_at: u64,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub action: Action,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub inputs: Vec<Witness>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub ledgers: Vec<Ledger>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub forward: Option<Candidate>,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub restored: Option<Candidate>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    pub schema_version: u32,
    pub operation_id: String,
    pub manifest_sha256: String,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub ledgers: Vec<Progress>,
}
pub fn invalid() -> Error {
    Error::attention(
        "invalid_source_transaction",
        "来源事务身份、范围或持久证据不一致",
    )
}
fn content(state: &State, blob: Option<&Blob>, mode: Option<u32>) -> bool {
    match (state, blob, mode) {
        (State::Missing, None, None) => true,
        (
            State::File {
                sha256,
                mode: actual,
                ..
            },
            Some(blob),
            Some(mode),
        ) => {
            capability_tx::state_valid(state)
                && capability_tx::blob_valid(blob)
                && blob.size <= 16 * 1024 * 1024
                && sha256 == &blob.sha256
                && *actual == mode
                && mode <= 0o777
        }
        _ => false,
    }
}
pub fn mode(state: &State) -> Option<u32> {
    match state {
        State::File { mode, .. } => Some(*mode),
        _ => None,
    }
}
pub fn paths(manifest: &Manifest, authority: &AuthorityPaths) -> Vec<PathBuf> {
    vec![
        crate::capability::rules::delivery::native_lock_path(&manifest.home, &manifest.scope),
        authority.dataset(crate::policy::governance::Dataset::AssetDefinitions),
        authority.managed.clone(),
        authority.overlays.clone(),
        manifest.layout.state.join("receipts.json"),
    ]
}
pub fn validate(manifest: &Manifest, authority: &AuthorityPaths, owner: u32) -> Result<()> {
    if manifest.schema_version != 5
        || !crate::policy::operation::state::valid_id(&manifest.operation_id)
        || manifest.inputs.len() > 10000
        || manifest.ledgers.len() != 5
        || manifest.pending != authority.pending
        || manifest.scope.home != manifest.home
    {
        return Err(invalid());
    }
    crate::skills::rules::skills::parse_change_id(&manifest.change_id).map_err(|_| invalid())?;
    for path in [
        &manifest.home,
        &manifest.settings,
        &manifest.layout.config,
        &manifest.layout.data,
        &manifest.layout.state,
        &manifest.layout.cache,
        &manifest.pending,
    ] {
        crate::policy::paths::lexical(path)?;
    }
    validate_action(manifest, authority, owner)?;
    let expected = paths(manifest, authority);
    let mut reserved = expected.clone();
    reserved.extend([
        manifest.settings.clone(),
        manifest.layout.config.clone(),
        manifest.layout.data.clone(),
        manifest.layout.state.clone(),
        manifest.layout.cache.clone(),
        manifest.pending.clone(),
        authority.backups.clone(),
        authority.manifests.clone(),
    ]);
    reserved.extend(manifest.inputs.iter().map(|w| w.path.clone()));
    match &manifest.action {
        Action::Adopt { root } => {
            crate::source::rules::source_baseline::disjoint(&root.baseline.slot.path(), &reserved)?;
        }
        Action::Upgrade { roots, .. } => {
            for root in roots {
                crate::source::rules::source_baseline::disjoint(
                    &root.change.live.path(),
                    &reserved,
                )?;
                crate::source::rules::source_baseline::disjoint(
                    &root.change.retained.path(),
                    &reserved,
                )?;
            }
        }
    }
    for ((ledger, slot), path) in manifest.ledgers.iter().zip(SLOTS).zip(expected) {
        let witness = &ledger.witness;
        capability_tx::namespace_valid(&witness.path, &witness.namespace)?;
        if witness.namespace.ancestor.uid != owner
            || witness.namespace.ancestor.mode > 0o777
            || witness.namespace.ancestor.mode & 0o022 != 0
            || witness.namespace.ancestor.requested != witness.namespace.ancestor.canonical
        {
            return Err(invalid());
        }
        if ledger.slot != slot
            || witness.path != path
            || !content(&witness.state, ledger.before.as_ref(), mode(&witness.state))
            || ledger.after.is_some() != ledger.after_mode.is_some()
            || ledger.after_mode.is_some_and(|m| m > 0o777)
            || ledger
                .after
                .as_ref()
                .is_some_and(|b| !capability_tx::blob_valid(b) || b.size > 16 * 1024 * 1024)
        {
            return Err(invalid());
        }
        if (ledger.before == ledger.after && ledger.after_mode != mode(&witness.state))
            || (matches!(manifest.action, Action::Adopt { .. })
                && slot != Slot::Managed
                && slot != Slot::Receipts
                && (ledger.before != ledger.after || ledger.after_mode != mode(&witness.state)))
        {
            return Err(invalid());
        }
    }
    validate_files(manifest, owner)?;
    for witness in &manifest.inputs {
        capability_tx::namespace_valid(&witness.path, &witness.namespace)?;
        if !capability_tx::state_valid(&witness.state)
            || manifest
                .ledgers
                .iter()
                .any(|l| l.witness.path == witness.path)
        {
            return Err(invalid());
        }
    }
    Ok(())
}
fn validate_files(manifest: &Manifest, owner: u32) -> Result<()> {
    let mut pairs = vec![];
    for ledger in &manifest.ledgers {
        let changed = ledger.before != ledger.after;
        match (&manifest.action, &ledger.pair) {
            (Action::Adopt { .. }, None) => (),
            (Action::Upgrade { .. }, None) if !changed => (),
            (Action::Upgrade { .. }, Some(pair)) if changed => {
                if pair.live.path() != ledger.witness.path
                    || pair.live.parent != ledger.witness.namespace.ancestor
                    || ledger.witness.namespace.remaining != vec![pair.live.name.clone()]
                    || pair.before.as_ref().map_or(
                        State::Missing,
                        crate::source::rules::source_files::Object::state,
                    ) != ledger.witness.state
                    || !content(
                        &pair.after.state(),
                        ledger.after.as_ref(),
                        ledger.after_mode,
                    )
                    || ledger
                        .after
                        .as_ref()
                        .is_none_or(|blob| blob.size != pair.after.size)
                    || pair
                        .before
                        .as_ref()
                        .zip(ledger.before.as_ref())
                        .is_some_and(|(object, blob)| object.size != blob.size)
                    || manifest.inputs.iter().any(|witness| {
                        pair.retained.path().starts_with(&witness.path)
                            || witness.path.starts_with(pair.retained.path())
                    })
                    || manifest.ledgers.iter().any(|other| {
                        pair.retained.path().starts_with(&other.witness.path)
                            || other.witness.path.starts_with(pair.retained.path())
                    })
                {
                    return Err(invalid());
                }
                pairs.push(pair.clone());
            }
            _ => return Err(invalid()),
        }
    }
    if let Action::Upgrade { roots, .. } = &manifest.action {
        crate::source::rules::source_files::validate_batch(
            &pairs,
            &roots
                .iter()
                .map(|root| root.change.clone())
                .collect::<Vec<_>>(),
            owner,
        )?;
    }
    Ok(())
}
fn validate_action(manifest: &Manifest, authority: &AuthorityPaths, owner: u32) -> Result<()> {
    match &manifest.action {
        Action::Adopt { root } => {
            root.baseline.authorize(
                &root.baseline.source,
                &root.baseline.slot,
                &root.tree,
                &manifest.home,
                owner,
            )?;
            validate_native(manifest, root)
        }
        Action::Upgrade { roots, invocation } => {
            if invocation.path
                != authority.dataset(crate::policy::governance::Dataset::InvocationRules)
                || !manifest.inputs.contains(invocation)
            {
                return Err(invalid());
            }
            if roots.is_empty() {
                return Err(invalid());
            }
            let changes: Vec<_> = roots.iter().map(|r| r.change.clone()).collect();
            crate::policy::source_tree::validate_changes(&changes, owner)?;
            let mut members = std::collections::BTreeSet::new();
            for root in roots {
                root.baseline.authorize(
                    &root.baseline.source,
                    &root.change.live,
                    &root.change.before,
                    &manifest.home,
                    owner,
                )?;
                crate::policy::source_git::ObjectId::parse(&root.commit)?;
                crate::policy::source_git::ObjectId::parse(&root.skill_tree)?;
                crate::policy::source_git::Reference::parse(root.requested_ref.as_deref())?;
                root.base_shape.validate(owner)?;
                if root.change.after.mode != root.change.before.mode
                    || root.base_shape.mode != root.change.after.mode
                    || root.base_shape.uid != root.change.after.uid
                    || root.change.retained.parent.mode != 0o700
                    || !matches!(root.change.after.entries.get("SKILL.md").map(|entry| &entry.content),Some(crate::policy::source_tree::Content::File {size,..}) if *size > 0)
                {
                    return Err(invalid());
                }
                let mut normalized = root.base_shape.clone();
                normalized.uid = 0;
                normalized.mode = 0o700;
                for entry in normalized.entries.values_mut() {
                    entry.uid = 0;
                }
                if !matches!(normalized.digest(),Ok(ref digest) if digest==&root.base_sha256) {
                    return Err(invalid());
                }

                if root.explicit_pin && root.requested_ref.as_deref() != Some(&root.commit) {
                    return Err(invalid());
                }
                if !members.insert(&root.baseline.source.member)
                    || root.native_before.is_some() != root.native_after.is_some()
                {
                    return Err(invalid());
                }
                for (tree, native) in [
                    (&root.change.before, &root.native_before),
                    (&root.change.after, &root.native_after),
                ] {
                    validate_native(
                        manifest,
                        &ObservedRoot {
                            baseline: root.baseline.clone(),
                            tree: tree.clone(),
                            native: native.clone(),
                        },
                    )?;
                }
                if let (Some(before), Some(after)) = (&root.native_before, &root.native_after)
                    && (serde_json::to_value(&before.package)?
                        != serde_json::to_value(&after.package)?
                        || before.query != after.query)
                {
                    return Err(invalid());
                }
            }
            Ok(())
        }
    }
}
fn validate_native(manifest: &Manifest, root: &ObservedRoot) -> Result<()> {
    let Some(native) = &root.native else {
        return if root.baseline.source.member
            == format!("asset:{}", root.baseline.source.canonical_name)
        {
            Ok(())
        } else {
            Err(invalid())
        };
    };
    let Provider::CapabilityObserved {
        capability: CapabilityKind::SkillsCli,
        manager,
        record,
        payload,
        native_id,
        source: Some(source),
        ..
    } = &native.package.provider
    else {
        return Err(invalid());
    };
    if manager != "skills-cli:github"
        || record
            != &crate::capability::rules::delivery::native_lock_path(
                &manifest.home,
                &manifest.scope,
            )
        || payload != &root.baseline.slot.path()
        || native_id != &root.baseline.source.canonical_name
        || crate::policy::source_git::Repository::parse(source)?.key()
            != root.baseline.source.repository
        || native.package.id != root.baseline.source.member
        || !matches!(
            native.expected,
            Observation::CapabilityPack {
                capability: CapabilityKind::SkillsCli,
                pack: Some(_)
            }
        )
    {
        return Err(invalid());
    }
    crate::capability::rules::capability_local::validate_package(&native.package)?;
    crate::capability::rules::capability_local::binding(
        &native.package,
        &QuerySpec::LocalCapability(native.query.clone()),
    )?;
    Ok(())
}
pub fn validate_pending(manifest: &Manifest, pending: &Pending, digest: &str) -> Result<()> {
    if pending.schema_version != 5
        || pending.operation_id != manifest.operation_id
        || pending.manifest_sha256 != digest
        || !crate::policy::config_local::valid_digest(digest)
        || pending.ledgers.len() != 5
    {
        return Err(invalid());
    }
    if matches!(manifest.action, Action::Upgrade { .. }) {
        return if pending
            .ledgers
            .iter()
            .all(|progress| progress.forward.is_none() && progress.restored.is_none())
        {
            Ok(())
        } else {
            Err(invalid())
        };
    }
    for (ledger, progress) in manifest.ledgers.iter().zip(&pending.ledgers) {
        for (candidate, blob, mode) in [
            (&progress.forward, ledger.after.as_ref(), ledger.after_mode),
            (
                &progress.restored,
                ledger.before.as_ref(),
                self::mode(&ledger.witness.state),
            ),
        ] {
            if let Some(candidate) = candidate {
                validate_candidate(candidate, ledger, blob, mode)?;
            }
        }
        if ledger.before == ledger.after
            && (progress.forward.is_some() || progress.restored.is_some())
        {
            return Err(invalid());
        }
        if progress.restored.is_some() && progress.forward.is_none() {
            return Err(invalid());
        }
    }
    Ok(())
}
fn validate_candidate(
    candidate: &Candidate,
    ledger: &Ledger,
    blob: Option<&Blob>,
    mode: Option<u32>,
) -> Result<()> {
    if candidate.target != ledger.witness.path || !content(&candidate.after, blob, mode) {
        return Err(invalid());
    }
    match (&candidate.staged, blob) {
        (None, None) => Ok(()),
        (Some(path), Some(_)) => {
            crate::policy::paths::lexical(path)?;
            let valid_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix('.'))
                .and_then(|n| n.strip_suffix(".cap.tmp"))
                .is_some_and(crate::policy::operation::state::valid_id);
            if path.parent() != candidate.target.parent() || !valid_name {
                return Err(invalid());
            }
            Ok(())
        }
        _ => Err(invalid()),
    }
}
pub fn is_before(ledger: &Ledger, progress: &Progress, current: &State) -> bool {
    current == &ledger.witness.state
        || progress
            .restored
            .as_ref()
            .is_some_and(|r| &r.after == current)
}
pub fn is_after(ledger: &Ledger, progress: &Progress, current: &State) -> bool {
    if ledger.before == ledger.after {
        current == &ledger.witness.state
    } else {
        progress
            .forward
            .as_ref()
            .is_some_and(|c| &c.after == current)
    }
}
