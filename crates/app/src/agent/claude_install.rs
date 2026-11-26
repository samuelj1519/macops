use crate::{
    agent::claude_install_source::{self as source, Offer},
    agent::codex_install_source::Files,
    agent::native_plugins_files::Passive,
    agent::profile_install::{InitialActivation, Stage},
    context::Context,
    domain::{
        Action, CapabilityKind, CommandSpec, FileId, Journal, Kind, Observation, Package, Phase,
        Plan, Provider, QuerySpec, Receipt, fingerprint,
    },
    error::{Error, Result},
    operations::native_runner::{self, FrozenInvocation, NativeOutcome},
    operations::store::{self, Store},
    platform::fs::Directory,
    policy::source_tree::{Slot, Tree},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[path = "claude_install_state.rs"]
mod state;

const PROTOCOL: &str = "claude-isolated-install-v1:";
const PUBLICATION: &str = "claude-isolated-receipt-v1:";
const PREFIX: &str = "claude-profile.";
const BINARY_SHA: &str = "e85547190ef8cd983033ba61b17194045a6fea00035da349efeabed86badacc9";
const DIRECTORIES: [&str; 4] = ["home", "home/.claude", "cwd", "tmp"];

fn invalid() -> Error {
    Error::attention(
        "claude_install_evidence_changed",
        "Claude Code 单独配置的范围或原操作证据不匹配",
    )
}

fn physical(path: &Path) -> Result<Directory> {
    let dir = Directory::open(path)?;
    if dir.path != path {
        return Err(invalid());
    }
    dir.verify()?;
    Ok(dir)
}

fn label(root: &Path) -> Option<&str> {
    root.file_name()?
        .to_str()?
        .strip_prefix(PREFIX)
        .filter(|name| {
            !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

pub(crate) fn reserved(profile: &Path) -> bool {
    profile.file_name() == Some(OsStr::new(".claude"))
        && profile.parent().is_some_and(|home| {
            home.file_name() == Some(OsStr::new("home")) && home.parent().and_then(label).is_some()
        })
}

pub(crate) fn require_ordinary_adoption(package: &Package) -> Result<()> {
    if let Provider::CapabilityObserved {
        capability: CapabilityKind::ClaudePlugin,
        record,
        ..
    } = &package.provider
        && let Some(profile) = record.parent().and_then(Path::parent)
        && reserved(&crate::platform::fs::read_path(profile)?)
    {
        return Err(Error::attention(
            "claude_install_profile_owned",
            "请使用原首装收据或恢复操作",
        ));
    }
    Ok(())
}

fn account_home() -> Result<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .map_err(std::io::Error::from)?
        .ok_or_else(invalid)?;
    crate::platform::fs::read_path(&user.dir)
}

fn protected_paths(ctx: &Context) -> Result<Vec<PathBuf>> {
    [
        ctx.harness.claude.as_deref(),
        ctx.harness.codex.as_deref(),
        Some(Path::new("/Library/Keychains")),
    ]
    .into_iter()
    .flatten()
    .map(crate::platform::fs::read_path)
    .collect()
}

fn capture_directories(root: &Path) -> Result<Vec<FileId>> {
    DIRECTORIES
        .iter()
        .map(|leaf| Ok(physical(&root.join(leaf))?.identity().clone()))
        .collect()
}

fn certified(reader: &mut Passive, path: &Path) -> Result<()> {
    if reader.binary(path)? != BINARY_SHA {
        return Err(Error::attention(
            "claude_install_binary_unverified",
            "需要已核验的 Apple Silicon Claude Code 2.1.328",
        ));
    }
    Ok(())
}

pub(crate) fn binary(ctx: &Context) -> Result<PathBuf> {
    select_binary(
        ctx,
        crate::operations::command::executable("claude").ok(),
        |path| certified(&mut Passive::new()?, path),
    )
}

fn select_binary(
    ctx: &Context,
    preferred: Option<PathBuf>,
    mut verify: impl FnMut(&Path) -> Result<()>,
) -> Result<PathBuf> {
    let installed = ctx
        .scope
        .paths
        .get("MISE_DATA_DIR")
        .and_then(Clone::clone)
        .unwrap_or_else(|| ctx.home.join(".local/share/mise"))
        .join("installs/aqua-anthropics-claude-code/2.1.328/claude");
    for candidate in preferred.into_iter().chain([installed]) {
        if let Ok(path) = crate::platform::fs::read_path(&candidate)
            && (!ctx.isolated || path.starts_with(&ctx.home))
            && verify(&path).is_ok()
        {
            return Ok(path);
        }
    }
    Err(Error::attention(
        "claude_install_binary_unverified",
        "需要已核验的 Apple Silicon Claude Code 2.1.328",
    ))
}

type Checkout = (Slot, Tree, Files);

struct NativeIo {
    binary: fn(&mut Passive, &Path) -> Result<()>,
    checkout: fn(&Offer, &Directory) -> Result<Checkout>,
    launch: fn(&Store, &mut Journal, &FrozenInvocation) -> Result<NativeOutcome>,
    outcome: fn(&Journal) -> Result<NativeOutcome>,
    progress: fn(Stage),
}

const NATIVE: NativeIo = NativeIo {
    binary: certified,
    checkout: source::checkout,
    launch: |store, journal, invocation| {
        let binding = native_runner::prepare(store, journal, invocation)?;
        native_runner::launch(store, journal, &binding)
    },
    outcome: native_runner::original_outcome,
    progress: |_| {},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<crate::agent::resource_agents::InstallTarget>,
    root: PathBuf,
    identity: FileId,
    directories: Vec<FileId>,
    host_home: PathBuf,
    account_home: PathBuf,
    protected: Vec<PathBuf>,
    binary: PathBuf,
    checkout_slot: Slot,
    checkout_tree: Tree,
    files: Files,
    offer: Offer,
    enabled: bool,
}

impl Prepared {
    fn profile(&self) -> PathBuf {
        self.root.join("home/.claude")
    }
    fn checkout(&self) -> PathBuf {
        self.root.join("checkout")
    }
    fn payload(&self) -> PathBuf {
        self.profile()
            .join("plugins/cache")
            .join(&self.offer.marketplace)
            .join(&self.offer.plugin)
            .join(&self.offer.version)
    }

    fn allocation(&self, state: &Path) -> Result<Directory> {
        self.offer.validate()?;
        if self.root.parent() != Some(state)
            || label(&self.root).is_none()
            || self.account_home != account_home()?
        {
            return Err(invalid());
        }
        let root = physical(&self.root)?;
        let meta = root.file().metadata()?;
        if root.identity() != &self.identity
            || meta.uid() != nix::unistd::geteuid().as_raw()
            || meta.mode() & 0o7777 != 0o700
            || capture_directories(&self.root)? != self.directories
            || self.checkout_slot.parent.requested != self.root
            || self.checkout_slot.parent.canonical != self.root
            || self.checkout_slot.parent.identity != self.identity
            || self.checkout_slot.name != "checkout"
        {
            return Err(invalid());
        }
        if crate::source::source_tree::observe(&self.checkout_slot)?.as_ref()
            != Some(&self.checkout_tree)
        {
            return Err(invalid());
        }
        root.verify()?;
        Ok(root)
    }

    fn guard(&self, state: &Path, native: &NativeIo) -> Result<()> {
        let root = self.allocation(state)?;
        if physical(&self.profile())?
            .metadata(OsStr::new(".credentials.json"))?
            .is_some()
        {
            return Err(invalid());
        }
        let mut reader = Passive::new()?;
        (native.binary)(&mut reader, &self.binary)?;
        reader.verify()?;
        root.verify()
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Market,
    Plugin { market: String },
    Activation { plugin: String },
}

impl Step {
    fn prior(&self) -> Option<&str> {
        match self {
            Self::Market => None,
            Self::Plugin { market } => Some(market),
            Self::Activation { plugin } => Some(plugin),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    prepared: Prepared,
    step: Step,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct After {
    operation: String,
    plan: String,
    worker: Option<u32>,
    state: state::Projection,
}

fn text(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|value| !value.chars().any(char::is_control))
        .ok_or_else(invalid)
}

fn invocation(input: &Frozen, timeout_secs: u64) -> Result<FrozenInvocation> {
    let p = &input.prepared;
    let home = p.root.join("home");
    let tmp = p.root.join("tmp");
    let quote = |path: &Path| -> Result<String> { Ok(serde_json::to_string(text(path)?)?) };
    let protected = p
        .protected
        .iter()
        .map(|path| Ok(format!("(subpath {})", quote(path)?)))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let policy = format!(
        "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath {}) (subpath {}) (literal \"/dev/null\"))(deny file-read-data (require-all (require-any (subpath {}) (subpath {}) {protected}) (require-not (require-any (subpath {}) (literal {})))))(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.SecurityServer\"))",
        quote(&home)?,
        quote(&tmp)?,
        quote(&p.host_home)?,
        quote(&p.account_home)?,
        quote(&p.root)?,
        quote(&p.binary)?,
    );
    let mut args = vec![
        "-p".into(),
        policy,
        text(&p.binary)?.into(),
        "plugin".into(),
    ];
    match input.step {
        Step::Market => args.extend([
            "marketplace".into(),
            "add".into(),
            text(&p.checkout())?.into(),
        ]),
        Step::Plugin { .. } | Step::Activation { .. } => {
            let verb = match input.step {
                Step::Plugin { .. } => "install",
                Step::Activation { .. } if p.enabled => "enable",
                _ => "disable",
            };
            args.extend([
                verb.into(),
                p.offer.native_id(),
                "--scope".into(),
                "user".into(),
            ]);
        }
    }
    Ok(FrozenInvocation {
        command: CommandSpec {
            program: "/usr/bin/sandbox-exec".into(),
            args,
        },
        env: BTreeMap::from([
            ("HOME".into(), text(&home)?.into()),
            ("CLAUDE_CONFIG_DIR".into(), text(&p.profile())?.into()),
            ("TMPDIR".into(), text(&tmp)?.into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("DISABLE_TELEMETRY".into(), "1".into()),
            ("DISABLE_ERROR_REPORTING".into(), "1".into()),
        ]),
        cwd: p.root.join("cwd"),
        timeout_secs,
    })
}

fn sidecar(journal: &Journal, purpose: &str) -> String {
    format!("claude-{}-{purpose}.json", journal.id)
}

pub(crate) fn is_install(journal: &Journal) -> bool {
    journal.plan.action == Action::Install
        && journal.plan.delegated
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|tag| tag.starts_with(PROTOCOL))
}

pub(crate) fn is_publication(journal: &Journal) -> bool {
    journal.plan.action == Action::Adopt
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|tag| tag.starts_with(PUBLICATION))
}

pub(crate) fn awaiting_followup(
    ctx: &Context,
    journals: &[Journal],
) -> Result<(std::collections::BTreeSet<String>, bool)> {
    let mut stages = journals
        .iter()
        .filter(|journal| is_install(journal) && scope(ctx, journal).is_ok())
        .peekable();
    if stages.peek().is_none() {
        return Ok((std::collections::BTreeSet::new(), false));
    }
    let directory = physical(&ctx.state)?;
    let mut pending = std::collections::BTreeSet::new();
    let mut superseded = std::collections::BTreeSet::new();
    let mut incomplete = false;
    for journal in stages {
        if let Ok(input) = frozen(journal, &directory) {
            if let Some(prior) = input.step.prior() {
                superseded.insert(prior.to_owned());
            }
            if journal.phase == Phase::Succeeded {
                pending.insert(journal.id.clone());
            }
        } else {
            incomplete = true;
        }
    }
    let recorded: std::collections::BTreeSet<_> =
        journals.iter().map(|journal| journal.id.as_str()).collect();
    pending.retain(|id| {
        !superseded.contains(id) && !recorded.contains(format!("{id}-receipt").as_str())
    });
    Ok((pending, incomplete))
}

fn original(directory: &Directory, id: &str) -> Result<Option<Journal>> {
    if !store::valid_id(id) {
        return Err(invalid());
    }
    let operations = directory.child(OsStr::new("operations"))?;
    let item: Option<Journal> = Store::read(operations.file(), &format!("{id}.json"))?;
    if let Some(journal) = &item {
        store::validate_journal(journal)?;
        if journal.id != id || journal.state != directory.path {
            return Err(invalid());
        }
    }
    Ok(item)
}

fn frozen(journal: &Journal, state: &Directory) -> Result<Frozen> {
    store::validate_journal(journal)?;
    let input: Frozen =
        Store::read(state.file(), &sidecar(journal, "plan"))?.ok_or_else(invalid)?;
    if !is_install(journal)
        || journal.state != state.path
        || crate::platform::fs::read_path(&journal.home)? != input.prepared.host_home
        || journal.plan.warnings.first() != Some(&format!("{PROTOCOL}{}", fingerprint(&input)?))
        || journal.plan.command.as_ref()
            != Some(&invocation(&input, journal.plan.timeout_secs)?.command)
    {
        return Err(invalid());
    }
    Ok(input)
}

fn clean(outcome: &NativeOutcome) -> bool {
    *outcome
        == (NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
}

fn proof(journal: &Journal, state: &Directory, native: &NativeIo) -> Result<After> {
    let recorded: After =
        Store::read(state.file(), &sidecar(journal, "after"))?.ok_or_else(invalid)?;
    if recorded.operation != journal.id
        || recorded.plan != journal.plan_sha256
        || recorded.worker != journal.process_group
        || recorded.worker.is_none()
        || !clean(&(native.outcome)(journal)?)
    {
        return Err(invalid());
    }
    Ok(recorded)
}

fn predecessor(
    input: &Frozen,
    journal: &Journal,
    state: &Directory,
    native: &NativeIo,
) -> Result<Option<(Frozen, After)>> {
    let Some(id) = input.step.prior() else {
        return Ok(None);
    };
    let original = original(state, id)?.ok_or_else(invalid)?;
    if original.phase != Phase::Succeeded
        || original.scope != journal.scope
        || original.id == journal.id
    {
        return Err(invalid());
    }
    let before = frozen(&original, state)?;
    if fingerprint(&before.prepared)? != fingerprint(&input.prepared)?
        || !matches!(
            (&input.step, &before.step),
            (Step::Plugin { .. }, Step::Market) | (Step::Activation { .. }, Step::Plugin { .. })
        )
    {
        return Err(invalid());
    }
    Ok(Some((before, proof(&original, state, native)?)))
}

fn before(input: &Frozen, journal: &Journal, state: &Directory, native: &NativeIo) -> Result<()> {
    input.prepared.guard(&state.path, native)?;
    if let Some((prior, observed)) = predecessor(input, journal, state, native)? {
        if state::observe(&input.prepared, &prior.step)? != observed.state {
            return Err(invalid());
        }
    } else {
        state::empty(&input.prepared)?;
    }
    Ok(())
}

fn after(
    input: &Frozen,
    journal: &Journal,
    state: &Directory,
    native: &NativeIo,
) -> Result<state::Projection> {
    input.prepared.guard(&state.path, native)?;
    let actual = state::observe(&input.prepared, &input.step)?;
    if let Some((_, previous)) = predecessor(input, journal, state, native)?
        && (actual.market != previous.state.market
            || (matches!(input.step, Step::Activation { .. })
                && (actual.plugins != previous.state.plugins
                    || actual.payload != previous.state.payload)))
    {
        return Err(invalid());
    }
    Ok(actual)
}

pub(crate) fn worker_guard(journal: &Journal, argv: &FrozenInvocation) -> Result<()> {
    let state = physical(&journal.state)?;
    let input = frozen(journal, &state)?;
    if *argv != invocation(&input, journal.plan.timeout_secs)? {
        return Err(invalid());
    }
    before(&input, journal, &state, &NATIVE)
}

pub(crate) fn worker_after(
    journal: &Journal,
    argv: &FrozenInvocation,
    outcome: &NativeOutcome,
) -> Result<()> {
    record_after(journal, argv, outcome, &NATIVE)
}

fn record_after(
    journal: &Journal,
    argv: &FrozenInvocation,
    outcome: &NativeOutcome,
    native: &NativeIo,
) -> Result<()> {
    if !clean(outcome) {
        return Err(invalid());
    }
    let state = physical(&journal.state)?;
    let input = frozen(journal, &state)?;
    if *argv != invocation(&input, journal.plan.timeout_secs)? {
        return Err(invalid());
    }
    let verified = after(&input, journal, &state, native)?;
    Store::atomic_new(
        state.file(),
        &sidecar(journal, "after"),
        &After {
            operation: journal.id.clone(),
            plan: journal.plan_sha256.clone(),
            worker: journal.process_group,
            state: verified,
        },
    )
}

fn prepare(
    ctx: &Context,
    store: &Store,
    offer: Offer,
    binary: PathBuf,
    enabled: bool,
    native: &NativeIo,
) -> Result<Prepared> {
    offer.validate()?;
    let mut reader = Passive::new()?;
    (native.binary)(&mut reader, &binary)?;
    reader.verify()?;
    store.verify_namespace()?;
    let allocation = tempfile::Builder::new()
        .prefix(PREFIX)
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(&store.path)?;
    let root = physical(allocation.path())?;
    for path in DIRECTORIES {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root.path.join(path))?;
    }
    let config = physical(&root.path.join("home/.claude"))?;
    Store::atomic_bytes(config.file(), "settings.json", b"{}\n", true)?;
    let (checkout_slot, checkout_tree, files) = (native.checkout)(&offer, &root)?;
    if crate::capability::capability_local::plugin_payload_files(
        &physical(&root.path.join("checkout"))?,
        CapabilityKind::ClaudePlugin,
    )? != files
    {
        return Err(invalid());
    }
    let prepared = Prepared {
        target: None,
        root: root.path.clone(),
        identity: root.identity().clone(),
        directories: capture_directories(&root.path)?,
        host_home: crate::platform::fs::read_path(&ctx.home)?,
        account_home: account_home()?,
        protected: protected_paths(ctx)?,
        checkout_slot,
        checkout_tree,
        files,
        binary,
        offer,
        enabled,
    };
    prepared.guard(&store.path, native)?;
    let _ = allocation.keep();
    Ok(prepared)
}

fn stage(ctx: &Context, store: &Store, input: &Frozen, native: &NativeIo) -> Result<Journal> {
    let argv = invocation(input, ctx.timeout_secs)?;
    let id = input.prepared.offer.native_id();
    let package = Package {
        id: format!("claude-first-install:{id}"),
        name: id,
        kind: Kind::Plugin,
        official_url: input.prepared.offer.url()?,
        provider: Provider::Macops,
        paths: vec![],
        exact_version_required: true,
    };
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Install,
        before: Observation::Missing { evidence: None },
        target: Some(input.prepared.offer.version.clone()),
        command: Some(argv.command.clone()),
        query: None,
        warnings: vec![format!("{PROTOCOL}{}", fingerprint(input)?)],
        synchronous: true,
        delegated: true,
    };
    let mut journal = crate::operations::new_journal(ctx, plan)?;
    before(input, &journal, &physical(&store.path)?, native)?;
    Store::atomic_new(&store.root, &sidecar(&journal, "plan"), input)?;
    store.save(&journal)?;
    (native.progress)(match input.step {
        Step::Market => Stage::RegisterMarketplace,
        Step::Plugin { .. } => Stage::InstallPlugin,
        Step::Activation { .. } => Stage::CheckActivation,
    });
    if let Ok(outcome) = (native.launch)(store, &mut journal, &argv) {
        settle(store, &mut journal, input, &outcome, native, false)?;
    } else {
        if journal.process_group.is_some() {
            journal.phase = Phase::Unknown;
            journal.message = "Claude Code 原生阶段需核对原进程".into();
        } else {
            journal.phase = Phase::Prepared;
            journal.message = "原生阶段未取得启动许可".into();
        }
        store.save(&journal)?;
    }
    Ok(journal)
}

fn settle(
    store: &Store,
    journal: &mut Journal,
    input: &Frozen,
    outcome: &NativeOutcome,
    native: &NativeIo,
    recovering: bool,
) -> Result<()> {
    let state = physical(&store.path)?;
    let verified = || -> Result<()> {
        if !clean(outcome)
            || proof(journal, &state, native)?.state != after(input, journal, &state, native)?
        {
            return Err(invalid());
        }
        Ok(())
    };
    if verified().is_ok() {
        if recovering
            && !matches!(input.step, Step::Market)
            && !crate::agent::plugin_target::publication_recorded(store, journal)?
            && let Some(note) =
                existing_install(&input.prepared.offer, input.prepared.target.as_ref())?
        {
            journal.phase = Phase::Failed;
            journal.message = format!("本次安装未写入原配置。{note}");
            return store.save(journal);
        }
        journal.phase = Phase::Succeeded;
        journal.message = "Claude Code 原生阶段已核验".into();
    } else {
        journal.phase = Phase::Unknown;
        journal.message = match outcome {
            NativeOutcome::Exited {
                code: Some(0),
                signal: None,
            } => "Claude Code 已正常退出，但 mo 还不能确认临时目录里的安装结果。",
            NativeOutcome::Exited { .. } => {
                "Claude Code 安装命令失败。mo 保留了这次安装的临时文件。"
            }
            _ => "mo 还不能确认 Claude Code 安装命令是否执行完成。",
        }
        .into();
    }
    store.save(journal)
}

fn scope(ctx: &Context, journal: &Journal) -> Result<()> {
    if ctx.scope != journal.scope
        || ctx.home != journal.home
        || ctx.state != journal.state
        || ctx.cache != journal.cache
        || ctx.settings != journal.settings
    {
        return Err(invalid());
    }
    Ok(())
}

fn fence(store: &Store, allowed: &str) -> Result<()> {
    store.check_migration_pending()?;
    if let Some(active) = Store::read::<Journal>(&store.root, "active.json")? {
        store::validate_journal(&active)?;
        if crate::policy::operation::state::pending(active.phase) && active.id != allowed {
            return Err(Error::attention(
                "recovery_required",
                "先核对当前未收口的原操作",
            ));
        }
    }
    store.verify_namespace()
}

fn adoption(input: &Frozen, journal: &Journal) -> Result<Plan> {
    let p = &input.prepared;
    let profile = p.profile();
    let profile_id = crate::domain::resource::id("agent", &("claude", &profile))?;
    let native_id = p.offer.native_id();
    let package = Package {
        id: crate::capability::rules::capability_local::plugin_capability_id(
            &profile_id,
            &native_id,
        ),
        name: p.offer.plugin.clone(),
        kind: Kind::Plugin,
        official_url: p.offer.url()?,
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::ClaudePlugin,
            manager: "claude-plugin".into(),
            native_id,
            record: profile.join("plugins/installed_plugins.json"),
            payload: p.payload(),
            source: None,
            profile_id: Some(profile_id),
            settings: Some(profile.join("settings.json")),
        },
        paths: vec![],
        exact_version_required: false,
    };
    let query = crate::capability::capability_local::capture(&package)?;
    let before = crate::capability::capability_local::observe_default(&package.id, &query)?;
    if !matches!(&before, Observation::CapabilityPack { pack: Some(pack), .. }
        if pack.configured_enabled == Some(p.enabled))
    {
        return Err(invalid());
    }
    Ok(Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: journal.plan.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Adopt,
        before,
        target: None,
        command: None,
        query: Some(QuerySpec::LocalCapability(query)),
        warnings: vec![format!("{PUBLICATION}{}", journal.id)],
        synchronous: true,
        delegated: false,
    })
}

fn verify_installation(
    input: &Frozen,
    journal: &Journal,
    directory: &Directory,
    native: &NativeIo,
) -> Result<()> {
    if journal.phase != Phase::Succeeded || matches!(input.step, Step::Market) {
        return Err(invalid());
    }
    let actual = after(input, journal, directory, native)?;
    if actual.enabled != Some(input.prepared.enabled)
        || actual != proof(journal, directory, native)?.state
    {
        return Err(invalid());
    }
    Ok(())
}

fn matches_reservation(candidate: &Journal, reserved: &Journal) -> bool {
    candidate.id == reserved.id
        && candidate.plan_sha256 == reserved.plan_sha256
        && candidate.scope == reserved.scope
        && candidate.home == reserved.home
        && candidate.state == reserved.state
        && candidate.cache == reserved.cache
        && candidate.settings == reserved.settings
        && candidate.started_at == reserved.started_at
}

fn publish(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    input: &Frozen,
    native: &NativeIo,
) -> Result<Value> {
    if let Some(target) = &input.prepared.target {
        if let Some(result) =
            crate::agent::plugin_target::resume_publication(ctx, store, journal, target)?
        {
            return Ok(result);
        }
        verify_installation(input, journal, &physical(&store.path)?, native)?;
        let p = &input.prepared;
        (native.progress)(Stage::Publish);
        return crate::agent::plugin_target::publish(
            ctx,
            store,
            journal,
            target,
            &crate::agent::plugin_target::InstalledPlugin {
                native_id: p.offer.native_id(),
                marketplace: &p.offer.marketplace,
                offer: fingerprint(&p.offer)?,
                config: p.profile().join("settings.json"),
                payload: p.payload(),
                market: None,
                checkout: Some(&(p.checkout_slot.clone(), p.checkout_tree.clone())),
            },
        );
    }
    let directory = physical(&store.path)?;
    let id = format!("{}-receipt", journal.id);
    scope(ctx, journal)?;
    fence(store, &id)?;
    verify_installation(input, journal, &directory, native)?;
    let plan = adoption(input, journal)?;
    let historical = original(&directory, &id)?;
    let recorded = store.receipts()?.installations.remove(&plan.package.id);
    if let Some(receipt) = &recorded
        && (receipt.operation_id != id
            || receipt.query != plan.query
            || receipt.observation != plan.before
            || receipt.scope.as_ref() != Some(&ctx.scope)
            || fingerprint(&receipt.package)? != plan.recipe_sha256)
    {
        return Err(invalid());
    }
    let name = sidecar(journal, "adoption");
    let reserved: Journal = if let Some(reserved) = Store::read(&store.root, &name)? {
        reserved
    } else {
        if historical.is_some() || recorded.is_some() {
            return Err(invalid());
        }
        let reserved = crate::operations::new_journal_with_id(ctx, plan.clone(), id.clone())?;
        Store::atomic_new(&store.root, &name, &reserved)?;
        reserved
    };
    store::validate_journal(&reserved)?;
    scope(ctx, &reserved)?;
    if reserved.id != id
        || reserved.phase != Phase::Prepared
        || reserved.plan_sha256 != fingerprint(&plan)?
    {
        return Err(invalid());
    }
    let mut record = if let Some(historical) = historical {
        if !matches!(historical.phase, Phase::Prepared | Phase::Succeeded)
            || !matches_reservation(&historical, &reserved)
        {
            return Err(invalid());
        }
        historical
    } else {
        store.save(&reserved)?;
        reserved
    };
    finish_publication(
        ctx,
        store,
        journal,
        input,
        native,
        &mut record,
        recorded.is_some(),
    )?;
    Ok(json!({"journal":record,"claudeConfigDir":input.prepared.profile(),"loaded":"unknown"}))
}

fn finish_publication(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    input: &Frozen,
    native: &NativeIo,
    record: &mut Journal,
    had_receipt: bool,
) -> Result<()> {
    if record.phase == Phase::Succeeded {
        if !had_receipt {
            return Err(invalid());
        }
        if let Some(active) = Store::read::<Journal>(&store.root, "active.json")?
            && active.id == record.id
            && crate::policy::operation::state::pending(active.phase)
        {
            store
                .publish_existing_terminal(&active)?
                .ok_or_else(invalid)?;
        }
        return Ok(());
    }
    store
        .commit_checked(record, record.plan.before.clone(), || {
            fence(store, &record.id)?;
            verify_installation(input, journal, &physical(&store.path)?, native)?;
            if adoption(input, journal)?.before != record.plan.before {
                return Err(invalid());
            }
            scope(ctx, record)
        })
        .map_err(|error| match error {
            crate::operations::store::CommitFailure::Check(e)
            | crate::operations::store::CommitFailure::Outcome(e) => e,
        })?;
    record.phase = Phase::Succeeded;
    record.message = "Claude Code 单独配置已纳入管理".into();
    store.save(record)
}

pub(crate) fn install(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    enabled: bool,
    progress: fn(Stage),
) -> Result<Value> {
    install_using(
        ctx,
        offer,
        binary,
        enabled,
        &NativeIo { progress, ..NATIVE },
    )
}

fn install_using(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    enabled: bool,
    native: &NativeIo,
) -> Result<Value> {
    install_with_target(ctx, offer, binary, enabled, native, None)
}

pub(crate) fn install_at(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    enabled: bool,
    target: crate::agent::resource_agents::InstallTarget,
    progress: fn(Stage),
) -> Result<Value> {
    install_with_target(
        ctx,
        offer,
        binary,
        enabled,
        &NativeIo { progress, ..NATIVE },
        Some(target),
    )
}

fn install_with_target(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    enabled: bool,
    native: &NativeIo,
    target: Option<crate::agent::resource_agents::InstallTarget>,
) -> Result<Value> {
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    if let Some(target) = &target {
        target.open()?;
        if let Some(done) =
            crate::agent::plugin_target::previous(ctx, &store, target, &fingerprint(&offer)?)?
        {
            return Ok(done);
        }
        if let Some(note) = existing_install(&offer, Some(target))? {
            return Ok(
                json!({"skipped":true,"message":note,"targetName":target.name,
                "profile": target.directory.canonical}),
            );
        }
        if let Some(previous) = crate::agent::plugin_target::previous_stage(ctx, |journal| {
            matching_activation(journal, &offer, Some(target)).map(|mode| mode.is_some())
        })? {
            let input = frozen(&previous, &physical(&store.path)?)?;
            if input.prepared.enabled != enabled {
                return Err(invalid());
            }
            return match input.step {
                Step::Market => {
                    install_after_market(ctx, &store, input.prepared, previous.id, native)
                }
                Step::Plugin { .. } => install_after_plugin(ctx, &store, previous, input, native),
                Step::Activation { .. } => publish(ctx, &store, &previous, &input, native),
            };
        }
    }
    (native.progress)(Stage::VerifySource);
    let mut prepared = prepare(ctx, &store, offer, binary, enabled, native)?;
    prepared.target = target;
    let market = stage(
        ctx,
        &store,
        &Frozen {
            prepared: prepared.clone(),
            step: Step::Market,
        },
        native,
    )?;
    if market.phase != Phase::Succeeded {
        return Ok(json!({"journal":market,"stage":"marketplace"}));
    }
    install_after_market(ctx, &store, prepared, market.id, native)
}

fn existing_install(
    offer: &Offer,
    target: Option<&crate::agent::resource_agents::InstallTarget>,
) -> Result<Option<String>> {
    let Some(target) = target else {
        return Ok(None);
    };
    crate::agent::plugin_target::claude_first_install_conflict(
        target,
        &offer.native_id(),
        &offer.marketplace,
        &offer.version,
    )
}

fn install_after_market(
    ctx: &Context,
    store: &Store,
    prepared: Prepared,
    market: String,
    native: &NativeIo,
) -> Result<Value> {
    let plugin = Frozen {
        prepared,
        step: Step::Plugin { market },
    };
    let installed = stage(ctx, store, &plugin, native)?;
    if installed.phase != Phase::Succeeded {
        return Ok(json!({"journal":installed,"stage":"plugin"}));
    }
    install_after_plugin(ctx, store, installed, plugin, native)
}

fn install_after_plugin(
    ctx: &Context,
    store: &Store,
    installed: Journal,
    plugin: Frozen,
    native: &NativeIo,
) -> Result<Value> {
    let outcome = proof(&installed, &physical(&store.path)?, native)?;
    if outcome.state.enabled == Some(plugin.prepared.enabled) {
        return publish(ctx, store, &installed, &plugin, native);
    }
    let activation = Frozen {
        prepared: plugin.prepared,
        step: Step::Activation {
            plugin: installed.id,
        },
    };
    let configured = stage(ctx, store, &activation, native)?;
    if configured.phase != Phase::Succeeded {
        return Ok(json!({"journal":configured,"stage":"activation"}));
    }
    publish(ctx, store, &configured, &activation, native)
}

struct Continuation {
    journal: Journal,
    input: Frozen,
}

fn continuation(ctx: &Context, id: &str, native: &NativeIo) -> Result<Continuation> {
    let directory = physical(&ctx.state)?;
    let journal = original(&directory, id)?.ok_or_else(invalid)?;
    scope(ctx, &journal)?;
    if journal.phase != Phase::Succeeded {
        return Err(invalid());
    }
    let input = frozen(&journal, &directory)?;
    let completed = proof(&journal, &directory, native)?.state;
    if matches!(input.step, Step::Activation { .. })
        || input.prepared.protected != protected_paths(ctx)?
        || completed.enabled == Some(input.prepared.enabled)
        || completed != after(&input, &journal, &directory, native)?
    {
        return Err(invalid());
    }
    for later in store::history(ctx)? {
        if is_install(&later)
            && later.id != id
            && frozen(&later, &directory)?.step.prior() == Some(id)
        {
            return Err(Error::attention(
                "claude_install_next_stage_exists",
                "后续原生阶段已有记录，请使用它的操作标识核对",
            ));
        }
    }
    Ok(Continuation { journal, input })
}

pub(crate) fn resume(
    ctx: &Context,
    id: &str,
    dry: bool,
    yes: bool,
    progress: fn(Stage),
) -> Result<Value> {
    resume_using(
        ctx,
        id,
        dry,
        yes,
        &NativeIo { progress, ..NATIVE },
        crate::operations::confirm,
    )
}

fn resume_using(
    ctx: &Context,
    id: &str,
    dry: bool,
    yes: bool,
    native: &NativeIo,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
) -> Result<Value> {
    let first = continuation(ctx, id, native)?;
    let p = &first.input.prepared;
    let step = if matches!(first.input.step, Step::Market) {
        "安装插件"
    } else {
        "设置初始启停"
    };
    if dry {
        return Ok(json!({"mode":"dry-run","operation":id,"action":step,
            "profile":p.profile(),"plugin":p.offer.native_id(),"commit":p.offer.commit,
            "enabled":p.enabled,"persistentWrites":false}));
    }
    confirm(
        &format!(
            "继续原操作 {} · {} · {}\n档案 {}\n来源提交 {}\n初始设置 {}",
            id,
            p.offer.native_id(),
            step,
            crate::browser::sanitize(text(&p.profile())?, false),
            p.offer.commit,
            if p.enabled { "启用" } else { "停用" },
        ),
        yes,
    )?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let current = continuation(ctx, id, native)?;
    if fingerprint(&first.journal)? != fingerprint(&current.journal)?
        || fingerprint(&first.input)? != fingerprint(&current.input)?
    {
        return Err(invalid());
    }
    match current.input.step {
        Step::Market => install_after_market(
            ctx,
            &store,
            current.input.prepared,
            current.journal.id,
            native,
        ),
        Step::Plugin { .. } => {
            install_after_plugin(ctx, &store, current.journal, current.input, native)
        }
        Step::Activation { .. } => Err(invalid()),
    }
}

pub(crate) fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    recover_using(ctx, store, journal, resolve_failed, yes, &NATIVE)
}

pub(crate) fn matching_activation(
    journal: &Journal,
    offer: &Offer,
    target: Option<&crate::agent::resource_agents::InstallTarget>,
) -> Result<Option<InitialActivation>> {
    if !is_install(journal)
        || journal.plan.package.id != format!("claude-first-install:{}", offer.native_id())
    {
        return Ok(None);
    }
    let saved = frozen(journal, &physical(&journal.state)?)?;
    if saved.prepared.target.as_ref() != target
        || fingerprint(&saved.prepared.offer)? != fingerprint(offer)?
    {
        return Ok(None);
    }
    Ok(Some(if saved.prepared.enabled {
        InitialActivation::Enabled
    } else {
        InitialActivation::Disabled
    }))
}

pub(crate) fn matches_prior(journal: &Journal, id: &str) -> Result<bool> {
    if !is_install(journal) {
        return Ok(false);
    }
    Ok(frozen(journal, &physical(&journal.state)?)?.step.prior() == Some(id))
}

pub(crate) fn existing_target(
    journal: &Journal,
) -> Result<Option<crate::agent::resource_agents::InstallTarget>> {
    if !is_install(journal) {
        return Ok(None);
    }
    Ok(frozen(journal, &physical(&journal.state)?)?.prepared.target)
}

pub(crate) fn registered_checkout_repository(
    ctx: &Context,
    profile: &Path,
    native_id: &str,
    checkout: &Path,
) -> Result<Option<crate::policy::source_git::Repository>> {
    Ok(
        registered_checkout_source(ctx, profile, native_id, checkout)?
            .map(|(repository, _)| repository),
    )
}

pub(crate) fn registered_checkout_source(
    ctx: &Context,
    profile: &Path,
    native_id: &str,
    checkout: &Path,
) -> Result<Option<(crate::policy::source_git::Repository, String)>> {
    if !checkout
        .parent()
        .is_some_and(|root| root.parent() == Some(ctx.state.as_path()) && label(root).is_some())
        || checkout.file_name() != Some(OsStr::new("checkout"))
    {
        return Ok(None);
    }
    let package_id = format!("claude-first-install:{native_id}");
    for journal in store::history(ctx)? {
        if journal.phase != Phase::Succeeded
            || !is_install(&journal)
            || journal.plan.package.id != package_id
            || !journal.plan.command.as_ref().is_some_and(|command| {
                matches!(command.args.as_slice(), [.., market, add, path]
                    if market == "marketplace" && add == "add" && Path::new(path) == checkout)
            })
        {
            continue;
        }
        if ctx.home != journal.home || ctx.state != journal.state {
            return Err(invalid());
        }
        let input = frozen(&journal, &physical(&ctx.state)?)?;
        let installed = &input.prepared;
        if !matches!(input.step, Step::Market)
            || installed.checkout() != checkout
            || installed.offer.native_id() != native_id
        {
            return Err(invalid());
        }
        let same_profile = if let Some(target) = &installed.target {
            target.directory.canonical == profile && target.open()?.path == profile
        } else {
            installed.profile() == profile
        };
        if same_profile {
            if crate::source::source_tree::observe(&installed.checkout_slot)?.as_ref()
                != Some(&installed.checkout_tree)
            {
                return Err(invalid());
            }
            return Ok(Some((
                crate::policy::source_git::Repository::parse(&installed.offer.repository)?,
                installed.offer.commit.clone(),
            )));
        }
    }
    Ok(None)
}

fn recover_using(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
    native: &NativeIo,
) -> Result<Value> {
    scope(ctx, journal)?;
    let directory = physical(&store.path)?;
    let input = frozen(journal, &directory)?;
    if crate::policy::operation::state::pending(journal.phase) {
        fence(store, &journal.id)?;
        if let Some(terminal) = store.publish_existing_terminal(journal)? {
            *journal = terminal;
        }
    }
    if crate::policy::operation::state::pending(journal.phase) {
        finish_original(store, journal, &input, resolve_failed, yes, native)?;
    }
    if journal.phase == Phase::Succeeded
        && !matches!(input.step, Step::Market)
        && proof(journal, &directory, native)?.state.enabled == Some(input.prepared.enabled)
    {
        return publish(ctx, store, journal, &input, native);
    }
    let continuation =
        journal.phase == Phase::Succeeded && continuation(ctx, &journal.id, native).is_ok();
    Ok(
        json!({"journal":journal,"continuationRequired":continuation,
        "continuationCommand":continuation.then(|| format!("mo agent resume {}", journal.id))}),
    )
}

fn finish_original(
    store: &Store,
    journal: &mut Journal,
    input: &Frozen,
    resolve_failed: bool,
    yes: bool,
    native: &NativeIo,
) -> Result<()> {
    if journal.phase == Phase::Prepared && journal.process_group.is_none() {
        journal.phase = Phase::Failed;
        journal.message = "原 Claude Code 操作未收到启动许可".into();
        return store.save(journal);
    }
    let outcome = (native.outcome)(journal)?;
    if outcome == NativeOutcome::Running {
        return Err(Error::attention("operation_in_flight", "原进程仍在运行"));
    }
    let exited_failed = matches!(outcome, NativeOutcome::Exited { code: Some(code), .. } if code != 0)
        || matches!(
            outcome,
            NativeOutcome::Exited {
                signal: Some(_),
                ..
            }
        );
    if resolve_failed {
        if !exited_failed {
            return Err(Error::attention(
                "claude_install_original_exit_unverified",
                "需要原进程的失败退出证据",
            ));
        }
        crate::operations::confirm("将已确认失败的原阶段记为失败，保留临时档案和证据", yes)?;
        journal.phase = Phase::Failed;
        journal.message = "原 Claude Code 进程已失败退出".into();
        return store.save(journal);
    }
    let state = physical(&store.path)?;
    let absent_proof = state
        .metadata(OsStr::new(&sidecar(journal, "after")))?
        .is_none();
    if exited_failed
        && absent_proof
        && journal.process_group.is_some()
        && !crate::agent::plugin_target::publication_recorded(store, journal)?
        && input.prepared.guard(&store.path, native).is_ok()
    {
        journal.phase = Phase::Failed;
        journal.message = "上次 Claude Code 安装命令已失败退出，未写入目标配置。".into();
        return store.save(journal);
    }
    if clean(&outcome) && journal.process_group.is_some() && absent_proof {
        // The original worker may have exited between recording its status and
        // verifying the private staging directory. Run the same verifier.
        let argv = invocation(input, journal.plan.timeout_secs)?;
        if record_after(journal, &argv, &outcome, native).is_err() {
            journal.message = "Claude Code 已正常退出，但临时目录里的插件与安装计划对不上。".into();
            journal.phase = Phase::Unknown;
            return store.save(journal);
        }
    }
    settle(store, journal, input, &outcome, native, true)
}

pub(crate) fn recover_publication(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    resolve_failed: bool,
) -> Result<Value> {
    recover_publication_using(ctx, store, journal, resolve_failed, &NATIVE)
}

fn recover_publication_using(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    resolve_failed: bool,
    native: &NativeIo,
) -> Result<Value> {
    scope(ctx, journal)?;
    if resolve_failed {
        return Err(invalid());
    }
    let id = journal
        .plan
        .warnings
        .first()
        .and_then(|label| label.strip_prefix(PUBLICATION))
        .ok_or_else(invalid)?;
    if journal.id != format!("{id}-receipt") {
        return Err(invalid());
    }
    let directory = physical(&store.path)?;
    let native_journal = original(&directory, id)?.ok_or_else(invalid)?;
    let input = frozen(&native_journal, &directory)?;
    publish(ctx, store, &native_journal, &input, native)
}

struct PublishedProfile {
    name: String,
    native: Journal,
    adoption: Journal,
    prepared: Prepared,
    receipt: Receipt,
}

fn published_profiles(ctx: &Context, selected: Option<&Path>) -> Result<Vec<PublishedProfile>> {
    let mut profiles = vec![];
    for receipt in crate::catalog::target::receipts(ctx)?
        .installations
        .into_values()
    {
        let Provider::CapabilityObserved {
            capability: CapabilityKind::ClaudePlugin,
            record,
            ..
        } = &receipt.package.provider
        else {
            continue;
        };
        let Some(profile) = record
            .parent()
            .and_then(Path::parent)
            .filter(|path| reserved(path) && path.ancestors().nth(3) == Some(ctx.state.as_path()))
        else {
            continue;
        };
        if selected.is_some_and(|selected| selected != profile) {
            continue;
        }
        let Some(native_id) = receipt.operation_id.strip_suffix("-receipt") else {
            continue;
        };
        let directory = physical(&ctx.state)?;
        let native = original(&directory, native_id)?.ok_or_else(invalid)?;
        let input = frozen(&native, &directory)?;
        let Some(adopted) = original(&directory, &receipt.operation_id)? else {
            continue;
        };
        let reservation: Journal =
            Store::read(directory.file(), &sidecar(&native, "adoption"))?.ok_or_else(invalid)?;
        store::validate_journal(&reservation)?;
        if native.phase == Phase::Succeeded
            && adopted.phase == Phase::Succeeded
            && is_publication(&adopted)
            && reservation.phase == Phase::Prepared
            && native.scope == adopted.scope
            && adopted
                .plan
                .warnings
                .first()
                .and_then(|tag| tag.strip_prefix(PUBLICATION))
                == Some(native_id)
            && matches_reservation(&adopted, &reservation)
            && input.prepared.profile() == profile
            && !matches!(input.step, Step::Market)
            && receipt.scope.as_ref() == Some(&adopted.scope)
            && receipt.query == adopted.plan.query
            && receipt.observation == adopted.plan.before
            && receipt.package.id == adopted.plan.package.id
        {
            let suffix = label(&input.prepared.root).ok_or_else(invalid)?;
            profiles.push(PublishedProfile {
                name: format!(".claude-mo-{}-{suffix}", input.prepared.offer.plugin),
                native,
                adoption: adopted,
                prepared: input.prepared,
                receipt,
            });
        }
    }
    Ok(profiles)
}

pub(crate) fn managed_profiles(ctx: &Context) -> Result<Vec<(String, PathBuf)>> {
    Ok(published_profiles(ctx, None)?
        .into_iter()
        .map(|entry| (entry.name, entry.prepared.profile()))
        .collect())
}

pub(crate) fn allocation_identity(ctx: &Context, profile: &Path) -> Result<FileId> {
    published_profiles(ctx, Some(profile))?
        .into_iter()
        .find(|entry| entry.prepared.profile() == profile)
        .map(|entry| entry.prepared.identity)
        .ok_or_else(invalid)
}

pub(crate) struct SessionTarget {
    pub(crate) plugin: String,
    pub(crate) authority: String,
    pub(crate) model: Option<String>,
    pub(crate) evidence: String,
    prepared: Prepared,
    reader: Passive,
}

impl SessionTarget {
    pub(crate) fn binary(&self) -> &Path {
        &self.prepared.binary
    }

    pub(crate) fn verify(&self, ctx: &Context) -> Result<()> {
        self.prepared.allocation(&ctx.state)?;
        self.reader.verify()
    }
}

pub(crate) fn session_target(
    ctx: &Context,
    path: &Path,
) -> Result<(Directory, Directory, SessionTarget)> {
    session_target_using(ctx, path, certified)
}

fn session_target_using(
    ctx: &Context,
    path: &Path,
    binary: fn(&mut Passive, &Path) -> Result<()>,
) -> Result<(Directory, Directory, SessionTarget)> {
    let mut profiles = published_profiles(ctx, Some(path))?;
    if profiles.len() != 1 {
        return Err(invalid());
    }
    let entry = profiles.pop().ok_or_else(invalid)?;
    scope(ctx, &entry.native)?;
    scope(ctx, &entry.adoption)?;
    let root = entry.prepared.allocation(&ctx.state)?;
    let profile = physical(path)?;
    let authority = fingerprint(&(
        &entry.native.plan_sha256,
        &entry.adoption.id,
        &entry.adoption.plan_sha256,
        &entry.receipt,
    ))?;
    let mut reader = Passive::new()?;
    binary(&mut reader, &entry.prepared.binary)?;
    let Observation::CapabilityPack {
        pack: Some(registered),
        ..
    } = &entry.receipt.observation
    else {
        return Err(invalid());
    };
    let (model, evidence) =
        state::published(&entry.prepared, &registered.payload_identity, &mut reader)?;
    reader.verify()?;
    Ok((
        root,
        profile,
        SessionTarget {
            plugin: entry.prepared.offer.native_id(),
            authority,
            model,
            evidence,
            prepared: entry.prepared,
            reader,
        },
    ))
}

#[cfg(test)]
#[path = "../../tests/unit/claude_install.rs"]
mod tests;
