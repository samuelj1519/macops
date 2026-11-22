use crate::{context::Context, error::Result, policy::resources::Filter, resource::Snapshot};
use std::path::Path;

pub fn snapshot(ctx: &Context, kind: Option<&str>, project: Option<&Path>) -> Result<Snapshot> {
    crate::governance::read(ctx, |_| snapshot_with_authority(ctx, kind, project, true))
}

pub(crate) fn applications(ctx: &Context, discover_brew: bool) -> Result<Snapshot> {
    crate::governance::read(ctx, |_| {
        snapshot_with_authority(ctx, Some("applications"), None, discover_brew)
    })
}

fn snapshot_with_authority(
    ctx: &Context,
    kind: Option<&str>,
    project: Option<&Path>,
    discover_brew: bool,
) -> Result<Snapshot> {
    let filter = Filter::parse(kind)?;
    if let Some(project) = project {
        let resolved = crate::paths::read_path(project)?;
        if !std::fs::metadata(resolved)?.is_dir() {
            return Err(crate::error::Error::new(
                "invalid_project_root",
                "project 必须是目录",
            ));
        }
    }
    let mut snapshot = Snapshot {
        schema_version: 1,
        home: ctx.home.clone(),
        project: project.map(Path::to_path_buf),
        resources: vec![],
        coverage: vec![],
        incomplete: false,
    };
    if filter.includes(Filter::Applications) {
        let (mut resources, mut coverage) =
            crate::resource_software::collect(ctx, project, discover_brew)?;
        snapshot.resources.append(&mut resources);
        snapshot.coverage.append(&mut coverage);
    }
    if filter == Filter::Applications {
        snapshot.finish();
        return Ok(snapshot);
    }
    let context_apps;
    let applications = if filter.includes(Filter::Applications) {
        &snapshot.resources
    } else {
        let (apps, coverage) = crate::resource_software::mise(ctx, project)?;
        context_apps = apps;
        snapshot.coverage.push(coverage);
        &context_apps
    };
    let (profiles, mut profile_coverage) =
        crate::resource_agents::collect(ctx, project, applications)?;
    if filter.includes(Filter::Configs) {
        let (mut resources, mut coverage) =
            crate::resource_configs::collect(ctx, &profiles, project)?;
        snapshot.resources.append(&mut resources);
        snapshot.coverage.append(&mut coverage);
    }
    if filter.includes(Filter::Capabilities) {
        let (mut resources, mut coverage) =
            crate::resource_capabilities::collect(ctx, &profiles, project)?;
        snapshot.resources.append(&mut resources);
        snapshot.coverage.append(&mut coverage);
    }
    snapshot.coverage.append(&mut profile_coverage);
    if filter.includes(Filter::Agents) {
        snapshot.resources.extend(profiles);
    }
    snapshot.finish();
    Ok(snapshot)
}

pub fn query(ctx: &Context, command: &crate::cli::Command) -> Result<Option<serde_json::Value>> {
    match command {
        crate::cli::Command::List { kind, project } => Ok(Some(serde_json::to_value(snapshot(
            ctx,
            kind.as_deref(),
            project.as_deref(),
        )?)?)),
        crate::cli::Command::Info { id, project } => {
            let kind = if id.starts_with("agent:") {
                Some("agents")
            } else if id.starts_with("capability:") {
                Some("capabilities")
            } else if id.starts_with("config:") {
                Some("configs")
            } else {
                None
            };
            let data = snapshot(ctx, kind, project.as_deref())?;
            Ok(Some(
                serde_json::json!({"resource":data.select(id)?,"coverage":&data.coverage,"incomplete":data.incomplete}),
            ))
        }
        _ => Ok(None),
    }
}

pub fn native_query(
    ctx: &Context,
    spec: &crate::domain::CommandSpec,
    cwd: &Path,
    client_root: Option<(&str, &Path)>,
) -> Result<crate::domain::ProcessOutput> {
    if ctx.isolated && !crate::paths::read_path(&spec.program)?.starts_with(&ctx.home) {
        return Err(crate::error::Error::new(
            "fixture_native_refused",
            "合成HOME不调用宿主原生程序",
        ));
    }
    crate::process::query_at(&ctx.scope, spec, cwd, client_root)
}

pub fn native_program(ctx: &Context, name: &str) -> Result<std::path::PathBuf> {
    let program = crate::process::executable(name)?;
    if ctx.isolated && !crate::paths::read_path(&program)?.starts_with(&ctx.home) {
        return Err(crate::error::Error::attention(
            "manager_missing",
            "该原生程序不在合成作用域",
        ));
    }
    Ok(program)
}
