use crate::{cli, domain, error};
use serde_json::Value;

pub(crate) fn adopt_target(command: &cli::Command) -> Option<cli::AdoptTarget<'_>> {
    let cli::Command::App {
        project: _,
        command:
            Some(cli::AppCmd::Adopt {
                target,
                id,
                bind_recipe: _,
            }),
    } = command
    else {
        return None;
    };
    id.as_deref()
        .or(target.as_deref())
        .map(|key| cli::AdoptTarget {
            key,
            exact: id.is_some(),
        })
}

pub fn selected<'a>(
    catalog: &'a domain::Catalog,
    id: Option<&str>,
) -> error::Result<Vec<&'a domain::Package>> {
    id.map_or_else(
        || Ok(catalog.packages.iter().collect()),
        |id| Ok(vec![catalog.select(id)?]),
    )
}

pub(crate) enum Continuation {
    Ready,
    Unverified,
}

pub(crate) fn continuation(value: &Value) -> Option<Continuation> {
    if value.get("journal").unwrap_or(value)["phase"] != "succeeded" {
        return None;
    }
    value["continuationRequired"].as_bool().map(|ready| {
        if ready {
            Continuation::Ready
        } else {
            Continuation::Unverified
        }
    })
}

pub fn attention(command: &cli::Command, value: &Value) -> bool {
    if value.get("incomplete") == Some(&Value::Bool(true)) {
        return true;
    }
    if value.get("uncertain") == Some(&Value::Bool(true)) {
        return true;
    }
    if matches!(
        command,
        cli::Command::App {
            project: _,
            command: Some(cli::AppCmd::Detail(_))
        }
    ) {
        return value
            .get("observation")
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|s| !matches!(s, "present" | "brew-installed"));
    }
    if matches!(
        command,
        cli::Command::Agent {
            project: _,
            command: Some(cli::AgentCmd::Credential {
                command: cli::Credential::List
                    | cli::Credential::Add { .. }
                    | cli::Credential::Activate { .. }
            })
        }
    ) {
        return false;
    }
    if matches!(
        command,
        cli::Command::History { .. }
            | cli::Command::Search { .. }
            | cli::Command::Config {
                project: _,
                command: None | Some(cli::Config::List { .. } | cli::Config::Resources)
            }
            | cli::Command::App {
                project: _,
                command: None | Some(cli::AppCmd::Resources)
            }
            | cli::Command::Skill {
                project: _,
                details: _,
                only: _,
                command: None | Some(cli::SkillCmd::Detail(_) | cli::SkillCmd::Resources)
            }
            | cli::Command::Agent {
                project: _,
                command: None | Some(cli::AgentCmd::Detail(_) | cli::AgentCmd::Resources)
            }
            | cli::Command::Dirs { .. }
    ) {
        return false;
    }
    if value.get("reconcile_state").and_then(Value::as_str) == Some("drift") {
        return true;
    }
    if value.get("mode").and_then(Value::as_str) == Some("dry-run") {
        return false;
    }
    if continuation(value).is_some() {
        return true;
    }
    if let Some(journal) = value.get("journal") {
        return attention(command, journal);
    }
    if let Some(phase) = value.get("phase").and_then(Value::as_str) {
        return phase != "succeeded";
    }
    if value
        .get("nativeExit")
        .is_some_and(|v| v.as_i64() != Some(0))
    {
        return true;
    }
    if value.get("requires_running") == Some(&Value::Bool(true))
        && value.get("status").and_then(Value::as_str) == Some("stopped")
    {
        return true;
    }
    rows_need_attention(value) || value.get("results").is_some_and(rows_need_attention)
}

fn rows_need_attention(rows: &Value) -> bool {
    rows.as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row.get("incomplete") == Some(&Value::Bool(true))
                || row
                    .get("baseline")
                    .and_then(|v| v.get("comparison"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| s != "same")
                || row.get("status").and_then(Value::as_str).is_some_and(|s| {
                    matches!(
                        s,
                        "unknown" | "missing" | "brew-missing" | "different" | "drift"
                    )
                })
                || row
                    .get("observation")
                    .and_then(|o| o.get("status"))
                    .and_then(Value::as_str)
                    .is_some_and(|s| matches!(s, "unknown" | "missing" | "brew-missing"))
        })
    })
}

#[cfg(test)]
#[path = "../../tests/unit/cli_policy.rs"]
mod tests;

pub fn config_scope(command: &cli::Command, requested: bool) -> error::Result<()> {
    if requested
        && !matches!(
            command,
            cli::Command::App {
                project: _,
                command: Some(cli::AppCmd::Upgrade { id: Some(id), .. })
            } if crate::agent::ai_policy::upgrade_target(id).is_some()
        )
        && !matches!(
            command,
            cli::Command::App {
                project: _,
                command: None
                    | Some(cli::AppCmd::Adopt { .. } | cli::AppCmd::Doctor { id: Some(_) })
            } | cli::Command::Recover { .. }
                | cli::Command::History { .. }
                | cli::Command::Search { .. }
                | cli::Command::Config {
                    project: _,
                    command: None
                        | Some(
                            cli::Config::List { .. }
                                | cli::Config::Enroll
                                | cli::Config::SourcePath { .. }
                                | cli::Config::Status { .. }
                                | cli::Config::Verify { .. }
                                | cli::Config::Diff { .. }
                                | cli::Config::Apply { .. }
                                | cli::Config::Reconcile { .. }
                        )
                }
                | cli::Command::Agent {
                    project: _,
                    command: None
                        | Some(
                            cli::AgentCmd::Install { .. }
                                | cli::AgentCmd::Upgrade { .. }
                                | cli::AgentCmd::Detail(_)
                                | cli::AgentCmd::Credential { .. }
                        )
                }
                | cli::Command::Skill {
                    project: _,
                    details: _,
                    only: _,
                    command: Some(
                        cli::SkillCmd::Policy {
                            command: cli::Policy::Recover { .. }
                        } | cli::SkillCmd::Sync(_)
                            | cli::SkillCmd::Detail(_)
                    )
                }
        )
    {
        return Err(error::Error::new(
            "config_scope_unsupported",
            "chezmoi 目录参数仅用于配置查询、配置治理与事务恢复",
        ));
    }
    Ok(())
}
