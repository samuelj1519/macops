use crate::{
    capability::{
        capability_store as files,
        scoped_files::{private, regular, scope_changed as invalid, write_cas_checked},
    },
    context::Context,
    domain::fingerprint,
    error::{Error, Result},
    operations::store::Store,
    platform::fs::{Directory, EntryKind},
    policy::{
        capability_tx::{Namespace, State, Witness},
        native_skills::{self as policy, Recipe},
        source_tree::{Slot, Tree},
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Object {
    Entry {
        path: PathBuf,
        namespace: Namespace,
        state: State,
    },
    Directory {
        path: PathBuf,
        slot: Slot,
        tree: Tree,
    },
}
impl Object {
    pub fn path(&self) -> &Path {
        match self {
            Self::Entry { path, .. } | Self::Directory { path, .. } => path,
        }
    }
    pub fn verify(&self) -> Result<()> {
        if capture(self.path())? != *self {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn present(&self) -> bool {
        !matches!(
            self,
            Self::Entry {
                state: State::Missing,
                ..
            }
        )
    }
}
pub fn capture(path: &Path) -> Result<Object> {
    let namespace = files::namespace(path)?;
    if let Some(parent) = files::parent(&namespace)? {
        let name = path.file_name().ok_or_else(invalid)?;
        if parent
            .metadata(name)?
            .is_some_and(|m| m.kind == EntryKind::Directory)
        {
            let slot = Slot {
                parent: files::binding(&parent, &parent.path)?,
                name: name.to_str().ok_or_else(invalid)?.into(),
            };
            let tree = crate::source::source_tree::observe(&slot)?.ok_or_else(invalid)?;
            return Ok(Object::Directory {
                path: path.into(),
                slot,
                tree,
            });
        }
    }
    let state = files::observe(&namespace, 16 * 1024 * 1024)?.0;
    Ok(Object::Entry {
        path: path.into(),
        namespace,
        state,
    })
}
pub fn witness(path: &Path) -> Result<(Witness, Option<Vec<u8>>)> {
    let namespace = files::namespace(path)?;
    let (state, raw) = files::observe(&namespace, 16 * 1024 * 1024)?;
    if matches!(state, State::Link { .. }) {
        return Err(invalid());
    }
    if let Some(parent) = files::parent(&namespace)? {
        let leaf = path.file_name().ok_or_else(invalid)?;
        if parent.metadata(leaf)?.is_some() {
            let uid =
                rustix::fs::statat(parent.file(), leaf, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                    .map_err(std::io::Error::from)?
                    .st_uid;
            if uid != nix::unistd::geteuid().as_raw() {
                return Err(invalid());
            }
        }
    }
    Ok((
        Witness {
            path: path.into(),
            namespace,
            state,
        },
        raw,
    ))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
    pub before: Witness,
    pub after: Vec<u8>,
    pub pair: Option<crate::policy::source_files::Pair>,
}
impl Commit {
    pub fn prepare(before: Witness, after: Vec<u8>) -> Result<Self> {
        let (state, raw) = files::observe(&before.namespace, 16 * 1024 * 1024)?;
        if state != before.state {
            return Err(invalid());
        }
        let mode = match &state {
            State::File { mode, .. } => *mode,
            State::Missing => 0o600,
            State::Link { .. } => return Err(invalid()),
        };
        let pair = if raw.as_deref() == Some(after.as_slice()) {
            None
        } else {
            Some(crate::source::source_files::stage(&before, &after, mode)?)
        };
        Ok(Self {
            before,
            after,
            pair,
        })
    }
    pub fn verify(&self, target: &Path, expected: &[u8]) -> Result<()> {
        if self.before.path != target
            || files::target_path(&self.before.namespace) != target
            || self.after != expected
        {
            return Err(invalid());
        }
        if let Some(pair) = &self.pair {
            if pair.live.path() != target
                || pair
                    .before
                    .as_ref()
                    .map_or(State::Missing, crate::policy::source_files::Object::state)
                    != self.before.state
                || pair.after.sha256 != files::hash(expected)
                || pair.after.size != expected.len() as u64
                || pair.retained.parent.requested.parent() != target.parent()
            {
                return Err(invalid());
            }
            pair.validate(nix::unistd::geteuid().as_raw())?;
            if crate::source::source_files::position(pair)?
                == crate::policy::source_tree::Position::Unknown
            {
                return Err(invalid());
            }
        } else if files::observe(&self.before.namespace, 16 * 1024 * 1024)?.0 != self.before.state
            || regular(target)?.as_deref() != Some(expected)
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn publish(
        &self,
        target: &Path,
        expected: &[u8],
        guard: impl Fn() -> Result<()>,
    ) -> Result<()> {
        self.verify(target, expected)?;
        guard()?;
        if let Some(pair) = &self.pair {
            crate::source::source_files::publish_checked(pair, || {
                guard()?;
                self.verify(target, expected)
            })?;
        }
        self.verify(target, expected)?;
        guard()
    }
}
pub fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = private(path.parent().ok_or_else(invalid)?)?;
    let namespace = files::namespace(path)?;
    let (state, _) = files::observe(&namespace, 16 * 1024 * 1024)?;
    if state != State::Missing {
        return Err(Error::attention(
            "already_exists",
            "不可覆盖已存在的不可变配方或操作意图",
        ));
    }
    let pair = crate::source::source_files::stage(
        &Witness {
            path: path.into(),
            namespace,
            state,
        },
        &encode(value)?,
        0o600,
    )?;
    crate::source::source_files::publish_checked(&pair, || parent.verify())?;
    let retained = Directory::open(&pair.retained.parent.requested)?;
    if retained.identity() != &pair.retained.parent.identity
        || !crate::capability::capability_guard::listing(&retained)?.is_empty()
    {
        return Err(invalid());
    }
    let leaf = pair
        .retained
        .parent
        .requested
        .file_name()
        .ok_or_else(invalid)?;
    if parent.metadata(leaf)?.is_none_or(|m| {
        m.identity != pair.retained.parent.identity || m.kind != EntryKind::Directory
    }) {
        return Err(invalid());
    }
    rustix::fs::unlinkat(parent.file(), leaf, rustix::fs::AtFlags::REMOVEDIR)
        .map_err(std::io::Error::from)?;
    parent.file().sync_all()?;
    parent.verify()
}
pub fn write_cas(path: &Path, before: Option<&[u8]>, after: &[u8]) -> Result<()> {
    write_cas_checked(path, before, after, || Ok(()))
}

pub fn lock_path(ctx: &Context) -> PathBuf {
    crate::policy::delivery::native_lock_path(&ctx.home, &ctx.scope)
}
pub fn payload(ctx: &Context, name: &str) -> Result<PathBuf> {
    let p = ctx.home.join(".agents/skills").join(name);
    crate::platform::fs::write_path(&p)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub lock: Witness,
    pub rows: BTreeMap<String, Value>,
    pub selected: Object,
    pub others: Vec<Object>,
    pub central: BTreeSet<String>,
    pub inputs: Vec<Witness>,
    pub clients: BTreeSet<PathBuf>,
    pub client_digest: String,
}
fn children(path: &Path, selected: &str) -> Result<BTreeSet<String>> {
    match Directory::open(path) {
        Ok(root) => Ok(crate::capability::capability_guard::listing(&root)?
            .into_keys()
            .filter(|s| s != selected)
            .collect()),
        Err(error) if error.code == "path_missing" => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}
fn client_paths(ctx: &Context, name: &str) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    let home = Directory::open(&ctx.home)?;
    for leaf in crate::capability::capability_guard::listing(&home)?.keys() {
        if leaf.starts_with(".claude-") {
            paths.insert(ctx.home.join(leaf).join("skills").join(name));
        }
    }
    for leaf in [
        ".claude",
        ".claude-mi",
        ".claude-kimi",
        ".claude-glm",
        ".claude-zen",
        ".cursor",
        ".codex",
    ] {
        paths.insert(ctx.home.join(leaf).join("skills").join(name));
    }
    for resource in crate::agent::resource_agents::profile_roots(ctx)? {
        if let crate::domain::resource::Data::Agent { config_dir, .. } = resource.data {
            paths.insert(config_dir.join("skills").join(name));
        }
    }
    paths.remove(&ctx.home.join(".agents/skills").join(name));
    Ok(paths)
}
fn client_digest(ctx: &Context, name: &str) -> Result<String> {
    let central = crate::platform::fs::read_path(&ctx.home.join(".agents/skills"))?;
    let mut evidence = crate::agent::ai_payload::Evidence::default();
    let mut slots = vec![];
    let roots = client_paths(ctx, name)?
        .into_iter()
        .map(|p| p.parent().map(Path::to_path_buf).ok_or_else(invalid))
        .collect::<Result<BTreeSet<_>>>()?;
    for path in roots {
        let namespace = files::namespace(&path)?;
        let metadata = files::parent(&namespace)?
            .map(|p| p.metadata(path.file_name().ok_or_else(invalid)?))
            .transpose()?
            .flatten();
        match metadata.map(|m| m.kind) {
            Some(EntryKind::Directory) => evidence.tree(&path)?,
            Some(EntryKind::Symlink) => {
                slots.push(capture(&path)?);
                if crate::platform::fs::read_path(&path)? != central {
                    evidence.tree(&path)?;
                }
            }
            None => slots.push(capture(&path)?),
            _ => return Err(invalid()),
        }
    }
    evidence.verify()?;
    fingerprint(&(slots, evidence.digest()?))
}
pub fn local_scope(ctx: &Context, name: &str, reserved: &[PathBuf]) -> Result<()> {
    let mut roots = client_paths(ctx, name)?
        .into_iter()
        .map(|p| p.parent().map(Path::to_path_buf).ok_or_else(invalid))
        .collect::<Result<BTreeSet<_>>>()?;
    roots.insert(crate::platform::fs::read_path(
        &ctx.home.join(".agents/skills"),
    )?);
    for root in roots {
        crate::policy::source_baseline::disjoint(
            &crate::platform::fs::read_path(&root)?,
            reserved,
        )?;
    }
    Ok(())
}
pub fn snapshot(ctx: &Context, recipe: &Recipe) -> Result<Snapshot> {
    policy::validate(recipe)?;
    let lock = lock_path(ctx);
    crate::platform::fs::write_path(&lock)?;
    let (lock_witness, raw) = witness(&lock)?;
    let rows = policy::registry(raw.as_deref())?;
    let selected = capture(&payload(ctx, &recipe.install_name)?)?;
    let clients = client_paths(ctx, &recipe.install_name)?;
    let client_digest = client_digest(ctx, &recipe.install_name)?;
    let mut paths = clients.clone();
    for name in rows.keys().filter(|name| **name != recipe.install_name) {
        paths.insert(payload(ctx, name)?);
    }
    let mut others = vec![];
    let mut entries = 0usize;
    let mut bytes = 0u64;
    for path in paths {
        let object = capture(&path)?;
        if let Object::Directory { tree, .. } = &object {
            entries += tree.entries.len();
            for entry in tree.entries.values() {
                if let crate::policy::source_tree::Content::File { size, .. } = entry.content {
                    bytes = bytes.checked_add(size).ok_or_else(invalid)?;
                }
            }
        }
        if entries > 50000 || bytes > 128 * 1024 * 1024 {
            return Err(invalid());
        }
        others.push(object);
    }
    let central = children(&ctx.home.join(".agents/skills"), &recipe.install_name)?;
    let authority = crate::operations::governance::capture(ctx)?;
    authority.ensure_settled()?;
    let mut inputs = vec![];
    for path in [
        authority
            .paths
            .dataset(crate::policy::governance::Dataset::SkillProfiles),
        authority
            .paths
            .dataset(crate::policy::governance::Dataset::SourceChannels),
        ctx.state.join("migration-ledger.json"),
    ] {
        let namespace = files::namespace(&path)?;
        let (state, _) = files::observe(&namespace, 16 * 1024 * 1024)?;
        inputs.push(Witness {
            path,
            namespace,
            state,
        });
    }
    let snapshot = Snapshot {
        lock: lock_witness,
        rows,
        selected,
        others,
        central,
        inputs,
        clients,
        client_digest,
    };
    snapshot.before(ctx, recipe)?;
    Ok(snapshot)
}
impl Snapshot {
    pub fn before(&self, ctx: &Context, recipe: &Recipe) -> Result<()> {
        if files::observe(&self.lock.namespace, 16 * 1024 * 1024)?.0 != self.lock.state {
            return Err(invalid());
        }
        self.selected.verify()?;
        self.protected(ctx, recipe)
    }
    pub fn protected(&self, ctx: &Context, recipe: &Recipe) -> Result<()> {
        if client_paths(ctx, &recipe.install_name)? != self.clients {
            return Err(invalid());
        }
        if client_digest(ctx, &recipe.install_name)? != self.client_digest {
            return Err(invalid());
        }
        for witness in &self.inputs {
            if files::observe(&witness.namespace, 16 * 1024 * 1024)?.0 != witness.state {
                return Err(invalid());
            }
        }
        for object in &self.others {
            object.verify()?;
        }
        if children(&ctx.home.join(".agents/skills"), &recipe.install_name)? != self.central {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn after(
        &self,
        ctx: &Context,
        recipe: &Recipe,
        local: Option<&Path>,
        remove: bool,
    ) -> Result<BTreeMap<String, Value>> {
        self.protected(ctx, recipe)?;
        let rows = policy::registry(regular(&self.lock.path)?.as_deref())?;
        let mut unselected_before = self.rows.clone();
        let mut unselected_after = rows.clone();
        unselected_before.remove(&recipe.install_name);
        unselected_after.remove(&recipe.install_name);
        if unselected_before != unselected_after {
            return Err(invalid());
        }
        let current = capture(&payload(ctx, &recipe.install_name)?)?;
        if remove {
            if rows.contains_key(&recipe.install_name) || current.present() {
                return Err(invalid());
            }
        } else {
            policy::selected_after(
                recipe,
                rows.get(&recipe.install_name).ok_or_else(invalid)?,
                local,
            )?;
            if !matches!(current, Object::Directory { .. })
                || regular(&payload(ctx, &recipe.install_name)?.join("SKILL.md"))?
                    .is_none_or(|b| b.is_empty())
            {
                return Err(invalid());
            }
        }
        Ok(rows)
    }
    pub fn remove_unshared(&self, ctx: &Context, recipe: &Recipe) -> Result<()> {
        let selected = payload(ctx, &recipe.install_name)?;
        if self
            .others
            .iter()
            .any(|o| o.path().file_name() == selected.file_name() && o.present())
        {
            return Err(Error::attention(
                "native_skills_shared_installation",
                "其他客户端仍引用或保留该技能；先由对应 owner 解除引用，不能把 profile unlink 当中央卸载",
            ));
        }
        Ok(())
    }
}
pub fn runner_stamp(recipe: &Recipe) -> Result<String> {
    let package = recipe
        .runner
        .entry
        .parent()
        .and_then(Path::parent)
        .ok_or_else(invalid)?;
    let root = package.parent().ok_or_else(invalid)?;
    let value: Value = policy::decode(&crate::platform::fs::regular_bytes(
        &package.join("package.json"),
    )?)?;
    if value["name"] != "skills" || value["version"] != "1.9.0" {
        return Err(invalid());
    }
    let mut evidence = crate::agent::ai_payload::Evidence::default();
    evidence.file(&recipe.runner.node)?;
    evidence.tree(root)?;
    evidence.verify()?;
    evidence.digest()
}
pub fn frozen_environment(ctx: &Context) -> Result<BTreeMap<String, String>> {
    let empty = ctx.state.join("native-skills-empty-gitconfig");
    match regular(&empty)? {
        Some(bytes) if bytes.is_empty() => (),
        Some(_) => return Err(invalid()),
        None => {
            let root = private(&ctx.state)?;
            Store::atomic_bytes(root.file(), "native-skills-empty-gitconfig", b"", true)?;
        }
    }
    let temp = private(&ctx.cache.join("native-skills/tmp"))?;
    let mut env = BTreeMap::from([
        ("HOME".into(), ctx.home.display().to_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("DO_NOT_TRACK".into(), "1".into()),
        ("DISABLE_TELEMETRY".into(), "1".into()),
        ("CI".into(), "1".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), empty.display().to_string()),
        ("TMPDIR".into(), temp.path.display().to_string()),
    ]);
    for (key, path) in &ctx.scope.paths {
        if let Some(path) = path
            && key.starts_with("XDG_")
        {
            env.insert(key.clone(), path.display().to_string());
        }
    }
    Ok(env)
}
pub fn encode(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(value)?;
    b.push(b'\n');
    Ok(b)
}

pub fn same<T: Serialize>(left: &T, right: &T) -> Result<()> {
    if fingerprint(left)? != fingerprint(right)? {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/native_skills_files.rs"]
mod tests;
