use crate::{
    domain::{Action, CommandSpec, Package, Provider},
    error::{Error, Result},
    policy::parsers::exact_version,
};
use std::path::Path;
#[derive(Debug, PartialEq, Eq)]
pub enum Template {
    Exact(CommandSpec),
    Named { name: String, args: Vec<String> },
}

pub fn available_query(home: &Path, p: &Package) -> Result<Template> {
    crate::policy::uv_local::action(&p.provider, Action::Update)?;
    let (name, args) = match &p.provider {
        Provider::Command {
            available: Some(command),
            ..
        } => return Ok(Template::Exact(command.clone())),
        Provider::Brew { package, cask } => (
            "brew",
            vec![
                "info".into(),
                "--json=v2".into(),
                if *cask {
                    "--cask".into()
                } else {
                    "--formula".into()
                },
                package.clone(),
            ],
        ),
        Provider::Mise { package } => ("mise", vec!["latest".into(), package.clone()]),
        Provider::Npm {
            package,
            prefix,
            authority,
        } => (
            "npm",
            crate::policy::npm_local::expected_npm_view_args(
                home,
                prefix,
                package,
                authority.as_ref(),
            )?,
        ),
        Provider::Bun { package } => (
            "npm",
            vec![
                "view".into(),
                package.clone(),
                "version".into(),
                "--json".into(),
            ],
        ),
        _ => {
            return Err(Error::attention(
                "version_query_unsupported",
                "该渠道需要显式 --version，或由原生/legacy 来源维护",
            ));
        }
    };
    Ok(Template::Named {
        name: name.into(),
        args,
    })
}

pub fn discovery_query(manager: &str) -> Result<Template> {
    let args: &[&str] = match manager {
        "brew" => &["info", "--json=v2", "--installed"],
        "mise" => &["ls", "--installed", "--json"],
        "uv" => {
            return Err(Error::attention(
                "uv_local_query_required",
                "uv 仅使用本地只读查询",
            ));
        }
        "cargo" => &["install", "--list"],
        "bun" => &["pm", "ls", "-g"],
        "rustup" => &["toolchain", "list"],
        "conda" => &["env", "list", "--json"],
        _ => return Err(Error::new("unsupported_manager", manager)),
    };
    Ok(Template::Named {
        name: manager.into(),
        args: args.iter().map(|a| (*a).into()).collect(),
    })
}

pub fn mutation(
    home: &Path,
    mise_owned: bool,
    p: &Package,
    action: Action,
    version: Option<&str>,
) -> Result<Template> {
    crate::policy::uv_local::action(&p.provider, action)?;
    let target = version.map(exact_version).transpose()?;
    let (name, args): (&str, Vec<String>) = match &p.provider {
        Provider::Command { .. } => {
            return Ok(Template::Exact(custom_spec(p, action, target.as_deref())?));
        }
        Provider::Service { formula } => {
            ("brew", service_args(formula, action, target.as_deref())?)
        }
        Provider::Brew { package, cask } => {
            if !cask {
                return Err(Error::attention(
                    "brew_context_binding_required",
                    "formula写维护只允许已绑定的Native Brew计划",
                ));
            }
            if target.is_some() {
                return Err(Error::new(
                    "brew_version_unsupported",
                    "brew 不接受通用任意历史版本；请使用独立受审配方",
                ));
            }
            let verb = match action {
                Action::Install => "install",
                Action::Upgrade => "upgrade",
                Action::Uninstall => "uninstall",
                _ => return Err(Error::new("operation_unsupported", &p.id)),
            };
            ("brew", vec![verb.into(), "--cask".into(), package.clone()])
        }
        Provider::Mise { package } => {
            if mise_owned {
                return Err(Error::attention(
                    "configuration_owner_requires_review",
                    "mise 配置由 chezmoi 拥有；Agent CLI 请使用 mo agent install，通用 runtime 的 source 收敛需独立维护窗口",
                ));
            }
            let version = target
                .as_deref()
                .ok_or_else(|| Error::new("exact_version_required", &p.id))?;
            match action {
                Action::Install | Action::Upgrade => (
                    "mise",
                    vec![
                        "use".into(),
                        "--global".into(),
                        "--pin".into(),
                        format!("{package}@{version}"),
                    ],
                ),
                Action::Uninstall => {
                    return Err(Error::new(
                        "consumer_review_required",
                        "mise runtime 退役须核对项目、IDE 与启动项消费者；先用原生维护窗口",
                    ));
                }
                _ => return Err(Error::new("operation_unsupported", &p.id)),
            }
        }
        _ => package_manager_args(home, p, action, target.as_deref())?,
    };
    Ok(Template::Named {
        name: name.into(),
        args,
    })
}

fn service_args(formula: &str, action: Action, version: Option<&str>) -> Result<Vec<String>> {
    if version.is_some() {
        return Err(Error::new(
            "service_command_required",
            "服务控制不接受软件版本",
        ));
    }
    let verb = match action {
        Action::Start => "start",
        Action::Stop => "stop",
        _ => return Err(Error::new("service_command_required", "使用服务控制命令")),
    };
    Ok(vec!["services".into(), verb.into(), formula.into()])
}

pub fn query(home: &Path, p: &Package) -> Result<Template> {
    let (binary, args): (&str, Vec<String>) = match &p.provider {
        Provider::Service { formula } => (
            "brew",
            vec![
                "services".into(),
                "info".into(),
                formula.clone(),
                "--json".into(),
            ],
        ),
        Provider::Brew { package, cask } => (
            "brew",
            vec![
                "list".into(),
                "--versions".into(),
                if *cask {
                    "--cask".into()
                } else {
                    "--formula".into()
                },
                package.clone(),
            ],
        ),
        Provider::Mise { package } => (
            "mise",
            vec![
                "ls".into(),
                "--installed".into(),
                "--json".into(),
                package.clone(),
            ],
        ),
        Provider::Uv { .. } | Provider::UvObserved { .. } => {
            return Err(Error::attention(
                "uv_local_query_required",
                "uv 不执行原生命令查询",
            ));
        }
        Provider::Bun { .. } => ("bun", vec!["pm".into(), "ls".into(), "-g".into()]),
        Provider::Cargo { .. } => (
            "cargo",
            vec![
                "install".into(),
                "--list".into(),
                "--root".into(),
                home.join(".cargo").display().to_string(),
            ],
        ),
        Provider::Command { query, .. } => return Ok(Template::Exact(query.clone())),
        _ => return Err(Error::new("query_unsupported", &p.id)),
    };
    Ok(Template::Named {
        name: binary.into(),
        args,
    })
}

fn package_manager_args(
    home: &Path,
    p: &Package,
    action: Action,
    target: Option<&str>,
) -> Result<(&'static str, Vec<String>)> {
    Ok(match &p.provider {
        Provider::Npm {
            package,
            prefix,
            authority,
        } => (
            "npm",
            crate::policy::npm_local::expected_npm_write_args(
                home,
                prefix,
                package,
                authority.as_ref(),
                action,
                target,
            )?,
        ),
        Provider::Bun { package } => match action {
            Action::Install | Action::Upgrade => (
                "bun",
                vec![
                    "add".into(),
                    "--global".into(),
                    format!(
                        "{package}@{}",
                        target.ok_or_else(|| Error::new("exact_version_required", &p.id))?
                    ),
                ],
            ),
            Action::Uninstall => (
                "bun",
                vec!["remove".into(), "--global".into(), package.clone()],
            ),
            _ => return Err(Error::new("operation_unsupported", &p.id)),
        },
        Provider::Cargo { package, .. } => match action {
            Action::Install | Action::Upgrade => (
                "cargo",
                vec![
                    "install".into(),
                    package.clone(),
                    "--version".into(),
                    target
                        .map(str::to_owned)
                        .ok_or_else(|| Error::new("exact_version_required", &p.id))?,
                    "--locked".into(),
                    "--root".into(),
                    home.join(".cargo").display().to_string(),
                ],
            ),
            Action::Uninstall => (
                "cargo",
                vec![
                    "uninstall".into(),
                    package.clone(),
                    "--root".into(),
                    home.join(".cargo").display().to_string(),
                ],
            ),
            _ => return Err(Error::new("operation_unsupported", &p.id)),
        },
        _ => return Err(Error::new("operation_unsupported", &p.id)),
    })
}

fn custom_spec(p: &Package, action: Action, target: Option<&str>) -> Result<CommandSpec> {
    if let Provider::Command {
        install,
        upgrade,
        uninstall,
        ..
    } = &p.provider
    {
        let spec = match action {
            Action::Install => install,
            Action::Upgrade => upgrade,
            Action::Uninstall => uninstall,
            _ => &None,
        };
        let mut spec = spec
            .clone()
            .ok_or_else(|| Error::new("operation_unsupported", &p.id))?;
        for arg in &mut spec.args {
            if arg.contains("{version}") {
                *arg = arg.replace(
                    "{version}",
                    target.ok_or_else(|| Error::new("exact_version_required", &p.id))?,
                );
            }
        }
        Ok(spec)
    } else {
        Err(Error::new("operation_unsupported", &p.id))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/commands.rs"]
mod tests;
