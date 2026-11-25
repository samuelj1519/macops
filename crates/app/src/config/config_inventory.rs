use crate::{
    cli::Cli,
    context::Context,
    domain::resource::{Coverage, CoverageState, SourceRef},
    error::{Error, Result},
    platform::fs::{self as paths, Bindings, BoundTarget, Directory, EntryKind},
    policy::chezmoi::{self as policy, Budget, issue},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

pub use crate::policy::config_tx::ConfigRootInputs as Inputs;
impl Inputs {
    pub fn capture(cli: &Cli, isolated: bool) -> Self {
        Self {
            source: cli.chezmoi_source.clone(),
            destination: cli.chezmoi_destination.clone(),
            config: cli.chezmoi_config.clone(),
            ..Self::environment(isolated)
        }
    }

    pub(crate) fn environment(isolated: bool) -> Self {
        let mut env = BTreeMap::new();
        if !isolated {
            for key in [
                "XDG_CONFIG_HOME",
                "XDG_CONFIG_DIRS",
                "XDG_DATA_HOME",
                "XDG_DATA_DIRS",
            ] {
                if let Some(value) = std::env::var_os(key) {
                    env.insert(key.into(), PathBuf::from(value));
                }
            }
        }
        Self {
            source: None,
            destination: None,
            config: None,
            env,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Roots {
    pub config: Option<PathBuf>,
    pub configured_source: PathBuf,
    pub effective_source: PathBuf,
    pub destination: PathBuf,
    pub provenance: String,
}
#[derive(Clone, Debug)]
pub struct Mapping {
    pub source: SourceRef,
    pub target: PathBuf,
}
#[derive(Debug)]
pub struct Snapshot {
    pub roots: Option<Roots>,
    pub candidates: Vec<Mapping>,
    pub(crate) uncertain_targets: Vec<UnverifiedTarget>,
    pub native_targets: Vec<PathBuf>,
    pub coverage: Coverage,
    pub destination: Option<Destination>,
}

#[derive(Debug)]
pub(crate) struct UnverifiedTarget {
    path: PathBuf,
    descendants: bool,
}

pub(crate) fn uncertainty_affects(
    snapshot: &Snapshot,
    matches: &impl Fn(&Path) -> Result<bool>,
    overlaps: &impl Fn(&Path) -> Result<bool>,
) -> Result<bool> {
    for target in &snapshot.uncertain_targets {
        if matches(&target.path)? || (target.descendants && overlaps(&target.path)?) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug)]
pub enum Destination {
    Present(Directory),
    Missing(BoundTarget),
}
impl Destination {
    pub(crate) fn capture(path: &Path) -> Result<Self> {
        match Directory::open(path) {
            Ok(directory) => Ok(Self::Present(directory)),
            Err(error) if error.code == "path_missing" => {
                let probe = Bindings::new(64).target(path)?;
                if probe.metadata()?.is_some() {
                    return Err(Error::attention("directory_changed", "原缺失目标根已改变"));
                }
                Ok(Self::Missing(probe))
            }
            Err(error) => Err(error),
        }
    }
    pub fn verify(&self) -> Result<()> {
        match self {
            Self::Present(directory) => directory.verify(),
            Self::Missing(probe) => {
                if probe.metadata()?.is_none() {
                    Ok(())
                } else {
                    Err(Error::attention("directory_changed", "原缺失目标根已改变"))
                }
            }
        }
    }
}

pub fn discover(ctx: &Context) -> Snapshot {
    let mut snapshot = Snapshot {
        roots: None,
        candidates: vec![],
        uncertain_targets: vec![],
        native_targets: vec![],
        destination: None,
        coverage: Coverage {
            source: "chezmoi-static-candidates".into(),
            state: CoverageState::Unsupported,
            count: 0,
            roots: vec![],
            issues: vec!["physical_candidates_only".into()],
        },
    };
    match observe(ctx, &mut snapshot.coverage, &mut snapshot.uncertain_targets) {
        Ok(Some((roots, candidates, destination))) => {
            snapshot.destination = Some(destination);
            snapshot.roots = Some(roots);
            let mut seen = std::collections::BTreeSet::new();
            for candidate in &candidates {
                if !seen.insert(&candidate.target) {
                    issue(
                        &mut snapshot.coverage,
                        CoverageState::Invalid,
                        format!("source_target_conflict:{}", candidate.target.display()),
                    );
                }
            }
            snapshot.candidates = candidates;
        }
        Ok(None) => snapshot.coverage.state = CoverageState::Absent,
        Err(error) => {
            snapshot.coverage.state = CoverageState::Unavailable;
            snapshot.coverage.issues.push(error.code);
        }
    }
    snapshot.coverage.issues.sort();
    snapshot.coverage.issues.dedup();
    snapshot
}

fn observe(
    ctx: &Context,
    report: &mut Coverage,
    uncertain_targets: &mut Vec<UnverifiedTarget>,
) -> Result<Option<(Roots, Vec<Mapping>, Destination)>> {
    let (source, destination, config, provenance) = root_paths(ctx)?;
    let destination_binding = Destination::capture(&destination)?;
    report.roots.push(source.clone());
    if let Some(config) = &config {
        report.roots.push(config.clone());
    }
    let configured = match Directory::open(&source) {
        Ok(root) => root,
        Err(error) if error.code == "path_missing" => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut effective = None;
    if let Some(bytes) = configured.read_regular(std::ffi::OsStr::new(".chezmoiroot"), 4096)? {
        let relative = policy::directive(&bytes)?;
        let mut selected = None;
        for part in relative.components() {
            if let Component::Normal(name) = part {
                selected = Some(selected.as_ref().unwrap_or(&configured).child(name)?);
            }
        }
        effective = selected;
    }
    let directory = effective.as_ref().unwrap_or(&configured);
    let roots = Roots {
        config,
        configured_source: source,
        effective_source: directory.path.clone(),
        destination,
        provenance,
    };
    let candidates = SourceWalker {
        destination: &roots.destination,
        budget: Budget::new(10_000, 16),
        report,
        uncertain_targets,
    }
    .walk(directory, Path::new(""), 0)?;
    if let Some(selected) = &effective {
        selected.verify()?;
    }
    configured.verify()?;
    destination_binding.verify()?;
    Ok(Some((roots, candidates, destination_binding)))
}

fn root_paths(ctx: &Context) -> Result<(PathBuf, PathBuf, Option<PathBuf>, String)> {
    let input = &ctx.chezmoi;
    if let (Some(source), Some(destination), None) =
        (&input.source, &input.destination, &input.config)
    {
        return Ok((
            checked_explicit(source)?,
            checked_destination(destination)?,
            None,
            "explicit_physical_roots".into(),
        ));
    }
    let config = if let Some(path) = &input.config {
        Some(checked_explicit(path)?)
    } else {
        find_config(ctx)?
    };
    let fields = if let Some(path) = &config {
        let parent = Directory::open(
            path.parent()
                .ok_or_else(|| Error::new("invalid_config_path", "配置文件缺少父目录"))?,
        )?;
        let bytes = parent
            .read_regular(
                path.file_name()
                    .ok_or_else(|| Error::new("invalid_config_path", "配置文件需要文件名"))?,
                256 * 1024,
            )?
            .ok_or_else(|| Error::attention("config_missing", "显式或选中配置文件不存在"))?;
        let format = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        policy::root_fields(&bytes, format)?
    } else {
        policy::RootFields::default()
    };
    let declared_source = fields
        .source
        .as_deref()
        .map(|s| policy::path_value(s, &ctx.home))
        .transpose()?;
    let declared_destination = fields
        .destination
        .as_deref()
        .map(|s| policy::path_value(s, &ctx.home))
        .transpose()?;
    let source = match &input.source {
        Some(value) => checked_explicit(value)?,
        None => match declared_source {
            Some(value) => value,
            None => default_source(ctx)?,
        },
    };
    let destination = match &input.destination {
        Some(value) => checked_explicit(value)?,
        None => declared_destination.unwrap_or_else(|| ctx.home.clone()),
    };
    checked_destination(&destination)?;
    Ok((
        source,
        destination,
        config,
        if ctx.isolated {
            "logical_home_candidates"
        } else {
            "static_config_candidates"
        }
        .into(),
    ))
}
fn checked_explicit(path: &Path) -> Result<PathBuf> {
    let path = crate::policy::paths::lexical(path)?;
    paths::read_path(&path)?;
    Ok(path)
}
fn checked_destination(path: &Path) -> Result<PathBuf> {
    let path = checked_explicit(path)?;
    match Directory::open(&path) {
        Ok(directory) => directory.verify()?,
        Err(error) if error.code == "path_missing" => (),
        Err(error) => return Err(error),
    }
    Ok(path)
}
fn primary_config_directory(ctx: &Context) -> Result<PathBuf> {
    Ok(policy::xdg_home(
        ctx.chezmoi
            .env
            .get("XDG_CONFIG_HOME")
            .map(AsRef::<std::ffi::OsStr>::as_ref),
        &ctx.home,
        ".config",
    )?
    .join("chezmoi"))
}
const CONFIG_NAMES: [&str; 5] = [
    "chezmoi.json",
    "chezmoi.jsonc",
    "chezmoi.toml",
    "chezmoi.yaml",
    "chezmoi.yml",
];
pub(crate) fn lookup_candidates(ctx: &Context) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    if let Some(path) = &ctx.chezmoi.config {
        return Ok((vec![path.clone()], Some(path.clone())));
    }
    if ctx.chezmoi.source.is_some() && ctx.chezmoi.destination.is_some() {
        return Ok((vec![], None));
    }
    search_config(ctx)
}
fn search_config(ctx: &Context) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let primary = primary_config_directory(ctx)?;
    let mut candidates = CONFIG_NAMES.map(|name| primary.join(name)).to_vec();
    if let Some(config) = config_at(&primary)? {
        return Ok((candidates, Some(config)));
    }
    if !ctx.isolated {
        for root in policy::xdg_dirs(
            ctx.chezmoi
                .env
                .get("XDG_CONFIG_DIRS")
                .map(AsRef::<std::ffi::OsStr>::as_ref),
            &["/etc/xdg"],
        )? {
            let directory = root.join("chezmoi");
            candidates.extend(CONFIG_NAMES.map(|name| directory.join(name)));
            if let Some(config) = config_at(&directory)? {
                return Ok((candidates, Some(config)));
            }
        }
    }
    Ok((candidates, None))
}
fn find_config(ctx: &Context) -> Result<Option<PathBuf>> {
    Ok(search_config(ctx)?.1)
}
fn config_at(path: &Path) -> Result<Option<PathBuf>> {
    let directory = match Directory::open(path) {
        Ok(directory) => directory,
        Err(error) if error.code == "path_missing" => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut found = vec![];
    for name in CONFIG_NAMES {
        if directory.metadata(std::ffi::OsStr::new(name))?.is_some() {
            found.push(path.join(name));
        }
    }
    directory.verify()?;
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one.clone())),
        _ => Err(Error::attention(
            "config_format_conflict",
            "同一配置目录存在多个原生支持格式",
        )),
    }
}
fn default_source(ctx: &Context) -> Result<PathBuf> {
    Ok(search_source(ctx)?.1)
}
pub(crate) fn source_lookup(
    ctx: &Context,
    configuration: Option<(&Path, &[u8])>,
) -> Result<(Vec<PathBuf>, PathBuf)> {
    if let Some(source) = &ctx.chezmoi.source {
        return Ok((vec![], checked_explicit(source)?));
    }
    if let Some((path, raw)) = configuration {
        let fields =
            policy::root_fields(raw, path.extension().and_then(|p| p.to_str()).unwrap_or(""))?;
        if let Some(source) = fields.source {
            return Ok((vec![], policy::path_value(&source, &ctx.home)?));
        }
    }
    search_source(ctx)
}
fn search_source(ctx: &Context) -> Result<(Vec<PathBuf>, PathBuf)> {
    let home = policy::xdg_home(
        ctx.chezmoi
            .env
            .get("XDG_DATA_HOME")
            .map(AsRef::<std::ffi::OsStr>::as_ref),
        &ctx.home,
        ".local/share",
    )?;
    let fallback = home.join("chezmoi");
    let mut candidates = vec![fallback.clone()];
    if source_exists(&fallback)? {
        return Ok((candidates, fallback));
    }
    if !ctx.isolated {
        for root in policy::xdg_dirs(
            ctx.chezmoi
                .env
                .get("XDG_DATA_DIRS")
                .map(AsRef::<std::ffi::OsStr>::as_ref),
            &["/usr/local/share", "/usr/share"],
        )? {
            let path = root.join("chezmoi");
            candidates.push(path.clone());
            if source_exists(&path)? {
                return Ok((candidates, path));
            }
        }
    }
    Ok((candidates, fallback))
}

fn source_exists(path: &Path) -> Result<bool> {
    match Directory::open(path) {
        Ok(directory) => {
            directory.verify()?;
            Ok(true)
        }
        Err(error) if error.code == "path_missing" => Ok(false),
        Err(error) => Err(error),
    }
}

struct SourceWalker<'a> {
    destination: &'a Path,
    budget: Budget,
    report: &'a mut Coverage,
    uncertain_targets: &'a mut Vec<UnverifiedTarget>,
}
impl SourceWalker<'_> {
    fn walk(&mut self, dir: &Directory, relative: &Path, depth: usize) -> Result<Vec<Mapping>> {
        if depth >= self.budget.depth {
            issue(
                self.report,
                CoverageState::Truncated,
                format!("config_scan_depth:{}", dir.path.display()),
            );
            return Ok(vec![]);
        }
        let mut mappings = vec![];
        let mut names = dir.names()?;
        while self.budget.remaining > 0 {
            let Some(entry) = names.next() else {
                break;
            };
            let entry = entry.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
            let name = entry.file_name().to_bytes();
            if matches!(name, b"." | b"..") {
                continue;
            }
            if !self.budget.take(self.report) {
                break;
            }
            let Ok(name) = std::str::from_utf8(name) else {
                issue(
                    self.report,
                    CoverageState::Invalid,
                    format!("non_utf8_source_name:{}:{name:02x?}", dir.path.display()),
                );
                continue;
            };
            match self.entry(dir, name, relative, depth) {
                Ok(mut items) => mappings.append(&mut items),
                Err(error) => issue(
                    self.report,
                    CoverageState::Invalid,
                    format!("{}:{}", error.code, dir.path.join(name).display()),
                ),
            }
        }
        if self.budget.remaining == 0 {
            issue(
                self.report,
                CoverageState::Truncated,
                "config_scan_limit".into(),
            );
        }
        Ok(mappings)
    }
    fn entry(
        &mut self,
        dir: &Directory,
        name: &str,
        relative: &Path,
        depth: usize,
    ) -> Result<Vec<Mapping>> {
        let source = dir.path.join(name);
        let meta = dir
            .metadata(std::ffi::OsStr::new(name))?
            .ok_or_else(|| Error::attention("source_disappeared", "源条目在查询期间消失"))?;
        if !matches!(meta.kind, EntryKind::File | EntryKind::Directory) {
            issue(
                self.report,
                CoverageState::Unsupported,
                format!("unsupported_source_type:{}", source.display()),
            );
            self.mark_uncertain(name, relative, true);
            return Ok(vec![]);
        }
        let mapped = match policy::source_name(name, meta.kind == EntryKind::Directory) {
            Ok(Some(mapped)) => mapped,
            Ok(None) => {
                if name.starts_with(".chezmoi") {
                    issue(
                        self.report,
                        CoverageState::Unsupported,
                        format!("source_control_not_evaluated:{}", source.display()),
                    );
                }
                if name.starts_with(".chezmoiexternal.")
                    || name.starts_with(".chezmoiexternals.")
                    || name == ".chezmoiremove"
                    || name.starts_with(".chezmoiremove.")
                {
                    self.uncertain_targets.push(UnverifiedTarget {
                        path: self.destination.join(relative),
                        descendants: true,
                    });
                }
                return Ok(vec![]);
            }
            Err(error) if error.code == "unsupported_source_attribute" => {
                issue(
                    self.report,
                    CoverageState::Unsupported,
                    format!("{}:{}", error.code, source.display()),
                );
                self.mark_uncertain(name, relative, meta.kind == EntryKind::Directory);
                return Ok(vec![]);
            }
            Err(error) => return Err(error),
        };
        let target_relative = relative.join(&mapped.target);
        let target = self.destination.join(&target_relative);
        crate::policy::paths::lexical(&target)?;
        if meta.kind == EntryKind::Directory {
            let child = dir.child(std::ffi::OsStr::new(name))?;
            let items = self.walk(&child, &target_relative, depth + 1)?;
            dir.verify_child(std::ffi::OsStr::new(name), &child)?;
            Ok(items)
        } else {
            if mapped.template {
                issue(
                    self.report,
                    CoverageState::Unsupported,
                    format!("source_template_not_evaluated:{}", source.display()),
                );
            }
            if mapped.symlink {
                issue(
                    self.report,
                    CoverageState::Unsupported,
                    format!("source_link_expression_not_read:{}", source.display()),
                );
            }
            Ok(vec![Mapping {
                source: SourceRef {
                    path: source,
                    identity: meta.identity,
                },
                target,
            }])
        }
    }

    fn mark_uncertain(&mut self, name: &str, relative: &Path, directory: bool) {
        if name.starts_with("run_") {
            return;
        }
        let parent = self.destination.join(relative);
        let possible = ["modify_", "remove_", "exact_", "create_", "empty_"]
            .into_iter()
            .find_map(|prefix| name.strip_prefix(prefix))
            .or_else(|| {
                (!name.starts_with("encrypted_") && !name.starts_with("external_")).then_some(name)
            })
            .and_then(|name| policy::source_name(name, directory).ok().flatten());
        let descendants = directory || possible.is_none();
        let path = possible.map_or_else(|| parent.clone(), |mapped| parent.join(mapped.target));
        self.uncertain_targets
            .push(UnverifiedTarget { path, descendants });
    }
}

#[cfg(test)]
#[path = "../../tests/unit/config_inventory.rs"]
mod tests;
