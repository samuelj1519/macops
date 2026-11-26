use crate::{
    domain::{
        ExistingUvNamespace, FileId, LocalUvToolQuery, Observation, UvDirectoryAnchor, UvQueryKind,
        UvToolBinding, UvToolEvidence,
    },
    error::{Error, Result},
    software::rules::uv_local as policy,
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat, fstat, open, openat, readlinkat, statat};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub const TOOL_LIMIT: usize = 4096;
pub const SITE_LIMIT: usize = 4096;
pub const ENTRY_LIMIT: usize = 128;
pub const FILE_LIMIT: u64 = 1024 * 1024;
pub const PYVENV_LIMIT: u64 = 64 * 1024;
pub const TOTAL_LIMIT: u64 = 8 * 1024 * 1024;

fn unknown() -> Error {
    Error::attention(
        "uv_observation_unknown",
        "uv 本地布局不完整、已改变或不受支持",
    )
}
fn io_error(_: rustix::io::Errno) -> Error {
    unknown()
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn id(stat: &Stat) -> FileId {
    FileId {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    }
}
fn stamp(stat: &Stat) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}:{}:{}",
        stat.st_dev,
        stat.st_ino,
        stat.st_mode,
        stat.st_size,
        stat.st_mtime,
        stat.st_mtime_nsec,
        stat.st_ctime,
        stat.st_ctime_nsec
    )
}
fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}
fn leaf_flags() -> OFlags {
    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}
fn component(name: &OsStr) -> Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(p)) if p==name)
        || components.next().is_some()
    {
        return Err(unknown());
    }
    Ok(())
}
#[derive(Debug)]
struct Directory {
    file: File,
    path: PathBuf,
    identity: FileId,
    parent: Option<(Arc<Self>, OsString)>,
}
impl Directory {
    fn open(path: &Path) -> Result<Arc<Self>> {
        if crate::policy::paths::lexical(path)? != path {
            return Err(unknown());
        }
        let file = File::from(open("/", directory_flags(), Mode::empty()).map_err(io_error)?);
        let identity = id(&fstat(&file).map_err(io_error)?);
        let mut dir = Arc::new(Self {
            file,
            path: PathBuf::from("/"),
            identity,
            parent: None,
        });
        for part in path.components().skip(1) {
            dir = dir.child(part.as_os_str())?;
        }
        dir.verify()?;
        Ok(dir)
    }
    fn child(self: &Arc<Self>, name: &OsStr) -> Result<Arc<Self>> {
        component(name)?;
        let file = File::from(
            openat(&self.file, name, directory_flags(), Mode::empty()).map_err(io_error)?,
        );
        let identity = id(&fstat(&file).map_err(io_error)?);
        let child = Arc::new(Self {
            file,
            path: self.path.join(name),
            identity,
            parent: Some((self.clone(), name.to_owned())),
        });
        child.verify()?;
        Ok(child)
    }
    fn verify(&self) -> Result<()> {
        if id(&fstat(&self.file).map_err(io_error)?) != self.identity {
            return Err(unknown());
        }
        if let Some((parent, name)) = &self.parent {
            parent.verify()?;
            let stat = statat(&parent.file, name, AtFlags::SYMLINK_NOFOLLOW).map_err(io_error)?;
            if FileType::from_raw_mode(stat.st_mode) != FileType::Directory
                || id(&stat) != self.identity
            {
                return Err(unknown());
            }
        }
        Ok(())
    }
    fn stat(&self, name: &OsStr) -> Result<Option<Stat>> {
        component(name)?;
        match statat(&self.file, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(stat)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(e) => Err(io_error(e)),
        }
    }
    fn names(&self, limit: usize) -> Result<Vec<OsString>> {
        let mut out = vec![];
        for entry in rustix::fs::Dir::read_from(&self.file).map_err(io_error)? {
            let entry = entry.map_err(io_error)?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            if out.len() >= limit {
                return Err(Error::attention("uv_scan_limit", "uv 目录枚举超出预算"));
            }
            out.push(name.to_owned());
        }
        self.verify()?;
        out.sort();
        Ok(out)
    }
    fn tool_names(&self) -> Result<Vec<OsString>> {
        let mut names = self.names(TOOL_LIMIT + 1)?;
        names.retain(|n| n != OsStr::new(".lock"));
        if names.len() > TOOL_LIMIT {
            return Err(Error::attention("uv_scan_limit", "uv 工具目录超出预算"));
        }
        Ok(names)
    }
    fn leaf(&self, name: &OsStr, limit: u64, budget: &mut u64) -> Result<(Vec<u8>, String)> {
        self.leaf_with(name, limit, budget, |_, _| Ok(()))
    }
    fn open_leaf(
        &self,
        name: &OsStr,
        event: &mut impl FnMut(LeafEvent, Option<&FileId>) -> Result<()>,
    ) -> Result<File> {
        component(name)?;
        self.verify()?;
        event(LeafEvent::BeforeOpen, None)?;
        let file =
            File::from(openat(&self.file, name, leaf_flags(), Mode::empty()).map_err(io_error)?);
        let meta = fstat(&file).map_err(io_error)?;
        event(LeafEvent::Opened, Some(&id(&meta)))?;
        Ok(file)
    }
    fn leaf_with(
        &self,
        name: &OsStr,
        limit: u64,
        budget: &mut u64,
        mut event: impl FnMut(LeafEvent, Option<&FileId>) -> Result<()>,
    ) -> Result<(Vec<u8>, String)> {
        let file = self.open_leaf(name, &mut event)?;
        let before = fstat(&file).map_err(io_error)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
            return Err(unknown());
        }
        let size = u64::try_from(before.st_size).map_err(|_| unknown())?;
        if size > limit || size > *budget {
            return Err(Error::attention("uv_read_limit", "uv 文件读取超出预算"));
        }
        let original = stamp(&before);
        if self.stat(name)?.is_none_or(|s| stamp(&s) != original) {
            return Err(unknown());
        }
        *budget -= size;
        event(LeafEvent::BeforeRead, None)?;
        let mut bytes = vec![];
        (&file)
            .take(size.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size
            || stamp(&fstat(&file).map_err(io_error)?) != original
            || self.stat(name)?.is_none_or(|s| stamp(&s) != original)
        {
            return Err(unknown());
        }
        self.verify()?;
        Ok((bytes, original))
    }
    fn link(&self, name: &OsStr) -> Result<(PathBuf, String)> {
        let before = self.stat(name)?.ok_or_else(unknown)?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Symlink {
            return Err(unknown());
        }
        let text = readlinkat(&self.file, name, Vec::new()).map_err(io_error)?;
        let target = PathBuf::from(OsStr::from_bytes(text.to_bytes()));
        if self.stat(name)?.is_none_or(|s| stamp(&s) != stamp(&before)) {
            return Err(unknown());
        }
        self.verify()?;
        Ok((target, stamp(&before)))
    }
    fn anchor(&self) -> UvDirectoryAnchor {
        UvDirectoryAnchor {
            path: self.path.clone(),
            identity: self.identity.clone(),
        }
    }
    fn existing_lock(&self) -> Result<Option<File>> {
        let name = OsStr::new(".lock");
        let Some(before) = self.stat(name)? else {
            self.verify()?;
            return Ok(None);
        };
        let file = self.open_leaf(name, &mut |_, _| Ok(()))?;
        let meta = fstat(&file).map_err(io_error)?;
        if FileType::from_raw_mode(meta.st_mode) != FileType::RegularFile
            || meta.st_size > i64::try_from(PYVENV_LIMIT).map_err(|_| unknown())?
            || stamp(&meta) != stamp(&before)
        {
            return Err(unknown());
        }
        fs2::FileExt::try_lock_shared(&file).map_err(|_| unknown())?;
        if self.stat(name)?.is_none_or(|s| stamp(&s) != stamp(&meta)) {
            return Err(unknown());
        }
        self.verify()?;
        Ok(Some(file))
    }
}
use std::os::unix::ffi::OsStrExt;
#[derive(Debug)]
enum LeafEvent {
    BeforeOpen,
    Opened,
    BeforeRead,
}

pub fn capture(binding: &UvToolBinding, name: &str) -> Result<LocalUvToolQuery> {
    let root = Directory::open(&binding.tool_dir)?;
    let bin = Directory::open(&binding.bin_dir)?;
    Ok(LocalUvToolQuery {
        kind: UvQueryKind::LocalV1,
        name: policy::normalize_name(name)?,
        namespace: ExistingUvNamespace {
            tool_dir: root.anchor(),
            bin_dir: bin.anchor(),
        },
    })
}
fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| unknown())
}
fn read(
    dir: &Directory,
    name: &str,
    limit: u64,
    budget: &mut u64,
    proofs: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let (bytes, stamp) = dir.leaf(OsStr::new(name), limit, budget)?;
    proofs.push(format!(
        "{}:{stamp}:{}",
        dir.path.join(name).display(),
        digest(&bytes)
    ));
    Ok(bytes)
}
fn metadata(bytes: &[u8], name: &str) -> Result<String> {
    let mut names = vec![];
    let mut versions = vec![];
    for line in text(bytes)?.lines().take_while(|s| !s.is_empty()) {
        if line.starts_with(char::is_whitespace) {
            return Err(unknown());
        }
        if let Some((header, value)) = line.split_once(':') {
            if header.eq_ignore_ascii_case("Name") {
                names.push(value.trim());
            }
            if header.eq_ignore_ascii_case("Version") {
                versions.push(value.trim());
            }
        }
    }
    match (names.as_slice(), versions.as_slice()) {
        ([found], [version])
            if policy::normalize_name(found)? == name
                && !version.is_empty()
                && version.len() <= 128 =>
        {
            Ok((*version).into())
        }
        _ => Err(unknown()),
    }
}
fn receipt(bytes: &[u8], name: &str) -> Result<Vec<(String, PathBuf)>> {
    let doc = text(bytes)?
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| unknown())?;
    let tool = doc
        .get("tool")
        .and_then(toml_edit::Item::as_table)
        .ok_or_else(unknown)?;
    let requirements = tool
        .get("requirements")
        .and_then(toml_edit::Item::as_array)
        .ok_or_else(unknown)?;
    let mut primary = false;
    for value in requirements {
        let table = value.as_inline_table().ok_or_else(unknown)?;
        let n = table
            .get("name")
            .and_then(toml_edit::Value::as_str)
            .ok_or_else(unknown)?;
        if policy::normalize_name(n)? == name {
            primary = true;
        }
    }
    if !primary {
        return Err(unknown());
    }
    let entries = tool
        .get("entrypoints")
        .and_then(toml_edit::Item::as_array)
        .ok_or_else(unknown)?;
    if entries.len() > ENTRY_LIMIT {
        return Err(unknown());
    }
    let mut out = vec![];
    let mut seen = BTreeSet::new();
    for entry in entries {
        let table = entry.as_inline_table().ok_or_else(unknown)?;
        let n = table
            .get("name")
            .and_then(toml_edit::Value::as_str)
            .ok_or_else(unknown)?;
        if !crate::policy::catalog::resources::component(n) || !seen.insert(n) {
            return Err(unknown());
        }
        let path = table
            .get("install-path")
            .and_then(toml_edit::Value::as_str)
            .ok_or_else(unknown)?;
        out.push((n.into(), PathBuf::from(path)));
    }
    Ok(out)
}
fn scan_metadata(
    env: &Arc<Directory>,
    name: &str,
    budget: &mut u64,
    proofs: &mut Vec<String>,
) -> Result<(String, Vec<u8>)> {
    let lib = env.child(OsStr::new("lib"))?;
    let mut matches = vec![];
    let mut remaining_entries = SITE_LIMIT;
    for python in lib.names(SITE_LIMIT)? {
        let Some(label) = python.to_str() else {
            return Err(unknown());
        };
        if !label.starts_with("python") {
            continue;
        }
        let version = &label[6..];
        if !version.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty() && !b.is_empty() && a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit())
        }) {
            return Err(unknown());
        }
        let python = lib.child(&python)?;
        let site = python.child(OsStr::new("site-packages"))?;
        for dist in site.names(remaining_entries)? {
            remaining_entries -= 1;
            let Some(label) = dist.to_str() else {
                return Err(unknown());
            };
            let Some(base) = label.strip_suffix(".dist-info") else {
                continue;
            };
            let Some((package, _)) = base.rsplit_once('-') else {
                return Err(unknown());
            };
            if policy::normalize_name(package)? != name {
                continue;
            }
            let dist = site.child(&dist)?;
            let bytes = read(&dist, "METADATA", FILE_LIMIT, budget, proofs)?;
            let version = metadata(&bytes, name)?;
            matches.push((version, bytes));
            dist.verify()?;
        }
        site.verify()?;
        python.verify()?;
    }
    let [one] = <[(String, Vec<u8>); 1]>::try_from(matches).map_err(|_| unknown())?;
    lib.verify()?;
    Ok(one)
}
fn sample(query: &LocalUvToolQuery, budget: &mut u64) -> Result<Observation> {
    let root = Directory::open(&query.namespace.tool_dir.path)?;
    let bin = Directory::open(&query.namespace.bin_dir.path)?;
    if root.identity != query.namespace.tool_dir.identity
        || bin.identity != query.namespace.bin_dir.identity
        || policy::normalize_name(&query.name)? != query.name
    {
        return Err(unknown());
    }
    let lock_before = root.stat(OsStr::new(".lock"))?.as_ref().map(stamp);
    let _lock = root.existing_lock()?;
    let env = root.child(OsStr::new(&query.name))?;
    let mut proofs = vec![];
    let receipt_bytes = read(&env, "uv-receipt.toml", FILE_LIMIT, budget, &mut proofs)?;
    let entries = receipt(&receipt_bytes, &query.name)?;
    let pyvenv = read(&env, "pyvenv.cfg", PYVENV_LIMIT, budget, &mut proofs)?;
    let homes: Vec<_> = text(&pyvenv)?
        .lines()
        .filter_map(|line| {
            line.split_once('=')
                .filter(|(key, _)| key.trim() == "home")
                .map(|(_, value)| value.trim())
        })
        .collect();
    if !matches!(homes.as_slice(),[home] if Path::new(home).is_absolute()) {
        return Err(unknown());
    }
    let (version, metadata_bytes) = scan_metadata(&env, &query.name, budget, &mut proofs)?;
    let internal_bin = env.child(OsStr::new("bin"))?;
    let mut stamps = vec![];
    for (name, install_path) in entries {
        if install_path.parent() != Some(bin.path.as_path())
            || install_path.file_name() != Some(OsStr::new(&name))
        {
            return Err(unknown());
        }
        let (target, link_stamp) = bin.link(OsStr::new(&name))?;
        let target = if target.is_absolute() {
            target
        } else {
            bin.path.join(target)
        };
        let target = crate::policy::paths::normalize_link(&target)?;
        if target != internal_bin.path.join(&name) {
            return Err(unknown());
        }
        let bytes = read(&internal_bin, &name, FILE_LIMIT, budget, &mut proofs)?;
        stamps.push(format!(
            "{name}:{link_stamp}:{}:{}",
            target.display(),
            digest(&bytes)
        ));
    }
    for interpreter in ["python", "python3"] {
        if let Some(stat) = internal_bin.stat(OsStr::new(interpreter))? {
            if FileType::from_raw_mode(stat.st_mode) != FileType::Symlink {
                return Err(unknown());
            }
            let (target, stamp) = internal_bin.link(OsStr::new(interpreter))?;
            stamps.push(format!("{interpreter}:{stamp}:{}", target.display()));
        }
    }
    root.verify()?;
    bin.verify()?;
    env.verify()?;
    internal_bin.verify()?;
    if root.stat(OsStr::new(".lock"))?.as_ref().map(stamp) != lock_before {
        return Err(unknown());
    }
    proofs.push(format!("lock:{lock_before:?}"));
    let namespace_identity = crate::domain::fingerprint(&query.namespace)?;
    let evidence = UvToolEvidence {
        namespace_identity,
        payload_identity: env.identity.clone(),
        receipt_sha256: digest(&receipt_bytes),
        metadata_sha256: digest(&metadata_bytes),
        pyvenv_sha256: digest(&pyvenv),
        entrypoints: stamps,
        fingerprint: crate::domain::fingerprint(&proofs)?,
    };
    Ok(Observation::UvTool { version, evidence })
}
pub fn observe(query: &LocalUvToolQuery, budget: &mut u64) -> Result<Observation> {
    let before = sample(query, budget)?;
    let after = sample(query, budget)?;
    if before != after {
        return Err(unknown());
    }
    Ok(before)
}

#[cfg(test)]
#[path = "../../tests/unit/uv_local.rs"]
mod tests;

#[derive(Clone, Debug, Default)]
pub struct DiscoveryInputs {
    pub xdg_bin_home: Option<PathBuf>,
}
impl DiscoveryInputs {
    pub fn capture(isolated: bool) -> Self {
        Self {
            xdg_bin_home: if isolated {
                None
            } else {
                std::env::var_os("XDG_BIN_HOME").map(PathBuf::from)
            },
        }
    }
}
fn default_binding(ctx: &crate::context::Context) -> Result<UvToolBinding> {
    let get = |key: &str| ctx.scope.paths.get(key).and_then(Option::as_ref);
    let tool_dir = match get("UV_TOOL_DIR") {
        Some(path) => path.clone(),
        None if get("XDG_DATA_HOME").is_some() || get("XDG_STATE_HOME").is_some() => {
            return Err(Error::attention(
                "uv_default_root_unavailable",
                "XDG 覆盖的 uv 默认根尚未固定，请显式绑定",
            ));
        }
        None => ctx.home.join(".local/share/uv/tools"),
    };
    let bin_dir = get("UV_TOOL_BIN_DIR")
        .cloned()
        .or_else(|| ctx.uv.xdg_bin_home.clone())
        .or_else(|| get("XDG_DATA_HOME").and_then(|p| p.parent().map(|p| p.join("bin"))))
        .unwrap_or_else(|| ctx.home.join(".local/bin"));
    crate::policy::paths::lexical(&tool_dir)?;
    crate::policy::paths::lexical(&bin_dir)?;
    Ok(UvToolBinding { tool_dir, bin_dir })
}
pub fn bindings(
    ctx: &crate::context::Context,
) -> Result<(Vec<UvToolBinding>, Vec<crate::domain::resource::Coverage>)> {
    use crate::domain::resource::{Coverage, CoverageState};
    let catalog = crate::catalog::inventory::declared_catalog(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let mut bindings = vec![];
    let mut reports = vec![];
    for package in &catalog.packages {
        if let Some((_, binding)) = policy::descriptor(&package.provider) {
            bindings.push(binding.clone());
        }
    }
    for receipt in receipts.installations.values() {
        if let Some(query @ crate::domain::QuerySpec::LocalUvTool(local)) = &receipt.query {
            policy::binding(&receipt.package, query)?;
            bindings.push(crate::software::uv_ownership::query_binding(local));
        }
    }
    match default_binding(ctx) {
        Ok(binding) => bindings.push(binding),
        Err(e) => reports.push(Coverage {
            source: "uv-local".into(),
            state: CoverageState::Unavailable,
            count: 0,
            roots: vec![],
            issues: vec![e.code],
        }),
    }
    bindings.sort_by(|a, b| (&a.tool_dir, &a.bin_dir).cmp(&(&b.tool_dir, &b.bin_dir)));
    bindings.dedup();
    Ok((bindings, reports))
}
pub fn inventory(
    ctx: &crate::context::Context,
) -> Result<(
    Vec<crate::domain::resource::Resource>,
    Vec<crate::domain::resource::Coverage>,
)> {
    let (bindings, mut reports) = bindings(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let mut resources = vec![];
    for binding in bindings {
        let (mut items, report) = scan_namespace(&binding, &receipts, &ctx.scope)?;
        resources.append(&mut items);
        reports.push(report);
    }
    crate::software::uv_ownership::annotate(ctx, &mut resources, &mut reports)?;
    Ok((resources, reports))
}

fn adopted_resources(
    binding: &UvToolBinding,
    receipts: &crate::domain::Receipts,
    scope: &crate::domain::Scope,
) -> Result<Vec<crate::domain::resource::Resource>> {
    let mut resources = vec![];
    for receipt in receipts.installations.values() {
        let Some(query @ crate::domain::QuerySpec::LocalUvTool(local)) = &receipt.query else {
            continue;
        };
        if crate::software::uv_ownership::query_binding(local) != *binding {
            continue;
        }
        let observation =
            crate::operations::providers::observe_query(&receipt.package, query, scope, false)
                .unwrap_or_else(|e| Observation::Unknown { reason: e.code });
        let mut resource = observed_resource(binding, &local.name, &observation)?;
        if let crate::domain::resource::Data::Application { baseline, .. } = &mut resource.data {
            *baseline = Some(crate::policy::catalog::targets::baseline(
                &receipt.observation,
                observation,
            ));
        }
        resources.push(resource);
    }
    Ok(resources)
}

fn scan_namespace(
    binding: &UvToolBinding,
    receipts: &crate::domain::Receipts,
    scope: &crate::domain::Scope,
) -> Result<(
    Vec<crate::domain::resource::Resource>,
    crate::domain::resource::Coverage,
)> {
    use crate::domain::resource::{Coverage, CoverageState};
    let mut resources = adopted_resources(binding, receipts, scope)?;
    let adopted_ids: BTreeSet<_> = resources.iter().map(|item| item.id.clone()).collect();
    let mut report = Coverage {
        source: "uv-local".into(),
        state: CoverageState::Complete,
        count: resources.len(),
        roots: vec![binding.tool_dir.clone(), binding.bin_dir.clone()],
        issues: vec![],
    };
    for resource in &resources {
        if let crate::domain::resource::Data::Application {
            baseline: Some(baseline),
            issues,
            ..
        } = &resource.data
            && baseline.comparison != crate::domain::resource::Comparison::Same
        {
            report.state = CoverageState::Invalid;
            report.issues.extend(issues.clone());
            report.issues.push("uv_baseline_changed".into());
        }
    }
    let root = match Directory::open(&binding.tool_dir) {
        Ok(root) => root,
        Err(e) => {
            report.state = if !resources.is_empty() {
                CoverageState::Invalid
            } else if std::fs::symlink_metadata(&binding.tool_dir)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
            {
                CoverageState::Absent
            } else {
                CoverageState::Unavailable
            };
            report.issues.push(e.code);
            return Ok((resources, report));
        }
    };
    let names = match root.tool_names() {
        Ok(names) => names,
        Err(e) => {
            report.state = CoverageState::Invalid;
            report.issues.push(e.code);
            return Ok((resources, report));
        }
    };
    for name in names {
        let Some(name) = name.to_str() else {
            report.state = CoverageState::Invalid;
            report.issues.push("uv_invalid_name".into());
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        if policy::normalize_name(name).is_err() || policy::normalize_name(name)? != name {
            report.state = CoverageState::Invalid;
            report.issues.push("uv_invalid_name".into());
            continue;
        }
        let identity = policy::tool_id(&binding.tool_dir, name)?;
        if adopted_ids.contains(&identity) {
            continue;
        }
        let observation = capture(binding, name)
            .and_then(|q| observe(&q, &mut TOTAL_LIMIT.clone()))
            .unwrap_or_else(|e| Observation::Unknown { reason: e.code });
        if !matches!(observation, Observation::UvTool { .. }) {
            report.state = CoverageState::Invalid;
        }
        resources.push(observed_resource(binding, name, &observation)?);
        report.count += 1;
    }
    if root.verify().is_err() {
        report.state = CoverageState::Invalid;
        report.issues.push("uv_namespace_changed".into());
    }

    Ok((resources, report))
}

fn observed_resource(
    binding: &UvToolBinding,
    name: &str,
    observation: &Observation,
) -> Result<crate::domain::resource::Resource> {
    use crate::domain::resource::{Data, Management, Release, Resource, State};
    let status = if matches!(observation, Observation::UvTool { .. }) {
        State::Present
    } else {
        State::Unknown
    };
    let issues = if let Observation::Unknown { reason } = &observation {
        vec![reason.clone()]
    } else {
        vec![]
    };
    let resource = Resource {
        id: policy::tool_id(&binding.tool_dir, name)?,
        name: name.into(),
        aliases: vec![],
        management: Management::Native,
        status,
        data: Data::Application {
            manager: "uv".into(),
            native_id: name.into(),
            scope: binding.tool_dir.display().to_string(),
            releases: observation
                .version()
                .map(|v| Release {
                    version: v.into(),
                    path: Some(binding.tool_dir.join(name)),
                    active: true,
                })
                .into_iter()
                .collect(),
            baseline: None,
            issues,
            recipe_ref: None,
        },
    };
    Ok(resource)
}
