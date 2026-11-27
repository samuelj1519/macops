use crate::{
    agent::native_plugins::{Instance, RECIPE_FILE, Registry, Source, refusal},
    agent::native_plugins_files::{self as files, Passive, Payload, Registration, Stamp},
    context::Context,
    domain::{
        Action, CommandSpec, Journal, Kind, Observation, Package, Phase, Plan, Provider,
        fingerprint,
    },
    error::Result,
    operations::native_runner::{self, FrozenInvocation, NativeOutcome},
    operations::store::Store,
    platform::fs::{Directory, EntryKind},
    policy::governance::DirectoryBinding,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const PROTOCOL: &str = "native-plugin-v1:";
const CODEX_ARM64_0169: &str = "739cda86e15d30a6e2b40cf8dcbbfb988aaf48b305b27065573bb8e9fd41d002";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    scope: String,
    source: String,
    selected: String,
    unrelated: String,
    config: String,
    version: String,
}

impl Snapshot {
    fn accepts(&self, after: &Self) -> bool {
        self.scope == after.scope
            && self.source == after.source
            && self.unrelated == after.unrelated
            && self.config == after.config
            && self.version == after.version
            && self.source == after.selected
    }
}

#[derive(Debug)]
struct Captured {
    before: Snapshot,
    invocation: FrozenInvocation,
    native_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Frozen {
    key: String,
    registry: PathBuf,
    before: Snapshot,
    state: DirectoryBinding,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct After {
    operation: String,
    plan: String,
    worker: Option<u32>,
    observed: Snapshot,
}

pub fn certified(instance: &Instance) -> bool {
    instance.version == "0.169.0" && instance.binary_sha256 == CODEX_ARM64_0169
}

fn changed() -> crate::error::Error {
    refusal("native_plugin_input_changed")
}

fn intersects(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn bound_directory(reader: &mut Passive, path: &Path) -> Result<DirectoryBinding> {
    let dir = reader.directory(path)?.ok_or_else(changed)?;
    crate::capability::capability_store::binding(&dir, path)
}

fn disjoint_storage(
    reader: &mut Passive,
    registry: &Registry,
    target: &Instance,
) -> Result<Vec<(PathBuf, Option<crate::domain::FileId>)>> {
    let mut storage = vec![];
    for (id, sibling) in &registry.instances {
        if id == "codex" {
            continue;
        }
        for path in [&sibling.config_root, &sibling.plugin_root()] {
            if intersects(path, &target.config_root) {
                return Err(refusal("shared_native_storage_unsupported"));
            }
            let footprint = reader
                .directory(path)
                .map_err(|_| refusal("shared_native_storage_unsupported"))?;
            storage.push((
                path.clone(),
                footprint.as_ref().map(|root| root.identity().clone()),
            ));
        }
    }
    Ok(storage)
}

fn safe_environment(
    reader: &mut Passive,
    ctx: &Instance,
    state: &Path,
    source: &Path,
) -> Result<()> {
    if intersects(state, &ctx.config_root)
        || intersects(source, &ctx.config_root)
        || intersects(source, state)
        || intersects(&ctx.cwd, &ctx.config_root)
        || intersects(&ctx.cwd, source)
        || intersects(&ctx.cwd, state)
    {
        return Err(refusal("native_plugin_overlapping_paths"));
    }
    let cwd = reader.directory(&ctx.cwd)?.ok_or_else(changed)?;
    for ancestor in cwd.path.ancestors() {
        let dir = if ancestor == Path::new("/") {
            Directory::open(ancestor)?
        } else {
            reader.directory(ancestor)?.ok_or_else(changed)?
        };
        if dir.metadata(OsStr::new(".git"))?.is_some()
            || dir.metadata(OsStr::new(".codex"))?.is_some()
        {
            return Err(refusal("native_plugin_project_scope_unproven"));
        }
    }
    let config = reader.directory(&ctx.config_root)?.ok_or_else(changed)?;
    if config.metadata(OsStr::new("auth.json"))?.is_some() {
        return Err(refusal("native_plugin_auth_scope_unproven"));
    }
    Ok(())
}

fn config_fingerprint(reader: &mut Passive, path: &Path) -> Result<String> {
    let (_, stamp) = reader.stamped_file(path)?;
    let Stamp::File(_, mode, sha256) = stamp else {
        return Err(changed());
    };
    fingerprint(&(*mode, sha256))
}

fn invocation(
    instance: &Instance,
    native_id: &str,
    state: &Path,
    timeout: u64,
) -> Result<FrozenInvocation> {
    let text = |path: &Path| path.to_str().map(str::to_owned).ok_or_else(changed);
    Ok(FrozenInvocation {
        command: CommandSpec {
            program: instance.binary.clone(),
            args: vec![
                "plugin".into(),
                "add".into(),
                native_id.into(),
                "--json".into(),
            ],
        },
        env: BTreeMap::from([
            ("HOME".into(), text(&instance.home)?),
            ("CODEX_HOME".into(), text(&instance.config_root)?),
            ("TMPDIR".into(), text(state)?),
        ]),
        cwd: instance.cwd.clone(),
        timeout_secs: timeout,
    })
}

fn capture(home: &Path, state: &Path, path: &Path, key: &str, timeout: u64) -> Result<Captured> {
    let mut reader = Passive::new()?;
    let (raw, stamp) = reader.stamped_file(path)?;
    let registry_stamp = fingerprint(stamp)?;
    let registry = Registry::parse(raw.as_deref().ok_or_else(changed)?)?;
    let instance = registry.instances.get("codex").ok_or_else(changed)?;
    let recipe = registry.recipes.get(key).ok_or_else(changed)?;
    if !certified(instance) || instance.home != home || recipe.instances != ["codex"] {
        return Err(refusal("native_contract_required"));
    }
    let Source::LocalDirectory {
        root,
        member,
        sha256,
    } = &recipe.source
    else {
        return Err(refusal("native_contract_required"));
    };
    let observed = files::codex(&mut reader, instance, recipe)?;
    if observed.source_changed {
        return Err(refusal("marketplace_source_change_required"));
    }
    if observed.configured_enabled == Some(false) {
        return Err(refusal("codex_readd_would_enable"));
    }
    if !matches!(observed.registration, Registration::Present)
        || observed.configured_enabled != Some(true)
        || observed.marketplace_registered != Some(true)
    {
        return Err(refusal("native_plugin_registration_unknown"));
    }
    let Payload::Copied {
        source_sha256,
        installed_sha256,
    } = observed.payload
    else {
        return Err(refusal("native_plugin_payload_unknown"));
    };
    if sha256
        .as_ref()
        .is_some_and(|declared| declared != &source_sha256)
    {
        return Err(refusal("native_plugin_source_differs"));
    }
    let version = observed.version.ok_or_else(changed)?;
    let source_member = root.join(member);
    let source_manifest = files::object_file(
        &mut reader,
        &source_member.join(".codex-plugin/plugin.json"),
    )?;
    if !source_manifest.is_some_and(|value| {
        value.get("name").and_then(Value::as_str) == Some(recipe.plugin.as_str())
            && value.get("version").and_then(Value::as_str) == Some(version.as_str())
    }) || files::object_file(&mut reader, &root.join(".agents/plugins/marketplace.json"))?
        .is_none()
    {
        return Err(refusal("native_plugin_same_version_contract_required"));
    }
    safe_environment(&mut reader, instance, state, root)?;
    let disjoint = disjoint_storage(&mut reader, &registry, instance)?;
    let cache = instance.config_root.join("plugins/cache");
    let paths = [
        home,
        &instance.config_root,
        &instance.cwd,
        root,
        &source_member,
        &cache,
    ];
    let scope = paths
        .into_iter()
        .map(|path| bound_directory(&mut reader, path))
        .collect::<Result<Vec<_>>>()?;
    let binary = reader.executable_stamp(&instance.binary)?;
    if !matches!(&binary, Stamp::File(_, _, sha) if sha == CODEX_ARM64_0169) {
        return Err(refusal("native_plugin_binary_changed"));
    }
    let unrelated = reader.projected_tree(
        &cache,
        Some(&Path::new(&recipe.marketplace).join(&recipe.plugin)),
    )?;
    let config = config_fingerprint(&mut reader, &instance.config_root.join("config.toml"))?;
    reader.verify()?;
    let native_id = recipe.native_id();
    Ok(Captured {
        before: Snapshot {
            scope: fingerprint(&(scope, disjoint, registry_stamp, binary))?,
            source: source_sha256,
            selected: installed_sha256,
            unrelated,
            config,
            version,
        },
        invocation: invocation(instance, &native_id, state, timeout)?,
        native_id,
    })
}

pub fn is_plugin(journal: &Journal) -> bool {
    journal.plan.delegated
        && journal
            .plan
            .warnings
            .first()
            .is_some_and(|s| s.starts_with(PROTOCOL))
}

fn read<T: serde::de::DeserializeOwned>(directory: &Directory, name: &str) -> Result<T> {
    let (bytes, meta) = directory.regular_config_snapshot(OsStr::new(name), 4 * 1024 * 1024)?;
    if meta.kind != EntryKind::File || meta.mode != 0o600 {
        return Err(changed());
    }
    let meta = rustix::fs::statat(
        directory.file(),
        name,
        rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(std::io::Error::from)?;
    if meta.st_uid != nix::unistd::geteuid().as_raw() {
        return Err(changed());
    }
    Ok(
        serde_json::from_value::<crate::policy::json::Object<T>>(crate::policy::json::parse(
            &bytes,
        )?)?
        .0,
    )
}

fn plan_name(journal: &Journal) -> String {
    format!("native-plugin-{}-plan.json", journal.id)
}

fn after_name(journal: &Journal) -> String {
    format!("native-plugin-{}-after.json", journal.id)
}

fn frozen(journal: &Journal) -> Result<(Directory, Frozen)> {
    if !is_plugin(journal)
        || journal.plan.action != Action::Upgrade
        || !matches!(journal.plan.package.provider, Provider::Macops)
    {
        return Err(changed());
    }
    let dir = Directory::open(&journal.state)?;
    let meta = dir.file().metadata()?;
    if meta.uid() != nix::unistd::geteuid().as_raw() || meta.mode() & 0o7777 != 0o700 {
        return Err(changed());
    }
    let saved: Frozen = read(&dir, &plan_name(journal))?;
    if journal.plan.warnings.first() != Some(&format!("{PROTOCOL}{}", fingerprint(&saved)?))
        || crate::capability::capability_store::binding(&dir, &journal.state)? != saved.state
    {
        return Err(changed());
    }
    dir.verify()?;
    Ok((dir, saved))
}

fn reobserve(journal: &Journal, invocation: &FrozenInvocation, saved: &Frozen) -> Result<Snapshot> {
    let current = capture(
        &journal.home,
        &journal.state,
        &saved.registry,
        &saved.key,
        invocation.timeout_secs,
    )?;
    if &current.invocation != invocation {
        return Err(changed());
    }
    Ok(current.before)
}

pub fn worker_guard(journal: &Journal, invocation: &FrozenInvocation) -> Result<()> {
    let (_, saved) = frozen(journal)?;
    if reobserve(journal, invocation, &saved)? != saved.before {
        return Err(changed());
    }
    Ok(())
}

fn clean_exit(result: &NativeOutcome) -> bool {
    matches!(
        result,
        NativeOutcome::Exited {
            code: Some(0),
            signal: None
        }
    )
}

pub fn worker_after(
    journal: &Journal,
    invocation: &FrozenInvocation,
    result: &NativeOutcome,
) -> Result<()> {
    if !clean_exit(result) {
        return Err(refusal("native_plugin_completion_unknown"));
    }
    let (directory, saved) = frozen(journal)?;
    let current = reobserve(journal, invocation, &saved)?;
    if !saved.before.accepts(&current) {
        return Err(refusal("native_plugin_after_unverified"));
    }
    if crate::capability::capability_store::binding(&directory, &journal.state)? != saved.state {
        return Err(changed());
    }
    Store::atomic_new(
        directory.file(),
        &after_name(journal),
        &After {
            operation: journal.id.clone(),
            plan: journal.plan_sha256.clone(),
            worker: journal.process_group,
            observed: current,
        },
    )?;
    directory.verify()
}

fn finish(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    invocation: &FrozenInvocation,
    outcome: &NativeOutcome,
) -> Result<Value> {
    let result = (|| {
        let (directory, saved) = frozen(journal)?;
        if saved.registry != ctx.config.join(RECIPE_FILE) || !clean_exit(outcome) {
            return Err(refusal("native_plugin_completion_unknown"));
        }
        let after: After = read(&directory, &after_name(journal))?;
        if after.operation != journal.id
            || after.plan != journal.plan_sha256
            || after.worker != journal.process_group
            || !saved.before.accepts(&after.observed)
            || reobserve(journal, invocation, &saved)? != after.observed
        {
            return Err(refusal("native_plugin_after_unverified"));
        }
        store.verify_namespace()?;
        Ok(after)
    })();
    let (reason, evidence) = match result {
        Ok(after) => {
            journal.phase = Phase::Succeeded;
            journal.message = "已核对本地插件登记、来源、所选缓存和无关投影".into();
            (None, Some(after.observed))
        }
        Err(error) => {
            journal.phase = Phase::Unknown;
            journal.message.clone_from(&error.code);
            (Some(error.code), None)
        }
    };
    store.save(journal)?;
    let result = evidence.map(|projection| {
        json!({"registration":"present","configured_enabled":true,"effective_enabled":"unknown",
            "customization":"none","projection":projection})
    });
    Ok(
        json!({"journal":journal,"native_outcome":outcome,"reason":reason,"result":result,"loaded":"unknown"}),
    )
}

pub fn upgrade(ctx: &Context, key: &str, dry_run: bool, yes: bool) -> Result<Value> {
    upgrade_selected(ctx, key, dry_run, yes, None)
}

pub(crate) fn reinstall_from_inventory(
    ctx: &Context,
    key: &str,
    id: &str,
    directory: &Path,
) -> Result<Value> {
    upgrade_selected(ctx, key, false, false, Some((id, directory)))
}

fn upgrade_selected(
    ctx: &Context,
    key: &str,
    dry_run: bool,
    yes: bool,
    selected: Option<(&str, &Path)>,
) -> Result<Value> {
    upgrade_with_capture(
        ctx,
        key,
        dry_run,
        yes,
        |path| {
            let snapshot = capture(&ctx.home, &ctx.state, path, key, ctx.timeout_secs)?;
            if let Some((id, directory)) = selected
                && (snapshot.native_id != id
                    || snapshot.invocation.env.get("CODEX_HOME").map(Path::new) != Some(directory))
            {
                return Err(changed());
            }
            Ok(snapshot)
        },
        |store, path, current| {
            let (mut journal, binding) = prepare_upgrade(ctx, store, key, path, current)?;
            let outcome = native_runner::launch(store, &mut journal, &binding)?;
            finish(ctx, store, &mut journal, &binding.invocation, &outcome)
        },
    )
}

fn upgrade_with_capture(
    ctx: &Context,
    key: &str,
    dry_run: bool,
    yes: bool,
    mut capture: impl FnMut(&Path) -> Result<Captured>,
    proceed: impl FnOnce(&Store, PathBuf, Captured) -> Result<Value>,
) -> Result<Value> {
    let path = ctx.config.join(RECIPE_FILE);
    let initial = capture(&path)?;
    if dry_run {
        return Ok(
            json!({"mode":"dry-run","recipe":key,"native_id":initial.native_id,"command":initial.invocation.command,"persistentWrites":false}),
        );
    }
    crate::operations::confirm(&format!("按固定配方重新安装 {}", initial.native_id), yes)?;
    let store = crate::operations::lock_after_recovery(ctx)?;
    let current = capture(&path)?;
    if initial.before != current.before || initial.invocation != current.invocation {
        return Err(changed());
    }
    proceed(&store, path, current)
}

fn prepare_upgrade(
    ctx: &Context,
    store: &Store,
    key: &str,
    path: PathBuf,
    current: Captured,
) -> Result<(Journal, native_runner::RunBinding)> {
    let state = Directory::open(&store.path)?;
    let version = current.before.version.clone();
    let saved = Frozen {
        key: key.into(),
        registry: path,
        before: current.before,
        state: crate::capability::capability_store::binding(&state, &ctx.state)?,
    };
    let package = Package {
        id: format!("native-plugin:codex:{}", current.native_id),
        name: current.native_id,
        kind: Kind::Skill,
        official_url: "https://github.com/openai/codex".into(),
        provider: Provider::Macops,
        paths: vec![],
        exact_version_required: true,
    };
    let plan = Plan {
        brew: None,
        action: Action::Upgrade,
        before: Observation::Present {
            version: version.clone(),
            evidence: None,
        },
        target: Some(version),
        command: Some(current.invocation.command.clone()),
        query: None,
        recipe_sha256: fingerprint(&package)?,
        package,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        warnings: vec![format!("{PROTOCOL}{}", fingerprint(&saved)?)],
        synchronous: true,
        delegated: true,
    };
    let journal = crate::operations::new_journal(ctx, plan)?;
    store.verify_namespace()?;
    Store::atomic_new(&store.root, &plan_name(&journal), &saved)?;
    store.save(&journal)?;
    let binding = native_runner::prepare(store, &journal, &current.invocation)?;
    Ok((journal, binding))
}

pub fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    if ctx.home != journal.home || ctx.state != journal.state {
        return Err(changed());
    }
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(json!(journal));
    }
    let binding = native_runner::binding(store, journal)?;
    let outcome = native_runner::observe(store, journal, &binding)?;
    if outcome == NativeOutcome::Running {
        return Err(refusal("operation_in_flight"));
    }
    if resolve_failed {
        if !matches!(outcome, NativeOutcome::Exited { .. }) {
            return Err(refusal("native_plugin_completion_unknown"));
        }
        crate::operations::confirm("确认原进程已停止并将本次操作记为失败", yes)?;
        journal.phase = Phase::Failed;
        journal.message = "用户确认原进程停止；记录失败".into();
        store.save(journal)?;
        return Ok(json!(journal));
    }
    finish(ctx, store, journal, &binding.invocation, &outcome)
}

#[cfg(test)]
#[path = "../../tests/unit/native_plugin_tx.rs"]
mod tests;
