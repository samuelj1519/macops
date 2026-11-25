#![cfg(test)]
use super::*;
use crate::{cli::*, domain::*, test_support::*};
use serde_json::json;

#[test]
fn profile_configuration_and_its_recovery_accept_frozen_static_root_arguments() -> TestResult {
    use clap::Parser;
    for args in [
        vec!["macops", "skill", "sync", "--only", "profile:mi"],
        vec!["macops", "skill", "policy", "recover"],
        vec!["macops", "recover", "OP-1"],
    ] {
        let cli = crate::cli::Cli::try_parse_from(args)?;
        config_scope(&cli.command, true)?;
    }
    Ok(())
}

#[test]
fn exit_status_preserves_unknown_and_display_only_contracts() -> TestResult {
    let doctor = Command::App {
        project: None,
        command: Some(AppCmd::Doctor { id: None }),
    };
    assert!(attention(&doctor, &json!({"uncertain":true})));
    assert!(attention(
        &Command::App {
            project: None,
            command: Some(AppCmd::Detail(crate::cli::Detail {
                target: "demo".into()
            }))
        },
        &json!({"observation":{"status":"missing"}})
    ));
    assert!(!attention(
        &Command::App {
            project: None,
            command: Some(AppCmd::Detail(crate::cli::Detail {
                target: "demo".into()
            }))
        },
        &json!({"observation":{"status":"present"}})
    ));
    for command in [
        Command::History { query: None },
        Command::App {
            project: None,
            command: None,
        },
        Command::Dirs {
            id: None,
            roots: false,
        },
        Command::Skill {
            project: None,
            details: false,
            only: vec![],
            command: Some(SkillCmd::Detail(crate::cli::Detail {
                target: "demo".into(),
            })),
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
fn continuation_and_brew_evidence_preserve_attention_status() {
    let doctor = Command::App {
        project: None,
        command: Some(AppCmd::Doctor { id: None }),
    };
    for flag in [false, true] {
        assert!(attention(
            &doctor,
            &json!({"journal":{"phase":"succeeded"},"continuationRequired":flag})
        ));
    }
    assert!(
        continuation(&json!({"journal":{"phase":"failed"},"continuationRequired":false})).is_none()
    );
    assert!(attention(
        &doctor,
        &json!({"phase":"succeeded","continuationRequired":true})
    ));
    assert!(attention(
        &doctor,
        &json!([{"observation":{"status":"brew-missing"}}])
    ));
    let detail = Command::App {
        project: None,
        command: Some(AppCmd::Detail(crate::cli::Detail {
            target: "demo".into(),
        })),
    };
    assert!(!attention(
        &detail,
        &json!({"observation":{"status":"brew-installed"}})
    ));
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
            &Command::Agent {
                project: None,
                command: Some(AgentCmd::Credential { command })
            },
            &json!([{"status":"unknown"}])
        ));
    }
    assert!(attention(
        &Command::Agent {
            project: None,
            command: Some(AgentCmd::Credential {
                command: Credential::Check { profile: None }
            })
        },
        &json!([{"status":"missing"}])
    ));
    assert!(!attention(
        &Command::Agent {
            project: None,
            command: Some(AgentCmd::Credential {
                command: Credential::Check { profile: None }
            })
        },
        &json!([{"status":"present"}])
    ));
}

#[test]
fn incomplete_inventory_is_attention_even_for_display_commands() {
    assert!(attention(
        &Command::App {
            project: None,
            command: None,
        },
        &json!({"incomplete":true})
    ));
}

#[test]
fn doctor_flags_native_baseline_drift_and_unknown_comparison() {
    let doctor = Command::App {
        project: None,
        command: Some(AppCmd::Doctor { id: None }),
    };
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
#[allow(clippy::too_many_lines)]
fn candidate_scope_flags_cannot_be_silently_ignored_by_write_routes() -> TestResult {
    config_scope(
        &Command::Config {
            project: None,
            command: Some(Config::List { refresh: false }),
        },
        true,
    )?;
    config_scope(
        &Command::App {
            project: None,
            command: None,
        },
        true,
    )?;
    config_scope(
        &Command::Skill {
            project: None,
            details: false,
            only: vec![],
            command: Some(SkillCmd::Detail(crate::cli::Detail {
                target: "target".into(),
            })),
        },
        true,
    )?;
    config_scope(
        &Command::Config {
            project: None,
            command: Some(Config::Diff { path: None }),
        },
        true,
    )?;
    config_scope(
        &Command::App {
            project: None,
            command: Some(AppCmd::Adopt {
                bind_recipe: false,
                target: Some("config:demo".into()),
                id: None,
            }),
        },
        true,
    )?;
    config_scope(
        &Command::App {
            project: None,
            command: Some(AppCmd::Doctor {
                id: Some("config:demo".into()),
            }),
        },
        true,
    )?;
    assert!(
        config_scope(
            &Command::App {
                project: None,
                command: Some(AppCmd::Doctor { id: None })
            },
            true
        )
        .is_err()
    );
    config_scope(
        &Command::Config {
            project: None,
            command: Some(Config::Reconcile {
                id: Some("codex".into()),
            }),
        },
        true,
    )?;
    config_scope(
        &Command::Config {
            project: None,
            command: Some(Config::Apply {
                target: "/h/.rc".into(),
            }),
        },
        true,
    )?;
    config_scope(
        &Command::Agent {
            project: None,
            command: Some(AgentCmd::Upgrade {
                tool: None,
                force_live: false,
            }),
        },
        true,
    )?;
    assert!(
        config_scope(
            &Command::Config {
                project: None,
                command: Some(Config::Restore {
                    operation_id: "OP-1".into()
                })
            },
            true
        )
        .is_err()
    );
    assert!(attention(
        &doctor_of_none(),
        &json!({"reconcile_state":"drift"})
    ));
    assert!(!attention(
        &doctor_of_none(),
        &json!({"reconcile_state":"in_sync"})
    ));
    let install = Command::App {
        project: None,
        command: Some(AppCmd::Install {
            id: "target".into(),
            version: None,
        }),
    };
    assert!(config_scope(&install, true).is_err());
    config_scope(&install, false)?;
    Ok(())
}

fn doctor_of_none() -> Command {
    Command::App {
        project: None,
        command: Some(AppCmd::Doctor { id: None }),
    }
}

#[test]
fn ai_native_upgrade_and_compatibility_alias_honor_explicit_chezmoi_scope() -> TestResult {
    for (id, accepted) in [("ai:codex", true), ("ai:unknown", false), ("codex", false)] {
        let command = Command::App {
            project: None,
            command: Some(AppCmd::Upgrade {
                id: Some(id.into()),
                version: Some("2.0.0".into()),
            }),
        };
        assert_eq!(config_scope(&command, true).is_ok(), accepted);
    }
    config_scope(
        &Command::Agent {
            project: None,
            command: Some(AgentCmd::Install {
                tool: "codex".into(),
                version: Some("2.0.0".into()),
                force_live: false,
            }),
        },
        true,
    )?;
    config_scope(
        &Command::Agent {
            project: None,
            command: Some(AgentCmd::Upgrade {
                tool: Some("claude".into()),
                force_live: false,
            }),
        },
        true,
    )?;
    Ok(())
}
