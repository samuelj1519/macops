use crate::{
    config::config_inventory,
    context::Context,
    domain::resource::{
        self, Coverage, CoverageState, Data, Management, Resource, SourceRef, State,
    },
    error::Result,
    platform::fs::{Bindings, BoundTarget, Directory, EntryKind, Metadata},
    policy::chezmoi::{Budget, issue},
};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub fn collect(
    ctx: &Context,
    profiles: &[Resource],
    project: Option<&Path>,
) -> Result<(Vec<Resource>, Vec<Coverage>)> {
    let base = ctx
        .scope
        .paths
        .get("XDG_CONFIG_HOME")
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".config"));
    let mut bindings = Bindings::new(64);
    let root = RootBinding::capture(&base, &mut bindings);
    let known = known_targets(ctx, profiles, project, &base, &root, &mut bindings);
    let mapping = crate::config::chezmoi_native::cached_snapshot(ctx);
    let mut target_bindings = BTreeMap::new();
    if let (Some(config_inventory::Destination::Present(directory)), Some(roots)) =
        (&mapping.destination, &mapping.roots)
    {
        for candidate in &mapping.candidates {
            if !target_bindings.contains_key(&candidate.target) {
                let binding = candidate
                    .target
                    .strip_prefix(&roots.destination)
                    .map_err(|_| {
                        crate::error::Error::new("invalid_config_target", "目标越过声明根")
                    })
                    .and_then(|relative| bindings.relative(directory, relative));
                target_bindings.insert(candidate.target.clone(), binding);
            }
        }
    }
    let Filesystem {
        mut entries,
        coverage: mut fs,
        ..
    } = filesystem(&base, root, known, profiles)?;
    let destination_valid = mapping
        .destination
        .as_ref()
        .map(config_inventory::Destination::verify)
        .transpose();
    let mut native = mapping.coverage.clone();
    let mut groups = BTreeMap::<PathBuf, Vec<SourceRef>>::new();
    for item in &mapping.candidates {
        groups
            .entry(item.target.clone())
            .or_default()
            .push(item.source.clone());
    }
    for (target, mut sources) in groups {
        sources.sort_by(|a, b| a.path.cmp(&b.path));
        sources.dedup();
        if sources.len() > 1 {
            issue(
                &mut native,
                CoverageState::Invalid,
                format!("source_target_conflict:{}", target.display()),
            );
        }
        if let Some(item) = entries.get_mut(&target) {
            apply_sources(item, sources);
            continue;
        }
        let observed = if let Err(error) = &destination_valid {
            issue(&mut native, CoverageState::Invalid, error.code.clone());
            None
        } else if matches!(
            mapping.destination,
            Some(config_inventory::Destination::Missing(_))
        ) {
            Some(Ok(None))
        } else {
            target_bindings
                .remove(&target)
                .map(|binding| binding.and_then(|binding| binding.metadata()))
        };
        let (meta, unknown) = match observed {
            Some(Ok(meta)) => (meta, false),
            Some(Err(error)) => {
                let state = if error.code == "metadata_binding_limit" {
                    CoverageState::Truncated
                } else {
                    CoverageState::Invalid
                };
                issue(&mut native, state, error.code);
                (None, true)
            }
            None => (None, true),
        };
        entries.insert(
            target.clone(),
            config(&target, sources, meta, unknown, profiles)?,
        );
    }
    insert_virtual_targets(&mapping, &mut bindings, &mut entries, &mut native, profiles)?;
    apply_receipts(ctx, &mut entries, &mut fs, profiles)?;
    native.issues.sort();
    native.issues.dedup();
    Ok((entries.into_values().collect(), vec![native, fs]))
}

fn insert_virtual_targets(
    mapping: &config_inventory::Snapshot,
    bindings: &mut Bindings,
    entries: &mut BTreeMap<PathBuf, Resource>,
    coverage: &mut Coverage,
    profiles: &[Resource],
) -> Result<()> {
    for target in &mapping.native_targets {
        if entries.contains_key(target) {
            continue;
        }
        let observed = (|| {
            let Some(config_inventory::Destination::Present(directory)) =
                mapping.destination.as_ref()
            else {
                return Ok(None);
            };
            directory.verify()?;
            let relative = target
                .strip_prefix(
                    &mapping
                        .roots
                        .as_ref()
                        .ok_or_else(|| {
                            crate::error::Error::new("native_cache_invalid", "缺少原生根")
                        })?
                        .destination,
                )
                .map_err(|_| crate::error::Error::new("native_cache_invalid", "目标越界"))?;
            bindings.relative(directory, relative)?.metadata()
        })();
        let unknown = observed.is_err();
        if unknown {
            issue(
                coverage,
                CoverageState::Invalid,
                "native_virtual_target_unobserved".into(),
            );
        }
        let mut item = config(target, vec![], observed.unwrap_or(None), unknown, profiles)?;
        item.management = Management::Native;
        if let Data::Config { owner, .. } = &mut item.data {
            *owner = "chezmoi-native-virtual".into();
        }
        entries.insert(target.clone(), item);
    }
    Ok(())
}

fn apply_receipts(
    ctx: &Context,
    entries: &mut BTreeMap<PathBuf, Resource>,
    fs: &mut Coverage,
    profiles: &[Resource],
) -> Result<()> {
    let receipts = crate::catalog::target::receipts(ctx)?;
    for receipt in receipts.installations.values() {
        let crate::domain::Provider::ConfigObserved {
            target, authority, ..
        } = &receipt.package.provider
        else {
            continue;
        };
        let current = crate::catalog::inventory::observe(ctx, &receipt.package);
        let base = crate::policy::targets::baseline(&receipt.observation, current);
        if base.comparison == crate::domain::resource::Comparison::Different {
            issue(fs, CoverageState::Invalid, "adopted_config_drift".into());
        }
        if base.comparison == crate::domain::resource::Comparison::Unknown {
            issue(
                fs,
                CoverageState::Invalid,
                "adopted_config_unobserved".into(),
            );
        }
        if let Some(item) = entries.get_mut(target) {
            item.management = if *authority == crate::domain::ConfigAuthority::ChezmoiNative {
                Management::Native
            } else {
                Management::Adopted
            };
            if base.comparison == crate::domain::resource::Comparison::Unknown {
                item.status = State::Unknown;
            }
            if let Data::Config { baseline, .. } = &mut item.data {
                *baseline = Some(base);
            }
            continue;
        }
        let unknown = base.comparison == crate::domain::resource::Comparison::Unknown;
        let mut item = config(target, vec![], None, unknown, profiles)?;
        item.management = if *authority == crate::domain::ConfigAuthority::ChezmoiNative {
            Management::Native
        } else {
            Management::Adopted
        };
        if let Data::Config {
            owner, baseline, ..
        } = &mut item.data
        {
            if *authority == crate::domain::ConfigAuthority::ChezmoiNative {
                *owner = "chezmoi-native-cached".into();
            }
            if *authority == crate::domain::ConfigAuthority::ChezmoiCandidate {
                *owner = "chezmoi-candidate".into();
            }
            *baseline = Some(base);
        }
        entries.insert(target.clone(), item);
    }
    Ok(())
}

struct Filesystem {
    entries: BTreeMap<PathBuf, Resource>,
    coverage: Coverage,
}
#[derive(Debug)]
enum RootBinding {
    Present(Directory),
    Missing(BoundTarget),
    Failed(String),
}
impl RootBinding {
    fn capture(path: &Path, bindings: &mut Bindings) -> Self {
        match bindings.root(path) {
            Ok(directory) => Self::Present(directory),
            Err(error) if error.code == "path_missing" => match bindings.target(path) {
                Ok(target) => Self::Missing(target),
                Err(error) => Self::Failed(error.code),
            },
            Err(error) => Self::Failed(error.code),
        }
    }
}
fn known_targets(
    ctx: &Context,
    profiles: &[Resource],
    project: Option<&Path>,
    base: &Path,
    root: &RootBinding,
    bindings: &mut Bindings,
) -> Vec<(PathBuf, Result<BoundTarget>)> {
    let mut targets = std::collections::BTreeSet::new();
    for name in [
        ".zshrc",
        ".zprofile",
        ".bashrc",
        ".bash_profile",
        ".gitconfig",
        ".npmrc",
        ".claude.json",
    ] {
        targets.insert(ctx.home.join(name));
    }
    for profile in profiles {
        if let Data::Agent {
            config_dir, client, ..
        } = &profile.data
        {
            let names: &[&str] = if client == "codex" {
                &["config.toml"]
            } else {
                &["settings.json", "settings.local.json", ".claude.json"]
            };
            for name in names {
                targets.insert(config_dir.join(name));
            }
        }
    }
    if let Some(root) = project {
        for name in [
            "mise.toml",
            ".mise.toml",
            ".mcp.json",
            ".claude/settings.json",
            ".claude/settings.local.json",
            ".codex/config.toml",
        ] {
            targets.insert(root.join(name));
        }
    }
    targets
        .into_iter()
        .map(|path| {
            let bound = if let Ok(relative) = path.strip_prefix(base) {
                match root {
                    RootBinding::Present(directory) => bindings.relative(directory, relative),
                    RootBinding::Missing(target) => target.missing_descendant(relative),
                    RootBinding::Failed(code) => Err(crate::error::Error::attention(
                        code,
                        "配置根无法绑定，未补读新位置",
                    )),
                }
            } else {
                bindings.target(&path)
            };
            (path, bound)
        })
        .collect()
}
fn filesystem(
    base: &Path,
    root: RootBinding,
    known: Vec<(PathBuf, Result<BoundTarget>)>,
    profiles: &[Resource],
) -> Result<Filesystem> {
    let mut fs = Coverage {
        source: "config-filesystem".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![base.to_path_buf()],
        issues: vec![],
    };
    let mut budget = Budget::new(10_000, 4);
    let items = match root {
        RootBinding::Present(root) => {
            match fs_branch(&root, base, 0, &mut budget, &mut fs, profiles).and_then(|items| {
                root.verify()?;
                Ok(items)
            }) {
                Ok(items) => items,
                Err(error) => {
                    issue(&mut fs, CoverageState::Invalid, error.code);
                    vec![]
                }
            }
        }
        RootBinding::Missing(target) => {
            match target.metadata() {
                Ok(None) => (),
                Ok(Some(_)) => issue(&mut fs, CoverageState::Invalid, "directory_changed".into()),
                Err(error) => issue(&mut fs, CoverageState::Invalid, error.code),
            }
            vec![]
        }
        RootBinding::Failed(code) => {
            issue(&mut fs, CoverageState::Unavailable, code);
            vec![]
        }
    };
    let mut entries = items
        .into_iter()
        .map(|item| (PathBuf::from(&item.name), item))
        .collect();
    for (path, binding) in known {
        add_existing(&path, binding, &mut entries, &mut budget, &mut fs, profiles)?;
    }
    fs.issues.sort();
    fs.issues.dedup();
    Ok(Filesystem {
        entries,
        coverage: fs,
    })
}

fn apply_sources(item: &mut Resource, sources: Vec<SourceRef>) {
    item.management = Management::Native;
    if let Data::Config {
        source_candidates,
        owner,
        ..
    } = &mut item.data
    {
        if sources.len() > 1 {
            item.status = State::Unknown;
            *owner = "conflict".into();
        } else {
            *owner = "chezmoi-native-cached".into();
        }
        *source_candidates = sources;
    }
}
fn config(
    target: &Path,
    sources: Vec<SourceRef>,
    meta: Option<Metadata>,
    unknown: bool,
    profiles: &[Resource],
) -> Result<Resource> {
    let consumers = profiles
        .iter()
        .filter_map(|r| match &r.data {
            Data::Agent { config_dir, .. } if target.starts_with(config_dir) => Some(r.id.clone()),
            _ => None,
        })
        .collect();
    let kind = meta
        .as_ref()
        .map_or(if unknown { "unknown" } else { "missing" }, |m| {
            m.kind.as_str()
        });
    let mut item = Resource {
        id: resource::id("config", &target)?,
        name: target.display().to_string(),
        aliases: vec![],
        management: Management::Untracked,
        status: if unknown {
            State::Unknown
        } else if meta.is_some() {
            State::Present
        } else {
            State::Missing
        },
        data: Data::Config {
            target: target.to_path_buf(),
            source_candidates: vec![],
            owner: "untracked".into(),
            consumers,
            observed_type: kind.into(),
            mode: meta.map(|m| m.mode),
            baseline: None,
        },
    };
    if !sources.is_empty() {
        apply_sources(&mut item, sources);
    }
    Ok(item)
}

fn add_existing(
    path: &Path,
    binding: Result<BoundTarget>,
    entries: &mut BTreeMap<PathBuf, Resource>,
    budget: &mut Budget,
    report: &mut Coverage,
    profiles: &[Resource],
) -> Result<()> {
    if entries.contains_key(path) || !budget.reserve(report) {
        return Ok(());
    }
    match binding.and_then(|binding| binding.metadata()) {
        Ok(Some(meta)) => {
            report.count += 1;
            entries.insert(
                path.to_path_buf(),
                config(path, vec![], Some(meta), false, profiles)?,
            );
        }
        Ok(None) => (),
        Err(error) => issue(
            report,
            if error.code == "metadata_binding_limit" {
                CoverageState::Truncated
            } else {
                CoverageState::Invalid
            },
            error.code,
        ),
    }
    Ok(())
}

fn fs_branch(
    dir: &Directory,
    logical: &Path,
    depth: usize,
    budget: &mut Budget,
    report: &mut Coverage,
    profiles: &[Resource],
) -> Result<Vec<Resource>> {
    if depth >= budget.depth {
        issue(report, CoverageState::Truncated, "config_scan_limit".into());
        return Ok(vec![]);
    }
    let mut items = vec![];
    let mut names = dir.names()?;
    while budget.remaining > 0 {
        let Some(entry) = names.next() else {
            break;
        };
        let entry = entry.map_err(|e| std::io::Error::from_raw_os_error(e.raw_os_error()))?;
        let bytes = entry.file_name().to_bytes();
        if matches!(bytes, b"." | b"..") {
            continue;
        }
        if !budget.take(report) {
            break;
        }
        let Ok(name) = std::str::from_utf8(bytes) else {
            issue(
                report,
                CoverageState::Invalid,
                format!("non_utf8_config_name:{}:{bytes:02x?}", logical.display()),
            );
            continue;
        };
        let path = logical.join(name);
        let meta = match dir.metadata(OsStr::new(name)) {
            Ok(Some(meta)) => meta,
            Ok(None) => {
                issue(report, CoverageState::Invalid, "config_disappeared".into());
                continue;
            }
            Err(error) => {
                issue(report, CoverageState::Invalid, error.code);
                continue;
            }
        };
        if meta.kind == EntryKind::Directory {
            if matches!(
                name,
                ".git" | "node_modules" | "cache" | "caches" | "logs" | "tmp" | "temp"
            ) {
                report
                    .issues
                    .push(format!("excluded_directory:{}", path.display()));
                continue;
            }
            let child = match dir.child(OsStr::new(name)) {
                Ok(child) => child,
                Err(error) => {
                    issue(report, CoverageState::Invalid, error.code);
                    continue;
                }
            };
            match fs_branch(&child, &path, depth + 1, budget, report, profiles) {
                Ok(mut branch) => match dir.verify_child(OsStr::new(name), &child) {
                    Ok(()) => items.append(&mut branch),
                    Err(error) => issue(report, CoverageState::Invalid, error.code),
                },
                Err(error) => issue(report, CoverageState::Invalid, error.code),
            }
        } else {
            items.push(config(&path, vec![], Some(meta), false, profiles)?);
        }
    }
    if budget.remaining == 0 {
        issue(report, CoverageState::Truncated, "config_scan_limit".into());
    }
    Ok(items)
}

#[cfg(test)]
#[path = "../../tests/unit/config_fs.rs"]
mod tests;
