use super::{
    CONFIG, NATIVE, NativeIo, Prepared, PublishedProfile, published_profiles, same_directory, stale,
};
use crate::{
    agent::claude_install::{self, SessionTarget},
    agent::native_plugins_files::Passive,
    agent::resource_agents::{resolve_session_profile, session_unavailable},
    context::Context,
    domain::{FileId, Observation, Phase, QuerySpec, fingerprint},
    error::{Error, Result},
    operations::store::{self, Store},
    platform::fs::Directory,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io::{self, BufRead},
    os::unix::{fs::MetadataExt, process::CommandExt},
    path::Path,
    process::Command,
};

pub(crate) const PREFERENCE: &str = "macops-session-preference.json";

type ClaudeProfile = (Directory, Directory, SessionTarget);

pub(crate) struct SessionIo<'a> {
    codex: &'a NativeIo,
    claude: fn(&Context, &Path) -> Result<ClaudeProfile>,
}

impl SessionIo<'static> {
    pub(crate) const fn with_claude(claude: fn(&Context, &Path) -> Result<ClaudeProfile>) -> Self {
        Self {
            codex: &NATIVE,
            claude,
        }
    }
}

const LIVE: SessionIo<'static> = SessionIo::with_claude(claude_install::session_target);

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Native,
    Enabled,
    Disabled,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Native => "按原生设置",
            Self::Enabled => "启用",
            Self::Disabled => "停用",
        }
    }

    fn flag(self, plugin: &str) -> Result<Option<String>> {
        if self == Self::Native {
            return Ok(None);
        }
        if !plugin.split_once('@').is_some_and(|(name, market)| {
            [name, market]
                .into_iter()
                .all(|part| !part.contains('.') && crate::agent::native_plugins::valid_name(part))
        }) {
            return Err(Error::attention(
                "codex_session_plugin_key_unsupported",
                "此原生标识无法使用单项覆盖，请使用原生设置",
            ));
        }
        Ok(Some(format!(
            "plugins.{plugin}.enabled={}",
            self == Self::Enabled
        )))
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    schema_version: u32,
    authority: String,
    revision: u64,
    mode: Mode,
    updated_at: u64,
}

#[derive(PartialEq, Eq)]
struct FileWitness {
    identity: FileId,
    digest: String,
}

fn snapshot(directory: &Directory, name: &str, limit: u64) -> Result<(Vec<u8>, FileWitness)> {
    let (bytes, metadata) = directory.regular_config_snapshot(OsStr::new(name), limit)?;
    let witness = FileWitness {
        identity: metadata.identity,
        digest: super::source::sha(&bytes),
    };
    Ok((bytes, witness))
}

struct Selected {
    selector: String,
    name: String,
    authority: String,
    root: Directory,
    profile: Directory,
    target: Target,
    plugin: String,
    preference: Option<(Preference, FileWitness)>,
}

enum Target {
    Codex {
        prepared: Box<Prepared>,
        witness: Box<Passive>,
    },
    Claude(Box<crate::agent::claude_install::SessionTarget>),
}

impl Selected {
    fn mode(&self) -> Mode {
        self.preference
            .as_ref()
            .map_or(Mode::Native, |entry| entry.0.mode)
    }

    fn revision(&self) -> u64 {
        self.preference.as_ref().map_or(0, |entry| entry.0.revision)
    }

    fn verify_preference(&self) -> Result<()> {
        self.root.verify()?;
        let leaf = OsStr::new(PREFERENCE);
        match &self.preference {
            None if self.root.metadata(leaf)?.is_none() => Ok(()),
            Some((_, expected)) if snapshot(&self.root, PREFERENCE, 4096)?.1 == *expected => Ok(()),
            _ => Err(stale()),
        }
    }

    fn verify_scope(&self, ctx: &Context) -> Result<()> {
        match &self.target {
            Target::Codex { prepared, witness } => {
                witness.verify()?;
                prepared.allocation(&ctx.state)?;
            }
            Target::Claude(target) => target.verify(ctx)?,
        }
        self.profile.verify()?;
        self.verify_preference()
    }

    fn selection_flag(&self, mode: Mode) -> Result<()> {
        if matches!(&self.target, Target::Codex { .. }) {
            mode.flag(&self.plugin)?;
        }
        Ok(())
    }

    fn named_layer(&self, name: Option<&str>) -> Result<Option<FileWitness>> {
        if name.is_some() && matches!(&self.target, Target::Claude(_)) {
            return Err(Error::new(
                "native_profile_codex_only",
                "Claude Code 档案已在 mo 中选定；--native-profile 用于 Codex",
            ));
        }
        named_layer(&self.profile, name)
    }
}

fn preference(root: &Directory, authority: &str) -> Result<Option<(Preference, FileWitness)>> {
    if root.metadata(OsStr::new(PREFERENCE))?.is_none() {
        return Ok(None);
    }
    let (bytes, witness) = snapshot(root, PREFERENCE, 4096)?;
    let crate::policy::json::Object(record) = serde_json::from_value::<
        crate::policy::json::Object<Preference>,
    >(crate::policy::json::parse(&bytes)?)?;
    if record.schema_version != 1 || record.authority != authority || record.revision == 0 {
        return Err(stale());
    }
    Ok(Some((record, witness)))
}

fn owner(ctx: &Context, label: &str) -> Result<Option<PublishedProfile>> {
    let mut result = None;
    for item in published_profiles(ctx)? {
        let Some(name) = item.name.strip_prefix(".codex-") else {
            continue;
        };
        if label != format!("codex:{name}") {
            continue;
        }
        if result.replace(item).is_some() {
            return Err(stale());
        }
    }
    Ok(result)
}

fn verify_installation(item: &PublishedProfile, profile: &Directory) -> Result<()> {
    let receipt = &item.receipt;
    let Some(QuerySpec::LocalCapability(query)) = &receipt.query else {
        return Err(stale());
    };
    let Observation::CapabilityPack {
        pack: Some(expected),
        ..
    } = &receipt.observation
    else {
        return Err(stale());
    };
    let (bytes, before) = snapshot(profile, CONFIG, 1024 * 1024)?;
    let (_, normalized) = super::set_enabled(
        &bytes,
        &query.native_id,
        expected.configured_enabled.ok_or_else(stale)?,
    )?;
    let registration = crate::capability::rules::capability_local::parse_codex_record(
        &receipt.package.id,
        &query.record.requested,
        &query.payload.requested,
        &query.native_id,
        &normalized,
    )?
    .ok_or_else(stale)?;
    let current = crate::capability::capability_local::observe_default(&receipt.package.id, query)?;
    let Observation::CapabilityPack {
        pack: Some(actual), ..
    } = current
    else {
        return Err(stale());
    };
    if expected.record_digest != registration.digest
        || expected.payload_identity != actual.payload_identity
        || expected.payload_digest != actual.payload_digest
        || expected.file_count != actual.file_count
        || expected.revision != actual.revision
        || crate::capability::capability_local::plugin_payload_files(
            &same_directory(&item.prepared.market())?,
            crate::domain::CapabilityKind::CodexPlugin,
        )? != item.prepared.files
        || snapshot(profile, CONFIG, 1024 * 1024)?.1 != before
    {
        return Err(stale());
    }
    Ok(())
}

fn select(ctx: &Context, selector: &str, native: &SessionIo<'_>) -> Result<Selected> {
    if ctx.chezmoi.source.is_some()
        || ctx.chezmoi.destination.is_some()
        || ctx.chezmoi.config.is_some()
    {
        return Err(Error::new(
            "config_scope_unsupported",
            "当前仅选择了 chezmoi 配置范围",
        ));
    }
    let resource = resolve_session_profile(ctx, selector)?;
    if resource.client == "claude" {
        let (root, profile, target) = (native.claude)(ctx, &resource.path)?;
        return Ok(Selected {
            selector: selector.into(),
            name: resource.name,
            authority: target.authority.clone(),
            preference: preference(&root, &target.authority)?,
            root,
            profile,
            plugin: target.plugin.clone(),
            target: Target::Claude(Box::new(target)),
        });
    }
    select_codex(ctx, selector, &resource.name, &resource.path, native.codex)
}

fn select_codex(
    ctx: &Context,
    selector: &str,
    label: &str,
    path: &Path,
    native: &NativeIo,
) -> Result<Selected> {
    let item = owner(ctx, label)?.ok_or_else(session_unavailable)?;
    let adoption = &item.adoption;
    let reservation = &item.reservation;
    store::validate_journal(reservation)?;
    if adoption.phase != Phase::Succeeded
        || reservation.phase != Phase::Prepared
        || reservation.id != adoption.id
        || reservation.started_at != adoption.started_at
        || reservation.scope != adoption.scope
        || adoption.scope != ctx.scope
        || item.native.scope != adoption.scope
        || item.native.home != adoption.home
        || item.native.state != adoption.state
        || adoption.home != ctx.home
        || adoption.state != ctx.state
        || adoption.cache != ctx.cache
        || adoption.settings != ctx.settings
        || item.prepared.profile() != path
    {
        return Err(stale());
    }
    let root = item.prepared.allocation(&ctx.state)?;
    let profile = same_directory(&item.prepared.profile())?;
    let mut reader = Passive::new()?;
    (native.verify_binary)(&mut reader, &item.prepared.binary)?;
    verify_installation(&item, &profile)?;
    let authority = fingerprint(&(
        &item.native.plan_sha256,
        &item.adoption.id,
        &item.adoption.plan_sha256,
        &item.receipt,
    ))?;
    let preference = preference(&root, &authority)?;
    reader.verify()?;
    let plugin = item.prepared.offer.native_id();
    Ok(Selected {
        selector: selector.into(),
        name: label.into(),
        authority,
        root,
        profile,
        target: Target::Codex {
            prepared: Box::new(item.prepared),
            witness: Box::new(reader),
        },
        plugin,
        preference,
    })
}

fn same_selection(original: &Selected, current: &Selected) -> Result<()> {
    if original.authority != current.authority
        || original.root.identity() != current.root.identity()
        || original.profile.identity() != current.profile.identity()
        || original.preference.as_ref().map(|entry| &entry.1)
            != current.preference.as_ref().map(|entry| &entry.1)
    {
        return Err(stale());
    }
    if let (Target::Claude(original), Target::Claude(current)) = (&original.target, &current.target)
        && original.evidence != current.evidence
    {
        return Err(stale());
    }
    Ok(())
}

fn set_mode(ctx: &Context, selected: &Selected, mode: Mode, native: &SessionIo<'_>) -> Result<()> {
    selected.selection_flag(mode)?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let current = select(ctx, &selected.selector, native)?;
    same_selection(selected, &current)?;
    if current.mode() == mode {
        return Ok(());
    }
    let record = Preference {
        schema_version: 1,
        authority: current.authority.clone(),
        revision: current.revision().checked_add(1).ok_or_else(stale)?,
        mode,
        updated_at: store::now(),
    };
    let bytes = serde_json::to_vec(&record)?;
    Store::atomic_bytes_with_check(
        current.root.file(),
        PREFERENCE,
        &bytes,
        current.preference.is_none(),
        || {
            store.verify_namespace()?;
            current.verify_scope(ctx)
        },
    )
}

pub(crate) struct Details {
    pub plugin: String,
    pub preference: &'static str,
    pub observed_model: Option<String>,
}

pub(crate) fn details(ctx: &Context, label: &str) -> Option<Details> {
    details_using(ctx, label, &LIVE)
}

pub(crate) fn details_using(ctx: &Context, label: &str, native: &SessionIo<'_>) -> Option<Details> {
    let selected = select(ctx, label, native).ok()?;
    let observed_model = match &selected.target {
        Target::Codex { .. } => {
            let (bytes, _) = snapshot(&selected.profile, CONFIG, 1024 * 1024).ok()?;
            std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| text.parse::<toml_edit::Document<String>>().ok())
                .and_then(|parsed| parsed.get("model")?.as_str().map(str::to_owned))
        }
        Target::Claude(target) => target.model.clone(),
    };
    Some(Details {
        preference: selected.mode().label(),
        plugin: selected.plugin,
        observed_model,
    })
}

pub(crate) fn configure(ctx: &Context, label: &str) -> Result<()> {
    configure_using(
        ctx,
        label,
        &LIVE,
        io::stdin().lock(),
        crate::operations::confirm,
    )
}

pub(crate) fn configure_using(
    ctx: &Context,
    label: &str,
    native: &SessionIo<'_>,
    input: impl BufRead,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
) -> Result<()> {
    let selected = select(ctx, label, native)?;
    eprintln!(
        "{} · {} · 当前 {}\n1 启用  2 停用  3 按原生设置\n选择下次由 mo 启动时的插件设置（Enter 返回）",
        selected.name,
        selected.plugin,
        selected.mode().label()
    );
    let answer = input.lines().next().transpose()?.unwrap_or_default();
    let mode = match answer.trim() {
        "1" => Mode::Enabled,
        "2" => Mode::Disabled,
        "3" => Mode::Native,
        _ => return Err(Error::attention("cancelled", "操作已取消")),
    };
    selected.selection_flag(mode)?;
    confirm(
        &format!("{} · {} · {}", selected.name, selected.plugin, mode.label()),
        false,
    )?;
    crate::operations::recover_before_write(ctx, None)?;
    set_mode(ctx, &selected, mode, native)
}

fn named_layer(profile: &Directory, name: Option<&str>) -> Result<Option<FileWitness>> {
    let Some(name) = name else { return Ok(None) };
    if !crate::agent::native_plugins::valid_name(name) {
        return Err(Error::new("invalid_codex_profile", "请输入原生档案名称"));
    }
    snapshot(profile, &format!("{name}.config.toml"), 1024 * 1024).map(|(_, witness)| Some(witness))
}

fn native_command(
    selected: &Selected,
    workspace: &Directory,
    name: Option<&str>,
) -> Result<Command> {
    let (binary, profile_env) = match &selected.target {
        Target::Codex { prepared, .. } => (prepared.binary.as_path(), "CODEX_HOME"),
        Target::Claude(target) => (target.binary(), "CLAUDE_CONFIG_DIR"),
    };
    let mut command = Command::new(binary);
    command
        .env_clear()
        .current_dir(&workspace.path)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", selected.root.path.join("home"))
        .env(profile_env, &selected.profile.path)
        .env("TMPDIR", selected.root.path.join("tmp"));
    for key in ["TERM", "COLORTERM", "LANG", "LC_CTYPE"] {
        if let Ok(value) = std::env::var(key)
            && value.len() <= 128
            && !value.chars().any(char::is_control)
        {
            command.env(key, value);
        }
    }
    match &selected.target {
        Target::Codex { .. } => {
            if let Some(name) = name {
                command.args(["--profile", name]);
            }
            if let Some(flag) = selected.mode().flag(&selected.plugin)? {
                command.args(["-c", &flag]);
            }
        }
        Target::Claude(_) => {
            if selected.mode() != Mode::Native {
                command.arg("--settings").arg(
                    json!({"enabledPlugins": {&selected.plugin: selected.mode() == Mode::Enabled}})
                        .to_string(),
                );
            }
        }
    }
    Ok(command)
}

fn admit(
    ctx: &Context,
    selected: &Selected,
    workspace: &Directory,
    name: Option<&str>,
    named: Option<&FileWitness>,
    native: &SessionIo<'_>,
) -> Result<(Command, std::fs::File)> {
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let current = select(ctx, &selected.selector, native)?;
    same_selection(selected, &current)?;
    if current.named_layer(name)?.as_ref() != named {
        return Err(stale());
    }
    store.verify_namespace()?;
    current.verify_scope(ctx)?;
    workspace.verify()?;
    let lease = current.root.file().try_clone()?;
    fs2::FileExt::try_lock_shared(&lease)
        .map_err(|_| Error::attention("agent_profile_in_use", "该档案正在执行卸载或维护操作。"))?;
    rustix::io::fcntl_setfd(&lease, rustix::io::FdFlags::empty())
        .map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
    Ok((native_command(&current, workspace, name)?, lease))
}

pub(crate) fn launch(
    ctx: &Context,
    label: &str,
    name: Option<&str>,
    dry: bool,
    yes: bool,
) -> Result<Value> {
    launch_using(
        ctx,
        label,
        LaunchOptions {
            name,
            dry,
            yes,
            interactive: crate::browser::interactive_terminal(),
        },
        &LIVE,
        crate::operations::confirm,
        |mut command| Err(command.current_dir(".").exec().into()),
    )
}

#[derive(Clone, Copy)]
pub(crate) struct LaunchOptions<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) dry: bool,
    pub(crate) yes: bool,
    pub(crate) interactive: bool,
}

pub(crate) fn launch_using(
    ctx: &Context,
    label: &str,
    options: LaunchOptions<'_>,
    native: &SessionIo<'_>,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
    dispatch: impl FnOnce(Command) -> Result<Value>,
) -> Result<Value> {
    let LaunchOptions {
        name,
        dry,
        yes,
        interactive,
    } = options;
    let selected = select(ctx, label, native)?;
    selected.selection_flag(selected.mode())?;
    let named = selected.named_layer(name)?;
    let workspace = Directory::open(&std::env::current_dir()?)?;
    if dry {
        let command = native_command(&selected, &workspace, name)?;
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        return Ok(
            json!({"mode":"dry-run","profile":label,"plugin":selected.plugin,
            "preference":selected.mode(),"workspace":workspace.path,"nativeProfile":name,
            "argv":args,"persistentWrites":false}),
        );
    }
    if !interactive {
        return Err(Error::attention(
            "interactive_terminal_required",
            "启动单独配置需要交互终端",
        ));
    }
    let safe = |s: &str| crate::browser::sanitize(s, false);
    confirm(
        &format!(
            "启动 {} · {} · {}\n原生档案 {}\n工作目录 {}",
            selected.name,
            selected.plugin,
            selected.mode().label(),
            name.unwrap_or("默认"),
            safe(&workspace.path.display().to_string())
        ),
        yes,
    )?;
    crate::operations::recover_before_write(ctx, None)?;
    let (command, _lease) = admit(ctx, &selected, &workspace, name, named.as_ref(), native)?;
    let actual = std::fs::metadata(".")?;
    let inherited = FileId {
        device: actual.dev(),
        inode: actual.ino(),
    };
    if workspace.identity() != &inherited {
        return Err(stale());
    }
    dispatch(command)
}

#[cfg(test)]
#[path = "../../tests/unit/codex_session.rs"]
mod tests;
