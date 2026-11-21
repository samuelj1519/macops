#![cfg(test)]
use super::*;
use crate::{cli::*, domain::*, test_support::*};
use serde_json::json;

#[test]
fn profile_configuration_and_its_recovery_accept_frozen_static_root_arguments() -> TestResult {
    use clap::Parser;
    for args in [
        vec!["macops", "skills", "sync", "--only", "profile:mi"],
        vec!["macops", "skills", "policy", "recover"],
        vec!["macops", "recover", "OP-1"],
    ] {
        let cli = crate::cli::Cli::try_parse_from(args)?;
        config_scope(&cli.command, true)?;
    }
    Ok(())
}

#[test]
fn exit_status_preserves_unknown_and_display_only_contracts() -> TestResult {
    let doctor = Command::Doctor { id: None };
    assert!(attention(&doctor, &json!({"uncertain":true})));
    assert!(attention(
        &Command::App {
            command: App::Info {
                name: "demo".into()
            }
        },
        &json!({"observation":{"status":"missing"}})
    ));
    assert!(!attention(
        &Command::App {
            command: App::Info {
                name: "demo".into()
            }
        },
        &json!({"observation":{"status":"present"}})
    ));
    for command in [
        Command::History { id: None },
        Command::List {
            kind: None,
            project: None,
        },
        Command::Dirs {
            id: None,
            roots: false,
        },
        Command::Info {
            id: "demo".into(),
            project: None,
        },
    ] {
        assert!(!attention(&command, &json!({"status":"unknown"})));
    }
    assert!(!attention(
        &doctor,
        &json!({"mode":"dry-run","phase":"unknown"})
    ));
    assert!(attention(&doctor, &json!({"journal":{"phase":"unknown"}})));
    assert!(!attention(&doctor, &json!({"phase":"succeeded"})));
    assert!(attention(&doctor, &json!({"nativeExit":null})));
    assert!(attention(&doctor, &json!({"nativeExit":1})));
    assert!(!attention(&doctor, &json!({"nativeExit":0})));
    assert!(attention(
        &doctor,
        &json!({"requires_running":true,"status":"stopped"})
    ));
    assert!(!attention(
        &doctor,
        &json!({"requires_running":false,"status":"stopped"})
    ));
    for status in ["unknown", "missing", "different", "drift"] {
        assert!(attention(&doctor, &json!([{"status":status}])));
    }
    assert!(attention(
        &doctor,
        &json!([{"observation":{"status":"unknown"}}])
    ));
    assert!(attention(
        &doctor,
        &json!([{"observation":{"status":"missing"}}])
    ));
    assert!(!attention(
        &doctor,
        &json!([{"observation":{"status":"present"}}])
    ));
    assert!(attention(
        &doctor,
        &json!({"results":[{"status":"unknown"}]})
    ));
    assert!(!attention(&doctor, &json!(null)));
    let catalog = Catalog {
        schema_version: 1,
        packages: vec![package()],
    };
    assert_eq!(selected(&catalog, None)?.len(), 1);
    assert_eq!(selected(&catalog, Some("demo"))?.len(), 1);
    assert!(selected(&catalog, Some("not-found")).is_err());
    Ok(())
}

#[test]
fn credential_inventory_and_manual_routes_are_display_only_but_checks_need_attention() {
    use crate::policy::metadata::Profile;
    for command in [
        Credential::List,
        Credential::Add {
            profile: Profile::Mi,
            alias: "fixture".into(),
        },
        Credential::Activate {
            profile: Profile::Mi,
            alias: "fixture".into(),
        },
    ] {
        assert!(!attention(
            &Command::Credential { command },
            &json!([{"status":"unknown"}])
        ));
    }
    assert!(attention(
        &Command::Credential {
            command: Credential::Check { profile: None }
        },
        &json!([{"status":"missing"}])
    ));
    assert!(!attention(
        &Command::Credential {
            command: Credential::Check { profile: None }
        },
        &json!([{"status":"present"}])
    ));
}

#[test]
fn incomplete_inventory_is_attention_even_for_display_commands() {
    assert!(attention(
        &Command::List {
            kind: None,
            project: None
        },
        &json!({"incomplete":true})
    ));
}

#[test]
fn doctor_flags_native_baseline_drift_and_unknown_comparison() {
    let doctor = Command::Doctor { id: None };
    for comparison in ["different", "unknown"] {
        assert!(attention(
            &doctor,
            &json!([{"baseline":{"comparison":comparison}}])
        ));
    }
    assert!(!attention(
        &doctor,
        &json!([{"baseline":{"comparison":"same"}}])
    ));
}

#[test]
fn candidate_scope_flags_cannot_be_silently_ignored_by_write_routes() -> TestResult {
    config_scope(
        &Command::Config {
            command: Some(Config::List { refresh: false }),
        },
        true,
    )?;
    config_scope(
        &Command::List {
            kind: None,
            project: None,
        },
        true,
    )?;
    config_scope(
        &Command::Info {
            id: "target".into(),
            project: None,
        },
        true,
    )?;
    config_scope(
        &Command::Config {
            command: Some(Config::Diff { path: None }),
        },
        true,
    )?;
    config_scope(
        &Command::Adopt {
            bind_recipe: false,
            id: "config:demo".into(),
        },
        true,
    )?;
    config_scope(
        &Command::Doctor {
            id: Some("config:demo".into()),
        },
        true,
    )?;
    assert!(config_scope(&Command::Doctor { id: None }, true).is_err());
    config_scope(
        &Command::Config {
            command: Some(Config::Reconcile { id: "codex".into() }),
        },
        true,
    )?;
    config_scope(
        &Command::Config {
            command: Some(Config::Apply {
                target: "/h/.rc".into(),
            }),
        },
        true,
    )?;
    config_scope(
        &Command::Ai {
            command: Ai::Reconcile { tool: None },
        },
        true,
    )?;
    assert!(
        config_scope(
            &Command::Config {
                command: Some(Config::Restore {
                    operation_id: "OP-1".into()
                })
            },
            true
        )
        .is_err()
    );
    assert!(attention(
        &Command::Doctor { id: None },
        &json!({"reconcile_state":"drift"})
    ));
    assert!(!attention(
        &Command::Doctor { id: None },
        &json!({"reconcile_state":"in_sync"})
    ));
    let install = Command::Install {
        id: "target".into(),
        version: None,
    };
    assert!(config_scope(&install, true).is_err());
    config_scope(&install, false)?;
    Ok(())
}

#[test]
fn ai_native_upgrade_and_compatibility_alias_honor_explicit_chezmoi_scope() -> TestResult {
    config_scope(
        &Command::Ai {
            command: Ai::Upgrade {
                tool: "codex".into(),
                version: Some("2.0.0".into()),
                force_live: false,
            },
        },
        true,
    )?;
    config_scope(
        &Command::App {
            command: crate::cli::App::Upgrade {
                tool: "claude".into(),
                version: Some("2.0.0".into()),
                force_live: false,
            },
        },
        true,
    )?;
    Ok(())
}
