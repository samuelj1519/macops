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
    for root in roots {
        match profile(ctx, &root, project, applications) {
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
    for ProfileRoot {
        client,
        name,
        path,
        management,
    } in discover_roots(ctx)?
    {
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
            management,
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

pub(crate) struct SessionProfile {
    pub(crate) client: &'static str,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

pub(crate) fn resolve_session_profile(ctx: &Context, selector: &str) -> Result<SessionProfile> {
    let id_lookup = selector.starts_with("agent:");
    let ambiguous = || {
        Error::attention(
            "session_profile_ambiguous",
            "档案名称有重复，请使用详情中的档案 ID",
        )
    };
    let mut selection = None;
    let mut source_error = None;
    let sources = [
        (
            "codex",
            crate::codex_install::managed_profiles as fn(&Context) -> Result<_>,
        ),
        ("claude", crate::claude_install::managed_profiles),
    ];
    for (client, load) in sources {
        let prefix = format!("{client}:");
        if !id_lookup && !selector.starts_with(&prefix) {
            continue;
        }
        let profiles = match load(ctx) {
            Ok(profiles) => profiles,
            Err(error) => {
                source_error = Some(error);
                continue;
            }
        };
        for (leaf, path) in profiles {
            let Some(short) = leaf.strip_prefix(&format!(".{client}-")) else {
                continue;
            };
            let name = format!("{prefix}{short}");
            if (id_lookup && resource::id("agent", &(client, &path))? != selector)
                || (!id_lookup && name != selector)
            {
                continue;
            }
            if !id_lookup && client == "claude" {
                let alternate = paths::read_path(&ctx.home.join(&leaf))?;
                if alternate.is_dir() && !managed_root(&alternate) {
                    return Err(ambiguous());
                }
            }
            if selection
                .replace(SessionProfile { client, name, path })
                .is_some()
            {
                return Err(ambiguous());
            }
        }
    }
    selection.ok_or_else(|| source_error.unwrap_or_else(session_unavailable))
}

pub(crate) fn session_unavailable() -> Error {
    Error::attention(
        "codex_session_profile_unavailable",
        "请选择已纳管的独立档案",
    )
}

struct ProfileRoot {
    client: String,
    name: String,
    path: PathBuf,
    management: Management,
}

fn discover_roots(ctx: &Context) -> Result<Vec<ProfileRoot>> {
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
        if matches!(client, "codex" | "claude")
            && paths::read_path(&path).is_ok_and(|target| managed_root(&target))
        {
            continue;
        }
        if entry.file_type()?.is_dir() || entry.file_type()?.is_symlink() {
            roots.push(ProfileRoot {
                client: client.to_owned(),
                name: name.to_owned(),
                path,
                management: Management::Untracked,
            });
        }
    }
    for (name, profile) in crate::codex_install::managed_profiles(ctx)? {
        roots.push(ProfileRoot {
            client: "codex".into(),
            name,
            path: profile,
            management: Management::Adopted,
        });
    }
    for (name, profile) in crate::claude_install::managed_profiles(ctx)? {
        roots.push(ProfileRoot {
            client: "claude".into(),
            name,
            path: profile,
            management: Management::Adopted,
        });
    }
    for (client, path) in [
        ("claude", ctx.harness.claude.as_ref()),
        ("codex", ctx.harness.codex.as_ref()),
    ] {
        if let Some(path) = path {
            let physical = paths::read_path(path)?;
            if managed_root(&physical) {
                continue;
            }
            if !roots.iter().any(|root| &root.path == path) {
                roots.push(ProfileRoot {
                    client: client.into(),
                    name: format!(".{client}-environment"),
                    path: path.clone(),
                    management: Management::Untracked,
                });
            }
        }
    }
    Ok(roots)
}

fn managed_root(path: &Path) -> bool {
    crate::codex_install::reserved(path) || crate::claude_install::reserved(path)
}

fn profile(
    ctx: &Context,
    root: &ProfileRoot,
    project: Option<&Path>,
    applications: &[Resource],
) -> Result<Resource> {
    let client = root.client.as_str();
    let name = root.name.as_str();
    let path = root.path.as_path();
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
        management: root.management.clone(),
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
