use crate::{
    domain::{
        Action, ConfigFileStamp, FileId, LocalNpmQuery, NpmAnchor, NpmConfigBindings, NpmContract,
        NpmEvidence, NpmNamespace, NpmQueryKind, Observation, Scope, fingerprint,
    },
    error::{Error, Result},
    platform::fs::{self as paths, Directory, EntryKind},
    policy::config_local as config_policy,
    software::rules::npm_local as policy,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub fn capture(
    prefix: &Path,
    name: &str,
    contract: NpmContract,
    create: bool,
) -> Result<LocalNpmQuery> {
    capture_with_configs(None, prefix, name, contract, create)
}

pub fn capture_with_configs(
    home: Option<&Path>,
    prefix: &Path,
    name: &str,
    contract: NpmContract,
    create: bool,
) -> Result<LocalNpmQuery> {
    policy::name(name)?;
    let canonical = paths::read_path(prefix)?;
    let mut candidate = canonical.as_path();
    let root = loop {
        match Directory::open(candidate) {
            Ok(root) => break root,
            Err(error) if create && error.code == "path_missing" => {
                candidate = candidate.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    };
    root.verify()?;
    if root.path != candidate {
        return Err(changed());
    }
    let anchor = if candidate == canonical {
        NpmAnchor::ExistingPrefix {
            path: root.path.clone(),
            identity: root.identity().clone(),
        }
    } else {
        let tail = canonical.strip_prefix(candidate).map_err(|_| changed())?;
        let missing_tail = tail
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .map(str::to_owned)
                    .ok_or_else(changed)
            })
            .collect::<Result<Vec<_>>>()?;
        if root.metadata(OsStr::new(&missing_tail[0]))?.is_some() {
            return Err(changed());
        }
        NpmAnchor::CreatePrefix {
            path: root.path.clone(),
            identity: root.identity().clone(),
            missing_tail,
        }
    };
    let config_bindings = match home {
        Some(h) => Some(capture_config_bindings(h, &canonical)?),
        None => None,
    };
    Ok(LocalNpmQuery {
        kind: NpmQueryKind::LocalV1,
        prefix: prefix.to_path_buf(),
        name: name.into(),
        contract,
        namespace: NpmNamespace {
            canonical_prefix: canonical,
            anchor,
        },
        config_bindings,
    })
}

fn observe_optional_npmrc(path: &Path) -> Result<Option<ConfigFileStamp>> {
    let clean = crate::policy::paths::lexical(path)?;
    let parent = clean.parent().ok_or_else(changed)?;
    let leaf = clean
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(changed)?;
    let dir = match Directory::open(parent) {
        Ok(dir) => dir,
        Err(error) if error.code == "path_missing" => return Ok(None),
        Err(error) => return Err(error),
    };
    let lineage = dir.lineage()?;
    let Some(meta) = dir.metadata(OsStr::new(leaf))? else {
        dir.verify()?;
        return Ok(None);
    };
    if meta.kind != EntryKind::File {
        return Err(Error::attention(
            "unsupported_npm_config",
            "npm 配置文件必须是普通文件",
        ));
    }
    let (bytes, verified) = dir.regular_config_snapshot(OsStr::new(leaf), 256 * 1024)?;
    dir.verify()?;
    if dir.lineage()? != lineage {
        return Err(changed());
    }
    let id = config_policy::config_id(&clean)?;
    let raw = format!("{:x}", Sha256::digest(&bytes));
    Ok(Some(ConfigFileStamp {
        content_digest: config_policy::content_digest(&id, &raw)?,
        fingerprint: config_policy::stamp_fingerprint(
            &dir.path,
            leaf,
            &raw,
            &verified.identity,
            verified.mode,
            &lineage,
        )?,
        identity: verified.identity,
        mode: verified.mode,
    }))
}

pub fn capture_config_bindings(home: &Path, canonical_prefix: &Path) -> Result<NpmConfigBindings> {
    Ok(NpmConfigBindings {
        userconfig: observe_optional_npmrc(&home.join(".npmrc"))?,
        globalconfig: observe_optional_npmrc(&canonical_prefix.join("etc/npmrc"))?,
    })
}

pub fn verify_config_bindings(
    home: &Path,
    canonical_prefix: &Path,
    expected: &NpmConfigBindings,
) -> Result<()> {
    let current = capture_config_bindings(home, canonical_prefix)?;
    if &current != expected {
        return Err(Error::attention(
            "npm_config_changed",
            "冻结的 npmrc 配置文件在计划后发生变化",
        ));
    }
    Ok(())
}

pub fn verify_plain_namespace_for_write(query: &LocalNpmQuery, action: Action) -> Result<()> {
    if action == Action::Adopt {
        return Ok(());
    }
    match &query.namespace.anchor {
        NpmAnchor::CreatePrefix { path, identity, .. } => {
            let root = Directory::open(path)?;
            if root.path != *path || root.identity() != identity {
                return Err(changed());
            }
            root.verify()?;
            Ok(())
        }
        NpmAnchor::ExistingPrefix { path, identity } => {
            let root = Directory::open(path).map_err(|e| {
                if e.code == "not_a_directory" {
                    Error::attention("npm_namespace_link_unsafe", "npm 安装根不是真实普通目录")
                } else {
                    e
                }
            })?;
            if root.path != *path || root.identity() != identity {
                return Err(changed());
            }
            let require_all = matches!(action, Action::Upgrade | Action::Uninstall);
            let mut current = root;
            for part in ["lib", "node_modules"]
                .into_iter()
                .chain(query.name.split('/'))
            {
                let Some(meta) = current.metadata(OsStr::new(part))? else {
                    if require_all {
                        return Err(Error::attention(
                            "npm_namespace_changed",
                            "待变更的 npm 包目录已缺失",
                        ));
                    }
                    break;
                };
                if meta.kind != EntryKind::Directory {
                    return Err(Error::attention(
                        "npm_namespace_link_unsafe",
                        "npm 全局目录链包含符号链接或非目录项，拒绝执行写操作",
                    ));
                }
                let child = current.child(OsStr::new(part))?;
                current.verify_child(OsStr::new(part), &child)?;
                current = child;
            }
            current.verify()?;
            Ok(())
        }
    }
}

fn changed() -> Error {
    Error::attention("npm_namespace_changed", "npm 安装目录身份已改变")
}

struct Chain {
    directories: Vec<Directory>,
    lineage: Vec<FileId>,
}

impl Chain {
    fn new(root: Directory) -> Result<Self> {
        let lineage = root.lineage()?;
        Ok(Self {
            directories: vec![root],
            lineage,
        })
    }
}

impl Chain {
    fn last(&self) -> Result<&Directory> {
        self.directories.last().ok_or_else(changed)
    }

    fn descend(&mut self, name: &OsStr) -> Result<bool> {
        if self.directories.len() >= 64 {
            return Err(Error::attention(
                "npm_namespace_limit",
                "npm 目录链超出读取预算",
            ));
        }
        let parent = self.last()?;
        let Some(meta) = parent.metadata(name)? else {
            self.verify()?;
            if self.last()?.metadata(name)?.is_some() {
                return Err(changed());
            }
            return Ok(false);
        };
        if meta.kind != EntryKind::Directory {
            return Err(Error::attention(
                "unsupported_npm_placement",
                "npm 安装路径含链接或非目录项",
            ));
        }
        let child = parent.child(name)?;
        if child.identity() != &meta.identity {
            return Err(changed());
        }
        self.directories.push(child);
        Ok(true)
    }

    fn verify(&self) -> Result<()> {
        let root = self.directories.first().ok_or_else(changed)?;
        root.verify()?;
        if root.lineage()? != self.lineage {
            return Err(changed());
        }
        for pair in self.directories.windows(2) {
            let child = &pair[1];
            pair[0].verify_child(child.path.file_name().ok_or_else(changed)?, child)?;
        }
        Ok(())
    }

    fn identities(&self) -> Vec<&FileId> {
        self.directories.iter().map(Directory::identity).collect()
    }
}

pub fn observe(query: &LocalNpmQuery, post: bool, budget: &mut u64) -> Result<Observation> {
    observe_scoped(None, query, post, budget)
}

pub fn observe_scoped(
    scope: Option<&Scope>,
    query: &LocalNpmQuery,
    post: bool,
    budget: &mut u64,
) -> Result<Observation> {
    let canonical = &query.namespace.canonical_prefix;
    if paths::read_path(&query.prefix)? != *canonical {
        return Err(changed());
    }
    if let (Some(scope), Some(expected_cfg)) = (scope, &query.config_bindings) {
        verify_config_bindings(&scope.home, canonical, expected_cfg)?;
    }
    let (path, identity, tail) = match &query.namespace.anchor {
        NpmAnchor::ExistingPrefix { path, identity } => (path, identity, &[][..]),
        NpmAnchor::CreatePrefix {
            path,
            identity,
            missing_tail,
        } => (path, identity, missing_tail.as_slice()),
    };
    let root = Directory::open(path)?;
    if root.path != *path || root.identity() != identity {
        return Err(changed());
    }
    let mut chain = Chain::new(root)?;
    let result = observe_chain(query, &mut chain, tail, post, budget)?;
    chain.verify()?;
    if paths::read_path(&query.prefix)? != *canonical {
        return Err(changed());
    }
    Ok(result)
}

fn observe_chain(
    query: &LocalNpmQuery,
    chain: &mut Chain,
    tail: &[String],
    post: bool,
    budget: &mut u64,
) -> Result<Observation> {
    for part in tail {
        if !chain.descend(OsStr::new(part))? {
            return missing(query, None, chain, part);
        }
        if !post {
            return Err(changed());
        }
    }
    let prefix_identity = chain.last()?.identity().clone();
    for part in ["lib", "node_modules"]
        .into_iter()
        .chain(query.name.split('/'))
    {
        if !chain.descend(OsStr::new(part))? {
            return missing(query, Some(prefix_identity), chain, part);
        }
    }
    read_manifest(query, chain, prefix_identity, budget)
}

fn read_manifest(
    query: &LocalNpmQuery,
    chain: &Chain,
    prefix_identity: FileId,
    budget: &mut u64,
) -> Result<Observation> {
    if *budget == 0 {
        return Err(Error::attention(
            "npm_manifest_limit",
            "npm manifest 读取预算已用尽",
        ));
    }
    let limit = (*budget).min(1024 * 1024);
    let (bytes, manifest_id) = match chain
        .last()?
        .regular_snapshot(OsStr::new("package.json"), limit)
    {
        Ok(snapshot) => snapshot,
        Err(error) => {
            *budget -= limit;
            return Err(if error.code == "path_missing" {
                Error::attention("npm_manifest_missing", "包目录存在但缺少 manifest")
            } else {
                error
            });
        }
    };
    *budget -= bytes.len() as u64;
    let version = policy::manifest_version(&bytes, &query.name)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let digest = fingerprint(&(
        "macops-npm-local-v1",
        &query.namespace.canonical_prefix,
        &query.name,
        hash,
        chain.last()?.identity(),
        manifest_id,
        &query.name,
        &version,
        &prefix_identity,
        prefix_lineage(query, chain),
        chain
            .directories
            .iter()
            .rev()
            .take(if query.name.starts_with('@') { 4 } else { 3 })
            .map(Directory::identity)
            .collect::<Vec<_>>(),
    ))?;
    match query.contract {
        NpmContract::NativeV1 => Ok(Observation::NativePackage {
            version,
            source_fingerprint: digest,
            linked: false,
        }),
        NpmContract::DeclaredV1 => Ok(Observation::Present {
            version,
            evidence: Some(evidence(query, Some(prefix_identity), digest)?),
        }),
    }
}

fn prefix_lineage(query: &LocalNpmQuery, chain: &Chain) -> Vec<FileId> {
    let mut lineage = chain.lineage.clone();
    for directory in chain.directories.iter().skip(1) {
        if query
            .namespace
            .canonical_prefix
            .starts_with(&directory.path)
        {
            lineage.push(directory.identity().clone());
        }
    }
    lineage
}

fn missing(
    query: &LocalNpmQuery,
    prefix: Option<FileId>,
    chain: &Chain,
    component: &str,
) -> Result<Observation> {
    if chain.last()?.metadata(OsStr::new(component))?.is_some() {
        return Err(changed());
    }
    let digest = fingerprint(&(
        "macops-npm-local-missing-v1",
        &query.namespace,
        &query.name,
        chain.identities(),
        &chain.lineage,
        component,
    ))?;
    Ok(Observation::Missing {
        evidence: Some(evidence(query, prefix, digest)?),
    })
}

fn evidence(
    query: &LocalNpmQuery,
    prefix_identity: Option<FileId>,
    fingerprint: String,
) -> Result<NpmEvidence> {
    Ok(NpmEvidence {
        kind: NpmQueryKind::LocalV1,
        namespace: crate::policy::catalog::targets::npm_id(
            &query.namespace.canonical_prefix,
            &query.name,
        )?,
        canonical_prefix: query.namespace.canonical_prefix.clone(),
        prefix_identity,
        fingerprint,
    })
}

struct ScanBudget {
    entries: usize,
    bytes: u64,
}

struct NamespaceScan {
    prefix: PathBuf,
    sources: std::collections::BTreeSet<&'static str>,
    expected: Vec<FileId>,
}

pub fn inventory(
    ctx: &crate::context::Context,
) -> Result<(
    Vec<crate::domain::resource::Resource>,
    crate::domain::resource::Coverage,
    serde_json::Value,
)> {
    use crate::domain::resource::{Coverage, CoverageState};
    let mut report = Coverage {
        source: "npm".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![],
        issues: vec!["npm_scan_scope_default_catalog_receipts".into()],
    };
    let mut namespaces = Vec::<NamespaceScan>::new();
    let mut budget = ScanBudget {
        entries: 10_000,
        bytes: 32 * 1024 * 1024,
    };
    let catalog = crate::catalog::inventory::declared_catalog(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let mut catalog_roots = std::collections::BTreeSet::new();
    for package in &catalog.packages {
        if let Some(prefix) = package_prefix(package)? {
            match paths::read_path(&prefix) {
                Ok(prefix) => {
                    catalog_roots.insert(prefix);
                }
                Err(error) => issue(&mut report, CoverageState::Invalid, &error.code),
            }
        }
    }
    for prefix in catalog_roots {
        add_namespace(&mut namespaces, &mut report, prefix, "catalog", None);
    }
    let mut receipt_roots = vec![];
    for receipt in receipts.installations.values() {
        match receipt_root(receipt) {
            Ok(Some(root)) => receipt_roots.push(root),
            Ok(None) => (),
            Err(error) => issue(&mut report, CoverageState::Invalid, &error.code),
        }
    }
    receipt_roots.sort_by(|a, b| a.0.cmp(&b.0));
    for (prefix, identity) in receipt_roots {
        add_namespace(&mut namespaces, &mut report, prefix, "receipt", identity);
    }
    match default_prefix(ctx) {
        Ok(prefix) => add_namespace(&mut namespaces, &mut report, prefix, "default", None),
        Err(error) if error.code == "manager_missing" => {
            if namespaces.is_empty() && report.state == CoverageState::Complete {
                report.state = CoverageState::Absent;
            } else {
                issue(
                    &mut report,
                    CoverageState::Unavailable,
                    "npm_default_prefix_unavailable",
                );
            }
        }
        Err(error) => issue(&mut report, CoverageState::Unavailable, &error.code),
    }
    let mut packages = vec![];
    let mut namespace_rows = vec![];
    for mut namespace in namespaces {
        report.roots.push(namespace.prefix.clone());
        let rows = match scan_namespace(&mut namespace, &mut budget, &mut report) {
            Ok(rows) => rows,
            Err(error) => {
                issue(&mut report, CoverageState::Invalid, &error.code);
                vec![]
            }
        };
        namespace_rows.push(serde_json::json!({"prefix":namespace.prefix,"sources":namespace.sources,"count":rows.len()}));
        packages.extend(rows);
    }
    report.count = packages.len();
    report.issues.sort();
    report.issues.dedup();
    Ok((packages, report, serde_json::Value::Array(namespace_rows)))
}

fn receipt_root(receipt: &crate::domain::Receipt) -> Result<Option<(PathBuf, Option<FileId>)>> {
    if let Some(query @ crate::domain::QuerySpec::LocalNpm(local)) = &receipt.query {
        policy::binding(&receipt.package, query)?;
        let NpmAnchor::ExistingPrefix { path, identity } = &local.namespace.anchor else {
            return Err(Error::attention(
                "npm_receipt_missing_stable_root",
                "安装记录缺少已验证的根身份",
            ));
        };
        if paths::read_path(path)? != *path {
            return Err(Error::attention(
                "npm_receipt_root_rebound",
                "安装记录的根变为链接",
            ));
        }
        return Ok(Some((path.clone(), Some(identity.clone()))));
    }
    package_prefix(&receipt.package)?
        .map(|prefix| paths::read_path(&prefix).map(|path| (path, None)))
        .transpose()
}

fn package_prefix(package: &crate::domain::Package) -> Result<Option<PathBuf>> {
    match &package.provider {
        crate::domain::Provider::Npm { prefix, .. }
        | crate::domain::Provider::NpmObserved { prefix, .. } => Ok(Some(prefix.clone())),
        _ => Ok(crate::policy::catalog::targets::descriptor(package)?.map(|target| target.prefix)),
    }
}

fn default_prefix(ctx: &crate::context::Context) -> Result<PathBuf> {
    let program = crate::catalog::resource_inventory::native_program(ctx, "npm")?;
    let output = crate::catalog::resource_inventory::native_query(
        ctx,
        &crate::domain::CommandSpec {
            program,
            args: vec!["prefix".into(), "--global".into()],
        },
        &ctx.home,
        None,
    )?;
    if output.uncertain || output.code != Some(0) {
        return Err(Error::attention(
            "npm_default_prefix_unavailable",
            "npm 默认安装根无法确认",
        ));
    }
    paths::read_path(Path::new(output.stdout.trim()))
}

fn issue(
    report: &mut crate::domain::resource::Coverage,
    state: crate::domain::resource::CoverageState,
    code: &str,
) {
    crate::config::rules::chezmoi::issue(report, state, code.into());
}

fn add_namespace(
    namespaces: &mut Vec<NamespaceScan>,
    report: &mut crate::domain::resource::Coverage,
    prefix: PathBuf,
    source: &'static str,
    expected: Option<FileId>,
) {
    let namespace = if let Some(index) = namespaces.iter().position(|item| item.prefix == prefix) {
        &mut namespaces[index]
    } else {
        if namespaces.len() >= 32 {
            issue(
                report,
                crate::domain::resource::CoverageState::Truncated,
                "npm_namespace_limit",
            );
            return;
        }
        namespaces.push(NamespaceScan {
            prefix,
            sources: std::collections::BTreeSet::new(),
            expected: vec![],
        });
        let Some(namespace) = namespaces.last_mut() else {
            return;
        };
        namespace
    };
    if let Some(identity) = expected {
        if !namespace.expected.contains(&identity) {
            namespace.expected.push(identity);
        }
    } else {
        namespace.sources.insert(source);
    }
}

fn scan_namespace(
    namespace: &mut NamespaceScan,
    budget: &mut ScanBudget,
    report: &mut crate::domain::resource::Coverage,
) -> Result<Vec<crate::domain::resource::Resource>> {
    let prefix = &namespace.prefix;
    let root = Directory::open(prefix)?;
    if root.path != *prefix {
        return Err(changed());
    }
    if namespace
        .expected
        .iter()
        .any(|identity| identity != root.identity())
    {
        issue(
            report,
            crate::domain::resource::CoverageState::Invalid,
            "npm_receipt_root_rebound",
        );
    }
    if namespace.expected.contains(root.identity()) {
        namespace.sources.insert("receipt");
    }
    if namespace.sources.is_empty() {
        return Ok(vec![]);
    }
    let query = LocalNpmQuery {
        kind: NpmQueryKind::LocalV1,
        prefix: prefix.clone(),
        name: String::new(),
        contract: NpmContract::NativeV1,
        namespace: NpmNamespace {
            canonical_prefix: prefix.clone(),
            anchor: NpmAnchor::ExistingPrefix {
                path: prefix.clone(),
                identity: root.identity().clone(),
            },
        },
        config_bindings: None,
    };
    let mut chain = Chain::new(root)?;
    for part in ["lib", "node_modules"] {
        if !chain.descend(OsStr::new(part))? {
            return Ok(vec![]);
        }
    }
    let mut namespace_entries = 5000;
    let rows = scan_entries(&chain, &query, None, budget, &mut namespace_entries, report)?;
    chain.verify()?;
    Ok(rows)
}

fn scan_entries(
    chain: &Chain,
    query: &LocalNpmQuery,
    scope: Option<&str>,
    budget: &mut ScanBudget,
    namespace_entries: &mut usize,
    report: &mut crate::domain::resource::Coverage,
) -> Result<Vec<crate::domain::resource::Resource>> {
    use crate::domain::resource::CoverageState;
    let mut rows = vec![];
    let mut names = chain.last()?.names()?;
    loop {
        if budget.entries == 0 || *namespace_entries == 0 || budget.bytes == 0 {
            issue(report, CoverageState::Truncated, "npm_inventory_limit");
            break;
        }
        let Some(entry) = names.next() else { break };
        let entry =
            entry.map_err(|error| std::io::Error::from_raw_os_error(error.raw_os_error()))?;
        let raw = entry.file_name().to_bytes();
        if matches!(raw, b"." | b"..") {
            continue;
        }
        budget.entries -= 1;
        *namespace_entries -= 1;
        let Ok(name) = std::str::from_utf8(raw) else {
            issue(
                report,
                CoverageState::Invalid,
                &format!("npm_non_utf8_entry:{raw:02x?}"),
            );
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if scope.is_none() && name.starts_with('@') {
            let mut child = Chain {
                directories: chain.directories.clone(),
                lineage: chain.lineage.clone(),
            };
            if policy::name(&format!("{name}/a")).is_err() {
                issue(report, CoverageState::Invalid, "invalid_npm_scope");
                continue;
            }
            match child.descend(OsStr::new(name)) {
                Ok(true) => {
                    match scan_entries(&child, query, Some(name), budget, namespace_entries, report)
                        .and_then(|rows| {
                            child.verify()?;
                            Ok(rows)
                        }) {
                        Ok(mut packages) => rows.append(&mut packages),
                        Err(error) => issue(report, CoverageState::Invalid, &error.code),
                    }
                }
                Ok(false) => issue(report, CoverageState::Invalid, "npm_namespace_changed"),
                Err(error) => issue(report, CoverageState::Unsupported, &error.code),
            }
            continue;
        }
        let key = scope.map_or_else(|| name.to_owned(), |scope| format!("{scope}/{name}"));
        if policy::name(&key).is_err() {
            issue(report, CoverageState::Invalid, "invalid_npm_name");
            continue;
        }
        let mut query = query.clone();
        query.name = key;
        let observation = observe_entry(chain, &query, name, &mut budget.bytes);
        rows.push(package_row(&query, observation, report)?);
    }
    Ok(rows)
}

fn observe_entry(
    chain: &Chain,
    query: &LocalNpmQuery,
    name: &str,
    budget: &mut u64,
) -> Result<Observation> {
    let mut package = Chain {
        directories: chain.directories.clone(),
        lineage: chain.lineage.clone(),
    };
    if !package.descend(OsStr::new(name))? {
        return Err(changed());
    }
    let prefix = package
        .directories
        .first()
        .ok_or_else(changed)?
        .identity()
        .clone();
    let observation = read_manifest(query, &package, prefix, budget)?;
    package.verify()?;
    Ok(observation)
}

fn package_row(
    query: &LocalNpmQuery,
    observation: Result<Observation>,
    report: &mut crate::domain::resource::Coverage,
) -> Result<crate::domain::resource::Resource> {
    use crate::domain::resource::{CoverageState, Data, Management, Release, Resource, State};
    let prefix = &query.namespace.canonical_prefix;
    let (status, releases, issues) = match observation {
        Ok(Observation::NativePackage { version, .. }) => (
            State::Present,
            vec![Release {
                version,
                path: Some(prefix.join("lib/node_modules").join(&query.name)),
                active: true,
            }],
            vec![],
        ),
        Ok(_) => {
            issue(report, CoverageState::Invalid, "npm_namespace_changed");
            (State::Unknown, vec![], vec!["npm_namespace_changed".into()])
        }
        Err(error) => {
            let state = match error.code.as_str() {
                "unsupported_npm_manifest"
                | "unsupported_npm_placement"
                | "alias_requires_review" => CoverageState::Unsupported,
                "npm_manifest_limit" => CoverageState::Truncated,
                _ => CoverageState::Invalid,
            };
            issue(report, state, &error.code);
            (State::Unknown, vec![], vec![error.code])
        }
    };
    Ok(Resource {
        id: crate::policy::catalog::targets::npm_id(prefix, &query.name)?,
        name: query.name.clone(),
        aliases: vec![],
        management: Management::Native,
        status,
        data: Data::Application {
            manager: "npm".into(),
            native_id: query.name.clone(),
            scope: prefix.display().to_string(),
            releases,
            baseline: None,
            recipe_ref: None,
            issues,
        },
    })
}

#[cfg(test)]
#[path = "../../tests/unit/npm_scanner.rs"]
mod tests;
