#![cfg(test)]
use super::*;
use crate::test_support::*;

#[test]
fn service_intent_is_typed_and_requires_all_three_states() {
    for action in [Action::Start, Action::Stop] {
        assert!(is_control(action));
        let wanted = action == Action::Start;
        for running in [false, true] {
            for loaded in [false, true] {
                for registered in [false, true] {
                    assert_eq!(
                        expected(
                            action,
                            &Observation::Service {
                                running,
                                loaded,
                                registered
                            }
                        ),
                        running == wanted && loaded == wanted && registered == wanted
                    );
                }
            }
        }
    }
    assert!(!is_control(Action::Install));
    assert!(!expected(
        Action::Install,
        &Observation::Missing { evidence: None }
    ));
    assert!(!expected(
        Action::Start,
        &Observation::Missing { evidence: None }
    ));
}

fn actor() -> ServiceIdentity {
    ServiceIdentity {
        name: "fixture".into(),
        uid: 501,
    }
}
fn control_journal(action: Action) -> crate::error::Result<Journal> {
    let mut j = journal()?;
    j.plan.package = super::package();
    j.plan.action = action;
    j.plan.target = None;
    j.plan.before = Observation::Service {
        running: false,
        loaded: false,
        registered: false,
    };
    j.scope.service_identity = Some(actor());
    let verb = if action == Action::Start {
        "start"
    } else {
        "stop"
    };
    j.plan.command = Some(crate::domain::CommandSpec {
        program: "/fixture/bin/brew".into(),
        args: vec!["services".into(), verb.into(), "postgresql@16".into()],
    });
    j.plan.query = Some(
        crate::domain::CommandSpec {
            program: "/fixture/bin/brew".into(),
            args: vec![
                "services".into(),
                "info".into(),
                "postgresql@16".into(),
                "--json".into(),
            ],
        }
        .into(),
    );
    j.plan.recipe_sha256 = crate::domain::fingerprint(&j.plan.package)?;
    j.plan_sha256 = crate::domain::fingerprint(&j.plan)?;
    j.scope_sha256 = crate::domain::fingerprint(&j.scope)?;
    Ok(j)
}

#[test]
fn only_matching_non_root_identity_can_control_a_home() -> TestResult {
    let id = actor();
    validate_identity(&id)?;
    home_owner(&id, 501)?;
    same_identity(&id, &id)?;
    for bad in [
        ServiceIdentity {
            name: "root".into(),
            uid: 0,
        },
        ServiceIdentity {
            name: String::new(),
            uid: 501,
        },
        ServiceIdentity {
            name: "root".into(),
            uid: 501,
        },
    ] {
        assert!(validate_identity(&bad).is_err());
    }
    assert!(home_owner(&id, 0).is_err());
    assert!(
        same_identity(
            &id,
            &ServiceIdentity {
                name: "other".into(),
                uid: 501
            }
        )
        .is_err()
    );
    assert!(
        same_identity(
            &id,
            &ServiceIdentity {
                name: "fixture".into(),
                uid: 502
            }
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn service_observation_requires_complete_flags_and_proven_user_without_echoing_input() -> TestResult
{
    use serde_json::json;
    let id = actor();
    for running in [false, true] {
        for loaded in [false, true] {
            for registered in [false, true] {
                let text=json!([{"name":"postgresql@16","running":running,"loaded":loaded,"registered":registered,"user":"fixture","command":"SYNTHETIC_SECRET"}]).to_string();
                let observed = observation("postgresql@16", &output(&text), &id)?;
                assert_eq!(
                    observed,
                    Observation::Service {
                        running,
                        loaded,
                        registered
                    }
                );
            }
        }
    }
    let none = json!([{"name":"postgresql@16","running":false,"loaded":false,"registered":false,"user":null}]);
    observation("postgresql@16", &output(&none.to_string()), &id)?;
    for bad in [
        "not JSON".to_owned(),
        "[]".into(),
        json!([{"name":"other","running":false,"loaded":false,"registered":false,"user":null}])
            .to_string(),
        json!([none[0], none[0]]).to_string(),
    ] {
        assert!(observation("postgresql@16", &output(&bad), &id).is_err());
    }
    for key in ["running", "loaded", "registered"] {
        let mut bad = none.clone();
        bad[0].as_object_mut().ok_or("object")?.remove(key);
        assert!(observation("postgresql@16", &output(&bad.to_string()), &id).is_err());
        bad = none.clone();
        bad[0][key] = json!("SYNTHETIC_SECRET");
        let error = observation("postgresql@16", &output(&bad.to_string()), &id)
            .err()
            .ok_or("error")?;
        assert!(!error.to_string().contains("SYNTHETIC_SECRET"));
    }
    for user in [json!("root"), json!("other")] {
        let mut bad = none.clone();
        bad[0]["user"] = user;
        assert!(observation("postgresql@16", &output(&bad.to_string()), &id).is_err());
    }
    for key in ["running", "loaded", "registered"] {
        let mut bad = none.clone();
        bad[0][key] = json!(true);
        assert!(observation("postgresql@16", &output(&bad.to_string()), &id).is_err());
    }
    let mut bad = output("SYNTHETIC_SECRET");
    bad.stderr = "SYNTHETIC_SECRET".into();
    bad.code = Some(1);
    assert!(
        !observation("postgresql@16", &bad, &id)
            .err()
            .ok_or("error")?
            .to_string()
            .contains("SYNTHETIC_SECRET")
    );
    bad.code = Some(0);
    bad.uncertain = true;
    assert!(observation("postgresql@16", &bad, &id).is_err());
    Ok(())
}

#[test]
fn service_journal_requires_type_action_identity_and_a_frozen_command_pair() -> TestResult {
    let j = control_journal(Action::Start)?;
    validate_binding(&j)?;
    crate::policy::operation::state::validate_journal(&j)?;
    validate_binding(&journal()?)?;
    validate_binding(&control_journal(Action::Stop)?)?;
    let mut bad = j.clone();
    bad.plan.package = crate::test_support::package();
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.action = Action::Upgrade;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.package.kind = Kind::Cli;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.package.id = "other".into();
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.before = Observation::Missing { evidence: None };
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.target = Some("1.0.0".into());
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.delegated = true;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.synchronous = false;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.scope.service_identity = None;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.scope.service_identity = Some(ServiceIdentity {
        name: "root".into(),
        uid: 0,
    });
    assert!(validate_binding(&bad).is_err());
    let mut bad = journal()?;
    bad.scope.service_identity = Some(actor());
    assert!(validate_binding(&bad).is_err());
    Ok(())
}

#[test]
fn service_journal_never_accepts_other_formulas_programs_or_arguments() -> TestResult {
    let j = control_journal(Action::Start)?;
    let mut bad = j.clone();
    bad.plan.command = None;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.query = None;
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.package.provider = Provider::Service {
        formula: "other".into(),
    };
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.command.as_mut().ok_or("command")?.program = "/other/bin/brew".into();
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.command.as_mut().ok_or("command")?.args = vec!["--all".into()];
    assert!(validate_binding(&bad).is_err());
    let mut bad = j.clone();
    let Some(crate::domain::QuerySpec::Command(query)) = &mut bad.plan.query else {
        return Err("query".into());
    };
    query.args = vec!["--all".into()];
    assert!(validate_binding(&bad).is_err());
    assert!(crate::policy::operation::state::expected(
        &j,
        &Observation::Service {
            running: true,
            loaded: true,
            registered: true
        }
    ));
    assert!(!crate::policy::operation::state::expected(
        &j,
        &Observation::Service {
            running: true,
            loaded: true,
            registered: false
        }
    ));
    Ok(())
}
