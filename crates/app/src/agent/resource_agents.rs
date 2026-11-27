use crate::{
    context::Context,
    domain::resource::{
        self, Coverage, CoverageState, Data, Management, Model, Resource, RuntimeBinding, State,
    },
    error::{Error, Result},
    platform::fs as paths,
    policy::catalog::resources as policy,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const PROFILE_REGISTRY: &str = "agent-profiles.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InstallTarget {
    pub name: String,
    pub client: String,
    pub directory: crate::policy::governance::DirectoryBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<PathBuf>,
}

fn target_changed() -> Error {
    Error::attention(
        "agent_install_target_changed",
        "档案目录已变化。请重新选择安装位置。",
    )
}

impl InstallTarget {
    pub(crate) fn capture(ctx: &Context, profile: &Resource) -> Result<Self> {
        let Data::Agent {
            client, config_dir, ..
        } = &profile.data
        else {
            return Err(target_changed());
        };
        let directory = paths::Directory::open(config_dir)?;
        let registered = registered_target(ctx, config_dir)?;
        if directory.path != *config_dir
            || (directory.path.parent() != Some(ctx.home.as_path())
                && !(profile.management == Management::Adopted
                    && directory.path.starts_with(&ctx.state)
                    && managed_root(&directory.path))
                && !registered
                    .as_ref()
                    .is_some_and(|target| target.client == *client && target.open().is_ok()))
        {
            return Err(target_changed());
        }
        let target = Self {
            name: profile.name.clone(),
            client: client.clone(),
            directory: crate::capability::capability_store::binding(&directory, config_dir)?,
            command: registered.and_then(|target| target.command),
        };
        target.open()?;
        Ok(target)
    }

    pub(crate) fn open(&self) -> Result<paths::Directory> {
        let path = &self.directory.requested;
        let directory = paths::Directory::open(path)?;
        if directory.path != *path
            || self.directory.uid != nix::unistd::geteuid().as_raw()
            || self.directory.mode & 0o022 != 0
            || crate::capability::capability_store::binding(&directory, path)? != self.directory
        {
            return Err(target_changed());
        }
        directory.verify()?;
        Ok(directory)
    }
}

type Registry = BTreeMap<PathBuf, InstallTarget>;

fn registered(directory: &std::fs::File) -> Result<Registry> {
    Ok(crate::operations::store::Store::read(directory, PROFILE_REGISTRY)?.unwrap_or_default())
}

fn registered_target(ctx: &Context, path: &Path) -> Result<Option<InstallTarget>> {
    if !ctx.state.exists() {
        return Ok(None);
    }
    Ok(registered(paths::Directory::open(&ctx.state)?.file())?.remove(path))
}

fn selected_registration(book: &Registry, id: &str) -> Result<Option<InstallTarget>> {
    for (path, target) in book {
        if !managed_root(path)
            && resource::id("agent", &(&target.client, path))? == id
            && target.open().is_ok_and(|directory| directory.path == *path)
        {
            return Ok(Some(target.clone()));
        }
    }
    Ok(None)
}

pub(crate) fn can_forget_registration(ctx: &Context, id: &str) -> bool {
    paths::Directory::open(&ctx.state)
        .and_then(|state| registered(state.file()))
        .and_then(|book| selected_registration(&book, id))
        .is_ok_and(|entry| entry.is_some())
}

pub(crate) fn forget_registration(ctx: &Context, id: &str) -> Result<()> {
    let state = paths::Directory::open(&ctx.state)?;
    let chosen =
        selected_registration(&registered(state.file())?, id)?.ok_or_else(target_changed)?;
    crate::operations::confirm(
        &format!(
            "停止接管 {}\n配置目录 {}\n保留本机程序、配置和插件。",
            crate::output::sanitize(&chosen.name, false),
            crate::output::sanitize(&chosen.directory.canonical.display().to_string(), false),
        ),
        false,
    )?;
    let store = crate::operations::lock_after_recovery(ctx)?;
    let mut book = registered(&store.root)?;
    if selected_registration(&book, id)?.as_ref() != Some(&chosen) {
        return Err(target_changed());
    }
    let directory = chosen.open()?;
    book.remove(&chosen.directory.canonical);
    store.verify_namespace()?;
    directory.verify()?;
    crate::operations::store::Store::atomic(&store.root, PROFILE_REGISTRY, &book)
}

pub(crate) fn add(
    ctx: &Context,
    name: &str,
    config: &Path,
    client: Option<&str>,
    dry: bool,
    yes: bool,
) -> Result<serde_json::Value> {
    let invalid = || {
        Error::attention(
        "agent_registration_invalid", "请提供一个已有的 Agent 命令和独立的配置目录。",
    ).with_guidance(
        "未登记 Agent。",
        "使用 mo agent add 命令名 --config /绝对路径；自定义命令可加 --type claude、codex 或 cursor。",
    )
    };
    if name == "all" || !policy::component(name) || name.starts_with('.') {
        return Err(invalid());
    }
    let client = client
        .or_else(|| {
            ["claude", "codex", "cursor"].into_iter().find(|client| {
                name == *client
                    || name
                        .strip_prefix(client)
                        .is_some_and(|tail| tail.starts_with('-'))
            })
        })
        .ok_or_else(invalid)?;
    let command = crate::operations::command::executable(name)?;
    let directory = paths::Directory::open(config)?;
    if directory.path != config || ctx.home.starts_with(config) || managed_root(config) {
        return Err(invalid());
    }
    crate::source::rules::source_baseline::disjoint(
        config,
        &[
            ctx.config.clone(),
            ctx.data.clone(),
            ctx.state.clone(),
            ctx.cache.clone(),
            ctx.settings.clone(),
            ctx.catalog.clone(),
        ],
    )?;
    let profile = match (client, name) {
        ("claude", "claude") | ("codex", "codex") | ("cursor", "cursor" | "cursor-agent") => {
            "default"
        }
        _ => name
            .strip_prefix(client)
            .and_then(|suffix| suffix.strip_prefix('-'))
            .unwrap_or(name),
    };
    let target = InstallTarget {
        name: format!("{client}:{profile}"),
        client: client.into(),
        directory: crate::capability::capability_store::binding(&directory, config)?,
        command: Some(command),
    };
    target.open()?;
    check_registration_scope(ctx, &target)?;
    let same = registered_target(ctx, config)?.as_ref() == Some(&target);
    if !dry && !same {
        crate::operations::confirm(
            &format!("登记 Agent {name}\n配置目录 {}", config.display()),
            yes,
        )?;
        enroll(ctx, &target)?;
    }
    Ok(serde_json::json!({
        "agentRegistration":true, "name":name, "config":config,
        "status":if dry {"preview"} else if same {"already-registered"} else {"registered"},
        "persistentWrites":!dry && !same,
    }))
}

fn check_registration_scope(ctx: &Context, target: &InstallTarget) -> Result<()> {
    for other in profile_roots(ctx)? {
        let Data::Agent {
            config_dir, client, ..
        } = other.data
        else {
            continue;
        };
        let physical = paths::read_path(&config_dir)?;
        let selected = &target.directory.canonical;
        let overlaps = if physical == *selected {
            client != target.client
        } else {
            physical.starts_with(selected)
                || selected.starts_with(&physical)
                || other.name == target.name
        };
        if overlaps {
            return Err(Error::attention(
                "agent_configuration_overlap",
                "配置目录和已有 Agent 重叠，或命令名已经占用。请核对 mo agent 中的配置。",
            ));
        }
    }
    Ok(())
}

pub(crate) fn enroll(ctx: &Context, target: &InstallTarget) -> Result<()> {
    let store = crate::operations::store::Store::lock(ctx)?;
    store.check_pending()?;
    let directory = target.open()?;
    if target.command.is_some() {
        check_registration_scope(ctx, target)?;
    }
    let mut profiles = registered(&store.root)?;
    if profiles
        .values()
        .any(|old| old.name == target.name && old.directory.canonical != directory.path)
    {
        return Err(Error::attention(
            "agent_name_conflict",
            "这个 Agent 名称已经登记到另一个目录。",
        ));
    }
    if let Some(old) = profiles.get(&directory.path) {
        if old == target {
            return Ok(());
        }
        if old.name != target.name
            || old.client != target.client
            || old.directory != target.directory
            || old.command.is_some()
            || target.command.is_none()
        {
            return Err(target_changed());
        }
    }
    profiles.insert(directory.path.clone(), target.clone());
    store.verify_namespace()?;
    directory.verify()?;
    crate::operations::store::Store::atomic(&store.root, PROFILE_REGISTRY, &profiles)
}

pub fn collect(
    ctx: &Context,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<(Vec<Resource>, Vec<Coverage>)> {
    let roots = discover_roots(ctx)?;
    let mut profiles = vec![];
    let mut coverage = Coverage {
        source: "harness-profiles".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![ctx.home.clone()],
        issues: vec![],
    };
    for root in roots {
        match profile(ctx, &root, project, applications) {
            Ok(resource) => {
                if resource.status == State::Unknown {
                    coverage.state = CoverageState::Invalid;
                    coverage
                        .issues
                        .push(format!("profile_metadata_unknown:{}", resource.id));
                }
                profiles.push(resource);
            }
            Err(error) => {
                coverage.state = CoverageState::Invalid;
                coverage.issues.push(error.code);
            }
        }
    }
    coverage.count = profiles.len();
    Ok((profiles, vec![coverage]))
}

pub fn profile_roots(ctx: &Context) -> Result<Vec<Resource>> {
    profile_roots_matching(ctx, |_, _| true)
}

pub(crate) fn profile_roots_matching(
    ctx: &Context,
    include: impl Fn(&str, &str) -> bool,
) -> Result<Vec<Resource>> {
    let mut profiles = vec![];
    let enrolled = if ctx.state.exists() {
        registered(paths::Directory::open(&ctx.state)?.file())?
    } else {
        Registry::new()
    };
    for ProfileRoot {
        client,
        name,
        path,
        management,
    } in discover_roots(ctx)?
    {
        let prefix = format!(".{client}-");
        let prof = if name == format!(".{client}") {
            "default"
        } else {
            name.strip_prefix(&prefix).unwrap_or(&name)
        };
        if !include(&client, prof)
            || !std::fs::metadata(paths::read_path(&path)?).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        let command = enrolled
            .get(&path)
            .and_then(|target| target.command.clone());
        profiles.push(Resource {
            id: resource::id("agent", &(&client, &path))?,
            name: format!("{client}:{prof}"),
            aliases: vec![format!("{client}-{prof}"), format!("agent:{client}:{prof}")],
            management,
            status: State::Present,
            data: Data::Agent {
                client,
                profile: prof.into(),
                config_dir: path,
                model: Model::default(),
                runtime: RuntimeBinding {
                    selector: "unresolved".into(),
                    context: ctx.home.clone(),
                    binary: command,
                    install_ref: None,
                    evidence: None,
                },
            },
        });
    }
    Ok(profiles)
}

pub(crate) struct SessionProfile {
    pub(crate) client: &'static str,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

pub(crate) fn resolve_session_profile(ctx: &Context, selector: &str) -> Result<SessionProfile> {
    let id_lookup = selector.starts_with("agent:");
    let ambiguous = || {
        Error::attention(
            "session_profile_ambiguous",
            "档案名称有重复，请使用详情中的档案 ID",
        )
    };
    let mut selection = None;
    let mut source_error = None;
    let sources = [
        (
            "codex",
            crate::agent::codex_install::managed_profiles as fn(&Context) -> Result<_>,
        ),
        ("claude", crate::agent::claude_install::managed_profiles),
    ];
    for (client, load) in sources {
        let prefix = format!("{client}:");
        if !id_lookup
            && !selector.starts_with(&prefix)
            && !selector.starts_with(&format!("{} · ", client_title(client)))
        {
            continue;
        }
        let profiles = match load(ctx) {
            Ok(profiles) => profiles,
            Err(error) => {
                source_error = Some(error);
                continue;
            }
        };
        for (leaf, path) in profiles {
            let Some(short) = leaf.strip_prefix(&format!(".{client}-")) else {
                continue;
            };
            let name = format!("{prefix}{short}");
            if (id_lookup && resource::id("agent", &(client, &path))? != selector)
                || (!id_lookup && name != selector && profile_label(&name, &path) != selector)
            {
                continue;
            }
            if !id_lookup && client == "claude" {
                let alternate = paths::read_path(&ctx.home.join(&leaf))?;
                if alternate.is_dir() && !managed_root(&alternate) {
                    return Err(ambiguous());
                }
            }
            if selection
                .replace(SessionProfile { client, name, path })
                .is_some()
            {
                return Err(ambiguous());
            }
        }
    }
    selection.ok_or_else(|| source_error.unwrap_or_else(session_unavailable))
}

pub(crate) fn session_unavailable() -> Error {
    Error::attention(
        "codex_session_profile_unavailable",
        "请选择已纳入管理的单独配置",
    )
}

struct ProfileRoot {
    client: String,
    name: String,
    path: PathBuf,
    management: Management,
}

impl ProfileRoot {
    fn registered(path: PathBuf, target: InstallTarget) -> Option<Self> {
        let profile = target.name.strip_prefix(&format!("{}:", target.client))?;
        if !matches!(target.client.as_str(), "claude" | "codex" | "cursor")
            || !target.open().is_ok_and(|directory| directory.path == path)
        {
            return None;
        }
        let name = if profile == "default" {
            format!(".{}", target.client)
        } else {
            format!(".{}-{profile}", target.client)
        };
        Some(Self {
            client: target.client,
            name,
            path,
            management: Management::Adopted,
        })
    }
}

fn discover_roots(ctx: &Context) -> Result<Vec<ProfileRoot>> {
    let mut roots = vec![];
    let enrolled = if ctx.state.exists() {
        registered(paths::Directory::open(&ctx.state)?.file())?
    } else {
        Registry::new()
    };
    for entry in std::fs::read_dir(paths::read_path(&ctx.home)?)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let client = if name == ".claude" || name.starts_with(".claude-") {
            "claude"
        } else if name == ".codex" {
            "codex"
        } else if name == ".cursor" {
            "cursor"
        } else if matches!(name, ".openclaw" | ".kimi" | ".mimocode" | ".opencode") {
            &name[1..]
        } else {
            continue;
        };
        let path = entry.path();
        if matches!(client, "codex" | "claude")
            && paths::read_path(&path).is_ok_and(|target| managed_root(&target))
        {
            continue;
        }
        if entry.file_type()?.is_dir() || entry.file_type()?.is_symlink() {
            roots.push(ProfileRoot {
                client: client.to_owned(),
                name: name.to_owned(),
                management: if enrolled
                    .get(&path)
                    .is_some_and(|target| target.client == client && target.open().is_ok())
                {
                    Management::Adopted
                } else {
                    Management::Untracked
                },
                path,
            });
        }
    }
    for (name, profile) in crate::agent::codex_install::managed_profiles(ctx)? {
        roots.push(ProfileRoot {
            client: "codex".into(),
            name,
            path: profile,
            management: Management::Adopted,
        });
    }
    for (name, profile) in crate::agent::claude_install::managed_profiles(ctx)? {
        roots.push(ProfileRoot {
            client: "claude".into(),
            name,
            path: profile,
            management: Management::Adopted,
        });
    }
    for (path, enrolled) in enrolled {
        if !roots.iter().any(|root| root.path == path)
            && let Some(root) = ProfileRoot::registered(path, enrolled)
        {
            roots.push(root);
        }
    }
    for (client, path) in [
        ("claude", ctx.harness.claude.as_ref()),
        ("codex", ctx.harness.codex.as_ref()),
    ] {
        if let Some(path) = path {
            let physical = paths::read_path(path)?;
            if managed_root(&physical) {
                continue;
            }
            if !roots.iter().any(|root| &root.path == path) {
                roots.push(ProfileRoot {
                    client: client.into(),
                    name: format!(".{client}-environment"),
                    path: path.clone(),
                    management: Management::Untracked,
                });
            }
        }
    }
    Ok(roots)
}

pub(crate) fn managed_root(path: &Path) -> bool {
    crate::agent::codex_install::reserved(path) || crate::agent::claude_install::reserved(path)
}

fn archive_path(root: &Path) -> Result<PathBuf> {
    let leaf = root
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(target_changed)?;
    Ok(root.with_file_name(format!(".archived-{leaf}")))
}

fn profile_anchor(receipt: &crate::domain::Receipt) -> Option<&crate::domain::ConfigAnchor> {
    use crate::domain::{CapabilityKind, QuerySpec};
    let Some(QuerySpec::LocalCapability(query)) = &receipt.query else {
        return None;
    };
    match query.capability {
        CapabilityKind::CodexPlugin => Some(&query.record),
        CapabilityKind::ClaudePlugin => query.settings.as_ref(),
        CapabilityKind::SkillsCli => None,
    }
}

pub(crate) fn receipt_is_archived(ctx: &Context, receipt: &crate::domain::Receipt) -> bool {
    let Some(anchor) = profile_anchor(receipt) else {
        return false;
    };
    let profile = &anchor.canonical_parent;
    let Some(root) = profile.ancestors().nth(2) else {
        return false;
    };
    if !managed_root(profile)
        || root.parent() != Some(ctx.state.as_path())
        || !root
            .symlink_metadata()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return false;
    }
    let Ok(archive) = archive_path(root) else {
        return false;
    };
    let Ok(relative) = profile.strip_prefix(root) else {
        return false;
    };
    let target = archive.join(relative);
    paths::Directory::open(&target).is_ok_and(|directory| {
        directory.path == target && directory.identity() == &anchor.parent_identity
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ArchivePlan {
    name: String,
    root: PathBuf,
    destination: PathBuf,
    identity: crate::domain::FileId,
    profile: PathBuf,
    profile_identity: crate::domain::FileId,
}

fn plan_archive(ctx: &Context, selector: &str) -> Result<ArchivePlan> {
    let selected = resolve_session_profile(ctx, selector)?;
    let root = selected
        .path
        .ancestors()
        .nth(2)
        .ok_or_else(target_changed)?;
    if root.parent() != Some(ctx.state.as_path()) || !managed_root(&selected.path) {
        return Err(target_changed());
    }
    let identity = if selected.client == "codex" {
        crate::agent::codex_install::allocation_identity(ctx, &selected.path)?
    } else {
        crate::agent::claude_install::allocation_identity(ctx, &selected.path)?
    };
    let destination = archive_path(root)?;
    let name = profile_label(&selected.name, &selected.path);
    let receipts = crate::catalog::target::receipts(ctx)?;
    let mut anchors = receipts
        .installations
        .values()
        .filter_map(profile_anchor)
        .filter(|anchor| anchor.canonical_parent == selected.path);
    let anchor = anchors.next().ok_or_else(target_changed)?;
    if anchors.any(|other| other.parent_identity != anchor.parent_identity) {
        return Err(target_changed());
    }
    let plan = ArchivePlan {
        name,
        root: root.into(),
        destination,
        identity,
        profile: selected.path,
        profile_identity: anchor.parent_identity.clone(),
    };
    archive_root(&plan)?;
    Ok(plan)
}

fn archive_root(plan: &ArchivePlan) -> Result<paths::Directory> {
    use std::os::unix::fs::MetadataExt as _;
    let root = paths::Directory::open(&plan.root)?;
    let metadata = root.file().metadata()?;
    if root.path != plan.root
        || *root.identity() != plan.identity
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(target_changed());
    }
    verify_profile_directory(&plan.profile, &plan.profile_identity)?;
    root.verify()?;
    Ok(root)
}

fn verify_profile_directory(path: &Path, identity: &crate::domain::FileId) -> Result<()> {
    let profile = paths::Directory::open(path)?;
    if profile.path != path || profile.identity() != identity {
        return Err(target_changed());
    }
    profile.verify()
}

pub(crate) fn uninstall(
    ctx: &Context,
    selector: &str,
    dry: bool,
    yes: bool,
) -> Result<serde_json::Value> {
    uninstall_using(ctx, selector, dry, yes, crate::operations::confirm)
}

fn uninstall_using(
    ctx: &Context,
    selector: &str,
    dry: bool,
    yes: bool,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
) -> Result<serde_json::Value> {
    use crate::browser::sanitize;
    let plan = plan_archive(ctx, selector)?;
    if dry {
        return Ok(
            serde_json::json!({"mode":"dry-run","profile":plan.name,"location":plan.root,"archive":plan.destination,"persistentWrites":false}),
        );
    }
    confirm(
        &format!(
            "卸载 {}\n当前目录 {}\n归档目录 {}\n插件、配置和会话文件会一起移入归档。请先退出正在使用此档案的会话。",
            sanitize(&plan.name, false),
            sanitize(&plan.root.display().to_string(), false),
            sanitize(&plan.destination.display().to_string(), false),
        ),
        yes,
    )?;
    let store = crate::operations::lock_after_recovery(ctx)?;
    if plan_archive(ctx, selector)? != plan {
        return Err(target_changed());
    }
    archive(&store, &plan)?;
    Ok(serde_json::json!({"phase":"succeeded","profile":plan.name,"archive":plan.destination}))
}

fn archive(store: &crate::operations::store::Store, plan: &ArchivePlan) -> Result<()> {
    let root = archive_root(plan)?;
    fs2::FileExt::try_lock_exclusive(root.file()).map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Error::attention(
                "agent_profile_in_use",
                "仍有会话在使用该档案。请关闭会话后重试。",
            )
        } else {
            error.into()
        }
    })?;
    store.verify_namespace()?;
    if plan.root.parent() != Some(store.path.as_path())
        || plan.destination != archive_path(&plan.root)?
    {
        return Err(target_changed());
    }
    root.verify()?;
    rustix::fs::renameat_with(
        &store.root,
        plan.root.file_name().ok_or_else(target_changed)?,
        &store.root,
        plan.destination.file_name().ok_or_else(target_changed)?,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            Error::attention(
                "agent_profile_archive_exists",
                "归档位置已有文件，原档案未移动。",
            )
        } else {
            std::io::Error::from_raw_os_error(error.raw_os_error()).into()
        }
    })?;
    store.root.sync_all()?;
    let archived = paths::Directory::open(&plan.destination)?;
    if archived.path != plan.destination
        || *archived.identity() != plan.identity
        || !plan
            .root
            .symlink_metadata()
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Err(target_changed());
    }
    let relative = plan
        .profile
        .strip_prefix(&plan.root)
        .map_err(|_| target_changed())?;
    verify_profile_directory(&plan.destination.join(relative), &plan.profile_identity)?;
    store.verify_namespace()
}

pub(crate) fn profile_label(name: &str, path: &Path) -> String {
    if managed_root(path)
        && let Some((client, profile)) = name.split_once(":mo-")
        && let Some(allocation) = path.ancestors().nth(2).and_then(Path::file_name)
        && let Some(suffix) = allocation
            .to_str()
            .and_then(|leaf| leaf.split_once('.'))
            .map(|(_, suffix)| suffix)
        && let Some(title) = profile
            .strip_suffix(suffix)
            .and_then(|name| name.strip_suffix('-'))
    {
        return format!("{} · {title}", client_title(client));
    }
    name.into()
}

fn client_title(client: &str) -> &str {
    match client {
        "codex" => "Codex",
        "claude" => "Claude Code",
        _ => client,
    }
}

pub(crate) fn command_name(client: &str, profile: &str) -> String {
    match (client, profile) {
        ("cursor", "default") => "cursor-agent".into(),
        (_, "default") => client.into(),
        _ => format!("{client}-{profile}"),
    }
}

fn profile(
    ctx: &Context,
    root: &ProfileRoot,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<Resource> {
    let client = root.client.as_str();
    let name = root.name.as_str();
    let path = root.path.as_path();
    if !std::fs::metadata(paths::read_path(path)?)?.is_dir() {
        return Err(Error::new("invalid_profile_root", "profile 根不是目录"));
    }
    let prefix = format!(".{client}-");
    let profile = if name == format!(".{client}") {
        "default"
    } else {
        name.strip_prefix(&prefix).unwrap_or(name)
    };
    let mut status = State::Present;
    let settings = path.join("settings.json");
    let model = if client == "claude" && paths::read_path(&settings)?.exists() {
        if let Ok(model) = policy::claude_model(&paths::regular_bytes(&settings)?) {
            model
        } else {
            status = State::Unknown;
            Model::default()
        }
    } else {
        Model::default()
    };
    let id = resource::id("agent", &(client, path))?;
    let mut runtime = runtime(ctx, client, profile, path, project, applications)?;
    let registered = registered_target(ctx, path)?.and_then(|target| target.command);
    let display = registered
        .as_ref()
        .and_then(|command| command.file_name()?.to_str())
        .map_or_else(|| command_name(client, profile), str::to_owned);
    if let Some(command) = registered {
        runtime.selector = "registered-command".into();
        runtime.binary = Some(command);
    }
    let mut aliases = vec![display];
    for alias in [
        format!("{client}-{profile}"),
        format!("agent:{client}:{profile}"),
    ] {
        if !aliases.contains(&alias) {
            aliases.push(alias);
        }
    }
    Ok(Resource {
        id,
        name: format!("{client}:{profile}"),
        aliases,
        management: root.management.clone(),
        status,
        data: Data::Agent {
            client: client.into(),
            profile: profile.into(),
            config_dir: path.to_path_buf(),
            model,
            runtime,
        },
    })
}

fn runtime(
    ctx: &Context,
    client: &str,
    profile: &str,
    root: &Path,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<RuntimeBinding> {
    let cwd = project.unwrap_or(&ctx.home);
    let helper = ctx.home.join(".local/bin/claudectl");
    let wrapper = ctx.home.join(format!(".local/bin/claude-{profile}"));
    let is_mise = client == "claude"
        && paths::read_path(&helper).is_ok()
        && paths::read_path(&wrapper).is_ok()
        && helper.is_file()
        && wrapper.is_file()
        && policy::legacy_mise_resolver(&paths::regular_bytes(&helper)?)
        && policy::legacy_claude_wrapper(&paths::regular_bytes(&wrapper)?, &helper, profile);
    let mut runtime = RuntimeBinding {
        selector: if is_mise {
            "inspected-mise-context"
        } else {
            "unresolved"
        }
        .into(),
        context: cwd.to_path_buf(),
        binary: None,
        install_ref: None,
        evidence: is_mise.then_some(wrapper),
    };
    if is_mise {
        let Ok(program) = crate::catalog::resource_inventory::native_program(ctx, "mise") else {
            return Ok(runtime);
        };
        let output = crate::catalog::resource_inventory::native_query(
            ctx,
            &crate::domain::CommandSpec {
                program,
                args: vec!["which".into(), "claude".into()],
            },
            cwd,
            Some(("CLAUDE_CONFIG_DIR", root)),
        )?;
        if output.code == Some(0) && !output.uncertain {
            let path = PathBuf::from(output.stdout.trim());
            if let Ok(binary) = paths::read_path(&path) {
                runtime.install_ref = policy::bind_runtime(&binary, applications);
                runtime.binary = Some(path);
            }
        }
    }
    Ok(runtime)
}
