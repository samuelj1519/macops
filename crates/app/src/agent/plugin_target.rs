use crate::{
    agent::resource_agents::InstallTarget,
    capability::{
        capability_store,
        rules::capability_tx::{Namespace, State, Witness},
    },
    context::Context,
    domain::{FileId, Journal, Phase, fingerprint},
    error::{Error, Result},
    operations::store::Store,
    platform::fs::Directory,
    policy::source_tree::{Position, Slot, Tree},
    source::rules::source_files::{Object, Pair},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const TAG: &str = "plugin-target-v1:";
const LOCK_FILE: &str = ".macops-plugin.lock";

fn conflict() -> Error {
    Error::attention(
        "plugin_target_changed",
        "所选档案的插件、设置或文件位置已变化。请检查该档案后再继续。",
    )
}

fn io(error: rustix::io::Errno) -> Error {
    std::io::Error::from_raw_os_error(error.raw_os_error()).into()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transfer {
    source: Slot,
    target: Slot,
    tree: TreeWitness,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct TreeWitness {
    identity: FileId,
    sha256: String,
}

fn read_tree(slot: &Slot) -> Result<Option<TreeWitness>> {
    let parent = capability_store::open(&namespace(slot))?;
    let name = OsStr::new(&slot.name);
    if parent.metadata(name)?.is_none() {
        return Ok(None);
    }
    let root = parent.child(name)?;
    let meta = root.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o022 != 0 {
        return Err(conflict());
    }
    let sha256 = crate::agent::ai_payload::native_plugin_publication_digest(&root)?;
    parent.verify_child(name, &root)?;
    parent.verify()?;
    Ok(Some(TreeWitness {
        identity: root.identity().clone(),
        sha256,
    }))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Publication {
    profile: InstallTarget,
    offer: String,
    trees: Vec<Transfer>,
    files: Vec<Pair>,
    checkout: Option<(Slot, Tree)>,
}

impl Publication {
    fn verify_paths(&self, state: &Path) -> Result<()> {
        let root = &self.profile.directory.canonical;
        let (files, markets): (&[&str], &[&str]) = match self.profile.client.as_str() {
            "claude" => (
                &[
                    "plugins/known_marketplaces.json",
                    "plugins/installed_plugins.json",
                    "settings.json",
                ],
                &["plugins/cache"],
            ),
            "codex" => (&["config.toml"], &[".tmp/marketplaces", "plugins/cache"]),
            _ => return Err(conflict()),
        };
        if self.files.len() != files.len() || self.trees.len() != markets.len() {
            return Err(conflict());
        }
        for (entry, relative) in self.files.iter().zip(files) {
            if entry.live.path() != root.join(relative) {
                return Err(conflict());
            }
        }
        for (entry, prefix) in self.trees.iter().zip(markets) {
            let source = entry.source.path();
            let target = entry.target.path();
            let relative = target.strip_prefix(root).map_err(|_| conflict())?;
            if !relative.starts_with(prefix)
                || !source.starts_with(state)
                || !source.ends_with(relative)
            {
                return Err(conflict());
            }
        }
        if let Some((checkout, _)) = &self.checkout
            && (self.profile.client != "claude"
                || checkout.name != "checkout"
                || checkout.parent.canonical.parent() != Some(state))
        {
            return Err(conflict());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reserved {
    journal: Journal,
    publication: Publication,
}

pub(crate) struct InstalledPlugin<'a> {
    pub native_id: String,
    pub marketplace: &'a str,
    pub offer: String,
    pub config: PathBuf,
    pub payload: PathBuf,
    pub market: Option<PathBuf>,
    pub checkout: Option<&'a (Slot, Tree)>,
}

fn slot(path: &Path) -> Result<Slot> {
    let parent = path.parent().ok_or_else(conflict)?;
    let directory = Directory::open(parent)?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(conflict)?;
    if directory.path != parent {
        return Err(conflict());
    }
    Ok(Slot {
        parent: capability_store::binding(&directory, parent)?,
        name: name.into(),
    })
}

fn namespace(slot: &Slot) -> Namespace {
    Namespace {
        ancestor: slot.parent.clone(),
        remaining: vec![slot.name.clone()],
    }
}

fn regular(slot: &Slot) -> Result<()> {
    let parent = capability_store::open(&namespace(slot))?;
    match rustix::fs::statat(
        parent.file(),
        &slot.name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    ) {
        Ok(stat) if stat.st_nlink == 1 => Ok(()),
        Err(rustix::io::Errno::NOENT) => Ok(()),
        _ => Err(conflict()),
    }
}

fn value(path: &Path) -> Result<(Option<Object>, Vec<u8>)> {
    let slot = match slot(path) {
        Ok(slot) => slot,
        Err(error) if error.code == "path_missing" => return Ok((None, Vec::new())),
        Err(error) => return Err(error),
    };
    regular(&slot)?;
    let before = crate::source::source_files::observe(&slot)?;
    let (state, bytes) = capability_store::observe(&namespace(&slot), 1024 * 1024)?;
    if state != before.as_ref().map_or(State::Missing, Object::state) {
        return Err(conflict());
    }
    Ok((before, bytes.unwrap_or_default()))
}

pub(super) fn claude_first_install_conflict(
    target: &InstallTarget,
    id: &str,
    market: &str,
    requested_version: &str,
) -> Result<Option<String>> {
    let profile = target.open()?;
    let read = |name| -> Result<Value> {
        let (stamp, raw) = value(&profile.path.join(name))?;
        stamp.map_or_else(|| Ok(json!({})), |_| crate::policy::json::parse(&raw))
    };
    let installed = read("plugins/installed_plugins.json")?;
    let settings = read("settings.json")?;
    let markets = read("plugins/known_marketplaces.json")?;
    profile.verify()?;
    if let Some(entries) = installed["plugins"].get(id) {
        let version = entries
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row["version"].as_str());
        return Ok(Some(match version {
            Some(version) if version == requested_version => format!("已装有 {id} {version}。"),
            Some(version) if markets[market]["source"]["source"] == "directory" => {
                format!("已装有 {id} {version}。请用原安装方式更新本地插件市场，再检查版本。")
            }
            Some(version) => {
                format!("已装有 {id} {version}。请在该 Claude Code 的插件菜单中检查更新。")
            }
            None => format!("已有 {id} 的安装记录。请在该 Claude Code 的插件菜单中检查。"),
        }));
    }
    if settings["enabledPlugins"].get(id).is_some() {
        return Ok(Some(format!(
            "已有 {id} 的启停设置。请在该 Claude Code 的插件菜单中检查。"
        )));
    }
    if settings["extraKnownMarketplaces"].get(market).is_some() || markets.get(market).is_some() {
        return Ok(Some(format!(
            "已有同名插件市场 {market}。请在该 Claude Code 的插件菜单中检查来源。"
        )));
    }
    Ok(None)
}

pub(super) fn publication_recorded(store: &Store, native: &Journal) -> Result<bool> {
    let state = Directory::open(&store.path)?;
    let operations = state.child(OsStr::new("operations"))?;
    let id = &native.id;
    Ok(state
        .metadata(OsStr::new(&format!("plugin-target-{id}-receipt.json")))?
        .is_some()
        || state
            .metadata(OsStr::new(&format!("plugin-target-{id}-staging.json")))?
            .is_some()
        || state
            .metadata(OsStr::new(&format!("claude-{id}-adoption.json")))?
            .is_some()
        || operations
            .metadata(OsStr::new(&format!("{id}-receipt.json")))?
            .is_some())
}

fn make_child(parent: &Directory, name: &OsStr) -> Result<Directory> {
    parent.verify()?;
    if parent.metadata(name)?.is_none() {
        use rustix::fs::{Mode, mkdirat};
        match mkdirat(parent.file(), name, Mode::RWXU) {
            Ok(()) => parent.file().sync_all()?,
            Err(rustix::io::Errno::EXIST) => (),
            Err(error) => return Err(io(error)),
        }
    }
    let directory = parent.child(name)?;
    let meta = directory.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o022 != 0 {
        return Err(conflict());
    }
    parent.verify_child(name, &directory)?;
    Ok(directory)
}

fn within(root: &Directory, relative: &Path) -> Result<Directory> {
    relative
        .components()
        .try_fold(root.clone(), |parent, item| {
            let std::path::Component::Normal(name) = item else {
                return Err(conflict());
            };
            make_child(&parent, name)
        })
}

fn transfer(root: &Directory, stage: &Path, source: &Path) -> Result<Transfer> {
    let relative = source.strip_prefix(stage).map_err(|_| conflict())?;
    let parent = within(root, relative.parent().ok_or_else(conflict)?)?;
    let target = slot(&parent.path.join(relative.file_name().ok_or_else(conflict)?))?;
    let source = slot(source)?;
    let tree = read_tree(&source)?.ok_or_else(conflict)?;
    if read_tree(&target)?.is_some()
        || source.parent.identity.device != target.parent.identity.device
    {
        return Err(conflict());
    }
    Ok(Transfer {
        source,
        target,
        tree,
    })
}

fn move_tree(item: &Transfer) -> Result<()> {
    let actual = read_tree(&item.target)?;
    let staged = read_tree(&item.source)?;
    if staged.is_none() && actual.as_ref() == Some(&item.tree) {
        return Ok(());
    }
    if actual.is_some() || staged.as_ref() != Some(&item.tree) {
        return Err(conflict());
    }
    let from = capability_store::open(&namespace(&item.source))?;
    let to = capability_store::open(&namespace(&item.target))?;
    rustix::fs::renameat_with(
        from.file(),
        &item.source.name,
        to.file(),
        &item.target.name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io)?;
    from.file().sync_all()?;
    to.file().sync_all()?;
    if read_tree(&item.target)?.as_ref() != Some(&item.tree) || read_tree(&item.source)?.is_some() {
        return Err(conflict());
    }
    Ok(())
}

struct ConfigUpdate {
    path: PathBuf,
    before: Option<Object>,
    bytes: Vec<u8>,
}

impl ConfigUpdate {
    fn stage(&self, register: impl FnOnce(&Slot) -> Result<()>) -> Result<Pair> {
        let slot = slot(&self.path)?;
        regular(&slot)?;
        let before = self.before.as_ref();
        crate::source::source_files::stage_registered(
            &Witness {
                path: self.path.clone(),
                namespace: namespace(&slot),
                state: before.map_or(State::Missing, Object::state),
            },
            &self.bytes,
            before.map_or(0o600, |file| file.mode),
            register,
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Staging {
    profile: InstallTarget,
    slots: Vec<Slot>,
}

fn clear_staging(store: &Store, name: &str, profile: &InstallTarget) -> Result<()> {
    let Some(saved) = Store::read::<Staging>(&store.root, name)? else {
        return Ok(());
    };
    if saved.profile != *profile {
        return Err(conflict());
    }
    for slot in &saved.slots {
        let path = &slot.parent.canonical;
        if !path.starts_with(&profile.directory.canonical)
            || slot.name != "ledger"
            || slot.parent.mode != 0o700
            || !path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with(".macops-source-ledger-"))
        {
            return Err(conflict());
        }
        if !path.exists() {
            continue;
        }
        let root = capability_store::open(&namespace(slot))?;
        root.unlink_regular(OsStr::new(&slot.name), |_| regular(slot))?;
        remove_retained(&root)?;
    }
    rustix::fs::unlinkat(&store.root, name, rustix::fs::AtFlags::empty()).map_err(io)?;
    store.root.sync_all()?;
    Ok(())
}

fn merge_json(input: &mut Value, keys: &[&str], item: Value) -> Result<()> {
    let (key, tables) = keys.split_last().ok_or_else(conflict)?;
    let rows = tables.iter().try_fold(input, |row, field| {
        Ok::<_, Error>(
            row.as_object_mut()
                .ok_or_else(conflict)?
                .entry(*field)
                .or_insert_with(|| json!({})),
        )
    })?;
    let object = rows.as_object_mut().ok_or_else(conflict)?;
    if object.contains_key(*key) || item.is_null() {
        return Err(conflict());
    }
    object.insert((*key).into(), item);
    Ok(())
}

fn json_patch(
    root: &Directory,
    stage: &Path,
    file: &str,
    edits: &[Vec<&str>],
    mut replace: impl FnMut(&mut Value) -> Result<()>,
) -> Result<ConfigUpdate> {
    let generated =
        crate::policy::json::parse(&crate::platform::fs::regular_bytes(&stage.join(file))?)?;
    let path = root.path.join(file);
    let (before, bytes) = value(&path)?;
    let mut current = if before.is_some() {
        crate::policy::json::parse(&bytes)?
    } else if file == "plugins/installed_plugins.json" {
        json!({"version":2})
    } else {
        json!({})
    };
    if file == "plugins/installed_plugins.json" && current["version"] != 2 {
        return Err(conflict());
    }
    for keys in edits {
        let mut item = keys.iter().fold(&generated, |row, key| &row[*key]).clone();
        replace(&mut item)?;
        merge_json(&mut current, keys, item)?;
    }
    let mut bytes = serde_json::to_vec_pretty(&current)?;
    bytes.push(b'\n');
    Ok(ConfigUpdate {
        path,
        before,
        bytes,
    })
}

fn codex_config(root: &Directory, plugin: &InstalledPlugin<'_>) -> Result<ConfigUpdate> {
    let generated = crate::platform::fs::regular_bytes(&plugin.config)?;
    let parse = |raw: &[u8]| -> Result<toml_edit::DocumentMut> {
        std::str::from_utf8(raw)
            .map_err(|_| conflict())?
            .parse()
            .map_err(|_| conflict())
    };
    let source = parse(&generated)?;
    let path = root.path.join("config.toml");
    let (before, raw) = value(&path)?;
    let mut current = parse(&raw)?;
    for (table, key) in [
        ("marketplaces", plugin.marketplace),
        ("plugins", plugin.native_id.as_str()),
    ] {
        let item = source
            .get(table)
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|rows| rows.get(key))
            .ok_or_else(conflict)?
            .clone();
        let rows = current
            .entry(table)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_like_mut()
            .ok_or_else(conflict)?;
        if rows.contains_key(key) {
            return Err(conflict());
        }
        rows.insert(key, item);
    }
    Ok(ConfigUpdate {
        path,
        before,
        bytes: current.to_string().into_bytes(),
    })
}

fn claude_config(
    root: &Directory,
    stage: &Path,
    plugin: &InstalledPlugin<'_>,
) -> Result<Vec<ConfigUpdate>> {
    let market = plugin.marketplace;
    let id = plugin.native_id.as_str();
    let settings = json_patch(
        root,
        stage,
        "settings.json",
        &[
            vec!["extraKnownMarketplaces", market],
            vec!["enabledPlugins", id],
        ],
        |_| Ok(()),
    )?;
    let registry = json_patch(
        root,
        stage,
        "plugins/installed_plugins.json",
        &[vec!["plugins", id]],
        |entry| {
            let [row] = entry.as_array_mut().ok_or_else(conflict)?.as_mut_slice() else {
                return Err(conflict());
            };
            row["installPath"] = json!(
                root.path
                    .join(plugin.payload.strip_prefix(stage).map_err(|_| conflict())?)
            );
            Ok(())
        },
    )?;
    let market = json_patch(
        root,
        stage,
        "plugins/known_marketplaces.json",
        &[vec![market]],
        |_| Ok(()),
    )?;
    Ok(vec![market, registry, settings])
}

fn lock(root: &Directory) -> Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags, openat};
    let file = std::fs::File::from(
        openat(
            root.file(),
            LOCK_FILE,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(io)?,
    );
    let info = file.metadata()?;
    if !info.is_file()
        || info.nlink() != 1
        || info.uid() != nix::unistd::geteuid().as_raw()
        || info.mode() & 0o777 != 0o600
    {
        return Err(conflict());
    }
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|_| Error::attention("operation_busy", "正在更新这个 Agent 档案。"))?;
    Ok(file)
}

pub(crate) fn is_publication(journal: &Journal) -> bool {
    journal
        .plan
        .warnings
        .first()
        .is_some_and(|value| value.starts_with(TAG))
}

fn frozen(directory: &std::fs::File, journal: &Journal) -> Result<Publication> {
    let saved: Reserved = Store::read(directory, &format!("plugin-target-{}.json", journal.id))?
        .ok_or_else(conflict)?;
    if saved.journal.plan_sha256 != journal.plan_sha256
        || saved.journal.id != journal.id
        || saved.journal.scope != journal.scope
        || saved.journal.started_at != journal.started_at
        || journal.plan.warnings.first()
            != Some(&format!("{TAG}{}", fingerprint(&saved.publication)?))
    {
        return Err(conflict());
    }
    saved.publication.verify_paths(&journal.state)?;
    Ok(saved.publication)
}

fn result(journal: &Journal, target: &InstallTarget) -> Value {
    json!({"journal":journal,"targetName":target.name,"profile":target.directory.canonical,"loaded":"unknown"})
}

pub(crate) fn publish(
    ctx: &Context,
    store: &Store,
    native: &Journal,
    target: &InstallTarget,
    plugin: &InstalledPlugin<'_>,
) -> Result<Value> {
    store.verify_namespace()?;
    let root = target.open()?;
    let lease = lock(&root)?;
    let reserved = prepare(ctx, store, native, target, plugin, &root)?;
    let written = Store::atomic_new(
        &store.root,
        &format!("plugin-target-{}.json", reserved.journal.id),
        &reserved,
    );
    if let Err(error) = written {
        if matches!(
            Store::read::<Reserved>(
                &store.root,
                &format!("plugin-target-{}.json", reserved.journal.id),
            ),
            Ok(None)
        ) {
            discard_candidates(&reserved.publication.files);
        }
        return Err(error);
    }
    let Reserved {
        mut journal,
        publication,
    } = reserved;
    store.save(&journal)?;
    settle(store, &mut journal, &publication, &lease)
}

fn stage_updates(
    store: &Store,
    staging_name: &str,
    target: &InstallTarget,
    updates: Vec<ConfigUpdate>,
) -> Result<Vec<Pair>> {
    let mut staging = Staging {
        profile: target.clone(),
        slots: Vec::new(),
    };
    let mut files = Vec::with_capacity(updates.len());
    for update in updates {
        match update.stage(|slot| {
            staging.slots.push(slot.clone());
            Store::atomic(&store.root, staging_name, &staging)
        }) {
            Ok(pair) => files.push(pair),
            Err(error) => {
                discard_candidates(&files);
                let _ = clear_staging(store, staging_name, target);
                return Err(error);
            }
        }
    }
    Ok(files)
}

fn prepare(
    ctx: &Context,
    store: &Store,
    native: &Journal,
    target: &InstallTarget,
    plugin: &InstalledPlugin<'_>,
    root: &Directory,
) -> Result<Reserved> {
    let stage = plugin.config.parent().ok_or_else(conflict)?;
    let staging_name = format!("plugin-target-{}-staging.json", native.id);
    clear_staging(store, &staging_name, target)?;
    let updates = if plugin.market.is_some() {
        vec![codex_config(root, plugin)?]
    } else {
        claude_config(root, stage, plugin)?
    };
    let trees = plugin
        .market
        .iter()
        .chain(std::iter::once(&plugin.payload))
        .map(|path| transfer(root, stage, path))
        .collect::<Result<Vec<_>>>()?;
    let files = stage_updates(store, &staging_name, target, updates)?;
    let saved = Publication {
        profile: target.clone(),
        offer: plugin.offer.clone(),
        trees,
        files,
        checkout: plugin.checkout.cloned(),
    };
    let journal = (|| -> Result<Journal> {
        saved.verify_paths(&store.path)?;
        let mut plan = native.plan.clone();
        plan.command = None;
        plan.delegated = false;
        plan.warnings = vec![format!("{TAG}{}", fingerprint(&saved)?)];
        crate::operations::new_journal_with_id(ctx, plan, format!("{}-receipt", native.id))
    })();
    match journal {
        Ok(journal) => Ok(Reserved {
            journal,
            publication: saved,
        }),
        Err(error) => {
            discard_candidates(&saved.files);
            let _ = clear_staging(store, &staging_name, target);
            Err(error)
        }
    }
}

pub(crate) fn resume_publication(
    ctx: &Context,
    store: &Store,
    native: &Journal,
    target: &InstallTarget,
) -> Result<Option<Value>> {
    let id = format!("{}-receipt", native.id);
    let Some(saved) = Store::read::<Reserved>(&store.root, &format!("plugin-target-{id}.json"))?
    else {
        return Ok(None);
    };
    if saved.publication.profile != *target || saved.journal.id != id {
        return Err(conflict());
    }
    if Store::read::<Journal>(&store.root, "active.json")?.is_some_and(|active| {
        active.id != id && crate::policy::operation::state::pending(active.phase)
    }) {
        return Err(conflict());
    }
    store.check_migration_pending()?;
    let mut journal =
        Store::read::<Journal>(&store.operations, &format!("{id}.json"))?.unwrap_or(saved.journal);
    crate::operations::store::validate_journal(&journal)?;
    if crate::policy::operation::state::pending(journal.phase) {
        store.save(&journal)?;
    }
    recover(ctx, store, &mut journal, false, false).map(Some)
}

fn settle(
    store: &Store,
    journal: &mut Journal,
    saved: &Publication,
    lease: &std::fs::File,
) -> Result<Value> {
    let lock_id = crate::domain::FileId::from_file(lease)?;
    let check = || -> Result<()> {
        store.verify_namespace()?;
        let root = saved.profile.open()?;
        if root.metadata(OsStr::new(LOCK_FILE))?.is_none_or(|meta| {
            meta.kind != crate::platform::fs::EntryKind::File || meta.identity != lock_id
        }) {
            return Err(conflict());
        }
        if let Some((slot, tree)) = &saved.checkout
            && crate::source::source_tree::observe(slot)?.as_ref() != Some(tree)
        {
            return Err(conflict());
        }
        Ok(())
    };
    let update = || -> Result<()> {
        check()?;
        for step in &saved.files {
            regular(&step.live)?;
            regular(&step.retained)?;
            if crate::source::source_files::position(step)? == Position::Unknown {
                return Err(conflict());
            }
        }
        for tree in &saved.trees {
            check()?;
            move_tree(tree)?;
        }
        for step in &saved.files {
            crate::source::source_files::publish_checked(step, || {
                check()?;
                regular(&step.live)?;
                regular(&step.retained)
            })?;
        }
        for tree in &saved.trees {
            if read_tree(&tree.target)?.as_ref() != Some(&tree.tree) {
                return Err(conflict());
            }
        }
        for step in &saved.files {
            if crate::source::source_files::position(step)? != Position::OwnAfter {
                return Err(conflict());
            }
        }
        check()
    };
    if update().is_ok() {
        journal.phase = Phase::Succeeded;
        journal.message = "已安装到选定的 Agent 档案。".into();
    } else {
        journal.phase = Phase::Unknown;
        journal.message = "还不能确认安装结果。请检查选定档案中的设置和插件。".into();
    }
    store.save(journal)?;
    if journal.phase == Phase::Succeeded {
        for step in &saved.files {
            let _ = clear_preimage(step);
        }
    }
    Ok(result(journal, &saved.profile))
}

fn clear_preimage(pair: &Pair) -> Result<()> {
    if crate::source::source_files::position(pair)? != Position::OwnAfter {
        return Err(conflict());
    }
    clear_retained(pair, pair.before.as_ref())
}

fn discard_candidates(pairs: &[Pair]) {
    for pair in pairs {
        let _ = clear_retained(pair, Some(&pair.after));
    }
}

fn clear_retained(pair: &Pair, expected: Option<&Object>) -> Result<()> {
    let root = capability_store::open(&namespace(&pair.retained))?;
    root.unlink_regular(OsStr::new(&pair.retained.name), |_| {
        regular(&pair.retained)?;
        if crate::source::source_files::observe(&pair.retained)?.as_ref() != expected {
            return Err(conflict());
        }
        Ok(())
    })?;
    remove_retained(&root)
}

fn remove_retained(root: &Directory) -> Result<()> {
    let slot = slot(&root.path)?;
    let parent = capability_store::open(&namespace(&slot))?;
    let name = OsStr::new(&slot.name);
    parent.verify_child(name, root)?;
    rustix::fs::unlinkat(parent.file(), name, rustix::fs::AtFlags::REMOVEDIR).map_err(io)?;
    parent.file().sync_all()?;
    Ok(())
}

pub(crate) fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    mark_failed: bool,
    yes: bool,
) -> Result<Value> {
    verify_scope(ctx, journal)?;
    let saved = frozen(&store.root, journal)?;
    if !crate::policy::operation::state::pending(journal.phase) {
        return Ok(result(journal, &saved.profile));
    }
    if mark_failed {
        crate::operations::confirm(
            "确认已检查选定档案。将这次安装记为失败，保留现在的文件。",
            yes,
        )?;
        journal.phase = Phase::Failed;
        journal.message = "已记为失败。已保留当前设置和插件文件。".into();
        store.save(journal)?;
        return Ok(result(journal, &saved.profile));
    }
    let root = saved.profile.open()?;
    let lease = lock(&root)?;
    settle(store, journal, &saved, &lease)
}

fn verify_scope(ctx: &Context, journal: &Journal) -> Result<()> {
    if journal.home != ctx.home
        || journal.state != ctx.state
        || journal.scope != ctx.scope
        || journal.cache != ctx.cache
        || journal.settings != ctx.settings
    {
        return Err(Error::attention(
            "plugin_target_scope_changed",
            "这次使用的设置或状态目录与原安装不同。请带上原来的目录选项再试。",
        ));
    }
    Ok(())
}

fn target(journal: &Journal) -> Result<Option<InstallTarget>> {
    if !is_publication(journal) {
        return Ok(None);
    }
    let state = Directory::open(&journal.state)?;
    Ok(Some(frozen(state.file(), journal)?.profile))
}

pub(crate) fn recovery_target(journal: &Journal) -> Result<Option<InstallTarget>> {
    if is_publication(journal) {
        target(journal)
    } else if crate::agent::claude_install::is_install(journal) {
        crate::agent::claude_install::existing_target(journal)
    } else {
        crate::agent::codex_install::existing_target(journal)
    }
}

pub(crate) fn previous(
    ctx: &Context,
    store: &Store,
    target: &InstallTarget,
    offer: &str,
) -> Result<Option<Value>> {
    for journal in crate::operations::store::history(ctx)?.into_iter().rev() {
        if journal.phase != Phase::Succeeded || !is_publication(&journal) {
            continue;
        }
        let saved = frozen(&store.root, &journal)?;
        if saved.profile != *target || saved.offer != offer {
            continue;
        }
        verify_scope(ctx, &journal)?;
        target.open()?;
        if let Some((slot, tree)) = &saved.checkout
            && crate::source::source_tree::observe(slot)?.as_ref() != Some(tree)
        {
            return Err(conflict());
        }
        for item in &saved.trees {
            if read_tree(&item.target)?.as_ref() != Some(&item.tree) {
                return Err(conflict());
            }
        }
        for item in &saved.files {
            regular(&item.live)?;
            if crate::source::source_files::observe(&item.live)?.as_ref() != Some(&item.after) {
                return Err(Error::attention(
                    "plugin_target_already_installed",
                    "此插件曾安装到这个档案，安装后的设置已有变化。请在原档案查看插件状态。",
                ));
            }
        }
        return Ok(Some(result(&journal, &saved.profile)));
    }
    Ok(None)
}

pub(crate) fn previous_stage(
    ctx: &Context,
    selected: impl Fn(&Journal) -> Result<bool>,
) -> Result<Option<Journal>> {
    for journal in crate::operations::store::history(ctx)?.into_iter().rev() {
        if selected(&journal)? {
            verify_scope(ctx, &journal)?;
            return Ok((journal.phase == Phase::Succeeded).then_some(journal));
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "../../tests/unit/plugin_target.rs"]
mod tests;
