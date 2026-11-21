use crate::{
    context::Context,
    error::{Error, Result},
    paths,
    policy::resources as policy,
    resource::{
        self, Coverage, CoverageState, Data, Management, Model, Resource, RuntimeBinding, State,
    },
};
use std::path::{Path, PathBuf};

pub fn collect(
    ctx: &Context,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<(Vec<Resource>, Vec<Coverage>)> {
    let roots = discover_roots(ctx)?;
    let mut profiles = vec![];
    let mut coverage = Coverage {
        source: "harness-profiles".into(),
        state: CoverageState::Complete,
        count: 0,
        roots: vec![ctx.home.clone()],
        issues: vec![],
    };
    for (client, name, path) in roots {
        match profile(ctx, &client, &name, &path, project, applications) {
            Ok(resource) => {
                if resource.status == State::Unknown {
                    coverage.state = CoverageState::Invalid;
                    coverage
                        .issues
                        .push(format!("profile_metadata_unknown:{}", resource.id));
                }
                profiles.push(resource);
            }
            Err(error) => {
                coverage.state = CoverageState::Invalid;
                coverage.issues.push(error.code);
            }
        }
    }
    coverage.count = profiles.len();
    Ok((profiles, vec![coverage]))
}

pub fn profile_roots(ctx: &Context) -> Result<Vec<Resource>> {
    profile_roots_matching(ctx, |_, _| true)
}

pub(crate) fn profile_roots_matching(
    ctx: &Context,
    include: impl Fn(&str, &str) -> bool,
) -> Result<Vec<Resource>> {
    let mut profiles = vec![];
    for (client, name, path) in discover_roots(ctx)? {
        let prefix = format!(".{client}-");
        let prof = if name == format!(".{client}") {
            "default"
        } else {
            name.strip_prefix(&prefix).unwrap_or(&name)
        };
        if !include(&client, prof)
            || !std::fs::metadata(paths::read_path(&path)?).is_ok_and(|m| m.is_dir())
        {
            continue;
        }
        profiles.push(Resource {
            id: resource::id("agent", &(&client, &path))?,
            name: format!("{client}:{prof}"),
            aliases: vec![format!("{client}-{prof}"), format!("agent:{client}:{prof}")],
            management: Management::Untracked,
            status: State::Present,
            data: Data::Agent {
                client,
                profile: prof.into(),
                config_dir: path,
                model: Model::default(),
                runtime: RuntimeBinding {
                    selector: "unresolved".into(),
                    context: ctx.home.clone(),
                    binary: None,
                    install_ref: None,
                    evidence: None,
                },
            },
        });
    }
    Ok(profiles)
}

fn discover_roots(ctx: &Context) -> Result<Vec<(String, String, PathBuf)>> {
    let mut roots = vec![];
    for entry in std::fs::read_dir(paths::read_path(&ctx.home)?)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let client = if name == ".claude" || name.starts_with(".claude-") {
            "claude"
        } else if name == ".codex" {
            "codex"
        } else if name == ".cursor" {
            "cursor"
        } else if matches!(name, ".openclaw" | ".kimi" | ".mimocode" | ".opencode") {
            &name[1..]
        } else {
            continue;
        };
        let path = entry.path();
        if entry.file_type()?.is_dir() || entry.file_type()?.is_symlink() {
            roots.push((client.to_owned(), name.to_owned(), path));
        }
    }
    for (client, path) in [
        ("claude", ctx.harness.claude.as_ref()),
        ("codex", ctx.harness.codex.as_ref()),
    ] {
        if let Some(path) = path {
            paths::read_path(path)?;
            if !roots.iter().any(|(_, _, p)| p == path) {
                roots.push((
                    client.into(),
                    format!(".{client}-environment"),
                    path.clone(),
                ));
            }
        }
    }
    Ok(roots)
}

fn profile(
    ctx: &Context,
    client: &str,
    name: &str,
    path: &Path,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<Resource> {
    if !std::fs::metadata(paths::read_path(path)?)?.is_dir() {
        return Err(Error::new("invalid_profile_root", "profile 根不是目录"));
    }
    let prefix = format!(".{client}-");
    let profile = if name == format!(".{client}") {
        "default"
    } else {
        name.strip_prefix(&prefix).unwrap_or(name)
    };
    let mut status = State::Present;
    let settings = path.join("settings.json");
    let model = if client == "claude" && paths::read_path(&settings)?.exists() {
        if let Ok(model) = policy::claude_model(&paths::regular_bytes(&settings)?) {
            model
        } else {
            status = State::Unknown;
            Model::default()
        }
    } else {
        Model::default()
    };
    let id = resource::id("agent", &(client, path))?;
    Ok(Resource {
        id,
        name: format!("{client}:{profile}"),
        aliases: vec![
            format!("{client}-{profile}"),
            format!("agent:{client}:{profile}"),
        ],
        management: Management::Untracked,
        status,
        data: Data::Agent {
            client: client.into(),
            profile: profile.into(),
            config_dir: path.to_path_buf(),
            model,
            runtime: runtime(ctx, client, profile, path, project, applications)?,
        },
    })
}

fn runtime(
    ctx: &Context,
    client: &str,
    profile: &str,
    root: &Path,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<RuntimeBinding> {
    let cwd = project.unwrap_or(&ctx.home);
    let helper = ctx.home.join(".local/bin/claudectl");
    let wrapper = ctx.home.join(format!(".local/bin/claude-{profile}"));
    let is_mise = client == "claude"
        && paths::read_path(&helper).is_ok()
        && paths::read_path(&wrapper).is_ok()
        && helper.is_file()
        && wrapper.is_file()
        && policy::legacy_mise_resolver(&paths::regular_bytes(&helper)?)
        && policy::legacy_claude_wrapper(&paths::regular_bytes(&wrapper)?, &helper, profile);
    let mut runtime = RuntimeBinding {
        selector: if is_mise {
            "inspected-mise-context"
        } else {
            "unresolved"
        }
        .into(),
        context: cwd.to_path_buf(),
        binary: None,
        install_ref: None,
        evidence: is_mise.then_some(wrapper),
    };
    if is_mise {
        let Ok(program) = crate::resource_inventory::native_program(ctx, "mise") else {
            return Ok(runtime);
        };
        let output = crate::resource_inventory::native_query(
            ctx,
            &crate::domain::CommandSpec {
                program,
                args: vec!["which".into(), "claude".into()],
            },
            cwd,
            Some(("CLAUDE_CONFIG_DIR", root)),
        )?;
        if output.code == Some(0) && !output.uncertain {
            let path = PathBuf::from(output.stdout.trim());
            if let Ok(binary) = paths::read_path(&path) {
                runtime.install_ref = policy::bind_runtime(&binary, applications);
                runtime.binary = Some(path);
            }
        }
    }
    Ok(runtime)
}
