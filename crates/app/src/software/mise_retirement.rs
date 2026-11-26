use crate::{
    agent::native_plugins_files::{Passive, Stamp},
    context::Context,
    domain::{
        Action, CommandSpec, MiseQuery, MiseRetirement, Observation, Package, ProcessOutput,
        Provider, Scope, fingerprint,
    },
    error::{Error, Result},
    operations::store::{Journal, Store},
    platform::fs::{self as paths, Directory},
};
use serde::Deserialize;
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::Command,
};

fn review() -> Error {
    Error::attention(
        "consumer_review_required",
        "仅可卸载已纳入管理、在所选范围内停用的单个 mise 版本；其他情况请核对项目后使用原生命令",
    )
}

fn changed() -> Error {
    Error::attention(
        "mise_release_changed",
        "mise 版本、载荷或原生程序已改变，请重新核对",
    )
}

#[derive(Deserialize)]
struct Release {
    version: String,
    active: bool,
    install_path: PathBuf,
}

fn rows(output: &ProcessOutput) -> Result<Vec<Release>> {
    if output.uncertain || output.code != Some(0) {
        return Err(review());
    }
    let value = crate::policy::json::parse(output.stdout.as_bytes()).map_err(|_| review())?;
    crate::policy::json::object_vec(value).map_err(|_| review())
}

fn exact_release(output: &ProcessOutput, query: &MiseQuery) -> Result<Release> {
    let Ok([release]) = <Vec<Release> as TryInto<[Release; 1]>>::try_into(rows(output)?) else {
        return Err(review());
    };
    let installs = &query.namespace.installs;
    if release.active
        || !installs.starts_with(&query.namespace.home)
        || release.install_path.parent().and_then(Path::parent) != Some(installs)
        || release
            .install_path
            .file_name()
            .and_then(|part| part.to_str())
            != Some(&release.version)
    {
        return Err(review());
    }
    crate::policy::operation::parsers::exact_version(&release.version)?;
    Ok(release)
}

fn payload(release: &Release) -> Result<crate::domain::FileId> {
    let directory = Directory::open(&release.install_path)?;
    let metadata = directory.file().metadata()?;
    if directory.path != release.install_path || metadata.uid() != nix::unistd::geteuid().as_raw() {
        return Err(review());
    }
    directory.verify()?;
    Ok(directory.identity().clone())
}

fn binary(program: &Path) -> Result<(PathBuf, String)> {
    let physical = paths::read_path(program)?;
    let mut reader = Passive::new()?;
    let stamp = reader.executable_stamp(&physical)?;
    if stamp == Stamp::Missing {
        return Err(review());
    }
    let digest = fingerprint(&stamp)?;
    reader.verify()?;
    Ok((physical, digest))
}

fn verify_native_resolution(scope: &Scope, query: &MiseQuery, release: &Release) -> Result<()> {
    let [verb, installed, json, name] = query.command.args.as_slice() else {
        return Err(changed());
    };
    if [verb.as_str(), installed, json] != ["ls", "--installed", "--json"] {
        return Err(changed());
    }
    let mut lookup = query.clone();
    lookup.command.args = vec!["where".into(), format!("{name}@{}", release.version)];
    let output =
        crate::operations::command::capture_query(&mut super::query_command(scope, &lookup)?)?;
    let raw = output.stdout.trim_end_matches(['\r', '\n']);
    if output.uncertain || output.code != Some(0) || Path::new(raw) != release.install_path {
        return Err(changed());
    }
    Ok(())
}

pub(super) fn capture(
    ctx: &Context,
    package: &Package,
    query: &MiseQuery,
) -> Result<MiseRetirement> {
    let receipts = crate::catalog::target::receipts(ctx)?;
    let original = receipts
        .installations
        .get(&package.id)
        .ok_or_else(|| Error::attention("adoption_required", "先确认当前 mise 安装基线"))?;
    let (program, program_sha256) = binary(&query.command.program)?;
    let mut scoped = super::query_command(&ctx.scope, query)?;
    let observed = crate::operations::command::capture_query(&mut scoped)?;
    let release = exact_release(&observed, query)?;
    verify_native_resolution(&ctx.scope, query, &release)?;
    let identity = payload(&release)?;
    Ok(MiseRetirement {
        version: release.version,
        payload: release.install_path,
        identity,
        program,
        program_sha256,
        receipt_sha256: fingerprint(original)?,
    })
}

pub(super) fn checked_program(query: &MiseQuery, retirement: &MiseRetirement) -> Result<PathBuf> {
    let (program, digest) = binary(&query.command.program)?;
    if program != retirement.program || digest != retirement.program_sha256 {
        return Err(changed());
    }
    Ok(program)
}

pub(crate) fn observe(
    output: &ProcessOutput,
    scope: &Scope,
    query: &MiseQuery,
    retirement: &MiseRetirement,
    post: bool,
) -> Result<Observation> {
    if post {
        return if rows(output)?.is_empty() && path_absent(&retirement.payload)? {
            Ok(Observation::Missing { evidence: None })
        } else {
            Err(changed())
        };
    }
    let release = exact_release(output, query)?;
    verify_native_resolution(scope, query, &release)?;
    if release.version != retirement.version
        || release.install_path != retirement.payload
        || payload(&release)? != retirement.identity
    {
        return Err(changed());
    }
    Ok(Observation::Present {
        version: release.version,
        evidence: None,
    })
}

pub(crate) fn bound_query(plan: &crate::domain::Plan) -> Option<&MiseRetirement> {
    plan.query.as_ref().and_then(query_witness)
}

pub(crate) fn query_witness(query: &crate::domain::QuerySpec) -> Option<&MiseRetirement> {
    let crate::domain::QuerySpec::Mise(query) = query else {
        return None;
    };
    query.retirement.as_ref()
}

pub(super) fn check_command(
    plan: &crate::domain::Plan,
    command: &Command,
    retirement: &MiseRetirement,
) -> Result<()> {
    let expected = write_spec(
        &plan.package,
        plan.action,
        plan.target.as_deref(),
        retirement,
    )?;
    if command.get_program() != expected.program.as_os_str()
        || command
            .get_args()
            .ne(expected.args.iter().map(String::as_str))
    {
        return Err(changed());
    }
    Ok(())
}

pub(crate) fn write_spec(
    package: &Package,
    action: Action,
    target: Option<&str>,
    retirement: &MiseRetirement,
) -> Result<CommandSpec> {
    let Provider::Mise { package } = &package.provider else {
        return Err(changed());
    };
    if action != Action::Uninstall || target != Some(&retirement.version) {
        return Err(changed());
    }
    Ok(CommandSpec {
        program: retirement.program.clone(),
        args: vec![
            "uninstall".into(),
            format!("{package}@{}", retirement.version),
        ],
    })
}

pub(crate) fn check_receipt(ctx: &Context, plan: &crate::domain::Plan) -> Result<()> {
    let Some(retirement) = bound_query(plan) else {
        return Ok(());
    };
    let records = crate::catalog::target::receipts(ctx)?;
    verify_receipt(&records, &plan.package.id, retirement, false)
}

pub(crate) fn verify_receipt(
    records: &crate::domain::Receipts,
    package: &str,
    retirement: &MiseRetirement,
    allow_absent: bool,
) -> Result<()> {
    match records.installations.get(package) {
        Some(current) if fingerprint(current)? == retirement.receipt_sha256 => Ok(()),
        None if allow_absent => Ok(()),
        _ => Err(changed()),
    }
}

fn path_absent(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn freeze_absence(store: &Store, journal: &Journal, query: &MiseQuery) -> Result<()> {
    let Some(retirement) = &query.retirement else {
        return Ok(());
    };
    let label = format!("{}-mise-release-absence.json", journal.id);
    let expected = fingerprint(&(&journal.plan_sha256, &journal.scope_sha256, query))?;
    match Store::read::<String>(&store.root, &label)? {
        Some(record) if record == expected => Ok(()),
        Some(_) => Err(changed()),
        None => {
            let records = store.receipts()?;
            verify_receipt(&records, &journal.plan.package.id, retirement, false)?;
            verify_absence(retirement)?;
            Store::atomic_new(&store.root, &label, &expected)
        }
    }
}

pub(crate) fn verify_absence(retirement: &MiseRetirement) -> Result<()> {
    if !path_absent(&retirement.payload)? {
        return Err(changed());
    }
    Ok(())
}
