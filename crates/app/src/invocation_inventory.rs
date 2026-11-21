use crate::{
    context::Context,
    error::{Error, Result},
    governance::Governance,
    paths::{Bindings, BoundTarget, Directory, EntryKind, ObservedRegular},
    policy::{
        governance::Dataset,
        invocation::{self, Declaration, MetadataAccess, MetadataKind, OverlayRecord, OverlayRow},
    },
};
use serde_json::{Value, json};
use std::os::unix::ffi::OsStringExt;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::{Path, PathBuf},
};

const FILE_LIMIT: u64 = 1024 * 1024;
const BYTE_LIMIT: u64 = 64 * 1024 * 1024;
const ENTRY_LIMIT: usize = 5000;

struct FileSnapshot {
    bound: BoundTarget,
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
    identity: Option<crate::domain::FileId>,
}

type DirectoryEntries = BTreeMap<std::ffi::OsString, (crate::domain::FileId, EntryKind, u32)>;
struct DirectorySnapshot {
    root: Directory,
    lineage: Vec<crate::domain::FileId>,
    entries: DirectoryEntries,
}

fn directory_listing(root: &Directory) -> Result<DirectoryEntries> {
    let mut entries = BTreeMap::new();
    let mut seen = 0;
    for entry in root.names()? {
        seen += 1;
        if seen > ENTRY_LIMIT {
            return Err(Error::attention(
                "invocation_budget_limit",
                "目录扫描超过限制",
            ));
        }
        let entry = entry.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if matches!(entry.file_name().to_bytes(), b"." | b"..") {
            continue;
        }
        if let Some(meta) = root.metadata(&name)? {
            entries.insert(name, (meta.identity, meta.kind, meta.mode));
        }
    }
    root.verify()?;
    Ok(entries)
}

pub struct Reader {
    bindings: Bindings,
    files: BTreeMap<PathBuf, FileSnapshot>,
    roots: Vec<DirectorySnapshot>,
    missing_directories: Vec<(PathBuf, BoundTarget)>,
    budget: u64,
}

impl Reader {
    fn new() -> Self {
        Self {
            bindings: Bindings::new(512),
            files: BTreeMap::new(),
            roots: vec![],
            missing_directories: vec![],
            budget: BYTE_LIMIT,
        }
    }
    fn file(&mut self, path: &Path) -> Result<&FileSnapshot> {
        self.file_with_limit(path, FILE_LIMIT)
    }
    fn file_with_limit(&mut self, path: &Path, limit: u64) -> Result<&FileSnapshot> {
        if !self.files.contains_key(path) {
            if self.files.len() >= ENTRY_LIMIT {
                return Err(Error::attention(
                    "invocation_budget_limit",
                    "元数据条目超过限制",
                ));
            }
            let bound = self.bindings.target(path)?;
            let (bytes, mode, identity) = match bound.observe_regular(self.budget.min(limit))? {
                ObservedRegular::Missing => (None, None, None),
                ObservedRegular::NonRegular(metadata) => {
                    return Err(Error::attention(
                        "unsupported_invocation_file",
                        format!("元数据必须为普通文件，实际类型：{}", metadata.kind.as_str()),
                    ));
                }
                ObservedRegular::Regular { bytes, metadata } => {
                    self.budget = self.budget.saturating_sub(bytes.len() as u64);
                    (Some(bytes), Some(metadata.mode), Some(metadata.identity))
                }
            };
            self.files.insert(
                path.into(),
                FileSnapshot {
                    bound,
                    bytes,
                    mode,
                    identity,
                },
            );
        }
        self.files
            .get(path)
            .ok_or_else(|| Error::new("metadata_snapshot_missing", "元数据快照缺失"))
    }
    fn directory(&mut self, path: &Path) -> Result<Option<Directory>> {
        let probe = self
            .bindings
            .target(&path.join(".macops-directory-probe"))?;
        match self.bindings.root(path) {
            Ok(root) => {
                self.roots.push(DirectorySnapshot {
                    lineage: root.lineage()?,
                    entries: directory_listing(&root)?,
                    root: root.clone(),
                });
                Ok(Some(root))
            }
            Err(error) if error.code == "path_missing" => {
                self.missing_directories.push((path.into(), probe));
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
    pub fn witnesses(
        &self,
    ) -> Result<(
        Vec<crate::policy::capability_tx::Witness>,
        Vec<crate::policy::capability_tx::DirectoryWitness>,
    )> {
        use crate::{
            capability_store as store,
            policy::capability_tx::{DirectoryEntry, DirectoryWitness, State, Witness},
        };
        self.verify()?;
        let mut files = vec![];
        for path in self.files.keys() {
            let namespace = store::namespace(path)?;
            files.push(Witness {
                path: path.clone(),
                state: store::observe(&namespace, BYTE_LIMIT)?.0,
                namespace,
            });
        }
        for (path, _) in &self.missing_directories {
            files.push(Witness {
                path: path.clone(),
                state: State::Missing,
                namespace: store::namespace(path)?,
            });
        }
        let mut directories = vec![];
        for snapshot in &self.roots {
            let entries = snapshot
                .entries
                .iter()
                .map(|(name, (identity, kind, mode))| {
                    let name = name.to_str().ok_or_else(stale)?.to_owned();
                    Ok((
                        name,
                        DirectoryEntry {
                            kind: kind.as_str().into(),
                            identity: identity.clone(),
                            mode: *mode,
                        },
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?;
            directories.push(DirectoryWitness {
                binding: store::binding(&snapshot.root, &snapshot.root.path)?,
                entries,
            });
        }
        self.verify()?;
        Ok((files, directories))
    }
    pub fn verify(&self) -> Result<()> {
        for (_, missing) in &self.missing_directories {
            missing.verify()?;
        }
        for snapshot in &self.roots {
            snapshot.root.verify()?;
            if snapshot.root.lineage()? != snapshot.lineage
                || directory_listing(&snapshot.root)? != snapshot.entries
            {
                return Err(stale());
            }
        }
        for file in self.files.values() {
            match file.bound.observe_regular(
                file.bytes
                    .as_ref()
                    .map_or(FILE_LIMIT, |bytes| FILE_LIMIT.max(bytes.len() as u64)),
            )? {
                ObservedRegular::Missing if file.bytes.is_none() => (),
                ObservedRegular::Regular { bytes, metadata }
                    if file.bytes.as_deref() == Some(bytes.as_slice())
                        && file.mode == Some(metadata.mode)
                        && file.identity.as_ref() == Some(&metadata.identity) => {}
                _ => return Err(stale()),
            }
        }
        Ok(())
    }
}

fn stale() -> Error {
    Error::attention(
        "invocation_plan_stale",
        "调用策略来源、目录或目标在读取后改变",
    )
}

#[derive(Clone)]
pub struct Target {
    pub path: PathBuf,
    pub kind: MetadataKind,
    pub label: String,
    pub owner: String,
}
pub struct Patch {
    pub target: Target,
    pub before: Option<Vec<u8>>,
    pub before_mode: Option<u32>,
    pub after: Option<Vec<u8>>,
    pub after_mode: Option<u32>,
}
pub struct Assessment {
    pub reader: Reader,
    pub patches: Vec<Patch>,
    pub record_before: Option<Vec<u8>>,
    pub record_after: Option<Vec<u8>>,
    pub missing: Vec<String>,
    pub rows: Vec<Value>,
    pub configured: bool,
}

impl Assessment {
    pub fn verify(&self) -> Result<()> {
        self.reader.verify()
    }
    pub fn summary(&self) -> Value {
        let changed = !self.patches.is_empty() || self.record_before != self.record_after;
        let incomplete = changed || !self.missing.is_empty();
        json!({"backend":"rust","status":if !self.configured{"not_configured"}else if incomplete{"drift"}else{"same"},
            "results":self.rows,"missing":self.missing,"incomplete":incomplete,"targets_assessed":true,
            "changes":self.patches.iter().map(|patch|json!({"path":patch.target.path,"kind":patch.target.kind,
                "before_exists":patch.before.is_some(),"after_exists":patch.after.is_some(),
                "content_changed":patch.before!=patch.after,"before_mode":patch.before_mode,"after_mode":patch.after_mode})).collect::<Vec<_>>(),
            "planned_changes":self.patches.len(),"record_changed":self.record_before!=self.record_after,"persistentWrites":false})
    }
}

fn register(ctx: &Context, targets: &mut BTreeMap<PathBuf, Target>, target: Target) -> Result<()> {
    if invocation::metadata_access(&ctx.home, &target.path, target.kind)? != MetadataAccess::Managed
    {
        return Err(Error::attention(
            "read_only_invocation_target",
            "应用自有能力缓存仅允许读取",
        ));
    }
    if let Some(old) = targets.get(&target.path)
        && (old.kind != target.kind || old.owner != target.owner)
    {
        return Err(Error::attention(
            "invocation_owner_conflict",
            "同一元数据存在多个治理目标",
        ));
    }
    targets.insert(target.path.clone(), target);
    Ok(())
}

fn upstream_manual(reader: &mut Reader, book: &OverlayRecord, path: &Path) -> Result<bool> {
    let current = reader.file(path)?;
    let Some(bytes) = current.bytes.as_deref() else {
        return Ok(false);
    };
    if let Some(row) = book.files.get(path)
        && invocation::hash(bytes) == row.applied_hash
        && current.mode == Some(row.applied_mode)
    {
        let (original, _) = row.bytes()?;
        return original
            .as_deref()
            .map(invocation::manual_frontmatter)
            .transpose()
            .map(|v| v.unwrap_or(false));
    }
    invocation::manual_frontmatter(bytes)
}

fn entries(root: &Directory) -> Result<Vec<(std::ffi::OsString, EntryKind)>> {
    let mut result = vec![];
    let mut seen = 0;
    for entry in root.names()? {
        seen += 1;
        if seen > ENTRY_LIMIT {
            return Err(Error::attention(
                "invocation_budget_limit",
                "目录扫描超过限制",
            ));
        }
        let entry = entry.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let name = std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name.to_str().is_some_and(|n| n.starts_with('.')) {
            continue;
        }
        if let Some(metadata) = root.metadata(&name)? {
            result.push((name, metadata.kind));
        }
    }
    root.verify()?;
    Ok(result)
}

struct FixedRecipes {
    root: PathBuf,
    names: BTreeSet<String>,
}

impl FixedRecipes {
    fn read(ctx: &Context, reader: &mut Reader) -> Result<Self> {
        let book =
            reader.file_with_limit(&crate::native_skills::book_path(ctx), 16 * FILE_LIMIT)?;
        let names = crate::policy::native_skills::book(book.bytes.as_deref())?
            .recipes
            .into_iter()
            .map(|recipe| recipe.install_name)
            .collect();
        Ok(Self {
            root: ctx.home.join(".agents/skills"),
            names,
        })
    }

    fn contains(&self, name: &str) -> bool {
        self.names.contains(&name.to_ascii_lowercase())
    }

    fn owns(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut components = relative.components();
        let name = components.next().and_then(|part| part.as_os_str().to_str());
        name.is_some_and(|name| self.contains(name))
            && matches!(
                components.as_path().to_str(),
                Some("SKILL.md" | "agents/openai.yaml")
            )
    }
}

pub(crate) fn protect_fixed_recipe_files<'a>(
    ctx: &Context,
    mut paths: impl Iterator<Item = &'a Path>,
) -> Result<()> {
    let mut reader = Reader::new();
    let fixed = FixedRecipes::read(ctx, &mut reader)?;
    if paths.any(|path| fixed.owns(path)) {
        return Err(Error::attention(
            "fixed_skill_recipe_conflict",
            "旧策略批次包含固定技能配方的元数据，已保留当前记录和备份",
        ));
    }
    reader.verify()
}

fn standalone(
    ctx: &Context,
    reader: &mut Reader,
    decl: &Declaration,
    book: &OverlayRecord,
    fixed: &FixedRecipes,
    targets: &mut BTreeMap<PathBuf, Target>,
    missing: &mut Vec<String>,
) -> Result<()> {
    let base = ctx.home.join(".agents/skills");
    let mut wanted: BTreeSet<_> = decl.standalone_manual_only.iter().cloned().collect();
    if decl.mirror_standalone_upstream_manual
        && let Some(root) = reader.directory(&base)?
    {
        for (name, kind) in entries(&root)? {
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.to_ascii_lowercase().starts_with("devnorm") || fixed.contains(name) {
                continue;
            }
            if kind == EntryKind::Directory
                && upstream_manual(reader, book, &base.join(name).join("SKILL.md"))?
            {
                wanted.insert(name.into());
            }
        }
    }
    for name in wanted.into_iter().filter(|name| !fixed.contains(name)) {
        let path = base.join(&name).join("SKILL.md");
        if reader.file(&path)?.bytes.is_none() {
            missing.push(name);
            continue;
        }
        let owner = book
            .files
            .get(&path)
            .map_or("local", |row| row.owner.as_str())
            .to_owned();
        register(
            ctx,
            targets,
            Target {
                path,
                kind: MetadataKind::Frontmatter,
                label: name.clone(),
                owner: owner.clone(),
            },
        )?;
        register(
            ctx,
            targets,
            Target {
                path: base.join(&name).join("agents/openai.yaml"),
                kind: MetadataKind::CodexPolicy,
                label: name,
                owner,
            },
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct PluginScope<'a> {
    root: &'a Path,
    plugin: &'a str,
    codex: bool,
}

fn plugin_targets(
    ctx: &Context,
    reader: &mut Reader,
    decl: &Declaration,
    book: &OverlayRecord,
    scope: PluginScope<'_>,
    targets: &mut BTreeMap<PathBuf, Target>,
) -> Result<()> {
    let PluginScope {
        root,
        plugin,
        codex,
    } = scope;
    let Some(rule) = decl.plugins.get(plugin) else {
        return Ok(());
    };
    for (subdir, commands) in [("skills", false), ("commands", true)] {
        let Some(folder) = reader.directory(&root.join(subdir))? else {
            continue;
        };
        for (name, kind) in entries(&folder)? {
            let Some(text) = name.to_str() else {
                continue;
            };
            if text.to_ascii_lowercase().starts_with("devnorm") {
                continue;
            }
            let (label, path) = if commands
                && kind == EntryKind::File
                && Path::new(text)
                    .extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("md"))
            {
                (
                    Path::new(text)
                        .file_stem()
                        .and_then(OsStr::to_str)
                        .ok_or_else(stale)?
                        .to_owned(),
                    root.join(subdir).join(text),
                )
            } else if !commands && kind == EntryKind::Directory {
                (text.into(), root.join(subdir).join(text).join("SKILL.md"))
            } else {
                continue;
            };
            let manual = rule.manual_only.contains(&label)
                || (rule.mirror_upstream_manual && upstream_manual(reader, book, &path)?);
            if !manual || reader.file(&path)?.bytes.is_none() {
                continue;
            }
            register(
                ctx,
                targets,
                Target {
                    path: path.clone(),
                    kind: MetadataKind::Frontmatter,
                    label: format!("{plugin}:{label}"),
                    owner: book
                        .files
                        .get(&path)
                        .map_or(plugin, |r| r.owner.as_str())
                        .into(),
                },
            )?;
            if codex && !commands {
                let yaml = path.parent().ok_or_else(stale)?.join("agents/openai.yaml");
                register(
                    ctx,
                    targets,
                    Target {
                        owner: book
                            .files
                            .get(&yaml)
                            .map_or(plugin, |r| r.owner.as_str())
                            .into(),
                        path: yaml,
                        kind: MetadataKind::CodexPolicy,
                        label: format!("{plugin}:{label}"),
                    },
                )?;
            }
        }
    }
    Ok(())
}

fn plugins(
    ctx: &Context,
    reader: &mut Reader,
    decl: &Declaration,
    book: &OverlayRecord,
    targets: &mut BTreeMap<PathBuf, Target>,
) -> Result<()> {
    if decl.plugins.is_empty() {
        return Ok(());
    }
    for profile in crate::resource_agents::profile_roots(ctx)? {
        let crate::resource::Data::Agent {
            client, config_dir, ..
        } = profile.data
        else {
            continue;
        };
        if client == "claude" {
            let snapshot = reader.file(&config_dir.join("plugins/installed_plugins.json"))?;
            let Some(bytes) = snapshot.bytes.as_deref() else {
                continue;
            };
            crate::policy::json::parse(bytes)?;
            let registry = crate::policy::resources::plugin_book(bytes)?;
            for (id, records) in registry.plugins {
                let (plugin, market) = id.rsplit_once('@').ok_or_else(stale)?;
                if !decl.plugins.contains_key(plugin) {
                    continue;
                }
                let expected = config_dir.join("plugins/cache").join(market).join(plugin);
                for record in records {
                    if record.scope != "user" || record.project.is_some() {
                        continue;
                    }
                    if record.path.parent() != Some(expected.as_path()) {
                        return Err(Error::attention(
                            "unsupported_plugin_root",
                            "插件路径不在已登记的缓存命名空间",
                        ));
                    }
                    plugin_targets(
                        ctx,
                        reader,
                        decl,
                        book,
                        PluginScope {
                            root: &record.path,
                            plugin,
                            codex: false,
                        },
                        targets,
                    )?;
                }
            }
        } else if client == "codex" {
            let snapshot = reader.file(&config_dir.join("config.toml"))?;
            let Some(bytes) = snapshot.bytes.as_deref() else {
                continue;
            };
            let registry = crate::policy::resources::codex_plugins(bytes)?;
            for (id, _) in registry {
                let (plugin, market) = id.rsplit_once('@').ok_or_else(stale)?;
                if !decl.plugins.contains_key(plugin) {
                    continue;
                }
                let base = config_dir.join("plugins/cache").join(market).join(plugin);
                let Some(root) = reader.directory(&base)? else {
                    continue;
                };
                for (version, kind) in entries(&root)? {
                    if kind == EntryKind::Directory {
                        plugin_targets(
                            ctx,
                            reader,
                            decl,
                            book,
                            PluginScope {
                                root: &base.join(version),
                                plugin,
                                codex: true,
                            },
                            targets,
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn patch(
    target: Target,
    current: &FileSnapshot,
    proposal: invocation::Proposal,
    patches: &mut Vec<Patch>,
    rows: &mut Vec<Value>,
    files: &mut BTreeMap<PathBuf, OverlayRow>,
) {
    if let Some(row) = proposal.row {
        files.insert(target.path.clone(), row);
    }
    rows.push(json!({"path":target.path,"status":if proposal.changed{"drift"}else{"same"},"access":"managed"}));
    if proposal.changed {
        patches.push(Patch {
            target,
            before: current.bytes.clone(),
            before_mode: current.mode,
            after: proposal.after,
            after_mode: proposal.after_mode,
        });
    }
}

struct Evaluated {
    patches: Vec<Patch>,
    rows: Vec<Value>,
    files: BTreeMap<PathBuf, OverlayRow>,
}

fn evaluate(
    ctx: &Context,
    reader: &mut Reader,
    book: &OverlayRecord,
    targets: &BTreeMap<PathBuf, Target>,
    fixed: &FixedRecipes,
) -> Result<Evaluated> {
    let mut patches = vec![];
    let mut rows = vec![];
    let mut files = BTreeMap::new();
    for target in targets.values().filter(|target| !fixed.owns(&target.path)) {
        let current = reader.file(&target.path)?;
        let proposal = invocation::propose(
            target.kind,
            &target.label,
            &target.owner,
            current.bytes.as_deref(),
            current.mode,
            book.files.get(&target.path),
        )?;
        patch(
            target.clone(),
            current,
            proposal,
            &mut patches,
            &mut rows,
            &mut files,
        );
    }
    for (path, row) in &book.files {
        if fixed.owns(path) {
            files.insert(path.clone(), row.clone());
            rows.push(
                json!({"path":path,"status":"read_only","access":"read_only","owner":"skills-cli"}),
            );
            continue;
        }
        if targets.contains_key(path) {
            continue;
        }
        if invocation::metadata_access(&ctx.home, path, row.kind)? == MetadataAccess::ReadOnly {
            files.insert(path.clone(), row.clone());
            rows.push(json!({"path":path,"status":"read_only","access":"read_only"}));
            continue;
        }
        let current = reader.file(path)?;
        let proposal = invocation::retire(row, current.bytes.as_deref(), current.mode)?;
        patch(
            Target {
                path: path.clone(),
                kind: row.kind,
                label: row.label.clone(),
                owner: row.owner.clone(),
            },
            current,
            proposal,
            &mut patches,
            &mut rows,
            &mut files,
        );
    }
    Ok(Evaluated {
        patches,
        rows,
        files,
    })
}

pub fn assess(ctx: &Context, authority: &Governance) -> Result<Assessment> {
    let declaration_bytes = authority.read(Dataset::InvocationRules, FILE_LIMIT)?;
    let record_before = authority.read(Dataset::InvocationOverlays, 16 * FILE_LIMIT)?;
    let declaration = declaration_bytes
        .as_deref()
        .map(invocation::declaration)
        .transpose()?;
    let book = record_before
        .as_deref()
        .map(|b| invocation::record(&ctx.home, b))
        .transpose()?
        .unwrap_or(OverlayRecord {
            schema_version: 1,
            profile_policy_hash: None,
            profiles: BTreeMap::new(),
            policy_hash: None,
            files: BTreeMap::new(),
        });
    if declaration.is_none() && !book.files.is_empty() {
        return Err(Error::attention(
            "invocation_declaration_missing",
            "策略账本仍有条目但声明缺失",
        ));
    }
    let mut reader = Reader::new();
    let fixed = FixedRecipes::read(ctx, &mut reader)?;
    let mut targets = BTreeMap::new();
    let mut missing = vec![];
    for dataset in [Dataset::InvocationRules, Dataset::InvocationOverlays] {
        let expected = if dataset == Dataset::InvocationRules {
            declaration_bytes.as_deref()
        } else {
            record_before.as_deref()
        };
        if reader
            .file_with_limit(
                &authority.paths.dataset(dataset),
                if dataset == Dataset::InvocationOverlays {
                    16 * FILE_LIMIT
                } else {
                    FILE_LIMIT
                },
            )?
            .bytes
            .as_deref()
            != expected
        {
            return Err(stale());
        }
    }
    if let Some(decl) = &declaration {
        standalone(
            ctx,
            &mut reader,
            decl,
            &book,
            &fixed,
            &mut targets,
            &mut missing,
        )?;
        plugins(ctx, &mut reader, decl, &book, &mut targets)?;
    }
    let Evaluated {
        patches,
        rows,
        files,
    } = evaluate(ctx, &mut reader, &book, &targets, &fixed)?;
    let after = OverlayRecord {
        schema_version: book.schema_version,
        profile_policy_hash: book.profile_policy_hash.clone(),
        profiles: book.profiles.clone(),
        policy_hash: declaration_bytes.as_deref().map(invocation::hash),
        files,
    };
    let record_after = if after == book {
        record_before.clone()
    } else if record_before.is_none() && after.files.is_empty() && after.policy_hash.is_none() {
        None
    } else {
        let mut bytes = serde_json::to_vec_pretty(&after)?;
        bytes.push(b'\n');
        Some(bytes)
    };
    reader.verify()?;
    Ok(Assessment {
        reader,
        patches,
        record_before,
        record_after,
        missing,
        rows,
        configured: declaration.is_some(),
    })
}

#[cfg(test)]
#[path = "../tests/unit/invocation_inventory.rs"]
mod tests;
