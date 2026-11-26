use crate::{
    agent::codex_install_source::{self as source, Files, Offer},
    agent::native_plugins_files::Passive,
    agent::profile_install::{InitialActivation, Stage},
    capability::capability_local,
    context::Context,
    domain::{
        Action, CapabilityKind, CommandSpec, FileId, Journal, Kind, Observation, OwnedPath,
        Package, PathKind, Phase, Plan, Provider, QuerySpec, fingerprint,
    },
    error::{Error, Result},
    operations::native_runner::{self, FrozenInvocation, NativeOutcome},
    operations::store::Store,
    platform::fs::Directory,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

const PROTOCOL: &str = "codex-isolated-install-v1:";
const PUBLICATION: &str = "codex-isolated-receipt-v1:";
const BINARY_SHA: &str = "9248cfeebec9b4cc3034d5114caab10fbf426cbfa79dba410c5fa1b72762abd6";
const PREFIX: &str = "codex-profile.";
const CONFIG: &str = "config.toml";

struct NativeIo {
    verify_binary: fn(&mut Passive, &Path) -> Result<()>,
    acquire: fn(&Offer, &Path) -> Result<(PathBuf, Files)>,
    launch: fn(&Store, &mut Journal, &FrozenInvocation) -> Result<NativeOutcome>,
    outcome: fn(&Journal) -> Result<NativeOutcome>,
    progress: fn(Stage),
}

const NATIVE: NativeIo = NativeIo {
    verify_binary: |reader, binary| {
        if reader.binary(binary)? != BINARY_SHA {
            return Err(failed("codex_install_binary_unverified"));
        }
        Ok(())
    },
    acquire: source::acquire,
    launch: |store, journal, argv| {
        let binding = native_runner::prepare(store, journal, argv)?;
        native_runner::launch(store, journal, &binding)
    },
    outcome: native_runner::original_outcome,
    progress: |_| {},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target: Option<crate::agent::resource_agents::InstallTarget>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    initially_disabled: bool,
    root: PathBuf,
    identity: FileId,
    directories: Vec<FileId>,
    home: PathBuf,
    account_home: PathBuf,
    binary: PathBuf,
    mirror: String,
    files: Files,
    offer: Offer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    prepared: Prepared,
    market_operation: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct After {
    operation: String,
    plan: String,
    worker: Option<u32>,
    projection: String,
}

fn failed(code: &str) -> Error {
    Error::attention(code, "Codex 单独配置需核对原始操作证据")
}

fn stale() -> Error {
    failed("codex_install_evidence_changed")
}

impl Prepared {
    fn profile(&self) -> PathBuf {
        self.root.join("home/.codex")
    }

    fn repository(&self) -> PathBuf {
        self.root.join("git/objects.git")
    }

    fn market(&self) -> PathBuf {
        self.profile()
            .join(".tmp/marketplaces")
            .join(&self.offer.marketplace)
    }

    fn payload(&self) -> PathBuf {
        self.profile()
            .join("plugins/cache")
            .join(&self.offer.marketplace)
            .join(&self.offer.plugin)
            .join(&self.offer.declared_version)
    }

    fn git_config(&self) -> Result<String> {
        let url = self.offer.url()?;
        let bare = url.strip_suffix(".git").ok_or_else(stale)?;
        let repo = self.repository();
        let path = repo.to_str().ok_or_else(stale)?;
        let mut encoded = String::new();
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                use std::fmt::Write as _;
                let _ = write!(&mut encoded, "%{byte:02X}");
            }
        }
        Ok(format!(
            "[url \"file://{encoded}\"]\n\tinsteadOf = {url}\n\tinsteadOf = {bare}\n[core]\n\thooksPath = /dev/null\n"
        ))
    }

    fn allocation(&self, state: &Path) -> Result<Directory> {
        self.offer.validate()?;
        if self.root.parent() != Some(state) || !reserved_name(&self.root) {
            return Err(stale());
        }
        let root = same_directory(&self.root)?;
        let mode = root.file().metadata()?;
        if root.identity() != &self.identity
            || mode.uid() != nix::unistd::geteuid().as_raw()
            || mode.mode() & 0o7777 != 0o700
        {
            return Err(failed("codex_install_private_root_changed"));
        }
        if allocation_directories(&self.root)? != self.directories {
            return Err(failed("codex_install_private_root_changed"));
        }
        Ok(root)
    }

    fn guard(&self, state: &Path, native: &NativeIo) -> Result<()> {
        if self.account_home != account_home()? {
            return Err(stale());
        }
        self.allocation(state)?;
        let mut reader = Passive::new()?;
        (native.verify_binary)(&mut reader, &self.binary)?;
        if reader.file(&self.root.join("gitconfig"))?.as_deref()
            != Some(self.git_config()?.as_bytes())
            || reader.projected_tree(&self.repository(), None)? != self.mirror
        {
            return Err(failed("codex_install_mirror_changed"));
        }
        if same_directory(&self.profile())?
            .metadata(OsStr::new("auth.json"))?
            .is_some()
        {
            return Err(failed("codex_install_profile_in_use"));
        }
        reader.verify()
    }
}

fn account_home() -> Result<PathBuf> {
    let user = nix::unistd::User::from_uid(nix::unistd::geteuid())
        .map_err(std::io::Error::from)?
        .ok_or_else(stale)?;
    Ok(Directory::open(&user.dir)?.path.clone())
}

fn same_directory(path: &Path) -> Result<Directory> {
    let directory = Directory::open(path)?;
    if directory.path != path {
        return Err(stale());
    }
    directory.verify()?;
    Ok(directory)
}

fn reserved_name(root: &Path) -> bool {
    root.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            name.strip_prefix(PREFIX).is_some_and(|id| {
                !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric())
            })
        })
}

fn allocation_directories(root: &Path) -> Result<Vec<FileId>> {
    ["home", "home/.codex", "cwd", "tmp"]
        .iter()
        .map(|leaf| Ok(same_directory(&root.join(leaf))?.identity().clone()))
        .collect()
}

fn text(path: &Path) -> Result<String> {
    path.to_str()
        .filter(|s| !s.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(stale)
}

fn quote(path: &Path) -> Result<String> {
    Ok(serde_json::to_string(&text(path)?)?)
}

pub(crate) fn binary(ctx: &Context) -> Result<PathBuf> {
    let mise = ctx
        .home
        .join(".local/share/mise/installs/aqua-openai-codex/0.170.0/bin/codex");
    let present = crate::operations::command::executable("codex")
        .ok()
        .into_iter()
        .chain([mise]);
    for candidate in present {
        let Ok(path) = crate::platform::fs::read_path(&candidate) else {
            continue;
        };
        if Passive::new()?
            .binary(&path)
            .is_ok_and(|sha| sha == BINARY_SHA)
        {
            return Ok(path);
        }
    }
    Err(failed("codex_install_binary_unverified"))
}

fn invocation(input: &Frozen, timeout_secs: u64) -> Result<FrozenInvocation> {
    let p = &input.prepared;
    let home = p.root.join("home");
    let tmp = p.root.join("tmp");
    let policy = format!(
        "(version 1)(allow default)(deny network*)(deny file-write*)(allow file-write* (subpath {}) (subpath {}) (literal \"/dev/null\"))(deny file-read-data (require-all (require-any (subpath {}) (subpath {})) (require-not (require-any (subpath {}) (literal {})))))(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.SecurityServer\"))",
        quote(&home)?,
        quote(&tmp)?,
        quote(&p.home)?,
        quote(&p.account_home)?,
        quote(&p.root)?,
        quote(&p.binary)?,
    );
    let mut args = vec!["-p".into(), policy, text(&p.binary)?, "plugin".into()];
    if input.market_operation.is_some() {
        args.extend(["add".into(), p.offer.native_id()]);
    } else {
        args.extend([
            "marketplace".into(),
            "add".into(),
            p.offer.url()?,
            "--ref".into(),
            p.offer.marketplace_commit.clone(),
        ]);
    }
    args.push("--json".into());
    Ok(FrozenInvocation {
        command: CommandSpec {
            program: "/usr/bin/sandbox-exec".into(),
            args,
        },
        env: BTreeMap::from([
            ("HOME".into(), text(&home)?),
            ("CODEX_HOME".into(), text(&p.profile())?),
            ("TMPDIR".into(), text(&tmp)?),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("GIT_CONFIG_GLOBAL".into(), text(&p.root.join("gitconfig"))?),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_TERMINAL_PROMPT".into(), "0".into()),
            ("GIT_ALLOW_PROTOCOL".into(), "file".into()),
        ]),
        cwd: p.root.join("cwd"),
        timeout_secs,
    })
}

pub(crate) fn is_install(journal: &Journal) -> bool {
    journal.plan.delegated
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|entry| entry.starts_with(PROTOCOL))
}

fn name(journal: &Journal, purpose: &str) -> String {
    format!("codex-{}-{purpose}.json", journal.id)
}

pub(crate) fn is_publication(journal: &Journal) -> bool {
    journal.plan.action == Action::Adopt
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|entry| entry.starts_with(PUBLICATION))
}

fn frozen(journal: &Journal, state: &Directory) -> Result<Frozen> {
    let input: Frozen = Store::read(state.file(), &name(journal, "plan"))?.ok_or_else(stale)?;
    if !is_install(journal)
        || journal.plan.action != Action::Install
        || input.prepared.home != Directory::open(&journal.home)?.path
        || journal.plan.warnings.first() != Some(&format!("{PROTOCOL}{}", fingerprint(&input)?))
        || journal.plan.command.as_ref()
            != Some(&invocation(&input, journal.plan.timeout_secs)?.command)
    {
        return Err(stale());
    }
    Ok(input)
}

fn projected(p: &Prepared, installed: bool) -> Result<String> {
    let profile = same_directory(&p.profile())?;
    let (bytes, _) = profile.regular_config_snapshot(OsStr::new(CONFIG), 64 * 1024)?;
    project_config(p, &profile, &bytes, installed)
}

fn project_config(
    p: &Prepared,
    profile: &Directory,
    bytes: &[u8],
    installed: bool,
) -> Result<String> {
    let doc = std::str::from_utf8(bytes)
        .map_err(|_| stale())?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| stale())?;
    let table = doc
        .get("marketplaces")
        .and_then(toml_edit::Item::as_table_like)
        .ok_or_else(stale)?;
    let market = table
        .get(&p.offer.marketplace)
        .and_then(toml_edit::Item::as_table_like)
        .ok_or_else(stale)?;
    let field = |name| market.get(name).and_then(toml_edit::Item::as_str);
    if doc
        .iter()
        .any(|(key, _)| key != "marketplaces" && key != "plugins")
        || table.len() != 1
        || market.len() != 3
        || field("source_type") != Some("git")
        || field("source") != Some(p.offer.url()?.as_str())
        || field("ref") != Some(p.offer.marketplace_commit.as_str())
        || capability_local::codex_payload_files(&same_directory(&p.market())?)? != p.files
    {
        return Err(stale());
    }
    let plugins = crate::policy::catalog::resources::codex_plugins(bytes)?;
    if installed {
        let id = p.offer.native_id();
        let configured = doc
            .get("plugins")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|table| (table.len() == 1).then_some(table))
            .and_then(|table| table.get(&id))
            .and_then(toml_edit::Item::as_table_like);
        if plugins.len() != 1
            || plugins.get(&id) != Some(&Some(true))
            || configured.is_none_or(|table| table.len() != 1)
            || capability_local::codex_payload_files(&same_directory(&p.payload())?)? != p.files
        {
            return Err(stale());
        }
    } else if doc.get("plugins").is_some() || profile.metadata(OsStr::new("plugins"))?.is_some() {
        return Err(stale());
    }
    profile.verify()?;
    fingerprint(&(source::sha(bytes), &p.files, installed))
}

fn read_operation(state: &Directory, id: &str) -> Result<Option<Journal>> {
    if !crate::operations::store::valid_id(id) {
        return Err(stale());
    }
    let ops = state.child(OsStr::new("operations"))?;
    let record: Option<Journal> = Store::read(ops.file(), &format!("{id}.json"))?;
    if let Some(journal) = &record {
        crate::operations::store::validate_journal(journal)?;
        if journal.id != id || journal.state != state.path {
            return Err(stale());
        }
    }
    Ok(record)
}

pub(crate) fn awaiting_followup(
    ctx: &Context,
    records: &[Journal],
) -> Result<(BTreeSet<String>, bool)> {
    let mut stages = records
        .iter()
        .filter(|journal| {
            is_install(journal) && journal.home == ctx.home && journal.state == ctx.state
        })
        .peekable();
    if stages.peek().is_none() {
        return Ok((BTreeSet::new(), false));
    }
    let state = same_directory(&ctx.state)?;
    let recorded: BTreeSet<_> = records.iter().map(|journal| journal.id.as_str()).collect();
    let mut candidates = BTreeSet::new();
    let mut superseded = BTreeSet::new();
    let mut incomplete = false;
    for journal in stages {
        let Ok(input) = frozen(journal, &state) else {
            incomplete = true;
            continue;
        };
        if let Some(prior) = input.market_operation {
            superseded.insert(prior);
        }
        if journal.phase == Phase::Succeeded
            && !recorded.contains(format!("{}-receipt", journal.id).as_str())
        {
            candidates.insert(journal.id.clone());
        }
    }
    candidates.retain(|id| !superseded.contains(id));
    Ok((candidates, incomplete))
}

fn has_market_successor(
    ctx: &Context,
    state: &Directory,
    marketplace: &str,
    offer: &Offer,
) -> Result<bool> {
    let plugin_id = format!("codex-first-install:{}", offer.native_id());
    for journal in crate::operations::store::history(ctx)? {
        if journal.plan.package.id == plugin_id
            && is_install(&journal)
            && journal.scope == ctx.scope
            && crate::operations::require_recovery_roots(ctx, &journal).is_ok()
            && frozen(&journal, state)?.market_operation.as_deref() == Some(marketplace)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn observed(
    journal: &Journal,
    input: &Frozen,
    state: &Directory,
    native: &NativeIo,
) -> Result<String> {
    let after = original_projection(journal, state, native)?;
    if after != projected(&input.prepared, input.market_operation.is_some())? {
        return Err(stale());
    }
    Ok(after)
}

fn original_projection(journal: &Journal, state: &Directory, native: &NativeIo) -> Result<String> {
    let after: After = Store::read(state.file(), &name(journal, "after"))?.ok_or_else(stale)?;
    if !clean(&(native.outcome)(journal)?)
        || after.operation != journal.id
        || after.plan != journal.plan_sha256
        || after.worker != journal.process_group
        || after.worker.is_none()
    {
        return Err(stale());
    }
    Ok(after.projection)
}

fn set_enabled(bytes: &[u8], id: &str, enabled: bool) -> Result<(bool, Vec<u8>)> {
    let text = std::str::from_utf8(bytes).map_err(|_| stale())?;
    let doc = toml_edit::Document::parse(text).map_err(|_| stale())?;
    let field = doc
        .get("plugins")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|table| table.get(id))
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|table| table.get("enabled"))
        .ok_or_else(stale)?;
    let current = field.as_bool().ok_or_else(stale)?;
    let span = field.span().ok_or_else(stale)?;
    let literal = |enabled| {
        if enabled {
            b"true".as_slice()
        } else {
            b"false".as_slice()
        }
    };
    if bytes.get(span.clone()) != Some(literal(current)) {
        return Err(stale());
    }
    Ok((
        current,
        [&bytes[..span.start], literal(enabled), &bytes[span.end..]].concat(),
    ))
}

fn prove_disabled_choice(
    journal: &Journal,
    input: &Frozen,
    profile: &Directory,
    bytes: &[u8],
    state: &Directory,
    native: &NativeIo,
) -> Result<Option<Vec<u8>>> {
    let id = input.prepared.offer.native_id();
    let (enabled, original) = set_enabled(bytes, &id, true)?;
    if original_projection(journal, state, native)?
        != project_config(&input.prepared, profile, &original, true)?
    {
        return Err(stale());
    }
    let (_, disabled) = set_enabled(&original, &id, false)?;
    if enabled {
        return Ok(Some(disabled));
    }
    if disabled != bytes {
        return Err(stale());
    }
    Ok(None)
}

fn require_unpublished(
    store: &Store,
    state: &Directory,
    journal: &Journal,
    p: &Prepared,
) -> Result<()> {
    let receipt_id = format!("{}-receipt", journal.id);
    fence(store, &receipt_id)?;
    let root = p.profile();
    if Store::read::<Journal>(&store.root, "active.json")?
        .is_some_and(|active| active.id == receipt_id)
        || Store::read::<Journal>(state.file(), &name(journal, "adoption"))?.is_some()
        || read_operation(state, &receipt_id)?.is_some()
        || store.receipts()?.installations.values().any(|receipt| {
            matches!(&receipt.package.provider, Provider::CapabilityObserved { record, .. }
                if record.parent() == Some(root.as_path()))
        })
    {
        return Err(stale());
    }
    Ok(())
}

fn prepare_activation(
    store: &Store,
    state: &Directory,
    journal: &Journal,
    input: &Frozen,
    native: &NativeIo,
) -> Result<()> {
    if !input.prepared.initially_disabled {
        observed(journal, input, state, native)?;
        return Ok(());
    }
    let p = &input.prepared;
    let profile = same_directory(&p.profile())?;
    let leaf = OsStr::new(CONFIG);
    let (before, metadata) = profile.regular_config_snapshot(leaf, 64 * 1024)?;
    let Some(next) = prove_disabled_choice(journal, input, &profile, &before, state, native)?
    else {
        return Ok(());
    };
    require_unpublished(store, state, journal, p)?;
    profile.atomic_replace_regular(leaf, &next, metadata.mode, |dir| {
        require_unpublished(store, state, journal, p)?;
        p.guard(&store.path, native)?;
        let (current, stamp) = dir.regular_config_snapshot(leaf, 64 * 1024)?;
        if stamp.identity != metadata.identity || stamp.mode != metadata.mode || current != before {
            return Err(stale());
        }
        Ok(())
    })?;
    verify_activation(journal, input, state, native)
}

fn verify_activation(
    journal: &Journal,
    input: &Frozen,
    state: &Directory,
    native: &NativeIo,
) -> Result<()> {
    if input.prepared.initially_disabled {
        let profile = same_directory(&input.prepared.profile())?;
        let (bytes, _) = profile.regular_config_snapshot(OsStr::new(CONFIG), 64 * 1024)?;
        if prove_disabled_choice(journal, input, &profile, &bytes, state, native)?.is_some() {
            return Err(stale());
        }
    } else {
        observed(journal, input, state, native)?;
    }
    Ok(())
}

fn stage_before(
    input: &Frozen,
    state: &Directory,
    journal: &Journal,
    native: &NativeIo,
) -> Result<()> {
    input.prepared.guard(&journal.state, native)?;
    if let Some(id) = &input.market_operation {
        let market = read_operation(state, id)?.ok_or_else(stale)?;
        let original = frozen(&market, state)?;
        if market.phase != Phase::Succeeded
            || original.market_operation.is_some()
            || fingerprint(&original.prepared)? != fingerprint(&input.prepared)?
        {
            return Err(stale());
        }
        observed(&market, &original, state, native)?;
    } else {
        let profile = same_directory(&input.prepared.profile())?;
        for item in profile.names()? {
            let item = item.map_err(std::io::Error::from)?;
            if !matches!(item.file_name().to_bytes(), b"." | b"..") {
                return Err(stale());
            }
        }
    }
    Ok(())
}

pub(crate) fn worker_guard(journal: &Journal, argv: &FrozenInvocation) -> Result<()> {
    guard_worker(journal, argv, &NATIVE)
}

fn guard_worker(journal: &Journal, argv: &FrozenInvocation, native: &NativeIo) -> Result<()> {
    let state = same_directory(&journal.state)?;
    let input = frozen(journal, &state)?;
    if &invocation(&input, journal.plan.timeout_secs)? != argv {
        return Err(stale());
    }
    stage_before(&input, &state, journal, native)
}

fn clean(outcome: &NativeOutcome) -> bool {
    matches!(
        outcome,
        NativeOutcome::Exited {
            code: Some(0),
            signal: None
        }
    )
}

pub(crate) fn worker_after(
    journal: &Journal,
    argv: &FrozenInvocation,
    outcome: &NativeOutcome,
) -> Result<()> {
    record_worker_after(journal, argv, outcome, &NATIVE)
}

fn record_worker_after(
    journal: &Journal,
    argv: &FrozenInvocation,
    outcome: &NativeOutcome,
    native: &NativeIo,
) -> Result<()> {
    let state = same_directory(&journal.state)?;
    let input = frozen(journal, &state)?;
    if !clean(outcome) || &invocation(&input, journal.plan.timeout_secs)? != argv {
        return Err(stale());
    }
    input.prepared.guard(&journal.state, native)?;
    let projection = projected(&input.prepared, input.market_operation.is_some())?;
    Store::atomic_new(
        state.file(),
        &name(journal, "after"),
        &After {
            operation: journal.id.clone(),
            plan: journal.plan_sha256.clone(),
            worker: journal.process_group,
            projection,
        },
    )?;
    state.verify()
}

fn prepare_source(
    ctx: &Context,
    store: &Store,
    offer: Offer,
    binary: PathBuf,
    native: &NativeIo,
    activation: InitialActivation,
) -> Result<Prepared> {
    let isolated = tempfile::Builder::new()
        .prefix(PREFIX)
        .tempdir_in(&store.path)?;
    let root = isolated.path();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    for path in ["home", "home/.codex", "cwd", "tmp"] {
        let path = root.join(path);
        fs::create_dir(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    let (mirror, files) = (native.acquire)(&offer, &root.join("git"))?;
    let mut reader = Passive::new()?;
    let prepared = Prepared {
        target: None,
        initially_disabled: activation == InitialActivation::Disabled,
        root: root.to_path_buf(),
        home: Directory::open(&ctx.home)?.path.clone(),
        account_home: account_home()?,
        binary,
        identity: same_directory(root)?.identity().clone(),
        directories: allocation_directories(root)?,
        mirror: reader.projected_tree(&mirror, None)?,
        files,
        offer,
    };
    fs::write(root.join("gitconfig"), prepared.git_config()?)?;
    fs::set_permissions(root.join("gitconfig"), fs::Permissions::from_mode(0o600))?;
    reader.verify()?;
    prepared.guard(&store.path, native)?;
    store.verify_namespace()?;
    let _ = isolated.keep();
    Ok(prepared)
}

fn stage(ctx: &Context, store: &Store, input: &Frozen, native: &NativeIo) -> Result<Journal> {
    let argv = invocation(input, ctx.timeout_secs)?;
    let p = &input.prepared;
    let label = if input.market_operation.is_some() {
        p.offer.native_id()
    } else {
        p.offer.marketplace.clone()
    };
    let package = Package {
        id: format!("codex-first-install:{label}"),
        name: label,
        kind: Kind::Plugin,
        official_url: p.offer.url()?,
        provider: Provider::Macops,
        paths: vec![],
        exact_version_required: true,
    };
    let mut journal = crate::operations::new_journal(
        ctx,
        Plan {
            brew: None,
            uv_owner: None,
            npm_binding: None,
            timeout_secs: ctx.timeout_secs,
            recipe_sha256: fingerprint(&package)?,
            package,
            action: Action::Install,
            before: Observation::Missing { evidence: None },
            target: Some(p.offer.declared_version.clone()),
            command: Some(argv.command.clone()),
            query: None,
            warnings: vec![format!("{PROTOCOL}{}", fingerprint(input)?)],
            synchronous: true,
            delegated: true,
        },
    )?;
    stage_before(input, &same_directory(&store.path)?, &journal, native)?;
    Store::atomic_new(&store.root, &name(&journal, "plan"), input)?;
    store.save(&journal)?;
    (native.progress)(if input.market_operation.is_some() {
        Stage::InstallPlugin
    } else {
        Stage::RegisterMarketplace
    });
    let result = (native.launch)(store, &mut journal, &argv)?;
    settle(store, &mut journal, input, &result, native)?;
    Ok(journal)
}

fn settle(
    store: &Store,
    journal: &mut Journal,
    input: &Frozen,
    outcome: &NativeOutcome,
    native: &NativeIo,
) -> Result<()> {
    let state = same_directory(&store.path)?;
    if clean(outcome)
        && input.prepared.guard(&store.path, native).is_ok()
        && observed(journal, input, &state, native).is_ok()
    {
        journal.phase = Phase::Succeeded;
        journal.message = "单独配置中的原生安装阶段已核验".into();
    } else {
        journal.phase = Phase::Unknown;
        journal.message = "Codex 原生阶段缺少完整证据，需核对原操作".into();
    }
    store.save(journal)
}

fn adoption(ctx: &Context, p: &Prepared) -> Result<Plan> {
    let profile = p.profile();
    let profile_id = crate::domain::resource::id("agent", &("codex", &profile))?;
    let native_id = p.offer.native_id();
    let payload = p.payload();
    let package = Package {
        id: crate::capability::rules::capability_local::codex_capability_id(
            &profile_id,
            &native_id,
        )?,
        name: native_id.clone(),
        kind: Kind::Plugin,
        official_url: "https://developers.openai.com/codex/plugins".into(),
        paths: vec![OwnedPath {
            path: payload.clone(),
            kind: PathKind::Payload,
        }],
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::CodexPlugin,
            native_id,
            manager: "codex-config".into(),
            record: profile.join("config.toml"),
            payload,
            source: None,
            profile_id: Some(profile_id),
            settings: None,
        },
        exact_version_required: false,
    };
    crate::catalog::target::check_owner(ctx, &package)?;
    let query = capability_local::capture(&package)?;
    let before = capability_local::observe_default(&package.id, &query)?;
    crate::capability::rules::capability_local::adoptable_observation(&package, &before)?;
    Ok(Plan {
        brew: None,
        uv_owner: None,
        npm_binding: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Adopt,
        before,
        target: None,
        command: None,
        query: Some(QuerySpec::LocalCapability(query)),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    })
}

fn fence(store: &Store, allowed: &str) -> Result<()> {
    store.check_migration_pending()?;
    if let Some(active) = Store::read::<Journal>(&store.root, "active.json")? {
        crate::operations::store::validate_journal(&active)?;
        if crate::policy::operation::state::pending(active.phase) && active.id != allowed {
            return Err(failed("recovery_required"));
        }
    }
    store.verify_namespace()
}

fn publish(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    input: &Frozen,
    native: &NativeIo,
) -> Result<Value> {
    let state = same_directory(&store.path)?;
    let id = format!("{}-receipt", journal.id);
    fence(store, &id)?;
    if journal.phase != Phase::Succeeded || input.market_operation.is_none() {
        return Err(stale());
    }
    if let Some(target) = &input.prepared.target
        && let Some(result) =
            crate::agent::plugin_target::resume_publication(ctx, store, journal, target)?
    {
        return Ok(result);
    }
    input.prepared.guard(&store.path, native)?;
    (native.progress)(Stage::CheckActivation);
    prepare_activation(store, &state, journal, input, native)?;
    if let Some(target) = &input.prepared.target {
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
                config: p.profile().join(CONFIG),
                payload: p.payload(),
                market: Some(p.market()),
                checkout: None,
            },
        );
    }
    publish_isolated(ctx, store, journal, input, native)
}

fn publish_isolated(
    ctx: &Context,
    store: &Store,
    journal: &Journal,
    input: &Frozen,
    native: &NativeIo,
) -> Result<Value> {
    let state = same_directory(&store.path)?;
    let id = format!("{}-receipt", journal.id);
    let mut plan = adoption(ctx, &input.prepared)?;
    plan.timeout_secs = journal.plan.timeout_secs;
    plan.warnings.push(format!("{PUBLICATION}{}", journal.id));
    let old = read_operation(&state, &id)?;
    let existing = store.receipts()?.installations.remove(&plan.package.id);
    if let Some(receipt) = &existing
        && (receipt.operation_id != id
            || receipt.observation != plan.before
            || receipt.query != plan.query
            || receipt.scope.as_ref() != Some(&ctx.scope)
            || fingerprint(&receipt.package)? != plan.recipe_sha256)
    {
        return Err(failed("commit_conflict"));
    }
    let reserved: Journal =
        if let Some(record) = Store::read(state.file(), &name(journal, "adoption"))? {
            record
        } else {
            if old.is_some() || existing.is_some() {
                return Err(stale());
            }
            let record = crate::operations::new_journal_with_id(ctx, plan.clone(), id.clone())?;
            Store::atomic_new(state.file(), &name(journal, "adoption"), &record)?;
            record
        };
    crate::operations::store::validate_journal(&reserved)?;
    if reserved.id != id
        || reserved.plan_sha256 != fingerprint(&plan)?
        || reserved.scope != ctx.scope
        || reserved.phase != Phase::Prepared
    {
        return Err(stale());
    }
    let mut record = if let Some(journal) = old {
        if !matches!(journal.phase, Phase::Prepared | Phase::Succeeded)
            || journal.plan_sha256 != reserved.plan_sha256
            || journal.scope != ctx.scope
            || journal.started_at != reserved.started_at
        {
            return Err(stale());
        }
        journal
    } else {
        store.save(&reserved)?;
        reserved
    };
    if record.phase == Phase::Succeeded {
        if existing.is_none() {
            return Err(stale());
        }
        if let Some(active) = Store::read::<Journal>(&store.root, "active.json")?
            && active.id == record.id
            && crate::policy::operation::state::pending(active.phase)
        {
            store
                .publish_existing_terminal(&active)?
                .ok_or_else(stale)?;
        }
    } else {
        store
            .commit_checked(&record, record.plan.before.clone(), || {
                input.prepared.guard(&store.path, native)?;
                verify_activation(journal, input, &state, native)?;
                let Some(QuerySpec::LocalCapability(query)) = &record.plan.query else {
                    return Err(stale());
                };
                if capability_local::observe_default(&record.plan.package.id, query)?
                    != record.plan.before
                {
                    return Err(stale());
                }
                Ok(())
            })
            .map_err(|error| match error {
                crate::operations::store::CommitFailure::Check(error)
                | crate::operations::store::CommitFailure::Outcome(error) => error,
            })?;
        record.phase = Phase::Succeeded;
        record.message = "Codex 单独配置已纳入管理".into();
        store.save(&record)?;
    }
    Ok(json!({"journal":record, "codexHome":input.prepared.profile(), "loaded":"unknown"}))
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
    native_io: &NativeIo,
) -> Result<Value> {
    if resolve_failed {
        return Err(failed("codex_install_manual_resolution_unsupported"));
    }
    let id = journal
        .plan
        .warnings
        .first()
        .and_then(|entry| entry.strip_prefix(PUBLICATION))
        .ok_or_else(stale)?;
    if journal.id != format!("{id}-receipt") {
        return Err(stale());
    }
    let state = same_directory(&store.path)?;
    let native = read_operation(&state, id)?.ok_or_else(stale)?;
    let input = frozen(&native, &state)?;
    publish(ctx, store, &native, &input, native_io)
}

pub(crate) fn install(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    activation: InitialActivation,
    progress: fn(Stage),
) -> Result<Value> {
    install_using(
        ctx,
        offer,
        binary,
        &NativeIo { progress, ..NATIVE },
        activation,
    )
}

fn install_using(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    native: &NativeIo,
    activation: InitialActivation,
) -> Result<Value> {
    install_with_target(ctx, offer, binary, native, activation, None)
}

pub(crate) fn install_at(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    activation: InitialActivation,
    target: crate::agent::resource_agents::InstallTarget,
    progress: fn(Stage),
) -> Result<Value> {
    install_with_target(
        ctx,
        offer,
        binary,
        &NativeIo { progress, ..NATIVE },
        activation,
        Some(target),
    )
}

fn install_with_target(
    ctx: &Context,
    offer: Offer,
    binary: PathBuf,
    native: &NativeIo,
    activation: InitialActivation,
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
    }
    if let Some(previous) = crate::agent::plugin_target::previous_stage(ctx, |journal| {
        if target.is_none()
            && (journal.scope != ctx.scope
                || crate::operations::require_recovery_roots(ctx, journal).is_err())
        {
            return Ok(false);
        }
        matching_activation(journal, &offer, target.as_ref()).map(|mode| mode.is_some())
    })? {
        let state = same_directory(&store.path)?;
        let input = frozen(&previous, &state)?;
        let unpublished = input.market_operation.is_none()
            || read_operation(&state, &format!("{}-receipt", previous.id))?
                .is_none_or(|receipt| receipt.phase != Phase::Succeeded);
        if target.is_some() || unpublished {
            if input.prepared.initially_disabled != (activation == InitialActivation::Disabled) {
                let option = if input.prepared.initially_disabled {
                    "disabled"
                } else {
                    "yes"
                };
                return Err(Error::attention(
                    "plugin_install_activation_changed",
                    format!("上次安装使用了不同的启停设置。请重新运行安装并输入 {option}。"),
                ));
            }
            return if input.market_operation.is_some() {
                publish(ctx, &store, &previous, &input, native)
            } else {
                install_after_market(ctx, &store, input.prepared, previous.id, native)
            };
        }
    }
    (native.progress)(Stage::VerifySource);
    let mut prepared = prepare_source(ctx, &store, offer, binary, native, activation)?;
    prepared.target = target;
    let input = Frozen {
        prepared: prepared.clone(),
        market_operation: None,
    };
    let market = stage(ctx, &store, &input, native)?;
    if market.phase != Phase::Succeeded {
        return Ok(json!({"journal":market, "stage":"marketplace"}));
    }
    install_after_market(ctx, &store, prepared, market.id, native)
}

pub(crate) fn resume(
    ctx: &Context,
    original: &Journal,
    dry: bool,
    yes: bool,
    progress: fn(Stage),
) -> Result<Value> {
    resume_using(
        ctx,
        original,
        dry,
        yes,
        &NativeIo { progress, ..NATIVE },
        crate::operations::confirm,
    )
}

fn resume_using(
    ctx: &Context,
    original: &Journal,
    dry: bool,
    yes: bool,
    native: &NativeIo,
    confirm: impl FnOnce(&str, bool) -> Result<()>,
) -> Result<Value> {
    let state = same_directory(&ctx.state)?;
    let saved = frozen(original, &state)?;
    crate::operations::require_recovery_roots(ctx, original)?;
    if original.scope != ctx.scope
        || original.phase != Phase::Succeeded
        || saved.market_operation.is_some()
        || has_market_successor(ctx, &state, &original.id, &saved.prepared.offer)?
    {
        return Err(stale());
    }
    let prepared = &saved.prepared;
    prepared.guard(&ctx.state, native)?;
    observed(original, &saved, &state, native)?;
    let destination = if let Some(target) = &prepared.target {
        target.open()?.path
    } else {
        prepared.profile()
    };
    if dry {
        return Ok(json!({"mode":"dry-run","plugin":prepared.offer.native_id(),
            "commit":prepared.offer.payload_commit,"profile":destination,
            "enabled":!prepared.initially_disabled,"persistentWrites":false}));
    }
    confirm(
        &format!(
            "继续安装 {} · {} · {}\n目标 {}",
            prepared.offer.native_id(),
            prepared.offer.payload_commit,
            if prepared.initially_disabled {
                "保持停用"
            } else {
                "启用"
            },
            crate::output::sanitize(&destination.display().to_string(), false),
        ),
        yes,
    )?;
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let current = crate::operations::locked_journal(&store, &original.id)?;
    if fingerprint(&current)? != fingerprint(original)?
        || has_market_successor(ctx, &state, &original.id, &saved.prepared.offer)?
    {
        return Err(stale());
    }
    if let Some(target) = &saved.prepared.target {
        target.open()?;
    }
    install_after_market(ctx, &store, saved.prepared, current.id, native)
}

pub(crate) fn matches_prior(journal: &Journal, id: &str) -> Result<bool> {
    if !is_install(journal) {
        return Ok(false);
    }
    Ok(frozen(journal, &same_directory(&journal.state)?)?
        .market_operation
        .as_deref()
        == Some(id))
}

fn install_after_market(
    ctx: &Context,
    store: &Store,
    prepared: Prepared,
    market: String,
    native: &NativeIo,
) -> Result<Value> {
    if has_market_successor(ctx, &same_directory(&store.path)?, &market, &prepared.offer)? {
        return Err(Error::attention(
            "codex_install_next_stage_exists",
            "后续安装步骤已有记录。请运行 mo recover 查看当前状态。",
        ));
    }
    let input = Frozen {
        prepared,
        market_operation: Some(market),
    };
    let plugin = stage(ctx, store, &input, native)?;
    if plugin.phase != Phase::Succeeded {
        return Ok(json!({"journal":plugin, "stage":"plugin"}));
    }
    publish(ctx, store, &plugin, &input, native)
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

fn recover_using(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
    native: &NativeIo,
) -> Result<Value> {
    let state = same_directory(&store.path)?;
    let input = frozen(journal, &state)?;
    if journal.scope != ctx.scope || journal.state != ctx.state {
        return Err(stale());
    }
    if journal.phase == Phase::Failed {
        return Ok(json!(journal));
    }
    if crate::policy::operation::state::pending(journal.phase) {
        fence(store, &journal.id)?;
        if let Some(active) = Store::read::<Journal>(&store.root, "active.json")?
            && active.id == journal.id
            && crate::policy::operation::state::pending(active.phase)
            && let Some(terminal) = store.publish_existing_terminal(&active)?
        {
            *journal = terminal;
        }
        if journal.phase == Phase::Prepared && journal.process_group.is_none() {
            journal.phase = Phase::Failed;
            journal.message = "原安装阶段未收到启动许可".into();
            store.save(journal)?;
            return Ok(json!(journal));
        }
    }
    if crate::policy::operation::state::pending(journal.phase) {
        let outcome = (native.outcome)(journal)?;
        if outcome == NativeOutcome::Running {
            return Err(failed("operation_in_flight"));
        }
        if resolve_failed {
            if !matches!(outcome, NativeOutcome::Exited { .. }) {
                return Err(failed("codex_install_native_exit_unverified"));
            }
            crate::operations::confirm("将原安装阶段标记为失败，保留隔离档案和证据", yes)?;
            journal.phase = Phase::Failed;
            journal.message = "原进程已退出；用户确认记录失败".into();
            store.save(journal)?;
            return Ok(json!(journal));
        }
        settle(store, journal, &input, &outcome, native)?;
    }
    if journal.phase != Phase::Succeeded {
        return Ok(json!(journal));
    }
    if input.market_operation.is_some() {
        return publish(ctx, store, journal, &input, native);
    }
    let can_continue = !has_market_successor(ctx, &state, &journal.id, &input.prepared.offer)?
        && observed(journal, &input, &state, native).is_ok();
    let mut result = json!(journal);
    result["continuationRequired"] = json!(can_continue);
    result["continuationCommand"] =
        json!(can_continue.then(|| format!("mo agent resume {}", journal.id)));
    Ok(result)
}

pub(crate) fn matching_activation(
    journal: &Journal,
    offer: &Offer,
    target: Option<&crate::agent::resource_agents::InstallTarget>,
) -> Result<Option<InitialActivation>> {
    if !is_install(journal)
        || (journal.plan.package.id != format!("codex-first-install:{}", offer.marketplace)
            && journal.plan.package.id != format!("codex-first-install:{}", offer.native_id()))
    {
        return Ok(None);
    }
    let saved = frozen(journal, &same_directory(&journal.state)?)?;
    if saved.prepared.target.as_ref() != target
        || fingerprint(&saved.prepared.offer)? != fingerprint(offer)?
    {
        return Ok(None);
    }
    Ok(Some(if saved.prepared.initially_disabled {
        InitialActivation::Disabled
    } else {
        InitialActivation::Enabled
    }))
}

pub(crate) fn existing_target(
    journal: &Journal,
) -> Result<Option<crate::agent::resource_agents::InstallTarget>> {
    if !is_install(journal) {
        return Ok(None);
    }
    Ok(frozen(journal, &same_directory(&journal.state)?)?
        .prepared
        .target)
}

pub(crate) fn reserved(profile: &Path) -> bool {
    profile.file_name() == Some(OsStr::new(".codex"))
        && profile.parent().is_some_and(|home| {
            home.file_name() == Some(OsStr::new("home")) && home.parent().is_some_and(reserved_name)
        })
}

pub(crate) fn require_ordinary_adoption(package: &Package) -> Result<()> {
    if let Provider::CapabilityObserved {
        capability: CapabilityKind::CodexPlugin,
        record,
        ..
    } = &package.provider
        && let Some(parent) = record.parent()
        && reserved(&crate::platform::fs::read_path(parent)?)
    {
        return Err(failed("codex_install_profile_owned"));
    }
    Ok(())
}

struct PublishedProfile {
    name: String,
    prepared: Prepared,
    native: Journal,
    adoption: Journal,
    reservation: Journal,
    receipt: crate::domain::Receipt,
}

fn published_profiles(ctx: &Context) -> Result<Vec<PublishedProfile>> {
    let mut output = Vec::new();
    for receipt in crate::catalog::target::receipts(ctx)?
        .installations
        .into_values()
    {
        let Provider::CapabilityObserved {
            capability: CapabilityKind::CodexPlugin,
            record,
            ..
        } = &receipt.package.provider
        else {
            continue;
        };
        let Some(profile) = record.parent().filter(|profile| {
            reserved(profile) && profile.ancestors().nth(3) == Some(ctx.state.as_path())
        }) else {
            continue;
        };
        let Some(id) = receipt.operation_id.strip_suffix("-receipt") else {
            continue;
        };
        let state = same_directory(&ctx.state)?;
        let native = read_operation(&state, id)?.ok_or_else(stale)?;
        let plan = frozen(&native, &state)?;
        let adoption = read_operation(&state, &receipt.operation_id)?.ok_or_else(stale)?;
        let reserved: Journal =
            Store::read(state.file(), &name(&native, "adoption"))?.ok_or_else(stale)?;
        if native.phase == Phase::Succeeded
            && plan.market_operation.is_some()
            && plan.prepared.profile() == profile
            && is_publication(&adoption)
            && adoption.plan_sha256 == reserved.plan_sha256
            && adoption.plan.query == receipt.query
            && adoption.plan.before == receipt.observation
            && adoption.plan.package.id == receipt.package.id
            && receipt.scope.as_ref() == Some(&adoption.scope)
        {
            let leaf = plan
                .prepared
                .root
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(stale)?;
            let suffix = leaf.strip_prefix(PREFIX).ok_or_else(stale)?;
            output.push(PublishedProfile {
                name: format!(".codex-mo-{}-{suffix}", plan.prepared.offer.plugin),
                prepared: plan.prepared,
                native,
                adoption,
                reservation: reserved,
                receipt,
            });
        }
    }
    Ok(output)
}

pub(crate) fn managed_profiles(ctx: &Context) -> Result<Vec<(String, PathBuf)>> {
    Ok(published_profiles(ctx)?
        .into_iter()
        .map(|profile| (profile.name, profile.prepared.profile()))
        .collect())
}

pub(crate) fn allocation_identity(ctx: &Context, profile: &Path) -> Result<FileId> {
    published_profiles(ctx)?
        .into_iter()
        .find(|entry| entry.prepared.profile() == profile)
        .map(|entry| entry.prepared.identity)
        .ok_or_else(stale)
}

#[path = "codex_session.rs"]
pub(crate) mod session;

#[cfg(test)]
#[path = "../../tests/unit/codex_install.rs"]
mod tests;
