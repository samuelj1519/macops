use crate::{
    domain::{Action, CommandSpec, FileId, Observation, Plan, QuerySpec, Scope},
    error::{Error, Result},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Stamp {
    pub path: PathBuf,
    pub identity: Option<FileId>,
    pub mode: u32,
    pub digest: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub schema: String,
    pub scope: Scope,
    pub program: PathBuf,
    pub lookup: PathBuf,
    pub version: String,
    pub configured_source: PathBuf,
    pub effective_source: PathBuf,
    pub destination: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    pub persistent_state: PathBuf,
    pub record_state: PathBuf,
    pub anchors: Vec<Stamp>,
    pub inputs: Vec<Stamp>,
    pub effects: String,
    pub filtered: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum QueryKind {
    #[serde(rename = "chezmoi-native-v1")]
    NativeV1,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub kind: QueryKind,
    pub binding_sha256: String,
    pub targets: Vec<PathBuf>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub source_adopt: Option<Box<SourceChange>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::policy::json::optional_object"
    )]
    pub source_forget: Option<Box<SourceChange>>,
}
impl Query {
    pub fn source_change(&self) -> Option<(SourceMode, &SourceChange)> {
        self.source_adopt
            .as_deref()
            .map(|proof| (SourceMode::Adopt, proof))
            .or_else(|| {
                self.source_forget
                    .as_deref()
                    .map(|proof| (SourceMode::Forget, proof))
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Adopt,
    Forget,
}
impl SourceMode {
    pub fn action(self) -> Action {
        match self {
            Self::Adopt => Action::Adopt,
            Self::Forget => Action::Uninstall,
        }
    }
    pub fn before(self) -> Observation {
        match self {
            Self::Adopt => Observation::Missing { evidence: None },
            Self::Forget => Observation::Unknown {
                reason: "原生源成员已核对，实际文件保持原样".into(),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceObject {
    pub path: PathBuf,
    pub identity: FileId,
    pub mode: u32,
    pub sha256: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceChange {
    pub owner: Stamp,
    pub target: SourceObject,
    pub candidate: SourceObject,
    pub draft_directory: FileId,
    pub source_parent: PathBuf,
    pub parent_identity: FileId,
    pub old_cache_sha256: String,
    pub old_tree_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Managed {
    pub absolute: PathBuf,
    pub source_absolute: PathBuf,
    pub source_relative: PathBuf,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub path: PathBuf,
    pub local: char,
    pub desired: char,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCommand {
    Managed,
    Status,
    Apply,
    Verify,
    SourcePath,
}

fn invalid() -> Error {
    Error::attention(
        "invalid_chezmoi_native",
        "原生配置绑定、范围或结构化结果无效",
    )
}

pub fn validate(binding: &Binding) -> Result<()> {
    if binding.schema != "chezmoi-owner-v1"
        || binding.effects != "configured-native-v1"
        || !binding.version.starts_with("chezmoi version v")
        || binding.anchors.is_empty()
        || binding.inputs.is_empty()
    {
        return Err(invalid());
    }
    for path in [
        &binding.scope.home,
        &binding.program,
        &binding.lookup,
        &binding.configured_source,
        &binding.effective_source,
        &binding.destination,
        &binding.config,
        &binding.cache,
        &binding.persistent_state,
        &binding.record_state,
    ] {
        crate::policy::paths::lexical(path).map_err(|_| invalid())?;
        if path.to_str().is_none() {
            return Err(invalid());
        }
    }
    if !binding
        .effective_source
        .starts_with(&binding.configured_source)
    {
        return Err(invalid());
    }
    for required in [
        &binding.program,
        &binding.configured_source,
        &binding.effective_source,
        &binding.destination,
    ] {
        if !binding
            .anchors
            .iter()
            .any(|s| &s.path == required && s.identity.is_some())
        {
            return Err(invalid());
        }
    }
    let mut anchors = std::collections::BTreeSet::new();
    for stamp in binding.anchors.iter().chain(&binding.inputs) {
        crate::policy::paths::lexical(&stamp.path).map_err(|_| invalid())?;
        if stamp.mode > 0o7777 || !crate::policy::config_local::valid_digest(&stamp.digest) {
            return Err(invalid());
        }
    }
    for stamp in &binding.anchors {
        if !anchors.insert(&stamp.path) {
            return Err(invalid());
        }
    }
    for required in [
        binding.config.clone(),
        binding.configured_source.join(".chezmoiroot"),
        binding.effective_source.join(".chezmoiignore"),
    ] {
        if !binding.inputs.iter().any(|s| s.path == required) {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn selection(binding: &Binding, targets: &[PathBuf], allow_all: bool) -> Result<()> {
    if targets.is_empty() && !allow_all {
        return Err(invalid());
    }
    let mut seen = std::collections::BTreeSet::new();
    for target in targets {
        crate::policy::paths::lexical(target).map_err(|_| invalid())?;
        if !target.starts_with(&binding.destination)
            || target == &binding.destination
            || !seen.insert(target)
            || target
                .to_str()
                .is_none_or(|p| p.contains(['\n', '\r', '\0']))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

pub fn command(
    binding: &Binding,
    operation: NativeCommand,
    targets: &[PathBuf],
) -> Result<CommandSpec> {
    validate(binding)?;
    selection(binding, targets, operation != NativeCommand::Apply)?;
    let mut args = vec![];
    for (flag, path) in [
        ("--source", &binding.configured_source),
        ("--destination", &binding.destination),
        ("--config", &binding.config),
        ("--cache", &binding.cache),
        ("--persistent-state", &binding.persistent_state),
    ] {
        args.extend([flag.into(), path.to_string_lossy().into_owned()]);
    }
    let flags: &[&str] = match operation {
        NativeCommand::Managed => &["managed", "--format=json", "--path-style=all"],
        NativeCommand::Status => &["status", "--path-style=absolute"],
        NativeCommand::Apply => &["--error-on-conflict", "apply", "--recursive=false"],
        NativeCommand::Verify => &["verify"],
        NativeCommand::SourcePath => &["source-path"],
    };
    args.extend(flags.iter().map(|s| (*s).to_owned()));
    if matches!(operation, NativeCommand::Status | NativeCommand::Verify) {
        args.push(format!("--recursive={}", targets.is_empty()));
    }
    if !targets.is_empty() {
        args.push("--".into());
        args.extend(targets.iter().map(|p| p.to_string_lossy().into_owned()));
    }
    Ok(CommandSpec {
        program: binding.program.clone(),
        args,
    })
}

fn relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

pub const MANAGED_LIMIT: usize = 4096;

pub fn managed(binding: &Binding, bytes: &[u8]) -> Result<BTreeMap<String, Managed>> {
    let value = crate::policy::json::parse(bytes).map_err(|_| invalid())?;
    let entries: BTreeMap<String, Managed> =
        serde_json::from_value(value).map_err(|_| invalid())?;
    if entries.len() > MANAGED_LIMIT {
        return Err(invalid());
    }
    for (relative, entry) in &entries {
        selection(binding, std::slice::from_ref(&entry.absolute), false)?;
        if entry.absolute != binding.destination.join(relative)
            || entry.source_absolute != binding.effective_source.join(&entry.source_relative)
            || !self::relative(&entry.source_relative)
            || !self::relative(Path::new(relative))
        {
            return Err(invalid());
        }
    }
    Ok(entries)
}

pub fn status(binding: &Binding, bytes: &[u8], allowed: &[PathBuf]) -> Result<Vec<Status>> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut rows = vec![];
    for line in text.lines() {
        let raw = line.as_bytes();
        if raw.len() < 4
            || raw[2] != b' '
            || !b" AMDR?".contains(&raw[0])
            || !b" AMDR?".contains(&raw[1])
        {
            return Err(invalid());
        }
        let path = PathBuf::from(&line[3..]);
        selection(binding, std::slice::from_ref(&path), false)?;
        if !allowed.contains(&path) || rows.iter().any(|r: &Status| r.path == path) {
            return Err(invalid());
        }
        rows.push(Status {
            path,
            local: char::from(raw[0]),
            desired: char::from(raw[1]),
        });
    }
    Ok(rows)
}

pub fn is_native(plan: &Plan) -> bool {
    matches!(plan.query, Some(QuerySpec::NativeConfig(_)))
}

pub fn validate_plan(plan: &Plan) -> Result<()> {
    let Some(QuerySpec::NativeConfig(query)) = &plan.query else {
        return Ok(());
    };
    if !crate::policy::config_local::valid_digest(&query.binding_sha256) {
        return Err(invalid());
    }
    let [target] = query.targets.as_slice() else {
        return Err(invalid());
    };
    if !plan.synchronous
        || plan.delegated
        || plan.package.id != crate::policy::config_local::config_id(target)?
    {
        return Err(invalid());
    }
    if query.source_adopt.is_some() && query.source_forget.is_some() {
        return Err(invalid());
    }
    if let Some((mode, proof)) = query.source_change() {
        validate_source_change(plan, target, proof, mode)?;
    } else if plan.action != Action::Apply || plan.command.is_none() {
        return Err(invalid());
    }
    Ok(())
}

fn validate_source_change(
    plan: &Plan,
    target: &Path,
    proof: &SourceChange,
    mode: SourceMode,
) -> Result<()> {
    use crate::domain::{ConfigAuthority, Provider};
    use crate::policy::config_local::valid_digest;
    if plan.action != mode.action()
        || plan.command.is_some()
        || plan.target.is_some()
        || plan.before != mode.before()
        || proof.target.path != target
        || mode == SourceMode::Adopt && proof.candidate.sha256 != proof.target.sha256
        || !valid_digest(&proof.candidate.sha256)
        || !valid_digest(&proof.owner.digest)
        || proof.owner.identity.is_none()
        || !valid_digest(&proof.old_cache_sha256)
        || !valid_digest(&proof.old_tree_sha256)
        || !valid_digest(&proof.target.sha256)
        || proof.target.mode > 0o777
        || proof.candidate.mode > 0o777
    {
        return Err(invalid());
    }
    crate::policy::paths::lexical(&proof.candidate.path)?;
    crate::policy::paths::lexical(&proof.source_parent)?;
    if !matches!(
        &plan.package.provider,
        Provider::ConfigObserved {
            target: file,
            source: Some(source),
            authority: ConfigAuthority::ChezmoiNative,
        } if file == target && source.parent() == Some(&proof.source_parent)
    ) {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/chezmoi_native_policy.rs"]
mod tests;
