use super::super::{Cache, Managed, io, policy, validate_cache};
use crate::{
    context::Context,
    domain::{ConfigAuthority, FileId, Package, Provider, fingerprint},
    error::{Error, Result},
    paths::{self, Bindings, BoundTarget, Directory, EntryKind, Metadata},
    policy::chezmoi_native::{Binding, SourceMode, SourceObject, Stamp},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const LIMIT: u64 = 256 * 1024;
const MAX_DEPTH: usize = 24;
pub(super) const DRAFT_PREFIX: &str = ".chezmoi-source-";
pub(super) const DRAFT_LEAF: &str = "candidate";

pub(super) struct Inputs {
    pub binding: Binding,
    pub owner: Stamp,
    pub cache: Cache,
    pub target: SourceObject,
    pub contents: Vec<u8>,
    pub parent: Directory,
    pub tree_sha256: String,
    pub managed: Option<Managed>,
}

#[derive(Clone, Copy)]
pub(super) enum SourceDelta<'a> {
    None,
    Added(&'a Path),
    Retired(&'a Path, &'a SourceObject),
}

pub(super) fn unsupported() -> Error {
    Error::attention(
        "chezmoi_source_unsupported",
        "所选 chezmoi 源需要已登记的静态配置、完整映射和普通文件",
    )
}

pub(super) fn check_scope(ctx: &Context) -> Result<bool> {
    let unsupported = || {
        Error::attention(
            "chezmoi_source_state_unsupported",
            "将配置纳入管理需要系统账户 HOME 和固定 .local/state/macops",
        )
    };
    let uid = nix::unistd::geteuid();
    let user = nix::unistd::User::from_uid(uid)
        .map_err(|_| unsupported())?
        .ok_or_else(unsupported)?;
    if ctx.state != ctx.home.join(".local/state/macops") || paths::read_path(&ctx.home)? != ctx.home
    {
        return Err(unsupported());
    }
    if paths::read_path(&user.dir)? == ctx.home {
        return Ok(false);
    }
    let fixture = ctx.home.parent().ok_or_else(unsupported)?;
    let runtime_tmp = paths::read_path(Path::new("/tmp"))?;
    if ctx.home.file_name() != Some(OsStr::new("home"))
        || fixture.parent() != Some(runtime_tmp.as_path())
    {
        return Err(unsupported());
    }
    let fixture = Directory::open(fixture).map_err(|_| unsupported())?;
    let meta = fixture.file().metadata()?;
    if meta.mode() & 0o7777 != 0o700 || meta.uid() != uid.as_raw() {
        return Err(unsupported());
    }
    fixture.verify()?;
    Ok(true)
}

pub(super) fn file(path: &Path, limit: u64) -> Result<(Vec<u8>, SourceObject)> {
    let parent = Directory::open(path.parent().ok_or_else(unsupported)?)?;
    if path.parent() != Some(&parent.path) {
        return Err(unsupported());
    }
    let (contents, meta) =
        parent.regular_config_snapshot(path.file_name().ok_or_else(unsupported)?, limit)?;
    let sha256 = format!("{:x}", Sha256::digest(&contents));
    Ok((
        contents,
        SourceObject {
            path: path.to_path_buf(),
            identity: meta.identity,
            mode: meta.mode,
            sha256,
        },
    ))
}

fn static_config(binding: &Binding) -> Result<()> {
    if binding.config.extension() != Some(OsStr::new("toml")) {
        return Err(unsupported());
    }
    let (bytes, _) = file(&binding.config, LIMIT)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| unsupported())?;
    let settings = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| unsupported())?;
    for (key, item) in settings.iter() {
        if !matches!(
            key,
            "sourceDir" | "destDir" | "cacheDir" | "persistentState"
        ) || item.as_str().is_none()
        {
            return Err(unsupported());
        }
    }
    Ok(())
}

fn verify_owner(ctx: &Context, binding: &Binding, fixture: bool) -> Result<()> {
    if binding.scope != ctx.scope
        || binding.record_state != ctx.state
        || binding.destination != ctx.home
        || binding.configured_source != binding.effective_source
        || !binding.effective_source.starts_with(&ctx.home)
        || binding.effective_source == ctx.home
        || binding.version != "chezmoi version v2.73.3"
        || binding.filtered
        || fixture && !binding.program.starts_with(&ctx.home)
    {
        return Err(unsupported());
    }
    let source = Directory::open(&binding.effective_source)?;
    if source.file().metadata()?.uid() != nix::unistd::geteuid().as_raw() {
        return Err(unsupported());
    }
    io::verify(binding)?;
    static_config(binding)
}

fn check_target(
    ctx: &Context,
    binding: &Binding,
    target: &Path,
) -> Result<(BoundTarget, Metadata)> {
    policy::selection(binding, &[target.to_path_buf()], false)?;
    let home = Directory::open(&ctx.home)?;
    let relative = target.strip_prefix(&ctx.home).map_err(|_| unsupported())?;
    let physical = Bindings::new(24).relative(&home, relative)?;
    let selected = physical.metadata()?.ok_or_else(unsupported)?;
    require_unprotected(
        ctx,
        binding,
        target,
        physical.parent_directory()?,
        &selected.identity,
    )?;
    if selected.kind != EntryKind::File || selected.mode > 0o777 {
        return Err(unsupported());
    }
    let id = crate::policy::config_local::config_id(target)?;
    let declared = crate::inventory::declared_catalog(ctx)?;
    let receipts = crate::target::receipts(ctx)?;
    for package in &declared.packages {
        if same_owner_target(package, &id, &selected.identity)? {
            return Err(owner_conflict());
        }
    }
    for old in receipts.installations.values() {
        if !matches!(
            old.package.provider,
            Provider::ConfigObserved {
                authority: ConfigAuthority::ChezmoiNative,
                ..
            }
        ) && same_owner_target(&old.package, &id, &selected.identity)?
        {
            return Err(owner_conflict());
        }
    }
    Ok((physical, selected))
}

fn same_owner_target(package: &Package, id: &str, identity: &FileId) -> Result<bool> {
    if package.id == id {
        return Ok(true);
    }
    let Provider::ConfigObserved { target, .. } = &package.provider else {
        return Ok(false);
    };
    Ok(existing_file_id(target)?.as_ref() == Some(identity))
}

fn existing_file_id(path: &Path) -> Result<Option<FileId>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some(FileId {
            device: meta.dev(),
            inode: meta.ino(),
        })),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn owner_conflict() -> Error {
    Error::attention("owner_conflict", "已有配置配方或收据绑定该物理文件")
}

fn require_unprotected(
    ctx: &Context,
    binding: &Binding,
    target: &Path,
    parent: &Directory,
    selected: &FileId,
) -> Result<()> {
    let protected = || {
        Error::attention(
            "chezmoi_target_protected",
            "所选目标与 macops 或 chezmoi 的内部状态、配置或源目录重叠",
        )
    };
    let lineage = parent.lineage()?;
    for path in [
        &ctx.state,
        &ctx.cache,
        &ctx.data,
        &ctx.config,
        &ctx.catalog,
        &ctx.settings,
        &binding.configured_source,
        &binding.cache,
        &binding.config,
        &binding.persistent_state,
        &binding.program,
    ] {
        if target.starts_with(path) {
            return Err(protected());
        }
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let physical = FileId {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        if metadata.is_dir() && lineage.contains(&physical) || *selected == physical {
            return Err(protected());
        }
    }
    parent.verify()
}

pub(super) fn inspect(
    ctx: &Context,
    target: &Path,
    mode: SourceMode,
    delta: SourceDelta<'_>,
) -> Result<Inputs> {
    let fixture = check_scope(ctx)?;
    let owner_path = ctx.state.join("chezmoi-owner.json");
    let owner = io::stamp(&owner_path, true)?;
    let binding = io::read::<Binding>(ctx, "chezmoi-owner.json")?.ok_or_else(unsupported)?;
    verify_owner(ctx, &binding, fixture)?;
    let (physical, selected) = check_target(ctx, &binding, target)?;
    let cache = io::read::<Cache>(ctx, "chezmoi-metadata.json")?.ok_or_else(unsupported)?;
    validate_cache(ctx, &cache)?;
    if !cache.complete || cache.binding != binding || io::stamp(&owner_path, true)? != owner {
        return Err(unsupported());
    }
    if mode == SourceMode::Adopt && cache.entries.len() >= policy::MANAGED_LIMIT {
        return Err(Error::attention(
            "chezmoi_source_capacity",
            "原生管理清单没有空间增加条目",
        ));
    }
    check_mapping_identity(&cache, target, &selected.identity, mode)?;
    let info = fs::symlink_metadata(target)?;
    if info.nlink() > 1
        || info.dev() != selected.identity.device
        || info.ino() != selected.identity.inode
    {
        return Err(unsupported());
    }
    let contents = physical.read_selected(&selected.identity, LIMIT)?;
    if physical.metadata()?.is_none_or(|after| {
        after.identity != selected.identity
            || after.mode != selected.mode
            || after.kind != EntryKind::File
    }) {
        return Err(io::changed());
    }
    let observed = SourceObject {
        path: target.to_path_buf(),
        identity: selected.identity,
        mode: selected.mode,
        sha256: format!("{:x}", Sha256::digest(&contents)),
    };
    let relative = target.strip_prefix(&ctx.home).map_err(|_| unsupported())?;
    let parent = Directory::open(&source_parent(&cache, relative)?)?;
    let source_depth = parent
        .path
        .strip_prefix(&binding.effective_source)
        .map_err(|_| unsupported())?
        .components()
        .count();
    if mode == SourceMode::Adopt && source_depth >= MAX_DEPTH {
        return Err(Error::attention(
            "chezmoi_source_capacity",
            "目标将超出原生源目录深度上限",
        ));
    }
    let managed = cache
        .entries
        .get(relative.to_str().ok_or_else(unsupported)?)
        .cloned();
    if mode == SourceMode::Forget && managed.is_none() {
        return Err(Error::attention(
            "chezmoi_target_unmanaged",
            "所选目标尚未由当前 chezmoi 管理",
        ));
    }
    let tree_sha256 = tree(&binding, &cache, delta)?;
    Ok(Inputs {
        binding,
        owner,
        cache,
        target: observed,
        contents,
        parent,
        tree_sha256,
        managed,
    })
}

fn check_mapping_identity(
    cache: &Cache,
    target: &Path,
    identity: &FileId,
    mode: SourceMode,
) -> Result<()> {
    let managed = || {
        Error::attention(
            "chezmoi_target_managed",
            "所选文件已在当前管理清单，或与其中一个文件是同一物理对象",
        )
    };
    for row in cache.entries.values() {
        if row.absolute == target {
            if mode == SourceMode::Adopt {
                return Err(managed());
            }
            continue;
        }
        match fs::symlink_metadata(&row.absolute) {
            Ok(meta) if meta.is_file() => {
                if meta.dev() == identity.device && meta.ino() == identity.inode {
                    return Err(managed());
                }
            }
            Ok(_) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn source_parent(cache: &Cache, target: &Path) -> Result<PathBuf> {
    let parent = target.parent().ok_or_else(unsupported)?;
    if parent.as_os_str().is_empty() {
        return Ok(cache.binding.effective_source.clone());
    }
    cache
        .entries
        .get(parent.to_str().ok_or_else(unsupported)?)
        .map(|entry| entry.source_absolute.clone())
        .ok_or_else(unsupported)
}

fn tree(binding: &Binding, cache: &Cache, delta: SourceDelta<'_>) -> Result<String> {
    let root = Directory::open(&binding.effective_source)?;
    let mut files = BTreeMap::new();
    let skip = if let SourceDelta::Added(path) = delta {
        Some(path)
    } else {
        None
    };
    let mut budget = policy::MANAGED_LIMIT;
    scan(&root, &root.path, skip, &mut files, &mut budget)?;
    if let SourceDelta::Retired(source, retained) = delta {
        let relative = source.strip_prefix(&root.path).map_err(|_| unsupported())?;
        if files
            .insert(
                relative.to_path_buf(),
                (retained.identity.clone(), retained.mode),
            )
            .is_some()
        {
            return Err(unsupported());
        }
    }
    if files.len() != cache.entries.len() {
        return Err(unsupported());
    }
    for value in cache.entries.values() {
        if !files.contains_key(&value.source_relative) {
            return Err(unsupported());
        }
    }
    fingerprint(&files)
}

fn scan(
    dir: &Directory,
    root: &Path,
    addition: Option<&Path>,
    files: &mut BTreeMap<PathBuf, (FileId, u32)>,
    budget: &mut usize,
) -> Result<()> {
    for entry in dir.names()? {
        let entry = entry.map_err(|_| unsupported())?;
        let name = entry.file_name();
        let text = name.to_str().map_err(|_| unsupported())?;
        if matches!(text, "." | "..") {
            continue;
        }
        let leaf = OsStr::new(text);
        let meta = dir.metadata(leaf)?.ok_or_else(unsupported)?;
        let path = dir.path.join(leaf);
        if addition == Some(path.as_path()) {
            continue;
        }
        if dir.path == root && text == ".git" && meta.kind == EntryKind::Directory {
            continue;
        }
        *budget = budget.checked_sub(1).ok_or_else(unsupported)?;
        if !static_name(text) || !matches!(meta.kind, EntryKind::File | EntryKind::Directory) {
            return Err(unsupported());
        }
        let rel = path.strip_prefix(root).map_err(|_| unsupported())?;
        if rel.components().count() > MAX_DEPTH {
            return Err(unsupported());
        }
        files.insert(rel.to_path_buf(), (meta.identity, meta.mode));
        if meta.kind == EntryKind::Directory {
            scan(&dir.child(leaf)?, root, addition, files, budget)?;
        }
    }
    dir.verify()
}

fn static_name(text: &str) -> bool {
    let name = text.strip_prefix("private_").unwrap_or(text);
    !name.is_empty()
        && !name.starts_with(['.', '-'])
        && !Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("tmpl"))
        && ![
            "create_",
            "empty_",
            "encrypted_",
            "exact_",
            "executable_",
            "modify_",
            "private_",
            "remove_",
            "run_",
            "symlink_",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        && !name.contains(['\r', '\n'])
}

pub(super) struct Staged {
    pub draft: tempfile::TempDir,
    pub candidate: SourceObject,
    pub source: PathBuf,
    pub identity: FileId,
}

pub(super) fn stage(ctx: &Context, inputs: &Inputs, mode: SourceMode) -> Result<Staged> {
    let state = Directory::open(&ctx.state)?;
    let state_meta = state.file().metadata()?;
    if state_meta.mode() & 0o7777 != 0o700
        || state_meta.uid() != nix::unistd::geteuid().as_raw()
        || state.identity().device != inputs.parent.identity().device
    {
        return Err(unsupported());
    }
    let draft = tempfile::Builder::new()
        .prefix(DRAFT_PREFIX)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(&ctx.state)?;
    let native = tempfile::tempdir_in(draft.path())?;
    let binding = stage_binding(native.path(), inputs)?;
    let relative = inputs
        .target
        .path
        .strip_prefix(&ctx.home)
        .map_err(|_| unsupported())?;
    let target = binding.destination.join(relative);
    fs::create_dir_all(target.parent().ok_or_else(unsupported)?)?;
    fs::write(&target, &inputs.contents)?;
    fs::set_permissions(&target, fs::Permissions::from_mode(inputs.target.mode))?;
    let arguments = stage_args(&binding);
    let execute =
        |tail: &[&OsStr]| invoke(&binding, native.path(), &arguments, tail, ctx.timeout_secs);
    let candidate = draft.path().join(DRAFT_LEAF);
    let (source, object) = match mode {
        SourceMode::Adopt => stage_add(inputs, &binding, &target, &candidate, execute)?,
        SourceMode::Forget => stage_forget(inputs, &binding, &target, &candidate, execute)?,
    };
    native.close()?;
    let root = Directory::open(draft.path())?;
    root.file().sync_all()?;
    state.verify()?;
    state.file().sync_all()?;
    Ok(Staged {
        draft,
        candidate: object,
        source,
        identity: root.identity().clone(),
    })
}

fn managed_stage(
    binding: &Binding,
    execute: &impl Fn(&[&OsStr]) -> Result<Vec<u8>>,
) -> Result<BTreeMap<String, Managed>> {
    let raw = execute(&[
        OsStr::new("managed"),
        OsStr::new("--format=json"),
        OsStr::new("--path-style=all"),
    ])?;
    policy::managed(binding, &raw)
}

fn stage_add(
    inputs: &Inputs,
    binding: &Binding,
    target: &Path,
    candidate: &Path,
    execute: impl Fn(&[&OsStr]) -> Result<Vec<u8>>,
) -> Result<(PathBuf, SourceObject)> {
    execute(&[
        OsStr::new("add"),
        OsStr::new("--recursive=false"),
        OsStr::new("--secrets=error"),
        OsStr::new("--"),
        target.as_os_str(),
    ])?;
    let relative = target
        .strip_prefix(&binding.destination)
        .map_err(|_| unsupported())?;
    let managed = managed_stage(binding, &execute)?;
    let entry = managed
        .get(relative.to_str().ok_or_else(unsupported)?)
        .ok_or_else(unsupported)?;
    execute(&[
        OsStr::new("verify"),
        OsStr::new("--recursive=false"),
        OsStr::new("--"),
        target.as_os_str(),
    ])?;
    let expected_parent = inputs
        .parent
        .path
        .strip_prefix(&inputs.binding.effective_source)
        .map_err(|_| unsupported())?;
    if entry.source_relative.parent() != Some(expected_parent) {
        return Err(unsupported());
    }
    let leaf = entry.source_relative.file_name().ok_or_else(unsupported)?;
    if !static_name(leaf.to_str().ok_or_else(unsupported)?) {
        return Err(unsupported());
    }
    let source = inputs.parent.path.join(leaf);
    fs::rename(&entry.source_absolute, candidate)?;
    let (_, object) = file(candidate, LIMIT)?;
    if object.sha256 != inputs.target.sha256 || object.mode > 0o777 {
        return Err(unsupported());
    }
    fs::File::open(candidate)?.sync_all()?;
    Ok((source, object))
}

fn stage_forget(
    inputs: &Inputs,
    binding: &Binding,
    target: &Path,
    candidate: &Path,
    execute: impl Fn(&[&OsStr]) -> Result<Vec<u8>>,
) -> Result<(PathBuf, SourceObject)> {
    let current = inputs.managed.as_ref().ok_or_else(unsupported)?;
    let source = current.source_absolute.clone();
    if source.parent() != Some(&inputs.parent.path) {
        return Err(unsupported());
    }
    let (_, mut retained) = file(&source, LIMIT)?;
    if retained.mode > 0o777 {
        return Err(unsupported());
    }
    let relative = target
        .strip_prefix(&binding.destination)
        .map_err(|_| unsupported())?;
    let key = relative.to_str().ok_or_else(unsupported)?;
    let staged = binding.effective_source.join(&current.source_relative);
    fs::write(&staged, b"static ordinary file")?;
    fs::set_permissions(&staged, fs::Permissions::from_mode(retained.mode))?;
    let managed = managed_stage(binding, &execute)?;
    if managed
        .get(key)
        .is_none_or(|entry| entry.source_absolute != staged)
    {
        return Err(unsupported());
    }
    execute(&[
        OsStr::new("forget"),
        OsStr::new("--force"),
        OsStr::new("--"),
        target.as_os_str(),
    ])?;
    if staged.exists() || managed_stage(binding, &execute)?.contains_key(key) {
        return Err(unsupported());
    }
    retained.path = candidate.to_path_buf();
    Ok((source, retained))
}

fn stage_binding(path: &Path, inputs: &Inputs) -> Result<Binding> {
    let mut result = inputs.binding.clone();
    result.destination = path.join("home");
    result.effective_source = path.join("source");
    result
        .configured_source
        .clone_from(&result.effective_source);
    result.config = path.join("config/chezmoi.toml");
    result.cache = path.join("cache");
    result.persistent_state = path.join("native-state");
    result.program = path.join("chezmoi");
    let expected = inputs
        .binding
        .anchors
        .iter()
        .find(|stamp| stamp.path == inputs.binding.program)
        .ok_or_else(unsupported)?;
    let (bytes, tool) = file(&inputs.binding.program, 128 * 1024 * 1024)?;
    let lineage =
        Directory::open(inputs.binding.program.parent().ok_or_else(unsupported)?)?.lineage()?;
    if expected.identity.as_ref() != Some(&tool.identity)
        || expected.mode != tool.mode
        || expected.digest != fingerprint(&(lineage, tool.sha256))?
    {
        return Err(io::changed());
    }
    fs::write(&result.program, bytes)?;
    fs::set_permissions(&result.program, fs::Permissions::from_mode(0o700))?;
    let source_relative = inputs
        .parent
        .path
        .strip_prefix(&inputs.binding.effective_source)
        .map_err(|_| unsupported())?;
    for dir in [
        result.destination.clone(),
        result.effective_source.join(source_relative),
        result.cache.clone(),
        path.join("config"),
        path.join("tmp"),
    ] {
        fs::create_dir_all(dir)?;
    }
    fs::write(&result.config, "")?;
    Ok(result)
}

fn stage_args(binding: &Binding) -> Vec<&OsStr> {
    vec![
        OsStr::new("--source"),
        binding.effective_source.as_os_str(),
        OsStr::new("--destination"),
        binding.destination.as_os_str(),
        OsStr::new("--config"),
        binding.config.as_os_str(),
        OsStr::new("--cache"),
        binding.cache.as_os_str(),
        OsStr::new("--persistent-state"),
        binding.persistent_state.as_os_str(),
        OsStr::new("--no-tty"),
    ]
}

fn invoke(
    binding: &Binding,
    stage: &Path,
    prefix: &[&OsStr],
    tail: &[&OsStr],
    timeout: u64,
) -> Result<Vec<u8>> {
    let mut command = Command::new(&binding.program);
    command
        .args(prefix)
        .args(tail)
        .env_clear()
        .env("HOME", &binding.destination)
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", stage.join("tmp"))
        .env("XDG_CONFIG_HOME", stage.join("config"))
        .env("XDG_CACHE_HOME", &binding.cache)
        .env("LC_ALL", "C")
        .current_dir(stage)
        .stdin(Stdio::null())
        .process_group(0);
    let captured = crate::process::capture_owned_group_in(
        &mut command,
        Duration::from_secs(timeout),
        LIMIT,
        stage,
        || Ok(()),
    );
    let output = captured.result?;
    if output.uncertain || output.code != Some(0) {
        return Err(Error::attention(
            "chezmoi_source_native_unknown",
            "隔离原生操作未通过，可能发现敏感内容。实际源未写入",
        ));
    }
    Ok(output.stdout)
}
