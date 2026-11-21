use crate::{
    domain::{ConfigAnchor, FileId},
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    pub home: PathBuf,
    pub settings: PathBuf,
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl Locations {
    pub fn roots(&self) -> [&Path; 4] {
        [&self.config, &self.data, &self.state, &self.cache]
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl Layout {
    pub fn roots(&self) -> [&Path; 4] {
        [&self.config, &self.data, &self.state, &self.cache]
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectoryBinding {
    pub requested: PathBuf,
    pub canonical: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub lineage: Vec<FileId>,
    pub uid: u32,
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DurableBindings {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub config: DirectoryBinding,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub data: DirectoryBinding,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub state: DirectoryBinding,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Dataset {
    ManagedAssets,
    InvocationOverlays,
    ManagedUpdates,
    SkillProfiles,
    AssetDefinitions,
    InvocationRules,
    SourceChannels,
    MacApps,
}

pub const DATASETS: [Dataset; 8] = [
    Dataset::ManagedAssets,
    Dataset::InvocationOverlays,
    Dataset::ManagedUpdates,
    Dataset::SkillProfiles,
    Dataset::AssetDefinitions,
    Dataset::InvocationRules,
    Dataset::SourceChannels,
    Dataset::MacApps,
];

impl Dataset {
    pub fn source(self, p: &Locations) -> PathBuf {
        match self {
            Self::ManagedAssets => p.home.join(".agents/managed-assets.json"),
            Self::InvocationOverlays => p.home.join(".agents/invocation-overlays.json"),
            Self::ManagedUpdates => p.home.join(".agents/cache/managed-updates.json"),
            _ => p.settings.join("manifests").join(self.filename()),
        }
    }
    pub fn target(self, p: &Layout) -> PathBuf {
        match self {
            Self::ManagedAssets | Self::InvocationOverlays => p.state.join(self.filename()),
            Self::ManagedUpdates => p.cache.join(self.filename()),
            _ => p.config.join("manifests").join(self.filename()),
        }
    }
    pub fn filename(self) -> &'static str {
        match self {
            Self::ManagedAssets => "managed-assets.json",
            Self::InvocationOverlays => "invocation-overlays.json",
            Self::ManagedUpdates => "managed-updates.json",
            Self::SkillProfiles => "skill-profiles.json",
            Self::AssetDefinitions => "managed-agent-assets.json",
            Self::InvocationRules => "agent-invocation-policy.json",
            Self::SourceChannels => "agent-source-channels.json",
            Self::MacApps => "mac-apps.tsv",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileProof {
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub anchor: ConfigAnchor,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub identity: FileId,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub lineage: Vec<FileId>,
    pub sha256: String,
    pub mode: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transfer {
    Missing,
    Copied {
        source: PathBuf,
        target: PathBuf,
        #[serde(deserialize_with = "crate::policy::json::object")]
        original: Box<FileProof>,
        target_sha256: String,
        target_mode: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyLink {
    pub path: PathBuf,
    pub text: PathBuf,
    pub target: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    Settings,
    Backups,
    RebuildState,
    UnifyState,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IndexedEntry {
    File {
        sha256: String,
        mode: u32,
        size: u64,
    },
    Directory {
        mode: u32,
    },
    Link {
        text: PathBuf,
        mode: u32,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Archive {
    pub kind: ArchiveKind,
    pub root: PathBuf,
    pub mode: u32,
    pub entries: BTreeMap<PathBuf, IndexedEntry>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RetainKind {
    NativeSkillsLock,
    NativeSkills,
    Translations,
    SkillAliases,
    SkillSources,
    Releases,
    NativeShare,
    Launchers,
    Harness,
    NativeState,
    AgentRepositoryMetadata,
    SystemMetadata,
    IndependentAgentEntry,
    EmptyLegacyLockContainer,
    LegacyGovernanceHistory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedEntry {
    Directory,
    File,
    Symlink { text: PathBuf },
    EmptyDirectory,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Retained {
    pub path: PathBuf,
    pub owner: RetainKind,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_retained_entry"
    )]
    pub entry: Option<RetainedEntry>,
}

fn present_retained_entry<'de, D: serde::Deserializer<'de>>(
    input: D,
) -> std::result::Result<Option<RetainedEntry>, D::Error> {
    crate::policy::json::object(input).map(Some)
}

pub const LEGACY_LEDGER_VERSION: u32 = 1;
pub const LEDGER_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    Committed,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommittedMigration {
    pub schema_version: u32,
    pub phase: MigrationPhase,
    pub migration_id: String,
    pub home: PathBuf,
    pub legacy_settings: PathBuf,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub layout: Layout,
    #[serde(deserialize_with = "crate::policy::json::object")]
    pub root_bindings: DurableBindings,
    #[serde(default, deserialize_with = "crate::policy::json::optional_object")]
    pub cache_audit: Option<DirectoryBinding>,
    pub converted: BTreeMap<Dataset, Transfer>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub legacy_links: Vec<LegacyLink>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub archives: Vec<Archive>,
    #[serde(deserialize_with = "crate::policy::json::object_vec")]
    pub retained: Vec<Retained>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Authority {
    Legacy,
    Migrated { migration_id: String },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AuthorityPaths {
    pub authority: Authority,
    pub manifests: PathBuf,
    pub managed: PathBuf,
    pub overlays: PathBuf,
    pub pending: PathBuf,
    pub backups: PathBuf,
    pub updates: PathBuf,
    pub profiles: PathBuf,
    pub required: BTreeSet<Dataset>,
}

pub(crate) fn invalid() -> Error {
    Error::attention(
        "invalid_migration_ledger",
        "迁移账本损坏、作用域不匹配或包含未支持记录",
    )
}

fn absolute(path: &Path) -> Result<()> {
    crate::policy::paths::lexical(path)?;
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(invalid());
    }
    Ok(())
}

fn separated(paths: &[&Path]) -> bool {
    paths.iter().enumerate().all(|(i, a)| {
        paths[i + 1..]
            .iter()
            .all(|b| !a.starts_with(b) && !b.starts_with(a))
    })
}

fn locations_valid(p: &Locations) -> Result<()> {
    for path in [&p.home, &p.settings, &p.config, &p.data, &p.state, &p.cache] {
        absolute(path)?;
    }
    Ok(())
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn ids_valid(identity: &FileId, lineage: &[FileId]) -> bool {
    identity.inode > 0
        && identity.device > 0
        && !lineage.is_empty()
        && lineage.len() <= 64
        && lineage.last() == Some(identity)
        && lineage.iter().all(|id| id.device > 0 && id.inode > 0)
}

fn binding_valid(binding: &DirectoryBinding, expected: &Path, private: bool) -> Result<()> {
    absolute(&binding.requested)?;
    absolute(&binding.canonical)?;
    if binding.requested != expected
        || !ids_valid(&binding.identity, &binding.lineage)
        || binding.mode > 0o7777
        || (private && binding.mode != 0o700)
    {
        return Err(invalid());
    }
    Ok(())
}

fn proof_valid(proof: &FileProof, source: &Path) -> Result<()> {
    crate::policy::config_local::validate_anchor(&proof.anchor)?;
    if proof.anchor.requested != source
        || !digest(&proof.sha256)
        || proof.mode > 0o7777
        || proof.identity.device == 0
        || proof.identity.inode == 0
        || !ids_valid(&proof.anchor.parent_identity, &proof.lineage)
    {
        return Err(invalid());
    }
    Ok(())
}

fn transfers_valid(p: &Locations, l: &CommittedMigration) -> Result<()> {
    if l.converted.len() != DATASETS.len() {
        return Err(invalid());
    }
    for (dataset, transfer) in &l.converted {
        let source = dataset.source(p);
        let target = dataset.target(&l.layout);
        if source == target {
            return Err(invalid());
        }
        match transfer {
            Transfer::Missing => (),
            Transfer::Copied {
                source: found_source,
                target: found_target,
                original,
                target_sha256,
                target_mode,
            } => {
                if *found_source != source
                    || *found_target != target
                    || !digest(target_sha256)
                    || *target_sha256 != original.sha256
                    || *target_mode != 0o600
                {
                    return Err(invalid());
                }
                proof_valid(original, &source)?;
            }
        }
    }
    Ok(())
}

fn links_valid(p: &Locations, l: &CommittedMigration) -> Result<()> {
    if l.legacy_links.len() > 1 {
        return Err(invalid());
    }
    for link in &l.legacy_links {
        let expected = Dataset::SkillProfiles.source(p);
        if link.path != p.home.join(".agents/skill-profiles.json")
            || link.target != expected
            || link.text.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(invalid());
        }
        let resolved =
            crate::policy::paths::normalize_link(&p.home.join(".agents").join(&link.text))?;
        if resolved != expected {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn archive_root(p: &Locations, kind: ArchiveKind) -> PathBuf {
    match kind {
        ArchiveKind::Settings => p.settings.clone(),
        ArchiveKind::Backups => p.home.join(".agents/backups"),
        ArchiveKind::RebuildState => p.home.join(".local/state/macos-rebuild"),
        ArchiveKind::UnifyState => p.home.join(".local/state/mo-unify"),
    }
}

fn archives_valid(p: &Locations, l: &CommittedMigration) -> Result<()> {
    let mut kinds = BTreeSet::new();
    let mut total = 0;
    for archive in &l.archives {
        if !kinds.insert(archive.kind)
            || archive.root != archive_root(p, archive.kind)
            || archive.mode > 0o7777
            || (matches!(
                archive.kind,
                ArchiveKind::Backups | ArchiveKind::RebuildState
            ) && archive.mode != 0o700)
        {
            return Err(invalid());
        }
        total += archive.entries.len();
        if total > 40_000 {
            return Err(invalid());
        }
        for (relative, entry) in &archive.entries {
            if relative.as_os_str().is_empty() || relative.is_absolute() {
                return Err(invalid());
            }
            let combined = archive.root.join(relative);
            absolute(&combined)?;
            match entry {
                IndexedEntry::File { sha256, mode, .. } if digest(sha256) && *mode <= 0o7777 => (),
                IndexedEntry::Directory { mode } if *mode <= 0o7777 => (),
                IndexedEntry::Link { text, mode }
                    if *mode <= 0o7777 && !text.as_os_str().as_encoded_bytes().contains(&0) => {}
                _ => return Err(invalid()),
            }
        }
    }
    Ok(())
}

const NATIVE_STATE_NAMES: [&str; 11] = [
    "agent-media",
    "billion-context",
    "claude",
    "claudectl",
    "cmux",
    "gh",
    "herdr",
    "herdr-core",
    "mimocode",
    "mise",
    "pnpm",
];

fn native_state_parent(p: &Locations) -> PathBuf {
    p.home.join(".local/state")
}

pub fn native_state_roots(p: &Locations) -> impl Iterator<Item = PathBuf> {
    let parent = native_state_parent(p);
    NATIVE_STATE_NAMES
        .into_iter()
        .map(move |name| parent.join(name))
}

pub fn legacy_control_roots(p: &Locations) -> impl Iterator<Item = PathBuf> + '_ {
    [
        p.settings.clone(),
        p.settings.join("manifests"),
        p.home.join(".local/settings"),
        p.home.join(".local/settings/manifests"),
        p.home.join(".local/bin"),
        p.home.join(".local/share/chezmoi"),
        p.home.join(".agents"),
        p.home.join(".agents/bin"),
        p.home.join(".agents/cache"),
        p.home.join(".agents/cache/upstream"),
        p.home.join(".agents/invocation-policy.pending.json"),
        p.home.join(".agents/locks"),
        p.home.join(".agents/out"),
        p.home.join(".agents/skill-profiles.json"),
        archive_root(p, ArchiveKind::Backups),
        archive_root(p, ArchiveKind::RebuildState),
        archive_root(p, ArchiveKind::UnifyState),
    ]
    .into_iter()
    .chain(DATASETS.into_iter().map(|dataset| dataset.source(p)))
}

pub fn legacy_history_roots(p: &Locations) -> impl Iterator<Item = PathBuf> + '_ {
    [
        ".agents/cache/upstream",
        ".agents/out",
        ".agents/bin/tests",
        ".agents/bin/__pycache__",
    ]
    .into_iter()
    .map(|relative| p.home.join(relative))
}

fn default_roots(p: &Locations) -> [PathBuf; 4] {
    [
        p.home.join(".config/macops"),
        p.home.join(".local/share/macops"),
        p.home.join(".local/state/macops"),
        p.home.join(".cache/macops"),
    ]
}

pub fn displaced_default_roots(p: &Locations, roots: [&Path; 4]) -> impl Iterator<Item = PathBuf> {
    default_roots(p)
        .into_iter()
        .zip(roots)
        .filter_map(|(default, root)| (default != root).then_some(default))
}

pub fn overlaps(a: &Path, b: &Path) -> bool {
    a.components()
        .zip(b.components())
        .all(|(a, b)| a.as_os_str().eq_ignore_ascii_case(b.as_os_str()))
}

pub fn root_scope_valid(p: &Locations, roots: [&Path; 4]) -> bool {
    roots
        .iter()
        .enumerate()
        .all(|(i, root)| roots[i + 1..].iter().all(|other| !overlaps(root, other)))
        && native_state_roots(p).all(|native| !overlaps(&p.settings, &native))
        && legacy_history_roots(p).all(|history| !overlaps(&p.settings, &history))
        && legacy_control_roots(p)
            .chain(native_state_roots(p))
            .chain(displaced_default_roots(p, roots))
            .all(|protected| roots.iter().all(|root| !overlaps(root, &protected)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetainedShape {
    Historical,
    Directory,
    File,
    NativeState,
    EmptyDirectory,
}

pub fn retained_rule(p: &Locations, path: &Path) -> Option<(RetainKind, RetainedShape)> {
    use RetainKind::{
        AgentRepositoryMetadata, EmptyLegacyLockContainer, Harness, IndependentAgentEntry,
        Launchers, LegacyGovernanceHistory, NativeShare, NativeSkills, NativeSkillsLock,
        NativeState, Releases, SkillAliases, SkillSources, SystemMetadata, Translations,
    };
    use RetainedShape::{Directory, EmptyDirectory, File, Historical};

    let relative = path.strip_prefix(&p.home).ok()?.to_str()?;
    let rule = match relative {
        ".agents/.skill-lock.json" => (NativeSkillsLock, Historical),
        ".agents/skills" => (NativeSkills, Historical),
        ".agents/translations" => (Translations, Historical),
        ".agents/skill-aliases.json" => (SkillAliases, Historical),
        ".agents/skill-sources.json" => (SkillSources, Historical),
        ".agents/releases" => (Releases, Historical),
        ".local/share" => (NativeShare, Historical),
        ".local/bin" => (Launchers, Historical),
        ".agents/.git" => (AgentRepositoryMetadata, Directory),
        ".agents/.gitignore"
        | ".agents/README.zh-CN.md"
        | ".agents/SKILL-LEARNING-GUIDE.zh-CN.md"
        | ".agents/SKILL-USAGE-GUIDE.zh-CN.md" => (AgentRepositoryMetadata, File),
        ".agents/.DS_Store" | ".local/state/.DS_Store" => (SystemMetadata, File),
        ".agents/bin/agentflow-sync-skills" => (IndependentAgentEntry, File),
        ".agents/cache/upstream/cursor-plugins" => (LegacyGovernanceHistory, Directory),
        ".agents/out/M29-20261020-001-REPORT.txt"
        | ".agents/out/M29-20261020-001-apply.log"
        | ".agents/out/SKILLCTL-20261020-apply.log"
        | ".agents/out/SKILLCTL-20261020-check.txt"
        | ".agents/out/grok-bot-skills-M29-20261020-001.tar.gz"
        | ".agents/out/grok-bot-skills-SKILLSYNC-20261022-04.tar.gz"
        | ".agents/out/grok-bot-skills-SKILLSYNC-20261022-06.tar.gz"
        | ".agents/out/grok-bot-skills-SKILLSYNC-20261022-08.tar.gz"
        | ".agents/out/grok-bot-skills-SKILLSYNC-20261022-09.tar.gz"
        | ".agents/out/grok-bot-skills-SKILLSYNC-20261022-12.tar.gz"
        | ".agents/out/grok-bot-skills-SKILLSYNC-20261025-01.tar.gz"
        | ".agents/bin/tests/test_skillctl.py"
        | ".agents/bin/tests/__pycache__/test_skillctl.cpython-314.pyc"
        | ".agents/bin/__pycache__/skillctlcpython-314.pyc" => (LegacyGovernanceHistory, File),
        ".agents/locks" => (EmptyLegacyLockContainer, EmptyDirectory),
        ".claude" | ".codex" | ".cursor" | ".kimi" | ".mimocode" | ".opencode" | ".openclaw" => {
            (Harness, Historical)
        }
        name if !name.contains('/') && name.starts_with(".claude-") => (Harness, Historical),
        _ if native_state_roots(p).any(|root| root.as_os_str() == path.as_os_str()) => {
            (NativeState, RetainedShape::NativeState)
        }
        _ => return None,
    };
    Some(rule)
}

pub fn retained_path(p: &Locations, owner: RetainKind, path: &Path) -> bool {
    // Path equality preserves the spelling accepted by the original v1 reader.
    crate::policy::paths::lexical(path).is_ok_and(|normalized| {
        retained_rule(p, &normalized) == Some((owner, RetainedShape::Historical))
    })
}

fn native_link_target(p: &Locations, text: &Path) -> Option<PathBuf> {
    let target = native_link_input(p, text)?;
    crate::policy::paths::normalize_link(&target).ok()
}

pub fn native_link_input(p: &Locations, text: &Path) -> Option<PathBuf> {
    let text = text.to_str()?;
    if text.is_empty() || text.as_bytes().contains(&0) {
        return None;
    }
    Some(native_state_parent(p).join(text))
}

pub fn link_protected_roots(p: &Locations, roots: [&Path; 4]) -> impl Iterator<Item = PathBuf> {
    legacy_control_roots(p)
        .chain(default_roots(p))
        .chain(roots.map(Path::to_path_buf))
}

pub fn retained_entry_valid(p: &Locations, roots: [&Path; 4], retained: &Retained) -> bool {
    use RetainedEntry::{Directory, EmptyDirectory, File, Symlink};
    let Some((owner, shape)) = retained_rule(p, &retained.path) else {
        return false;
    };
    if owner != retained.owner {
        return false;
    }
    if roots.into_iter().chain([p.settings.as_path()]).any(|root| {
        overlaps(root, &retained.path)
            && !(owner == RetainKind::NativeShare
                && root != retained.path
                && root.starts_with(&retained.path))
    }) {
        return false;
    }
    match (shape, retained.entry.as_ref()) {
        (RetainedShape::Historical, None)
        | (RetainedShape::Directory | RetainedShape::NativeState, Some(Directory))
        | (RetainedShape::File, Some(File))
        | (RetainedShape::EmptyDirectory, Some(EmptyDirectory)) => true,
        (RetainedShape::NativeState, Some(Symlink { text })) => native_link_target(p, text)
            .is_some_and(|target| {
                link_protected_roots(p, roots).all(|protected| !overlaps(&protected, &target))
            }),
        _ => false,
    }
}

pub fn validate(p: &Locations, l: &CommittedMigration) -> Result<()> {
    locations_valid(p)?;
    for root in [
        &l.layout.config,
        &l.layout.data,
        &l.layout.state,
        &l.layout.cache,
    ] {
        absolute(root)?;
    }
    if !matches!(l.schema_version, LEGACY_LEDGER_VERSION | LEDGER_VERSION)
        || !crate::policy::state::valid_id(&l.migration_id)
        || l.migration_id.len() > 128
        || l.home != p.home
        || l.legacy_settings != p.settings
        || l.layout.config != p.config
        || l.layout.data != p.data
        || l.layout.state != p.state
        || !separated(&l.layout.roots())
        || (l.schema_version == LEDGER_VERSION
            && (!root_scope_valid(p, l.layout.roots()) || !root_scope_valid(p, p.roots())))
    {
        return Err(invalid());
    }
    binding_valid(&l.root_bindings.config, &l.layout.config, true)?;
    binding_valid(&l.root_bindings.data, &l.layout.data, true)?;
    binding_valid(&l.root_bindings.state, &l.layout.state, true)?;
    if let Some(cache) = &l.cache_audit {
        binding_valid(cache, &l.layout.cache, false)?;
    }
    transfers_valid(p, l)?;
    links_valid(p, l)?;
    archives_valid(p, l)?;
    if l.retained.len() > 10_000 {
        return Err(invalid());
    }
    let mut paths = BTreeSet::new();
    for retained in &l.retained {
        absolute(&retained.path)?;
        let valid = if l.schema_version == LEGACY_LEDGER_VERSION {
            retained.entry.is_none() && retained_path(p, retained.owner, &retained.path)
        } else {
            retained_entry_valid(p, l.layout.roots(), retained)
                && retained_entry_valid(p, p.roots(), retained)
        };
        if !paths.insert(&retained.path) || !valid {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn parse(p: &Locations, bytes: &[u8]) -> Result<CommittedMigration> {
    let value = crate::policy::json::parse(bytes).map_err(|_| invalid())?;
    let crate::policy::json::Object(ledger): crate::policy::json::Object<CommittedMigration> =
        serde_json::from_value(value).map_err(|_| invalid())?;
    validate(p, &ledger).map_err(|_| invalid())?;
    Ok(ledger)
}

impl AuthorityPaths {
    pub fn dataset(&self, dataset: Dataset) -> PathBuf {
        match dataset {
            Dataset::ManagedAssets => self.managed.clone(),
            Dataset::InvocationOverlays => self.overlays.clone(),
            Dataset::ManagedUpdates => self.updates.clone(),
            Dataset::SkillProfiles => self.profiles.clone(),
            _ => self.manifests.join(dataset.filename()),
        }
    }
}

pub fn cache_allowed(locations: &Locations, root: &Path) -> bool {
    [&locations.config, &locations.data, &locations.state]
        .iter()
        .all(|durable| !root.starts_with(durable) && !durable.starts_with(root))
}

pub fn select(p: &Locations, ledger: Option<&CommittedMigration>) -> Result<AuthorityPaths> {
    locations_valid(p)?;
    if let Some(ledger) = ledger {
        validate(p, ledger)?;
        let manifests = p.config.join("manifests");
        return Ok(AuthorityPaths {
            authority: Authority::Migrated {
                migration_id: ledger.migration_id.clone(),
            },
            profiles: manifests.join("skill-profiles.json"),
            manifests,
            managed: p.state.join("managed-assets.json"),
            overlays: p.state.join("invocation-overlays.json"),
            pending: p.state.join("invocation-policy.pending.json"),
            backups: p.state.join("backups"),
            updates: p.cache.join("managed-updates.json"),
            required: ledger
                .converted
                .iter()
                .filter_map(|(key, value)| {
                    if *key != Dataset::ManagedUpdates && matches!(value, Transfer::Copied { .. }) {
                        Some(*key)
                    } else {
                        None
                    }
                })
                .collect(),
        });
    }
    let manifests = p.settings.join("manifests");
    Ok(AuthorityPaths {
        authority: Authority::Legacy,
        profiles: manifests.join("skill-profiles.json"),
        manifests,
        managed: p.home.join(".agents/managed-assets.json"),
        overlays: p.home.join(".agents/invocation-overlays.json"),
        pending: p.home.join(".agents/invocation-policy.pending.json"),
        backups: p.home.join(".agents/backups"),
        updates: p.home.join(".agents/cache/managed-updates.json"),
        required: BTreeSet::new(),
    })
}

#[cfg(test)]
#[path = "../../tests/unit/governance_policy.rs"]
mod tests;
