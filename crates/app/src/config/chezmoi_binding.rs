use crate::{
    config::rules::chezmoi_native::{Binding, Stamp},
    context::Context,
    domain::CommandSpec,
    error::{Error, Result},
    operations::store::Store,
    platform::fs::{self as paths, Directory},
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub fn changed() -> Error {
    Error::attention(
        "chezmoi_binding_changed",
        "原生工具或配置 owner 身份改变，保留现场并重新选择上下文",
    )
}

pub fn stamp(path: &Path, contents: bool) -> Result<Stamp> {
    let path = crate::policy::paths::lexical(path)?;
    if paths::read_path(&path)? != path {
        return Err(changed());
    }
    let parent = path.parent().ok_or_else(changed)?;
    let mut existing = parent;
    while !existing.exists() {
        existing = existing.parent().ok_or_else(changed)?;
    }
    let dir = Directory::open(existing)?;
    if existing != parent {
        return Ok(Stamp {
            path,
            identity: None,
            mode: 0,
            digest: crate::domain::fingerprint(&(dir.path.clone(), dir.lineage()?))?,
        });
    }
    let name = path.file_name().ok_or_else(changed)?;
    let before = dir.metadata(name)?;
    let Some(meta) = before else {
        dir.verify()?;
        return Ok(Stamp {
            path,
            identity: None,
            mode: 0,
            digest: crate::domain::fingerprint(&dir.lineage()?)?,
        });
    };
    let content = if contents {
        dir.read_regular(
            name,
            if meta.mode & 0o111 != 0 {
                128 * 1024 * 1024
            } else {
                16 * 1024 * 1024
            },
        )?
        .ok_or_else(changed)?
    } else {
        vec![]
    };
    if dir.metadata(name)?.is_none_or(|now| {
        now.identity != meta.identity || now.mode != meta.mode || now.kind != meta.kind
    }) {
        return Err(changed());
    }
    dir.verify()?;
    let digest = crate::domain::fingerprint(&(
        dir.lineage()?,
        if contents {
            format!("{:x}", Sha256::digest(content))
        } else {
            String::new()
        },
    ))?;
    Ok(Stamp {
        path,
        identity: Some(meta.identity),
        mode: meta.mode,
        digest,
    })
}

fn root(path: &Path) -> Result<Stamp> {
    let dir = Directory::open(path)?;
    dir.verify()?;
    Ok(Stamp {
        path: path.to_path_buf(),
        identity: Some(dir.identity().clone()),
        mode: dir.file().metadata()?.permissions().mode() & 0o7777,
        digest: crate::domain::fingerprint(&dir.lineage()?)?,
    })
}

pub fn inputs(binding: &Binding, sources: &[PathBuf]) -> Result<Vec<Stamp>> {
    let mut paths = vec![
        binding.config.clone(),
        binding.configured_source.join(".chezmoiroot"),
        binding.effective_source.join(".chezmoiignore"),
    ];
    paths.extend_from_slice(sources);
    let mut directories = std::collections::BTreeSet::from([binding.effective_source.clone()]);
    for source in sources {
        let mut parent = source.parent();
        while let Some(directory) = parent {
            if !directory.starts_with(&binding.effective_source) {
                break;
            }
            if paths::read_path(directory)? != directory {
                return Err(changed());
            }
            if directory.is_dir() {
                directories.insert(directory.to_path_buf());
            }
            parent = directory.parent();
        }
    }
    for path in directories {
        let directory = Directory::open(&path)?;
        for entry in directory.names()? {
            let entry = entry.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
            let name = entry.file_name().to_string_lossy();
            if name == ".chezmoiignore"
                || name.starts_with(".chezmoidata.")
                || name == ".chezmoiremove"
                || name.starts_with(".chezmoiremove.")
                || name.starts_with(".chezmoiexternal.")
                || name.starts_with(".chezmoiexternals.")
                || name == ".chezmoiversion"
            {
                paths.push(path.join(name.as_ref()));
            }
        }
        directory.verify()?;
    }
    paths.sort();
    paths.dedup();
    paths.iter().map(|p| stamp(p, true)).collect()
}

pub fn verify(binding: &Binding) -> Result<()> {
    crate::config::rules::chezmoi_native::validate(binding)?;
    if paths::read_path(&binding.lookup)? != binding.program {
        return Err(changed());
    }
    for anchor in &binding.anchors {
        let fresh = if anchor.path == binding.program {
            stamp(&anchor.path, true)?
        } else {
            root(&anchor.path)?
        };
        if &fresh != anchor {
            return Err(changed());
        }
    }
    let tracked = binding
        .inputs
        .iter()
        .map(|input| input.path.clone())
        .collect::<Vec<_>>();
    if inputs(binding, &tracked)? != binding.inputs {
        return Err(Error::attention(
            "plan_stale",
            "冻结的配置、控制文件集合或选定 source 输入已改变",
        ));
    }
    Ok(())
}

pub fn environment(ctx: &Context) -> BTreeMap<String, String> {
    let mut env: BTreeMap<_, _> = if ctx.isolated {
        BTreeMap::new()
    } else {
        std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect()
    };
    env.insert("HOME".into(), ctx.home.to_string_lossy().into_owned());
    env.insert(
        "PATH".into(),
        std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
    );
    for (key, value) in &ctx.scope.paths {
        if let Some(value) = value {
            env.insert(key.clone(), value.to_string_lossy().into_owned());
        } else {
            env.remove(key);
        }
    }
    env.insert(
        "TMPDIR".into(),
        ctx.home
            .join(".cache/macops-native-tmp")
            .to_string_lossy()
            .into_owned(),
    );
    env.insert("LC_ALL".into(), "C".into());
    env
}

#[derive(serde::Serialize)]
struct QueryAudit<'a> {
    schema: &'static str,
    scope_sha256: String,
    environment_sha256: String,
    command: &'a CommandSpec,
    effects: &'static str,
    phase: &'static str,
    exit: Option<i32>,
    uncertain: bool,
    process_group: Option<u32>,
}

pub fn query(
    scope: &crate::domain::Scope,
    spec: &CommandSpec,
    env: &BTreeMap<String, String>,
    timeout: u64,
    state: &Path,
) -> Result<crate::platform::process::BinaryOutput> {
    let directory = Directory::open(state)?;
    if directory.file().metadata()?.permissions().mode() & 0o7777 != 0o700 {
        return Err(changed());
    }
    let name = format!(
        "chezmoi-query-{}.json",
        crate::operations::store::operation_id()
    );
    let mut audit = QueryAudit {
        schema: "chezmoi-native-query-v1",
        scope_sha256: crate::domain::fingerprint(scope)?,
        environment_sha256: crate::domain::fingerprint(env)?,
        command: spec,
        effects: "configured-native-v1",
        phase: "intent",
        exit: None,
        uncertain: true,
        process_group: None,
    };
    Store::atomic_new(directory.file(), &name, &audit)?;
    crate::operations::identity::validate(scope)?;
    let tmp = env.get("TMPDIR").ok_or_else(changed)?;
    let tmp = paths::write_path(Path::new(tmp))?;
    std::fs::create_dir_all(&tmp)?;
    let mut command = std::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .env_clear()
        .envs(env)
        .current_dir(&scope.home)
        .stdin(std::process::Stdio::null());
    let captured = crate::platform::process::capture_owned_group_with_audit(
        &mut command,
        Duration::from_secs(timeout.min(60)),
        2 * 1024 * 1024,
        &directory.path,
        || {
            directory.verify()?;
            crate::operations::identity::validate(scope)
        },
        |pid| {
            audit.process_group = Some(pid);
            audit.phase = "running";
            Store::atomic(directory.file(), &name, &audit)
        },
    );
    let result = captured.result;
    audit.process_group = captured.pid;
    if let Ok(output) = &result {
        audit.exit = output.code;
        audit.uncertain = output.uncertain;
        audit.phase = if output.uncertain {
            "unknown"
        } else {
            "exited"
        };
    } else {
        audit.phase = "unknown";
    }
    Store::atomic(directory.file(), &name, &audit)?;
    result
}

pub fn success(output: crate::platform::process::BinaryOutput) -> Result<Vec<u8>> {
    if output.code != Some(0) || output.uncertain {
        return Err(Error::attention(
            "chezmoi_native_query_failed",
            "原生查询未明确成功；诊断正文未公开",
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| changed())?;
    let value = crate::domain::ProcessOutput {
        code: output.code,
        uncertain: output.uncertain,
        stdout,
        stderr: String::new(),
    };
    Ok(crate::policy::metadata::query_text(&value, false)?
        .as_bytes()
        .to_vec())
}

pub fn path_output(bytes: &[u8]) -> Result<PathBuf> {
    let value = std::str::from_utf8(bytes)
        .map_err(|_| changed())?
        .trim_end_matches('\n');
    if value.contains(['\n', '\r', '\0']) {
        return Err(changed());
    }
    crate::policy::paths::lexical(Path::new(value)).map_err(|_| changed())
}

const PATH_METADATA: &str = r#"{"configFile":{{ .chezmoi.configFile | toJson }},"cache":{{ .chezmoi.cacheDir | toJson }},"configuredState":{{ .chezmoi.config.persistentState | toJson }},"resolvedDefault":{{ joinPath (dir .chezmoi.configFile) "chezmoistate.boltdb" | toJson }}}"#;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NativePaths {
    config_file: String,
    cache: String,
    configured_state: String,
    resolved_default: String,
}
fn native_paths(bytes: &[u8]) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let metadata: NativePaths =
        serde_json::from_value(crate::policy::json::parse(bytes).map_err(|_| changed())?)
            .map_err(|_| changed())?;
    let state = if metadata.configured_state.is_empty() {
        metadata.resolved_default
    } else {
        metadata.configured_state
    };
    Ok((
        path_output(metadata.config_file.as_bytes())?,
        path_output(metadata.cache.as_bytes())?,
        path_output(state.as_bytes())?,
    ))
}

fn discovery_args(ctx: &Context) -> Result<Vec<String>> {
    let mut base = vec![];
    for (flag, path) in [
        ("--source", &ctx.chezmoi.source),
        ("--destination", &ctx.chezmoi.destination),
        ("--config", &ctx.chezmoi.config),
    ] {
        if let Some(path) = path {
            base.extend([
                flag.into(),
                crate::policy::paths::lexical(path)?
                    .to_str()
                    .ok_or_else(changed)?
                    .to_owned(),
            ]);
        }
    }
    Ok(base)
}

pub fn discover(ctx: &Context) -> Result<Binding> {
    let lookup = crate::operations::command::executable("chezmoi")?;
    let program = paths::read_path(&lookup)?;
    discover_bound(ctx, lookup, program)
}

fn discover_bound(ctx: &Context, lookup: PathBuf, program: PathBuf) -> Result<Binding> {
    if ctx.isolated && !program.starts_with(&ctx.home) {
        return Err(Error::attention(
            "fixture_native_refused",
            "合成 HOME 只能运行其目录内明确绑定的 chezmoi",
        ));
    }
    let tool = stamp(&program, true)?;
    let env = environment(ctx);
    let base = discovery_args(ctx)?;
    let run = |args: &[&str]| {
        if stamp(&program, true)? != tool || paths::read_path(&lookup)? != program {
            return Err(changed());
        }
        let mut args_out = base.clone();
        args_out.extend(args.iter().map(|s| (*s).into()));
        let bytes = success(query(
            &ctx.scope,
            &CommandSpec {
                program: program.clone(),
                args: args_out,
            },
            &env,
            ctx.timeout_secs,
            &ctx.state,
        )?)?;
        if stamp(&program, true)? != tool || paths::read_path(&lookup)? != program {
            return Err(changed());
        }
        Ok(bytes)
    };
    let version_output = String::from_utf8(run(&["--version"])?).map_err(|_| changed())?;
    let version = version_output
        .strip_prefix("chezmoi version v")
        .and_then(|s| s.split_whitespace().next())
        .ok_or_else(changed)?
        .trim_end_matches(',');
    let version = format!(
        "chezmoi version v{}",
        crate::policy::operation::parsers::exact_version(version).map_err(|_| changed())?
    );
    let config_data = crate::policy::json::parse(&run(&["dump-config", "--format=json"])?)
        .map_err(|_| changed())?;
    let configured_source = config_data
        .get("sourceDir")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(changed)?;
    crate::policy::paths::lexical(&configured_source).map_err(|_| changed())?;
    let effective_source = path_output(&run(&["source-path"])?)?;
    let destination = path_output(&run(&["target-path"])?)?;
    let (config, cache, persistent_state) =
        native_paths(&run(&["execute-template", PATH_METADATA])?)?;
    let config_parent = config.parent().ok_or_else(changed)?;
    let mut anchors = vec![
        tool,
        root(&configured_source)?,
        root(&effective_source)?,
        root(&destination)?,
    ];
    if config_parent.exists() {
        anchors.push(root(config_parent)?);
    }
    anchors.sort_by(|a, b| a.path.cmp(&b.path));
    anchors.dedup_by(|a, b| a.path == b.path);
    let mut binding = Binding {
        schema: "chezmoi-owner-v1".into(),
        scope: ctx.scope.clone(),
        program,
        lookup,
        version,
        configured_source,
        effective_source,
        destination,
        config,
        cache,
        persistent_state,
        record_state: ctx.state.clone(),
        anchors,
        inputs: vec![],
        effects: "configured-native-v1".into(),
        filtered: ["status", "verify"].iter().any(|name| {
            config_data
                .get(name)
                .and_then(|v| v.get("exclude"))
                .and_then(serde_json::Value::as_array)
                .is_none_or(|values| !values.is_empty())
        }),
    };
    binding.inputs = inputs(&binding, &[])?;
    verify(&binding)?;
    Ok(binding)
}

pub fn read<T: serde::de::DeserializeOwned>(ctx: &Context, name: &str) -> Result<Option<T>> {
    if !ctx.state.exists() {
        return Ok(None);
    }
    let directory = Directory::open(&ctx.state)?;
    let Some(bytes) = directory.read_regular(OsStr::new(name), 16 * 1024 * 1024)? else {
        return Ok(None);
    };
    let value = crate::policy::json::parse(&bytes).map_err(|_| changed())?;
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| changed())
}

pub fn resolve(ctx: &Context) -> Result<Binding> {
    if ctx.chezmoi.source.is_some()
        && ctx.chezmoi.destination.is_some()
        && ctx.chezmoi.config.is_some()
    {
        return discover(ctx);
    }
    let mut binding: Binding = read(ctx, "chezmoi-owner.json")?.ok_or_else(|| {
        Error::attention(
            "chezmoi_binding_unresolved",
            "先 config enroll 登记原生 owner，或明确指定 source、destination、config",
        )
    })?;
    if binding.scope != ctx.scope || binding.record_state != ctx.state {
        return Err(changed());
    }
    for (requested, actual) in [
        (&ctx.chezmoi.source, &binding.configured_source),
        (&ctx.chezmoi.destination, &binding.destination),
        (&ctx.chezmoi.config, &binding.config),
    ] {
        if requested.as_ref().is_some_and(|p| p != actual) {
            return Err(changed());
        }
    }
    let fresh_inputs = inputs(&binding, &[])?;
    let config_changed = binding.inputs.iter().find(|s| s.path == binding.config)
        != fresh_inputs.iter().find(|s| s.path == binding.config);
    binding.inputs = fresh_inputs;
    verify(&binding)?;
    if config_changed {
        let mut selected = ctx.clone();
        selected.chezmoi.source = Some(binding.configured_source.clone());
        selected.chezmoi.destination = Some(binding.destination.clone());
        selected.chezmoi.config = Some(binding.config.clone());
        let fresh = discover_bound(&selected, binding.lookup.clone(), binding.program.clone())?;
        if fresh.configured_source != binding.configured_source
            || fresh.effective_source != binding.effective_source
            || fresh.destination != binding.destination
            || fresh.config != binding.config
            || fresh.cache != binding.cache
            || fresh.persistent_state != binding.persistent_state
        {
            return Err(changed());
        }
        binding.inputs = fresh.inputs;
        binding.filtered = fresh.filtered;
    }
    Ok(binding)
}

pub fn save_owner(store: &Store, binding: &Binding) -> Result<()> {
    verify(binding)?;
    Store::atomic(&store.root, "chezmoi-owner.json", binding)
}

#[cfg(test)]
#[path = "../../tests/unit/chezmoi_binding.rs"]
mod tests;
