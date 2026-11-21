use crate::{
    context::Context,
    domain::{
        Action, CommandSpec, FileId, Journal, MiseNamespace, MiseQuery, Plan, Provider, QuerySpec,
        Scope,
    },
    error::{Error, Result},
    paths::{self, Directory},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

#[path = "mise_retirement.rs"]
pub(crate) mod retirement;

fn required() -> Error {
    Error::attention(
        "configuration_owner_requires_review",
        "mise 全局配置由 chezmoi 维护；请从对应配置源更新版本",
    )
}

fn uncertain() -> Error {
    Error::attention("config_authority_uncertain", "当前 mise 配置归属需要核对")
}

fn global_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        let name = name.to_ascii_lowercase();
        Path::new(&name).extension() == Some(OsStr::new("toml"))
            && (name.starts_with("config.") || name.starts_with("mise."))
            && !name.ends_with(".override.toml")
    })
}

fn possible_alias(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    let compatible = |a: std::path::Component<'_>, b: std::path::Component<'_>| {
        let (a, b) = (a.as_os_str(), b.as_os_str());
        a.eq_ignore_ascii_case(b) || !a.is_ascii() || !b.is_ascii()
    };
    if left.components().count() != right.components().count()
        || !left
            .components()
            .zip(right.components())
            .all(|(left, right)| compatible(left, right))
    {
        return false;
    }
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => true,
    }
}

fn possible_suffix(path: &Path, base: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.strip_prefix(base) {
        return Some(path.to_owned());
    }
    let count = base.components().count();
    let prefix = path.components().take(count).collect::<PathBuf>();
    possible_alias(&prefix, base).then(|| path.components().skip(count).collect())
}

fn fragment(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        !name.starts_with('.') && Path::new(name).extension() == Some(OsStr::new("toml"))
    })
}

fn config_leaf(path: &Path) -> bool {
    let parts: Vec<_> = path.iter().collect();
    match parts.as_slice() {
        [file] => global_name(file),
        [dir, file] if *dir == "conf.d" => fragment(file),
        [dir, folder, file] if *dir == "conf.d" => {
            folder.to_str().is_some_and(|name| !name.starts_with('.'))
                && file.to_str().is_some_and(|name| name.starts_with("mise."))
                && global_name(file)
        }
        _ => false,
    }
}

fn existing(path: &Path) -> Result<Option<FileId>> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(Some(FileId {
            device: meta.dev(),
            inode: meta.ino(),
        })),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn list(path: &Path) -> Result<Vec<String>> {
    let root = match Directory::open(&paths::read_path(path)?) {
        Ok(root) => root,
        Err(error) if error.code == "path_missing" => return Ok(vec![]),
        Err(error) => return Err(error),
    };
    Ok(crate::capability_guard::listing(&root)?
        .into_keys()
        .collect())
}

struct Footprint {
    directory: Option<PathBuf>,
    files: Vec<PathBuf>,
    identities: Vec<FileId>,
}

impl Footprint {
    fn capture(home: &Path, get: impl Fn(&str) -> Option<PathBuf>) -> Result<Self> {
        let global = get("MISE_GLOBAL_CONFIG_FILE");
        let mut paths = vec![];
        let directory = if let Some(global) = global {
            paths.push(global);
            None
        } else {
            let directory = get("MISE_CONFIG_DIR").unwrap_or_else(|| {
                get("XDG_CONFIG_HOME")
                    .unwrap_or_else(|| home.join(".config"))
                    .join("mise")
            });
            let directory = paths::read_path(&directory)?;
            paths.extend(get("MISE_CONFIG_FILE"));
            paths.push(home.join(".tool-versions"));
            paths.extend(
                [
                    "config.toml",
                    "mise.toml",
                    "config.local.toml",
                    "mise.local.toml",
                ]
                .map(|name| directory.join(name)),
            );
            paths.extend(
                list(&directory)?
                    .into_iter()
                    .filter(|leaf| global_name(OsStr::new(leaf)))
                    .map(|leaf| directory.join(leaf)),
            );
            let fragments = directory.join("conf.d");
            for leaf in list(&fragments)? {
                if paths.len() >= 5000 {
                    return Err(uncertain());
                }
                let candidate = fragments.join(&leaf);
                if fragment(OsStr::new(&leaf)) {
                    paths.push(candidate.clone());
                }
                if !leaf.starts_with('.') && candidate.is_dir() {
                    paths.extend(
                        list(&candidate)?
                            .into_iter()
                            .map(|leaf| candidate.join(leaf))
                            .filter(|path| path.strip_prefix(&directory).is_ok_and(config_leaf)),
                    );
                }
            }
            Some(directory)
        };
        let mut files = Vec::new();
        let mut identities = Vec::new();
        for path in paths {
            let resolved = paths::read_path(&path)?;
            if let Some(id) = existing(&resolved)? {
                identities.push(id);
            }
            files.extend([path, resolved]);
        }
        Ok(Self {
            directory,
            files,
            identities,
        })
    }

    fn contains(&self, path: &Path) -> Result<bool> {
        let resolved = paths::read_path(path)?;
        Ok(self
            .files
            .iter()
            .any(|other| possible_alias(other, path) || possible_alias(other, &resolved))
            || self.directory.as_ref().is_some_and(|directory| {
                [path, resolved.as_path()].into_iter().any(|path| {
                    possible_suffix(path, directory).is_some_and(|path| config_leaf(&path))
                })
            })
            || existing(&resolved)?.is_some_and(|id| self.identities.contains(&id)))
    }

    fn overlaps(&self, destination: &Path) -> Result<bool> {
        let resolved = paths::read_path(destination)?;
        Ok(self.files.iter().any(|path| {
            possible_suffix(path, destination).is_some()
                || possible_suffix(path, &resolved).is_some()
        }) || self.directory.as_ref().is_some_and(|directory| {
            possible_suffix(directory, &resolved).is_some()
                || possible_suffix(&resolved, directory).is_some()
        }))
    }
}

fn legacy(ctx: &Context) -> Result<bool> {
    Ok(paths::read_path(
        &ctx.home
            .join(".local/share/chezmoi/home/dot_config/mise/private_config.toml"),
    )?
    .exists())
}

fn owned(ctx: &Context, footprint: &Footprint) -> Result<bool> {
    let matches = |target: &Path| footprint.contains(target);
    let overlaps = |path: &Path| footprint.overlaps(path);
    if legacy(ctx)? {
        return Ok(true);
    }
    let snapshot = crate::config_inventory::discover(ctx);
    for candidate in &snapshot.candidates {
        if matches(&candidate.target)? {
            return Ok(true);
        }
    }
    if crate::config_inventory::uncertainty_affects(&snapshot, &matches, &overlaps)? {
        return Err(uncertain());
    }
    if matches!(
        snapshot.coverage.state,
        crate::resource::CoverageState::Invalid
            | crate::resource::CoverageState::Truncated
            | crate::resource::CoverageState::Unavailable
    ) && overlaps(snapshot.roots.as_ref().map_or_else(
        || ctx.chezmoi.destination.as_deref().unwrap_or(&ctx.home),
        |roots| roots.destination.as_path(),
    ))? {
        return Err(uncertain());
    }
    crate::chezmoi_native::owns_possible_target(ctx, &matches, &overlaps)
}

fn scope_paths(scope: &Scope, name: &str) -> Option<PathBuf> {
    scope.paths.get(name).and_then(Clone::clone)
}

pub(crate) fn declared_owner(ctx: &Context) -> Result<bool> {
    if legacy(ctx)? {
        return Ok(true);
    }
    owned(
        ctx,
        &Footprint::capture(&ctx.home, |key| scope_paths(&ctx.scope, key))?,
    )
}

pub(crate) fn applies(plan: &Plan) -> bool {
    matches!(plan.package.provider, Provider::Mise { .. })
        && matches!(
            plan.action,
            Action::Install | Action::Upgrade | Action::Uninstall
        )
}

fn command_path(command: &Command, key: &str) -> Option<PathBuf> {
    command
        .get_envs()
        .find(|(name, _)| *name == key)
        .and_then(|(_, value)| value.map(PathBuf::from))
}

fn command_footprint(command: &Command) -> Result<Footprint> {
    let get = |key: &str| command_path(command, key);
    let home = get("HOME").ok_or_else(uncertain)?;
    Footprint::capture(&home, get)
}

fn namespace(command: &Command) -> Result<MiseNamespace> {
    let get = |key: &str| command_path(command, key);
    let home = get("HOME").ok_or_else(uncertain)?;
    let scoped = |key, xdg, fallback| {
        get(key).unwrap_or_else(|| get(xdg).unwrap_or_else(|| home.join(fallback)).join("mise"))
    };
    let data = paths::read_path(&scoped("MISE_DATA_DIR", "XDG_DATA_HOME", ".local/share"))?;
    let config = paths::read_path(&scoped("MISE_CONFIG_DIR", "XDG_CONFIG_HOME", ".config"))?;
    let installs =
        paths::read_path(&get("MISE_INSTALLS_DIR").unwrap_or_else(|| data.join("installs")))?;
    let data_identity = directory_identity(&data)?;
    let config_identity = directory_identity(&config)?;
    let installs_identity = directory_identity(&installs)?;
    Ok(MiseNamespace {
        home,
        data,
        config,
        installs,
        global_file: get("MISE_GLOBAL_CONFIG_FILE"),
        config_file: get("MISE_CONFIG_FILE"),
        data_identity,
        config_identity,
        installs_identity,
    })
}

fn directory_identity(path: &Path) -> Result<Option<FileId>> {
    match Directory::open(path) {
        Ok(directory) if directory.path == path => Ok(Some(directory.identity().clone())),
        Ok(_) => Err(scope_changed()),
        Err(error) if error.code == "path_missing" => Ok(None),
        Err(error) => Err(error),
    }
}

fn scope_changed() -> Error {
    Error::attention(
        "mise_scope_changed",
        "mise 安装或配置目录在确认后发生变化，请重新核对",
    )
}

pub(crate) fn capture_query(scope: &Scope, command: CommandSpec) -> Result<MiseQuery> {
    let mut native = crate::process::command(&command, scope)?;
    apply_installs_override(scope, &mut native)?;
    let namespace = namespace(&native)?;
    Ok(MiseQuery {
        command,
        namespace,
        retirement: None,
    })
}

fn apply_installs_override(scope: &Scope, command: &mut Command) -> Result<()> {
    if std::env::var_os("HOME").is_some_and(|home| home == scope.home.as_os_str())
        && let Some(path) = std::env::var_os("MISE_INSTALLS_DIR").filter(|path| !path.is_empty())
    {
        command.env("MISE_INSTALLS_DIR", paths::read_path(Path::new(&path))?);
    } else {
        command.env_remove("MISE_INSTALLS_DIR");
    }
    Ok(())
}

fn bind_namespace(command: &mut Command, expected: &MiseNamespace) -> Result<()> {
    if &namespace(command)? != expected {
        return Err(scope_changed());
    }
    command
        .env("MISE_DATA_DIR", &expected.data)
        .env("MISE_CONFIG_DIR", &expected.config)
        .env("MISE_INSTALLS_DIR", &expected.installs);
    Ok(())
}

fn same_or_created(query: &MiseQuery, current: &MiseQuery) -> bool {
    let mut expected = query.namespace.clone();
    expected.data_identity = expected
        .data_identity
        .or_else(|| current.namespace.data_identity.clone());
    expected.config_identity = expected
        .config_identity
        .or_else(|| current.namespace.config_identity.clone());
    expected.installs_identity = expected
        .installs_identity
        .or_else(|| current.namespace.installs_identity.clone());
    current.command == query.command
        && current.namespace == expected
        && current.retirement == query.retirement
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Completion {
    plan: String,
    scope: String,
    query: MiseQuery,
}

fn completion_name(journal: &Journal) -> String {
    format!("{}-mise-completion.json", journal.id)
}

pub(crate) fn record_completion(store: &Store, journal: &Journal) -> Result<()> {
    let Some(QuerySpec::Mise(original)) = &journal.plan.query else {
        return Ok(());
    };
    let mut query = capture_query(&journal.scope, original.command.clone())?;
    query.retirement.clone_from(&original.retirement);
    if !same_or_created(original, &query) {
        return Err(scope_changed());
    }
    store.verify_namespace()?;
    Store::atomic_new(
        &store.root,
        &completion_name(journal),
        &Completion {
            plan: journal.plan_sha256.clone(),
            scope: journal.scope_sha256.clone(),
            query,
        },
    )
}

pub(crate) fn completion_query(
    store: &Store,
    journal: &Journal,
    original: &MiseQuery,
) -> Result<MiseQuery> {
    let missing = || {
        Error::attention(
            "mise_completion_unverified",
            "缺少原 mise 进程退出后的目录证据，需要人工核对",
        )
    };
    let record: Completion =
        Store::read(&store.root, &completion_name(journal))?.ok_or_else(missing)?;
    if record.plan != journal.plan_sha256
        || record.scope != journal.scope_sha256
        || !same_or_created(original, &record.query)
    {
        return Err(missing());
    }
    Ok(record.query)
}

pub(crate) fn query_command(scope: &Scope, query: &MiseQuery) -> Result<Command> {
    let exact;
    let spec = if let Some(retirement) = &query.retirement {
        exact = CommandSpec {
            program: retirement::checked_program(query, retirement)?,
            args: query.command.args.clone(),
        };
        &exact
    } else {
        &query.command
    };
    let mut command = crate::process::command(spec, scope)?;
    apply_installs_override(scope, &mut command)?;
    bind_namespace(&mut command, &query.namespace)?;
    Ok(command)
}

pub(crate) fn bind_mutation(scope: &Scope, plan: &Plan, command: &mut Command) -> Result<()> {
    let Some(QuerySpec::Mise(query)) = &plan.query else {
        return Err(Error::attention(
            "mise_scope_unbound",
            "mise 安装目录尚未登记，请重新规划",
        ));
    };
    apply_installs_override(scope, command)?;
    bind_namespace(command, &query.namespace)?;
    if let Some(retirement) = &query.retirement {
        retirement::checked_program(query, retirement)?;
        retirement::check_command(plan, command, retirement)?;
    } else if plan.action == Action::Uninstall {
        return Err(Error::attention(
            "mise_scope_unbound",
            "mise 卸载缺少已核对的版本和载荷",
        ));
    }
    Ok(())
}

pub(crate) fn plan_query(
    ctx: &Context,
    package: &crate::domain::Package,
    command: CommandSpec,
    action: Option<Action>,
) -> Result<MiseQuery> {
    let mut query = capture_query(&ctx.scope, command)?;
    if action == Some(Action::Uninstall) {
        query.retirement = Some(retirement::capture(ctx, package, &query)?);
    }
    Ok(query)
}
pub(crate) fn check_command(ctx: &Context, command: &Command) -> Result<()> {
    if owned(ctx, &command_footprint(command)?)? {
        return Err(required());
    }
    Ok(())
}

pub(crate) fn worker_context(journal: &Journal) -> Result<Context> {
    let home = journal.home.clone();
    let inherited_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(uncertain)?;
    let isolated = home != inherited_home;
    let chezmoi = crate::config_inventory::Inputs::environment(isolated);
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        let (current, _) =
            crate::policy::layout::scope_value(key, chezmoi.env.get(key).map(PathBuf::as_path));
        if current != scope_paths(&journal.scope, key) {
            return Err(uncertain());
        }
    }
    Ok(Context {
        scope: journal.scope.clone(),
        harness: crate::context::HarnessRoots::default(),
        uv: crate::uv_local::DiscoveryInputs::default(),
        timeout_secs: journal.plan.timeout_secs,
        config: home.clone(),
        data: home.clone(),
        catalog: home.clone(),
        home,
        settings: journal.settings.clone(),
        state: journal.state.clone(),
        cache: journal.cache.clone(),
        catalog_explicit: false,
        isolated,
        chezmoi,
    })
}

#[cfg(test)]
#[path = "../tests/unit/mise_owner.rs"]
mod tests;
