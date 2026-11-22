use crate::{cli, domain, error};
use serde_json::Value;

pub(crate) fn adopt_target(command: &cli::Command) -> Option<cli::AdoptTarget<'_>> {
    let cli::Command::Adopt { target, id, .. } = command else {
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
            command: cli::App::Info { .. }
        }
    ) {
        return value
            .get("observation")
            .and_then(|v| v.get("status"))
            .and_then(Value::as_str)
            .is_some_and(|s| s != "present");
    }
    let rows_need_attention = |rows: &Value| {
        rows.as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row.get("incomplete") == Some(&Value::Bool(true))
                    || row
                        .get("baseline")
                        .and_then(|v| v.get("comparison"))
                        .and_then(Value::as_str)
                        .is_some_and(|s| s != "same")
                    || row
                        .get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|s| matches!(s, "unknown" | "missing" | "different" | "drift"))
                    || row
                        .get("observation")
                        .and_then(|o| o.get("status"))
                        .and_then(Value::as_str)
                        .is_some_and(|s| matches!(s, "unknown" | "missing"))
            })
        })
    };
    if matches!(
        command,
        cli::Command::Credential {
            command: cli::Credential::List
                | cli::Credential::Add { .. }
                | cli::Credential::Activate { .. }
        }
    ) {
        return false;
    }
    if matches!(
        command,
        cli::Command::History { .. }
            | cli::Command::List { .. }
            | cli::Command::Dirs { .. }
            | cli::Command::Info { .. }
    ) {
        return false;
    }
    if value.get("reconcile_state").and_then(Value::as_str) == Some("drift") {
        return true;
    }
    if value.get("mode").and_then(Value::as_str) == Some("dry-run") {
        return false;
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

#[cfg(test)]
#[path = "../../tests/unit/cli_policy.rs"]
mod tests;

pub fn config_scope(command: &cli::Command, requested: bool) -> error::Result<()> {
    if requested
        && !matches!(command, cli::Command::Upgrade { id, .. } if crate::policy::ai::upgrade_target(id).is_some())
        && !matches!(
            command,
            cli::Command::List { .. }
                | cli::Command::Info { .. }
                | cli::Command::Adopt { .. }
                | cli::Command::Recover { .. }
                | cli::Command::Doctor { id: Some(_) }
                | cli::Command::Config {
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
                | cli::Command::Ai {
                    command: cli::Ai::Reconcile { .. } | cli::Ai::Upgrade { .. }
                }
                | cli::Command::App {
                    command: cli::App::Upgrade { .. }
                }
                | cli::Command::Skills {
                    command: Some(
                        cli::Skills::Sync(_)
                            | cli::Skills::Policy {
                                command: cli::Policy::Recover { .. }
                            }
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
