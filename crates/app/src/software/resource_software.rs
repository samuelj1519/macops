use crate::{
    catalog::inventory,
    context::Context,
    domain::{
        CommandSpec, Observation, Provider,
        resource::{self, Coverage, CoverageState, Data, Management, Release, Resource, State},
    },
    error::Result,
    platform::fs as paths,
    policy::catalog::resources as policy,
};
use std::{collections::BTreeSet, path::Path};

pub fn collect(
    ctx: &Context,
    project: Option<&Path>,
    discover_brew: bool,
) -> Result<(Vec<Resource>, Vec<Coverage>)> {
    let catalog = inventory::load(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let adopted = receipts.installations.keys().cloned().collect();
    let mut resources = declarations(ctx, &catalog, &adopted)?;
    let mut reports = vec![];
    let mut report = Coverage {
        source: "software-declarations".into(),
        state: CoverageState::Complete,
        count: resources.len(),
        roots: vec![
            ctx.catalog.clone(),
            ctx.settings.join("manifests/mac-apps.tsv"),
        ],
        issues: vec![],
    };
    declared_app_locations(ctx, &mut resources, &mut report)?;
    reports.push(report);
    let (mut mise, report) = mise(ctx, project)?;
    resources.append(&mut mise);
    reports.push(report);
    if discover_brew {
        let (mut items, report) = brew(ctx)?;
        resources.append(&mut items);
        reports.push(report);
    }
    let (mut items, report) = npm(ctx)?;
    resources.append(&mut items);
    reports.push(report);
    let (mut uv, mut uv_reports) = crate::software::uv_local::inventory(ctx)?;
    resources.append(&mut uv);
    reports.append(&mut uv_reports);
    for manager in ["cargo", "bun"] {
        let (mut items, report) = simple_manager(ctx, manager)?;
        resources.append(&mut items);
        reports.push(report);
    }
    for manager in ["rustup", "conda"] {
        let (mut items, report) = environments(ctx, manager)?;
        resources.append(&mut items);
        reports.push(report);
    }
    reports.push(local_binaries(ctx, &mut resources)?);
    let (mut resources, conflict) = crate::policy::catalog::targets::merge_views(resources);
    if conflict {
        reports.push(Coverage {
            source: "software-owner-mapping".into(),
            state: CoverageState::Invalid,
            count: 0,
            roots: vec![ctx.catalog.clone()],
            issues: vec!["owner_conflict".into()],
        });
    }
    if let Some(report) = baselines(ctx, &receipts, &mut resources)? {
        reports.push(report);
    }
    Ok((resources, reports))
}

pub(crate) fn recorded(ctx: &Context) -> Result<Vec<Resource>> {
    let catalog = inventory::load(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let adopted = receipts.installations.keys().cloned().collect();
    let mut items = declarations(ctx, &catalog, &adopted)?;
    for item in &mut items {
        if item.management == Management::Adopted && item.status == State::Declared {
            item.status = State::Unknown;
        }
    }
    Ok(items)
}

fn baselines(
    ctx: &Context,
    receipts: &crate::domain::Receipts,
    items: &mut [Resource],
) -> Result<Option<Coverage>> {
    let mut report = Coverage {
        source: "installation-baselines".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![ctx.state.join("receipts.json")],
        issues: vec![],
    };
    for item in items {
        let Data::Application {
            recipe_ref: Some(recipe),
            baseline,
            releases,
            ..
        } = &mut item.data
        else {
            continue;
        };
        let Some(receipt) = receipts.installations.get(recipe) else {
            continue;
        };
        let (prefix, name) = match &receipt.package.provider {
            Provider::Uv {
                package,
                binding: Some(binding),
            }
            | Provider::UvObserved { package, binding } => {
                (binding.tool_dir.clone(), package.clone())
            }
            Provider::NpmObserved { prefix, name } => (prefix.clone(), name.clone()),
            _ => match crate::policy::catalog::targets::descriptor(&receipt.package)? {
                Some(target) => (target.prefix, target.name),
                None => continue,
            },
        };
        let current = inventory::observe(ctx, &receipt.package);
        item.status = match &current {
            Observation::UvTool { .. } | Observation::NativePackage { .. } => State::Present,
            Observation::Missing { .. } => State::Missing,
            _ => State::Unknown,
        };
        *releases = current
            .version()
            .map(|version| Release {
                version: version.into(),
                path: Some(
                    if crate::software::rules::uv_local::is_uv(&receipt.package.provider) {
                        prefix.join(&name)
                    } else {
                        prefix.join("lib/node_modules").join(&name)
                    },
                ),
                active: true,
            })
            .into_iter()
            .collect();
        let assessment = crate::policy::catalog::targets::baseline(&receipt.observation, current);
        if assessment.comparison != resource::Comparison::Same {
            report.state = CoverageState::Invalid;
            report.issues.push(
                match assessment.comparison {
                    resource::Comparison::Different => "installation_drift",
                    _ => "baseline_unknown",
                }
                .into(),
            );
        }
        *baseline = Some(assessment);
        report.count += 1;
    }
    report.issues.sort();
    report.issues.dedup();
    Ok((report.count > 0).then_some(report))
}

fn provider_key(provider: &Provider, ctx: &Context) -> (String, String, String) {
    match provider {
        Provider::Brew { package, cask } => (
            if *cask { "brew-cask" } else { "brew" }.into(),
            package.clone(),
            ctx.home.display().to_string(),
        ),
        Provider::BrewFormula { query, .. } => (
            "brew".into(),
            query.key.full_name.clone(),
            query.key.canonical_prefix.display().to_string(),
        ),
        Provider::Mise { package } => (
            "mise".into(),
            package.clone(),
            mise_scope(ctx).display().to_string(),
        ),
        Provider::Npm {
            package, prefix, ..
        } => ("npm".into(), package.clone(), prefix.display().to_string()),
        Provider::Uv {
            package,
            binding: Some(binding),
        }
        | Provider::UvObserved { package, binding } => (
            "uv".into(),
            package.clone(),
            binding.tool_dir.display().to_string(),
        ),
        Provider::Uv {
            package,
            binding: None,
        } => (
            "uv-unbound".into(),
            package.clone(),
            ctx.home.display().to_string(),
        ),
        Provider::Bun { package } => (
            "bun".into(),
            package.clone(),
            ctx.home.display().to_string(),
        ),
        Provider::Cargo { package, .. } => (
            "cargo".into(),
            package.clone(),
            ctx.home.display().to_string(),
        ),
        Provider::ObservedApp {
            path, bundle_id, ..
        } => (
            "app-bundle".into(),
            bundle_id.clone(),
            path.display().to_string(),
        ),
        Provider::Service { formula } => (
            "service".into(),
            formula.clone(),
            ctx.home.display().to_string(),
        ),
        _ => (
            "declared-recipe".into(),
            String::new(),
            ctx.home.display().to_string(),
        ),
    }
}

fn declared_app_locations(
    ctx: &Context,
    resources: &mut Vec<Resource>,
    report: &mut Coverage,
) -> Result<()> {
    for row in inventory::app_rows(ctx)? {
        let Some(path) = crate::policy::paths::legacy_app_path(
            &row["path"],
            row.get("scope").map(String::as_str),
            &ctx.home,
        )?
        else {
            continue;
        };
        let id = resource::id("application", &("app-path", &path))?;
        if resources.iter().any(|r| r.id == id) {
            continue;
        }
        if let Some(package) = crate::policy::catalog::catalogue::app_row(&row, &path)? {
            let observed = inventory::observe(ctx, &package);
            let state = match observed {
                Observation::Present { evidence: None, .. } => State::Present,
                Observation::Missing { .. } => State::Missing,
                _ => State::Unknown,
            };
            let version = observed
                .version()
                .map(|v| Release {
                    version: v.into(),
                    path: Some(path.clone()),
                    active: true,
                })
                .into_iter()
                .collect();
            resources.push(Resource {
                id,
                name: package.name,
                aliases: vec![package.id],
                management: Management::Declared,
                status: state,
                data: Data::Application {
                    manager: "app-bundle".into(),
                    native_id: row["bundle_id"].clone(),
                    scope: path.display().to_string(),
                    releases: version,
                    baseline: None,
                    issues: vec![],
                    recipe_ref: None,
                },
            });
            report.count += 1;
        }
    }
    Ok(())
}

pub(crate) fn mise_scope(ctx: &Context) -> std::path::PathBuf {
    let get = |key: &str| ctx.scope.paths.get(key).and_then(Option::as_ref);
    get("MISE_DATA_DIR").cloned().unwrap_or_else(|| {
        get("XDG_DATA_HOME")
            .cloned()
            .unwrap_or_else(|| ctx.home.join(".local/share"))
            .join("mise")
    })
}

pub fn mise(ctx: &Context, project: Option<&Path>) -> Result<(Vec<Resource>, Coverage)> {
    let mut report = Coverage {
        source: "mise".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    };
    let program = match crate::catalog::resource_inventory::native_program(ctx, "mise") {
        Ok(program) => program,
        Err(error) if error.code == "manager_missing" => {
            report.state = CoverageState::Absent;
            return Ok((vec![], report));
        }
        Err(error) => return Err(error),
    };
    let output = crate::catalog::resource_inventory::native_query(
        ctx,
        &CommandSpec {
            program,
            args: vec!["ls".into(), "--installed".into(), "--json".into()],
        },
        project.unwrap_or(&ctx.home),
        None,
    )?;
    if output.uncertain || output.code != Some(0) {
        report.state = CoverageState::Unavailable;
        report.issues.push("mise_query_failed".into());
        return Ok((vec![], report));
    }
    let data = match policy::mise_releases(output.stdout.as_bytes()) {
        Ok(data) => data,
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
            return Ok((vec![], report));
        }
    };
    let scope = mise_scope(ctx);
    let mut resources = vec![];
    for (key, versions) in data {
        let name = policy::source_label(&key);
        let releases = versions
            .into_iter()
            .map(|v| Release {
                version: v.version,
                path: v.install_path,
                active: v.active,
            })
            .collect();
        resources.push(Resource {
            id: resource::id("application", &("mise", &key, &scope))?,
            name: name.clone(),
            aliases: vec![],
            management: Management::Native,
            status: State::Present,
            data: Data::Application {
                manager: "mise".into(),
                native_id: name,
                scope: scope.display().to_string(),
                releases,
                baseline: None,
                issues: vec![],
                recipe_ref: None,
            },
        });
    }
    report.count = resources.len();
    Ok((resources, report))
}

pub(crate) fn brew(ctx: &Context) -> Result<(Vec<Resource>, Coverage)> {
    let mut report = Coverage {
        source: "brew".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    };
    let (books, unverified) = match crate::software::brew::discovery_books(ctx) {
        Ok(discovery) => discovery,
        Err(error) => {
            report.state = if error.code == "invalid_brew_inventory" {
                CoverageState::Invalid
            } else {
                CoverageState::Unavailable
            };
            report.issues.push(error.code);
            return Ok((vec![], report));
        }
    };
    if books.is_empty() {
        match crate::catalog::resource_inventory::native_program(ctx, "brew") {
            Err(error) if error.code == "manager_missing" && unverified.is_empty() => {
                report.state = CoverageState::Absent;
                report.issues.push(error.code);
            }
            candidate => {
                report.state = CoverageState::Unavailable;
                report.issues.push("brew_context_unregistered".into());
                report.roots.extend(unverified);
                report.roots.extend(candidate);
            }
        }
        return Ok((vec![], report));
    }
    if !unverified.is_empty() {
        report.state = CoverageState::Unavailable;
        report.issues.push("brew_context_unregistered".into());
        report.roots.extend(unverified);
    }
    let mut resources = vec![];
    for context in books {
        let prefix = context.binding.prefix;
        let cellar = context.binding.cellar;
        report.roots.push(prefix.clone());
        let data = context.book;
        for formula in data.formulae {
            let native = formula.full_name.unwrap_or_else(|| formula.name.clone());
            let releases = formula
                .installed
                .into_iter()
                .map(|v| Release {
                    active: formula.linked_keg.as_ref() == Some(&v.version),
                    path: Some(cellar.join(&formula.name).join(&v.version)),
                    version: v.version,
                })
                .collect();
            resources.push(policy::application(
                "brew",
                &native,
                &formula.name,
                &prefix,
                releases,
            )?);
        }
        for cask in data.casks {
            let native = cask.full_token.unwrap_or_else(|| cask.token.clone());
            let releases = cask
                .installed
                .into_iter()
                .map(|version| Release {
                    path: Some(prefix.join("Caskroom").join(&cask.token).join(&version)),
                    version,
                    active: true,
                })
                .collect();
            resources.push(policy::application(
                "brew-cask",
                &native,
                &cask.token,
                &prefix,
                releases,
            )?);
        }
    }
    report.count = resources.len();
    Ok((resources, report))
}

fn npm(ctx: &Context) -> Result<(Vec<Resource>, Coverage)> {
    let (resources, coverage, _) = crate::software::npm_local::inventory(ctx)?;
    Ok((resources, coverage))
}

fn native_rows(
    manager: &str,
    root: &Path,
    packages: std::collections::BTreeMap<String, String>,
) -> Result<Vec<Resource>> {
    packages
        .into_iter()
        .map(|(name, version)| {
            Ok(Resource {
                id: resource::id("application", &(manager, root, &name))?,
                name: policy::source_label(&name),
                aliases: vec![],
                management: Management::Native,
                status: State::Present,
                data: Data::Application {
                    manager: manager.into(),
                    native_id: policy::source_label(&name),
                    scope: root.display().to_string(),
                    releases: vec![Release {
                        version,
                        path: None,
                        active: true,
                    }],
                    baseline: None,
                    issues: vec![],
                    recipe_ref: None,
                },
            })
        })
        .collect()
}

fn simple_manager(ctx: &Context, manager: &str) -> Result<(Vec<Resource>, Coverage)> {
    let mut report = Coverage {
        source: manager.into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    };
    let program = match crate::catalog::resource_inventory::native_program(ctx, manager) {
        Ok(p) => p,
        Err(error) => {
            report.state = CoverageState::Absent;
            report.issues.push(error.code);
            return Ok((vec![], report));
        }
    };
    let (root, args) = match manager {
        "cargo" => {
            let explicit = if ctx.isolated {
                None
            } else {
                std::env::var_os("CARGO_INSTALL_ROOT").map(std::path::PathBuf::from)
            };
            let root = explicit
                .or_else(|| {
                    ctx.scope
                        .paths
                        .get("CARGO_HOME")
                        .and_then(Option::as_ref)
                        .cloned()
                })
                .unwrap_or_else(|| ctx.home.join(".cargo"));
            paths::read_path(&root)?;
            let args = vec![
                "install".into(),
                "--list".into(),
                "--root".into(),
                root.display().to_string(),
            ];
            (root, args)
        }
        "bun" => (
            ctx.scope
                .paths
                .get("BUN_INSTALL_GLOBAL_DIR")
                .and_then(Option::as_ref)
                .cloned()
                .unwrap_or_else(|| ctx.home.join(".bun/install/global")),
            vec!["pm".into(), "ls".into(), "-g".into()],
        ),
        _ => {
            return Err(crate::error::Error::new(
                "unsupported_manager",
                "未支持的清单来源",
            ));
        }
    };
    report.roots.push(root.clone());
    let output = crate::catalog::resource_inventory::native_query(
        ctx,
        &CommandSpec { program, args },
        &ctx.home,
        None,
    )?;
    if manager == "bun" && crate::operations::providers::empty_bun_scope(&ctx.scope, &output)? {
        return Ok((vec![], report));
    }
    if output.uncertain || output.code != Some(0) {
        report.state = CoverageState::Unavailable;
        report.issues.push("native_query_failed".into());
        return Ok((vec![], report));
    }
    let parsed = if manager == "bun" {
        policy::bun_packages(&output.stdout)
    } else {
        policy::text_packages(&output.stdout, manager == "cargo").map(|p| (root.clone(), p))
    };
    let (root, packages) = match parsed {
        Ok(data) => data,
        Err(error) => {
            report.state = CoverageState::Invalid;
            report.issues.push(error.code);
            return Ok((vec![], report));
        }
    };
    let resources = native_rows(manager, &root, packages)?;
    report.count = resources.len();
    Ok((resources, report))
}

fn environments(ctx: &Context, manager: &str) -> Result<(Vec<Resource>, Coverage)> {
    let mut report = Coverage {
        source: manager.into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec![],
    };
    let program = match crate::catalog::resource_inventory::native_program(ctx, manager) {
        Ok(p) => p,
        Err(error) => {
            report.state = CoverageState::Absent;
            report.issues.push(error.code);
            return Ok((vec![], report));
        }
    };
    let args = if manager == "conda" {
        vec!["env".into(), "list".into(), "--json".into()]
    } else {
        vec!["toolchain".into(), "list".into()]
    };
    let output = crate::catalog::resource_inventory::native_query(
        ctx,
        &CommandSpec { program, args },
        &ctx.home,
        None,
    )?;
    if output.uncertain || output.code != Some(0) {
        report.state = CoverageState::Unavailable;
        report.issues.push("environment_query_failed".into());
        return Ok((vec![], report));
    }
    let mut resources = vec![];
    resources = if manager == "conda" {
        conda_rows(&output, &mut report)?
    } else {
        rustup_rows(ctx, &output, &mut report)?
    };
    report.count = resources.len();
    Ok((resources, report))
}

fn local_binaries(ctx: &Context, resources: &mut Vec<Resource>) -> Result<Coverage> {
    use std::os::unix::fs::PermissionsExt;
    let cargo = ctx
        .scope
        .paths
        .get("CARGO_HOME")
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".cargo"));
    let bun = ctx
        .scope
        .paths
        .get("BUN_INSTALL")
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".bun"));
    let roots = vec![
        ctx.home.join(".local/bin"),
        ctx.home.join(".mimocode/bin"),
        cargo.join("bin"),
        bun.join("bin"),
    ];
    let mut report = Coverage {
        source: "local-executable-entrypoints".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: roots.clone(),
        issues: vec![],
    };
    for root in roots {
        let safe = match paths::read_path(&root) {
            Ok(path) => path,
            Err(error) => {
                report.state = CoverageState::Invalid;
                report.issues.push(error.code);
                continue;
            }
        };
        if !safe.exists() {
            continue;
        }
        for entry in std::fs::read_dir(&safe)? {
            let entry = entry?;
            let logical = root.join(entry.file_name());
            let real = match paths::read_path(&logical) {
                Ok(path) => path,
                Err(error) => {
                    report.state = CoverageState::Invalid;
                    report.issues.push(error.code);
                    continue;
                }
            };
            let Ok(metadata) = std::fs::metadata(&real) else {
                report.state = CoverageState::Invalid;
                report.issues.push("broken_executable_link".into());
                continue;
            };
            if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                report.state = CoverageState::Invalid;
                report.issues.push("non_utf8_executable_name".into());
                continue;
            };
            resources.push(Resource {
                id: resource::id("application", &("entrypoint", &logical))?,
                name: name.clone(),
                aliases: vec![],
                management: Management::Untracked,
                status: State::Present,
                data: Data::Application {
                    manager: "local-executable".into(),
                    native_id: name,
                    scope: logical.display().to_string(),
                    releases: vec![],
                    baseline: None,
                    issues: vec![],
                    recipe_ref: None,
                },
            });
            report.count += 1;
        }
    }
    Ok(report)
}

fn conda_rows(
    output: &crate::domain::ProcessOutput,
    report: &mut Coverage,
) -> Result<Vec<Resource>> {
    let mut resources = vec![];
    let Ok(paths) = crate::policy::operation::parsers::parse_conda(&output.stdout) else {
        report.state = CoverageState::Invalid;
        report.issues.push("invalid_conda_inventory".into());
        return Ok(vec![]);
    };
    for path in paths["envs"]
        .as_array()
        .ok_or_else(|| crate::error::Error::new("invalid_conda_inventory", "环境列表缺失"))?
    {
        let path = Path::new(path.as_str().ok_or_else(|| {
            crate::error::Error::new("invalid_conda_inventory", "环境根不是路径")
        })?);
        paths::read_path(path)?;
        resources.push(Resource {
            id: resource::id("application", &("conda-env", path))?,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            aliases: vec![],
            management: Management::Native,
            status: State::Present,
            data: Data::Application {
                manager: "conda-environment".into(),
                native_id: path.display().to_string(),
                scope: path.display().to_string(),
                releases: vec![],
                baseline: None,
                issues: vec![],
                recipe_ref: None,
            },
        });
    }
    report.state = CoverageState::Unsupported;
    report
        .issues
        .push("environment_packages_not_scanned".into());
    Ok(resources)
}
fn rustup_rows(
    ctx: &Context,
    output: &crate::domain::ProcessOutput,
    report: &mut Coverage,
) -> Result<Vec<Resource>> {
    let mut resources = vec![];
    let root = ctx
        .scope
        .paths
        .get("RUSTUP_HOME")
        .and_then(Option::as_ref)
        .cloned()
        .unwrap_or_else(|| ctx.home.join(".rustup"));
    for line in output.stdout.lines().filter(|l| !l.trim().is_empty()) {
        let name = line.split_whitespace().next().ok_or_else(|| {
            crate::error::Error::new("invalid_rustup_inventory", "工具链名称缺失")
        })?;
        if !policy::component(name) {
            report.state = CoverageState::Invalid;
            report.issues.push("invalid_rustup_inventory".into());
            continue;
        }
        resources.push(Resource {
            id: resource::id("application", &("rustup-toolchain", &root, name))?,
            name: format!("rust:{name}"),
            aliases: vec![],
            management: Management::Native,
            status: State::Present,
            data: Data::Application {
                manager: "rustup-toolchain".into(),
                native_id: name.into(),
                scope: root.display().to_string(),
                releases: vec![],
                baseline: None,
                issues: vec![],
                recipe_ref: None,
            },
        });
    }
    Ok(resources)
}

fn declarations(
    ctx: &Context,
    catalog: &crate::domain::Catalog,
    adopted: &BTreeSet<String>,
) -> Result<Vec<Resource>> {
    let mut resources = vec![];
    for package in &catalog.packages {
        if matches!(
            package.kind,
            crate::domain::Kind::Skill | crate::domain::Kind::Plugin
        ) || matches!(
            package.provider,
            Provider::Manager { .. } | Provider::Macops | Provider::Legacy { .. }
        ) {
            continue;
        }
        let (mut manager, mut native, mut scope) = provider_key(&package.provider, ctx);
        if manager == "declared-recipe" {
            native.clone_from(&package.id);
        }
        let aliases = declaration_aliases(ctx, package)?;
        let identity = if let Some((name, binding)) =
            crate::software::rules::uv_local::descriptor(&package.provider)
        {
            native = crate::software::rules::uv_local::normalize_name(name)?;
            scope = binding.tool_dir.display().to_string();
            crate::software::rules::uv_local::tool_id(&binding.tool_dir, &native)?
        } else if let Provider::NpmObserved { prefix, name } = &package.provider {
            manager = "npm".into();
            native.clone_from(name);
            scope = prefix.display().to_string();
            crate::policy::catalog::targets::npm_id(prefix, name)?
        } else if let Some(target) = crate::policy::catalog::targets::descriptor(package)? {
            manager = "npm".into();
            native = target.name;
            scope = target.prefix.display().to_string();
            crate::policy::catalog::targets::npm_id(&target.prefix, &native)?
        } else if let Provider::Npm {
            package, prefix, ..
        } = &package.provider
        {
            let prefix = paths::read_path(prefix)?;
            scope = prefix.display().to_string();
            crate::policy::catalog::targets::npm_id(&prefix, package)?
        } else if let Provider::ObservedApp { path, .. } = &package.provider {
            resource::id("application", &("app-path", path))?
        } else if let Provider::BrewFormula { query, .. } = &package.provider {
            resource::id(
                "application",
                &("brew", &query.key.canonical_prefix, &query.key.full_name),
            )?
        } else {
            resource::id("application", &(&manager, &native, &scope))?
        };
        let observed = if matches!(package.provider, Provider::ObservedApp { .. }) {
            Some(inventory::observe(ctx, package))
        } else {
            None
        };
        let state = match &observed {
            Some(Observation::Present { evidence: None, .. }) => State::Present,
            Some(Observation::Missing { evidence: None }) => State::Missing,
            Some(_) => State::Unknown,
            None => State::Declared,
        };
        let releases = observed
            .as_ref()
            .and_then(Observation::version)
            .map(|version| Release {
                version: version.into(),
                path: package.paths.first().map(|p| p.path.clone()),
                active: true,
            })
            .into_iter()
            .collect();
        resources.push(Resource {
            id: identity,
            name: package.name.clone(),
            aliases,
            management: if adopted.contains(&package.id) {
                Management::Adopted
            } else {
                Management::Declared
            },
            status: state,
            data: Data::Application {
                manager,
                native_id: native,
                scope,
                releases,
                baseline: None,
                issues: vec![],
                recipe_ref: Some(package.id.clone()),
            },
        });
    }
    Ok(resources)
}

fn declaration_aliases(ctx: &Context, package: &crate::domain::Package) -> Result<Vec<String>> {
    let mut aliases = vec![package.id.clone()];
    let original = match &package.provider {
        Provider::Mise { package } => Some(("mise", package)),
        Provider::BrewFormula { query, .. } => {
            aliases.extend([query.selection.name.clone(), query.key.full_name.clone()]);
            Some(("declared-recipe", &package.id))
        }
        _ => None,
    };
    if let Some((manager, name)) = original {
        aliases.push(resource::id(
            "application",
            &(manager, name, ctx.home.display().to_string()),
        )?);
    }
    Ok(aliases)
}
