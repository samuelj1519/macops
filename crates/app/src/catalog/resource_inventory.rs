use crate::{
    context::Context, domain::resource::Snapshot, error::Result, policy::catalog::resources::Filter,
};
use std::path::Path;

pub fn snapshot(ctx: &Context, kind: Option<&str>, project: Option<&Path>) -> Result<Snapshot> {
    crate::operations::governance::read(ctx, |_| snapshot_with_authority(ctx, kind, project, true))
}

pub(crate) fn applications(ctx: &Context, discover_brew: bool) -> Result<Snapshot> {
    crate::operations::governance::read(ctx, |_| {
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
        let resolved = crate::platform::fs::read_path(project)?;
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
            crate::software::resource_software::collect(ctx, project, discover_brew)?;
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
        let (apps, coverage) = crate::software::resource_software::mise(ctx, project)?;
        context_apps = apps;
        snapshot.coverage.push(coverage);
        &context_apps
    };
    let (profiles, mut profile_coverage) =
        crate::agent::resource_agents::collect(ctx, project, applications)?;
    if filter.includes(Filter::Configs) {
        let (mut resources, mut coverage) =
            crate::config::resource_configs::collect(ctx, &profiles, project)?;
        snapshot.resources.append(&mut resources);
        snapshot.coverage.append(&mut coverage);
    }
    if filter.includes(Filter::Capabilities) {
        let (mut resources, mut coverage) =
            crate::capability::resource_capabilities::collect(ctx, &profiles, project)?;
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
    use crate::cli::{AgentCmd, AppCmd, Command, Config, SkillCmd};
    let (kind, project, detail) = match command {
        Command::App {
            command: None | Some(AppCmd::Resources),
            project,
        } => ("applications", project, None),
        Command::Config {
            command: None | Some(Config::Resources),
            project,
        } => ("configs", project, None),
        Command::Skill {
            command: None,
            project: Some(project),
            ..
        } => {
            return snapshot(ctx, Some("capabilities"), Some(project))
                .and_then(|data| Ok(Some(serde_json::to_value(data)?)));
        }
        Command::Skill {
            command: Some(SkillCmd::Resources),
            project,
            ..
        } => ("capabilities", project, None),
        Command::Agent {
            command: None | Some(AgentCmd::Resources),
            project,
        } => ("agents", project, None),
        Command::Config {
            command: Some(Config::Detail(detail)),
            project,
        } => ("configs", project, Some(&detail.target)),
        Command::Skill {
            command: Some(SkillCmd::Detail(detail)),
            project,
            ..
        } => ("capabilities", project, Some(&detail.target)),
        Command::Agent {
            command: Some(AgentCmd::Detail(detail)),
            project,
        } => ("agents", project, Some(&detail.target)),
        _ => return Ok(None),
    };
    let data = snapshot(ctx, Some(kind), project.as_deref())?;
    let mut value = if let Some(term) = detail {
        match snapshot_detail(&data, kind, term) {
            Err(error) if kind == "agents" && project.is_none() && error.code == "not_found" => {
                crate::agent::ai::detail(ctx, crate::scoped_retry("agent", term).unwrap_or(term))?
                    .ok_or(error)?
            }
            result => result?,
        }
    } else {
        serde_json::to_value(data)?
    };
    if matches!(
        command,
        Command::Agent {
            command: None,
            project: None
        }
    ) {
        let tools = crate::agent::ai::list(ctx)?;
        value["tools"] = tools["tools"].clone();
        value["config_coverage"] = tools["config_coverage"].clone();
        if tools["incomplete"] == true {
            value["incomplete"] = serde_json::Value::Bool(true);
        }
    }
    Ok(Some(value))
}

pub(crate) fn snapshot_detail(
    data: &Snapshot,
    kind: &str,
    term: &str,
) -> Result<serde_json::Value> {
    let noun = match kind {
        "applications" => "app",
        "capabilities" => "skill",
        "configs" => "config",
        _ => "agent",
    };
    let resource = match data.select(term) {
        Err(error) if error.code == "not_found" => {
            crate::scoped_retry(noun, term).map_or(Err(error), |key| data.select(key))
        }
        result => result,
    }?;
    Ok(
        serde_json::json!({"resource":resource,"coverage":&data.coverage,"incomplete":data.incomplete}),
    )
}

pub fn native_query(
    ctx: &Context,
    spec: &crate::domain::CommandSpec,
    cwd: &Path,
    client_root: Option<(&str, &Path)>,
) -> Result<crate::domain::ProcessOutput> {
    if ctx.isolated && !crate::platform::fs::read_path(&spec.program)?.starts_with(&ctx.home) {
        return Err(crate::error::Error::new(
            "fixture_native_refused",
            "合成HOME不调用宿主原生程序",
        ));
    }
    crate::operations::command::query_at(&ctx.scope, spec, cwd, client_root)
}

pub fn native_program(ctx: &Context, name: &str) -> Result<std::path::PathBuf> {
    let program = crate::operations::command::executable(name)?;
    if ctx.isolated && !crate::platform::fs::read_path(&program)?.starts_with(&ctx.home) {
        return Err(crate::error::Error::attention(
            "manager_missing",
            "该原生程序不在合成作用域",
        ));
    }
    Ok(program)
}
