use crate::{
    chezmoi_binding as io,
    cli::Config,
    context::Context,
    domain::{
        Action, ConfigAuthority, Kind, Observation, Package, Phase, Plan, Provider, QuerySpec,
        fingerprint,
    },
    error::{Error, Result},
    native_runner::{self, FrozenInvocation, NativeOutcome},
    operations,
    policy::chezmoi_native::{self as policy, Binding, Managed, NativeCommand, Query, QueryKind},
    store::{Journal, Store},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[path = "chezmoi_browser.rs"]
mod browser;
#[path = "chezmoi_source.rs"]
mod source;
pub(crate) use browser::{ConfigBrowser, ViewerFile};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cache {
    binding: Binding,
    entries: BTreeMap<String, Managed>,
    observed_at: u64,
    complete: bool,
    status: Vec<policy::Status>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    source_publication: Option<source::Publication>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    schema: String,
    plan_sha256: String,
    binding: Binding,
    invocation_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    schema: String,
    operation_id: String,
    plan_sha256: String,
    scope_sha256: String,
    binding_sha256: String,
    invocation_sha256: String,
    worker_pid: u32,
    files_match: Option<bool>,
    reason: String,
}
fn verification_name(id: &str) -> String {
    format!("chezmoi-{id}-verification.json")
}

fn private_read<T: serde::de::DeserializeOwned>(state: &Path, name: &str) -> Result<T> {
    let directory = crate::paths::Directory::open(state)?;
    let (bytes, metadata) =
        directory.regular_config_snapshot(std::ffi::OsStr::new(name), 16 * 1024 * 1024)?;
    if metadata.mode != 0o600 {
        return Err(io::changed());
    }
    serde_json::from_value(crate::policy::json::parse(&bytes).map_err(|_| io::changed())?)
        .map_err(|_| io::changed())
}

fn execute_query(
    binding: &Binding,
    env: &BTreeMap<String, String>,
    kind: NativeCommand,
    targets: &[PathBuf],
    timeout: u64,
) -> Result<crate::process::BinaryOutput> {
    io::verify(binding)?;
    let output = io::query(
        &binding.scope,
        &policy::command(binding, kind, targets)?,
        env,
        timeout,
        &binding.record_state,
    )?;
    io::verify(binding)?;
    Ok(output)
}

fn query_managed(
    ctx: &Context,
    binding: &Binding,
    env: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, Managed>> {
    policy::managed(
        binding,
        &io::success(execute_query(
            binding,
            env,
            NativeCommand::Managed,
            &[],
            ctx.timeout_secs,
        )?)?,
    )
}

fn query_differences(
    ctx: &Context,
    binding: &Binding,
    env: &BTreeMap<String, String>,
    entries: &BTreeMap<String, Managed>,
) -> (Result<Vec<policy::Status>>, bool) {
    let allowed = entries
        .values()
        .map(|e| e.absolute.clone())
        .collect::<Vec<_>>();
    let raw = execute_query(binding, env, NativeCommand::Status, &[], ctx.timeout_secs);
    let finished = raw
        .as_ref()
        .is_ok_and(|output| !output.uncertain && output.code.is_some());
    let status = raw
        .and_then(io::success)
        .and_then(|bytes| policy::status(binding, &bytes, &allowed));
    (status, finished)
}

fn refreshed(ctx: &Context, binding: &Binding) -> Result<Cache> {
    let env = io::environment(ctx);
    let entries = query_managed(ctx, binding, &env)?;
    let status = query_differences(ctx, binding, &env, &entries).0?;
    Ok(Cache {
        binding: binding.clone(),
        entries,
        observed_at: crate::store::now(),
        complete: !binding.filtered,
        status,
        source_publication: None,
    })
}

fn cache_result(cache: Option<&Cache>) -> Value {
    let paths = cache.map_or_default(|c| {
        c.entries
            .values()
            .map(|e| e.absolute.display().to_string())
            .collect::<Vec<_>>()
    });
    let mut value = json!({"backend":"chezmoi-native-v1","candidates":crate::policy::metadata::limited_values(&paths),"coverage":"cached","liveVerified":false,"nativeInvocations":0,"incomplete":cache.is_none_or(|c|!c.complete)});
    if let Some(cache) = cache {
        value["roots"] = json!({"configured_source":cache.binding.configured_source,"effective_source":cache.binding.effective_source,"destination":cache.binding.destination,"config":cache.binding.config});
        value["observedAt"] = json!(cache.observed_at);
        value["nativePaths"] =
            json!({"cache":cache.binding.cache,"persistentState":cache.binding.persistent_state});
        value["tool"] = json!({"program":cache.binding.program,"lookup":cache.binding.lookup,"version":cache.binding.version});
    } else {
        value["coverage"] = json!("unresolved");
        value["reason"] = json!("chezmoi_binding_unresolved");
    }
    value
}

fn validate_cache(ctx: &Context, cache: &Cache) -> Result<()> {
    policy::validate(&cache.binding)?;
    if cache.binding.scope != ctx.scope {
        return Err(io::changed());
    }
    policy::managed(&cache.binding, &serde_json::to_vec(&cache.entries)?)?;
    Ok(())
}

pub fn cached_list(ctx: &Context) -> Result<Value> {
    let cache: Option<Cache> = io::read(ctx, "chezmoi-metadata.json")?;
    if let Some(cache) = &cache {
        validate_cache(ctx, cache)?;
    }
    Ok(cache_result(cache.as_ref()))
}

fn restore_entry(ctx: &Context, command: &Config) -> Result<bool> {
    if let Config::Restore { operation_id } = command {
        let path = ctx
            .state
            .join("operations")
            .join(format!("{operation_id}.json"));
        if crate::store::valid_id(operation_id) && path.exists() {
            let journal: Journal = crate::paths::regular_json(&path)?;
            crate::store::validate_journal(&journal)?;
            if policy::is_native(&journal.plan) {
                return Err(Error::attention(
                    "native_restore_requires_forward_operation",
                    "原生 apply 不提供原子回滚；撤销 source 后重新 apply 是新的前向操作",
                ));
            }
        }
        return Ok(true);
    }
    Ok(false)
}

pub fn run(ctx: &Context, command: &Config, dry: bool, yes: bool) -> Result<Option<Value>> {
    match command {
        Config::Adopt { target } => {
            return source::run(ctx, target, dry, yes, policy::SourceMode::Adopt).map(Some);
        }
        Config::Forget { target } => {
            let path = source::forget_target(ctx, target)?;
            return source::run(ctx, &path, dry, yes, policy::SourceMode::Forget).map(Some);
        }
        _ => (),
    }
    if restore_entry(ctx, command)? {
        return Ok(None);
    }
    if matches!(command, Config::Reconcile { .. }) {
        return Ok(None);
    }
    if dry {
        return Ok(Some(
            json!({"mode":"dry-run","backend":"chezmoi-native-v1","nativeInvocations":0,"persistentWrites":false,"evidence":cached_list(ctx)?,"resolution":"unresolved"}),
        ));
    }
    if let Config::List { refresh: false } = command {
        return cached_list(ctx).map(Some);
    }
    if let Config::Where { query } = command {
        let mut value = cached_list(ctx)?;
        let lines = value["candidates"]["shown"]
            .as_array()
            .map_or_default(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .filter(|p| p.contains(query))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        value["matches"] = crate::policy::metadata::limited_lines(&lines);
        return Ok(Some(value));
    }
    if !(matches!(command, Config::Enroll)
        || ctx.chezmoi.source.is_some()
            && ctx.chezmoi.destination.is_some()
            && ctx.chezmoi.config.is_some())
        && io::read::<Binding>(ctx, "chezmoi-owner.json")?.is_none()
    {
        return Err(Error::attention(
            "chezmoi_binding_unresolved",
            "先 config enroll 或明确选择完整 Native 上下文",
        ));
    }
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let binding = if matches!(command, Config::Enroll) {
        io::discover(ctx)?
    } else {
        io::resolve(ctx)?
    };
    if matches!(command, Config::Enroll | Config::List { refresh: true }) {
        return register(ctx, &store, &binding, matches!(command, Config::Enroll)).map(Some);
    }
    match command {
        Config::Apply { target } => apply(ctx, &store, binding, target, yes).map(Some),
        Config::SourcePath { target } => {
            let targets = target.iter().cloned().collect::<Vec<_>>();
            policy::selection(&binding, &targets, true)?;
            let bytes = io::success(execute_query(
                &binding,
                &io::environment(ctx),
                NativeCommand::SourcePath,
                &targets,
                ctx.timeout_secs,
            )?)?;
            let path = io::path_output(&bytes)?;
            if !path.starts_with(&binding.effective_source) {
                return Err(io::changed());
            }
            Ok(Some(
                json!({"backend":"chezmoi-native-v1","path":path,"effects":binding.effects,"coverage":"selected-native-mapping"}),
            ))
        }
        Config::Diff { path } => query_status(
            ctx,
            &binding,
            &path.iter().cloned().collect::<Vec<_>>(),
            false,
        )
        .map(Some),
        Config::Status { targets } => query_status(ctx, &binding, targets, false).map(Some),
        Config::Verify { targets } => query_status(ctx, &binding, targets, true).map(Some),
        _ => Err(Error::new("invalid_chezmoi_entry", "配置入口未匹配")),
    }
}

fn register(ctx: &Context, store: &Store, binding: &Binding, enroll: bool) -> Result<Value> {
    let cache = if enroll {
        Cache {
            binding: binding.clone(),
            entries: BTreeMap::new(),
            observed_at: crate::store::now(),
            complete: false,
            status: vec![],
            source_publication: None,
        }
    } else {
        refreshed(ctx, binding)?
    };
    if enroll {
        io::save_owner(store, binding)?;
    }
    Store::atomic(&store.root, "chezmoi-metadata.json", &cache)?;
    let mut result = cache_result(Some(&cache));
    result["coverage"] = json!(if enroll {
        "owner-enrolled"
    } else {
        "native-evaluated"
    });
    result["incomplete"] = json!(false);
    result["effects"] = json!(binding.effects);
    if let Some(object) = result.as_object_mut() {
        object.remove("nativeInvocations");
    }
    result["nativeExecuted"] = json!(true);
    Ok(result)
}

fn query_status(
    ctx: &Context,
    binding: &Binding,
    targets: &[PathBuf],
    verify: bool,
) -> Result<Value> {
    policy::selection(binding, targets, !verify)?;
    let cache = scoped(ctx, binding, targets)?;
    require_managed(&cache, targets)?;
    let env = io::environment(ctx);
    let rows = &cache.status;
    let verified = if verify {
        let output = execute_query(
            binding,
            &env,
            NativeCommand::Verify,
            targets,
            ctx.timeout_secs,
        )?;
        if output.code == Some(0) && !output.uncertain && !binding.filtered {
            Some(rows.is_empty())
        } else {
            None
        }
    } else {
        None
    };
    Ok(
        json!({"backend":"chezmoi-native-v1","effects":binding.effects,"scope":if targets.is_empty(){"all-native-managed"}else{"selected"},"targets":targets,"coverage":if binding.filtered{"partial-native-entry-selection"}else{"native-evaluated"},"results":rows.iter().map(|row|json!({"path":row.path,"local":row.local,"desired":row.desired,"status":"drift"})).collect::<Vec<_>>(),"matched":verified,"incomplete":verify&&verified!=Some(true)||binding.filtered,"nativeExternalEffects":"unverified","sourceSnapshotCoverage":"selected-source-and-native-context","recursive":false,"sourceObservation":"native-mapping"}),
    )
}

fn scoped(ctx: &Context, binding: &Binding, targets: &[PathBuf]) -> Result<Cache> {
    if targets.is_empty() {
        return refreshed(ctx, binding);
    }
    policy::selection(binding, targets, false)?;
    let env = io::environment(ctx);
    let mut entries = BTreeMap::new();
    for target in targets {
        let source = io::path_output(&io::success(execute_query(
            binding,
            &env,
            NativeCommand::SourcePath,
            std::slice::from_ref(target),
            ctx.timeout_secs,
        )?)?)?;
        let source_relative = source
            .strip_prefix(&binding.effective_source)
            .map_err(|_| io::changed())?
            .to_path_buf();
        let relative = target
            .strip_prefix(&binding.destination)
            .map_err(|_| io::changed())?
            .to_string_lossy()
            .into_owned();
        entries.insert(
            relative,
            Managed {
                absolute: target.clone(),
                source_absolute: source,
                source_relative,
            },
        );
    }
    let rows = io::success(execute_query(
        binding,
        &env,
        NativeCommand::Status,
        targets,
        ctx.timeout_secs,
    )?)?;
    let status = policy::status(binding, &rows, targets)?;
    Ok(Cache {
        binding: binding.clone(),
        entries,
        observed_at: crate::store::now(),
        complete: !binding.filtered,
        status,
        source_publication: None,
    })
}

fn require_managed(cache: &Cache, targets: &[PathBuf]) -> Result<()> {
    for target in targets {
        if !cache.entries.values().any(|e| &e.absolute == target) {
            return Err(Error::attention(
                "chezmoi_target_unmanaged",
                "明确目标未出现在原生受管清单中，未退化为全量操作",
            ));
        }
    }
    Ok(())
}

fn package(target: &Path, source: &Path) -> Result<Package> {
    Ok(Package {
        id: crate::policy::config_local::config_id(target)?,
        name: target.display().to_string(),
        kind: Kind::Config,
        official_url: "https://www.chezmoi.io/".into(),
        provider: Provider::ConfigObserved {
            target: target.to_path_buf(),
            source: Some(source.to_path_buf()),
            authority: ConfigAuthority::ChezmoiNative,
        },
        paths: vec![],
        exact_version_required: false,
    })
}

fn operation_name(id: &str) -> String {
    format!("chezmoi-{id}.json")
}

fn apply(
    ctx: &Context,
    store: &Store,
    mut binding: Binding,
    target: &str,
    yes: bool,
) -> Result<Value> {
    let cached: Option<Cache> = io::read(ctx, "chezmoi-metadata.json")?;
    let path = if target.starts_with("config:") {
        cached
            .as_ref()
            .ok_or_else(|| Error::attention("not_found", "配置ID需要已有原生缓存映射"))?
            .entries
            .values()
            .find(|e| {
                crate::policy::config_local::config_id(&e.absolute).is_ok_and(|id| id == target)
            })
            .map(|e| e.absolute.clone())
            .ok_or_else(|| Error::attention("not_found", "未找到原生配置目标"))?
    } else {
        crate::policy::paths::lexical(Path::new(target))?
    };
    let targets = vec![path.clone()];
    let cache = scoped(ctx, &binding, &targets)?;
    policy::selection(&binding, &targets, false)?;
    require_managed(&cache, &targets)?;
    let entry = cache
        .entries
        .values()
        .find(|e| e.absolute == path)
        .ok_or_else(io::changed)?;
    binding.inputs = io::inputs(&binding, std::slice::from_ref(&entry.source_absolute))?;
    let package = package(&path, &entry.source_absolute)?;
    let query = Query {
        kind: QueryKind::NativeV1,
        binding_sha256: fingerprint(&binding)?,
        targets,
        source_adopt: None,
        source_forget: None,
    };
    let command = policy::command(&binding, NativeCommand::Apply, &query.targets)?;
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Apply,
        before: Observation::Unknown {
            reason: "native-precondition-observed".into(),
        },
        target: None,
        command: Some(command.clone()),
        query: Some(QuerySpec::NativeConfig(query)),
        warnings: vec![
            "原生 hooks/helper/cache/state/network 依 owner 上下文执行；不提供多文件原子回滚"
                .into(),
        ],
        synchronous: true,
        delegated: false,
    };
    policy::validate_plan(&plan)?;
    operations::confirm(&format!("Native apply {}", path.display()), yes)?;
    io::verify(&binding)?;
    let mut journal = operations::new_journal(ctx, plan)?;
    store.save(&journal)?;
    let invocation = FrozenInvocation {
        command,
        env: io::environment(ctx),
        cwd: ctx.home.clone(),
        timeout_secs: ctx.timeout_secs,
    };
    let runner = native_runner::prepare(store, &journal, &invocation)?;
    let operation = Operation {
        schema: "chezmoi-native-operation-v1".into(),
        plan_sha256: journal.plan_sha256.clone(),
        binding,
        invocation_sha256: fingerprint(&invocation)?,
    };
    Store::atomic(&store.root, &operation_name(&journal.id), &operation)?;
    let outcome = native_runner::launch(store, &mut journal, &runner)?;
    finish(
        store,
        &mut journal,
        &operation,
        &runner.invocation,
        &outcome,
    )
}

fn validate_operation(journal: &Journal, operation: &Operation) -> Result<()> {
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err(io::changed());
    };
    if operation.schema != "chezmoi-native-operation-v1"
        || operation.plan_sha256 != journal.plan_sha256
        || fingerprint(&operation.binding)? != query.binding_sha256
        || operation.binding.scope != journal.scope
        || operation.binding.record_state != journal.state
        || journal.plan.command.as_ref()
            != Some(&policy::command(
                &operation.binding,
                NativeCommand::Apply,
                &query.targets,
            )?)
    {
        return Err(io::changed());
    }
    if let Provider::ConfigObserved {
        source: Some(source),
        ..
    } = &journal.plan.package.provider
    {
        if !operation.binding.inputs.iter().any(|s| &s.path == source) {
            return Err(io::changed());
        }
    } else {
        return Err(io::changed());
    }
    Ok(())
}

pub fn worker_guard(journal: &Journal, invocation: &FrozenInvocation) -> Result<()> {
    if !policy::is_native(&journal.plan) {
        return Err(Error::attention(
            "native_adapter_mismatch",
            "该原生 worker 仅接受本适配器的冻结操作",
        ));
    }
    let operation: Operation = private_read(&journal.state, &operation_name(&journal.id))?;
    validate_operation(journal, &operation)?;
    if fingerprint(invocation)? != operation.invocation_sha256
        || invocation.cwd != journal.scope.home
        || invocation.env.get("HOME") != Some(&journal.scope.home.to_string_lossy().into_owned())
        || invocation.env.get("TMPDIR")
            != Some(
                &journal
                    .scope
                    .home
                    .join(".cache/macops-native-tmp")
                    .to_string_lossy()
                    .into_owned(),
            )
    {
        return Err(io::changed());
    }
    for (key, value) in &journal.scope.paths {
        if invocation.env.get(key)
            != value
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .as_ref()
        {
            return Err(io::changed());
        }
    }
    io::verify(&operation.binding)
}

pub fn worker_after(
    journal: &Journal,
    invocation: &FrozenInvocation,
    outcome: &NativeOutcome,
) -> Result<()> {
    if !policy::is_native(&journal.plan) {
        return Err(Error::attention(
            "native_adapter_mismatch",
            "错误的原生 after owner",
        ));
    }
    let operation: Operation = private_read(&journal.state, &operation_name(&journal.id))?;
    validate_operation(journal, &operation)?;
    if fingerprint(invocation)? != operation.invocation_sha256
        || journal.process_group != Some(std::process::id())
        || matches!(outcome, NativeOutcome::Running)
    {
        return Err(io::changed());
    }
    let (files_match, reason) = if operation.binding.filtered {
        (None, "partial-filtered".into())
    } else {
        let probe = (|| {
            io::verify(&operation.binding)?;
            let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
                return Err(io::changed());
            };
            let result = execute_query(
                &operation.binding,
                &invocation.env,
                NativeCommand::Verify,
                &query.targets,
                invocation.timeout_secs,
            )?;
            if result.code != Some(0) || result.uncertain {
                return Err(Error::attention(
                    "native_verify_unconfirmed",
                    "原生 verify 非零或结果未知，不能声称普通漂移或完整匹配",
                ));
            }
            let rows = policy::status(
                &operation.binding,
                &io::success(execute_query(
                    &operation.binding,
                    &invocation.env,
                    NativeCommand::Status,
                    &query.targets,
                    invocation.timeout_secs,
                )?)?,
                &query.targets,
            )?;
            Ok(rows.is_empty())
        })();

        match probe {
            Ok(matched) => (
                Some(matched),
                if matched {
                    "matched".into()
                } else {
                    "mismatch".into()
                },
            ),
            Err(error) => (None, error.code),
        }
    };
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err(io::changed());
    };
    let record = Verification {
        schema: "chezmoi-native-verification-v1".into(),
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        scope_sha256: journal.scope_sha256.clone(),
        binding_sha256: query.binding_sha256.clone(),
        invocation_sha256: operation.invocation_sha256,
        worker_pid: std::process::id(),
        files_match,
        reason,
    };
    let directory = crate::paths::Directory::open(&journal.state)?;
    Store::atomic_new(directory.file(), &verification_name(&journal.id), &record)
}

fn validate_verification(
    journal: &Journal,
    operation: &Operation,
    invocation: &FrozenInvocation,
    record: &Verification,
) -> Result<()> {
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err(io::changed());
    };
    if record.schema != "chezmoi-native-verification-v1"
        || record.operation_id != journal.id
        || record.plan_sha256 != journal.plan_sha256
        || record.scope_sha256 != journal.scope_sha256
        || record.binding_sha256 != query.binding_sha256
        || record.invocation_sha256 != fingerprint(invocation)?
        || record.invocation_sha256 != operation.invocation_sha256
        || Some(record.worker_pid) != journal.process_group
        || record.reason.is_empty()
        || record.reason.len() > 80
        || !record
            .reason
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        || record.files_match == Some(true)
            && (operation.binding.filtered || record.reason != "matched")
        || record.files_match == Some(false) && record.reason != "mismatch"
        || record.files_match.is_none() && matches!(record.reason.as_str(), "matched" | "mismatch")
    {
        return Err(io::changed());
    }
    Ok(())
}

fn verified_result(
    journal: &Journal,
    operation: &Operation,
    invocation: &FrozenInvocation,
) -> Result<Verification> {
    let record: Verification = private_read(&journal.state, &verification_name(&journal.id))?;
    validate_verification(journal, operation, invocation, &record)?;
    io::verify(&operation.binding)?;
    Ok(record)
}

fn finish(
    store: &Store,
    journal: &mut Journal,
    operation: &Operation,
    invocation: &FrozenInvocation,
    outcome: &NativeOutcome,
) -> Result<Value> {
    validate_operation(journal, operation)?;
    let (known_success, native_result) = match outcome {
        NativeOutcome::Running => (false, "running"),
        NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        } => (true, "succeeded"),
        NativeOutcome::Exited { .. } => (false, "failed-partial-possible"),
        NativeOutcome::Unknown { .. } => (false, "unknown"),
    };
    let after = verified_result(journal, operation, invocation).ok();
    let verified = after.as_ref().and_then(|record| record.files_match);
    if known_success && verified == Some(true) && !operation.binding.filtered {
        let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
            return Err(io::changed());
        };
        store.commit(
            journal,
            Observation::ConfigNative {
                binding_sha256: query.binding_sha256.clone(),
                matched: true,
            },
        )?;
        journal.phase = Phase::Succeeded;
        journal.message = "原生明确成功且所选配置文件通过原生核验；外部 hook 效果未单独验收".into();
    } else {
        journal.phase = Phase::Unknown;
        journal.message =
            "原生操作保留 partial/unknown 事实；查询恢复不会重新 apply 或声称回滚".into();
    }
    store.save(journal)?;
    Ok(
        json!({"backend":"chezmoi-native-v1","operation_id":journal.id,"phase":journal.phase,"nativeResult":native_result,"filesMatch":verified,"nativeExternalEffects":"unverified","rollback":"not-provided","nativeOutcome":outcome,"verification":after.as_ref().map_or("unconfirmed",|record|record.reason.as_str()),"sourceSnapshotCoverage":"selected-source-and-native-context","recursive":false,"sourceObservation":if operation.binding.inputs.iter().any(|s|s.identity.is_none()&&matches!(&journal.plan.package.provider,Provider::ConfigObserved{source:Some(path),..} if path==&s.path)){"native-virtual"}else{"selected-source-stamp"}}),
    )
}

pub fn recover(
    ctx: &Context,
    store: &Store,
    journal: &mut Journal,
    resolve_failed: bool,
    yes: bool,
) -> Result<Value> {
    if let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query
        && let Some((mode, proof)) = query.source_change()
    {
        let proof = proof.clone();
        return source::recover(ctx, store, journal, &proof, mode);
    }
    if matches!(journal.phase, Phase::Succeeded | Phase::Failed) {
        return Ok(
            json!({"backend":"chezmoi-native-v1","operation_id":journal.id,"phase":journal.phase}),
        );
    }
    if ctx.scope != journal.scope {
        return Err(io::changed());
    }
    if journal.phase == Phase::Prepared && journal.process_group.is_none() {
        journal.phase = Phase::Failed;
        journal.message = "未发送 Native 执行 gate；未执行或回滚配置".into();
        store.save(journal)?;
        return Ok(
            json!({"backend":"chezmoi-native-v1","operation_id":journal.id,"phase":journal.phase,"nativeResult":"not-started"}),
        );
    }
    let operation: Operation = private_read(&journal.state, &operation_name(&journal.id))?;
    if operation.schema != "chezmoi-native-operation-v1"
        || operation.plan_sha256 != journal.plan_sha256
        || operation.binding.scope != journal.scope
        || operation.binding.record_state != journal.state
        || ctx.scope != journal.scope
    {
        return Err(io::changed());
    }
    let runner = native_runner::binding(store, journal)?;
    if fingerprint(&runner.invocation)? != operation.invocation_sha256 {
        return Err(io::changed());
    }
    let outcome = native_runner::observe(store, journal, &runner)?;
    if matches!(outcome, NativeOutcome::Running)
        || journal
            .process_group
            .is_some_and(crate::process::group_alive)
    {
        return Err(Error::attention(
            "operation_still_running",
            "原生原操作仍在运行，未重跑",
        ));
    }
    if resolve_failed {
        operations::confirm("仅记录人工核对后的失败；不会撤销原生效果或重新执行", yes)?;
        journal.phase = Phase::Failed;
        journal.message = "人工确认失败；未回滚、未重跑".into();
        store.save(journal)?;
        return Ok(
            json!({"backend":"chezmoi-native-v1","operation_id":journal.id,"phase":journal.phase}),
        );
    }
    finish(store, journal, &operation, &runner.invocation, &outcome)
}

pub fn cached_snapshot(ctx: &Context) -> crate::config_inventory::Snapshot {
    use crate::resource::{Coverage, CoverageState, SourceRef};
    let mut result = crate::config_inventory::Snapshot {
        roots: None,
        candidates: vec![],
        uncertain_targets: vec![],
        native_targets: vec![],
        destination: None,
        coverage: Coverage {
            source: "chezmoi-native-cache".into(),
            state: CoverageState::Absent,
            count: 0,
            roots: vec![],
            issues: vec!["native_metadata_not_enrolled".into()],
        },
    };
    let loaded = (|| {
        let Some(cache): Option<Cache> = io::read(ctx, "chezmoi-metadata.json")? else {
            return Ok(());
        };
        validate_cache(ctx, &cache)?;
        result.roots = Some(crate::config_inventory::Roots {
            config: Some(cache.binding.config.clone()),
            configured_source: cache.binding.configured_source.clone(),
            effective_source: cache.binding.effective_source.clone(),
            destination: cache.binding.destination.clone(),
            provenance: "native-cached".into(),
        });
        result.destination = Some(crate::config_inventory::Destination::capture(
            &cache.binding.destination,
        )?);
        result.native_targets = cache.entries.values().map(|e| e.absolute.clone()).collect();
        for entry in cache.entries.values() {
            if let Some(identity) = io::stamp(&entry.source_absolute, false)?.identity {
                result.candidates.push(crate::config_inventory::Mapping {
                    source: SourceRef {
                        path: entry.source_absolute.clone(),
                        identity,
                    },
                    target: entry.absolute.clone(),
                });
            }
        }
        result.coverage.state = CoverageState::Unsupported;
        result.coverage.issues = vec!["cached_metadata_not_live".into()];
        result.coverage.count = result.native_targets.len();
        Ok::<(), Error>(())
    })();
    if loaded.is_err() {
        result.coverage.state = CoverageState::Invalid;
        result.coverage.issues = vec!["native_cache_invalid".into()];
    }
    result
}

pub(crate) fn cached_packages(ctx: &Context) -> Result<Vec<Package>> {
    let Some(cache) = io::read::<Cache>(ctx, "chezmoi-metadata.json")? else {
        return Ok(vec![]);
    };
    validate_cache(ctx, &cache)?;
    if !cache.complete {
        return Ok(vec![]);
    }
    cache
        .entries
        .values()
        .map(|entry| package(&entry.absolute, &entry.source_absolute))
        .collect()
}

pub(crate) fn owns_possible_target(
    ctx: &Context,
    matches: &impl Fn(&Path) -> Result<bool>,
    overlaps: &impl Fn(&Path) -> Result<bool>,
) -> Result<bool> {
    let cache = io::read::<Cache>(ctx, "chezmoi-metadata.json");
    if let Ok(Some(cache)) = &cache
        && cache.binding.record_state == ctx.state
        && validate_cache(ctx, cache).is_ok()
    {
        for candidate in cache.entries.values() {
            if matches(&candidate.absolute)? {
                return Ok(true);
            }
        }
    }
    let Some(binding) = io::read::<Binding>(ctx, "chezmoi-owner.json")? else {
        return Ok(false);
    };
    let relevant = overlaps(&binding.destination)?;
    let uncertain = || {
        Error::attention(
            "config_authority_uncertain",
            "需要刷新所选范围的 chezmoi 管理清单",
        )
    };
    if binding.scope != ctx.scope
        || binding.record_state != ctx.state
        || policy::validate(&binding).is_err()
    {
        return if relevant {
            Err(uncertain())
        } else {
            Ok(false)
        };
    }
    let mut registered = ctx.clone();
    registered.chezmoi.source = Some(binding.configured_source.clone());
    registered.chezmoi.destination = Some(binding.destination.clone());
    registered.chezmoi.config = None;
    let candidate = crate::config_inventory::discover(&registered);
    for file in &candidate.candidates {
        if matches(&file.target)? {
            return Ok(true);
        }
    }
    if crate::config_inventory::uncertainty_affects(&candidate, matches, overlaps)? {
        return Err(Error::attention(
            "config_authority_uncertain",
            "chezmoi 源包含可能影响 mise 配置的原生规则，请核对对应配置文件",
        ));
    }
    if !relevant {
        return Ok(false);
    }
    if matches!(
        candidate.coverage.state,
        crate::resource::CoverageState::Invalid
            | crate::resource::CoverageState::Truncated
            | crate::resource::CoverageState::Unavailable
            | crate::resource::CoverageState::Absent
    ) || candidate
        .roots
        .as_ref()
        .is_none_or(|roots| roots.effective_source != binding.effective_source)
    {
        return Err(uncertain());
    }
    let cache = cache.map_err(|_| uncertain())?.ok_or_else(uncertain)?;
    validate_cache(ctx, &cache).map_err(|_| uncertain())?;
    let cached = &cache.binding;
    if !cache.complete
        || cached.record_state != binding.record_state
        || cached.lookup != binding.lookup
        || cached.anchors != binding.anchors
        || cached.program != binding.program
        || cached.version != binding.version
        || cached.destination != binding.destination
        || cached.config != binding.config
        || cached.cache != binding.cache
        || cached.persistent_state != binding.persistent_state
        || cached.configured_source != binding.configured_source
        || cached.effective_source != binding.effective_source
        || io::verify(cached).is_err()
    {
        return Err(uncertain());
    }
    Ok(false)
}

pub fn public_package(ctx: &Context, key: &str) -> Result<Option<Package>> {
    let cache: Option<Cache> = io::read(ctx, "chezmoi-metadata.json")?;
    if let Some(cache) = cache {
        validate_cache(ctx, &cache)?;
        for entry in cache.entries.values() {
            if key == entry.absolute.to_string_lossy()
                || key == crate::policy::config_local::config_id(&entry.absolute)?
            {
                return package(&entry.absolute, &entry.source_absolute).map(Some);
            }
        }
    }
    for receipt in crate::target::receipts(ctx)?.installations.values() {
        if let Provider::ConfigObserved { target, .. } = &receipt.package.provider
            && (key == target.to_string_lossy() || key == receipt.package.id)
        {
            return Ok(Some(receipt.package.clone()));
        }
    }
    if key.starts_with("config:") || Path::new(key).is_absolute() {
        return Err(Error::attention(
            "config_native_explicit_entry_required",
            "配置 authority 需要明确原生上下文；使用 config enroll 与配置入口",
        ));
    }
    Ok(None)
}

#[cfg(test)]
#[path = "../tests/unit/chezmoi_native.rs"]
mod tests;
