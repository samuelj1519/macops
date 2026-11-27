#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

fn sample(id: &str, name: &str) -> Resource {
    Resource {
        id: id.into(),
        name: name.into(),
        aliases: vec!["alias".into()],
        management: Management::Native,
        status: State::Present,
        data: Data::Config {
            target: "/fixture/config".into(),
            source_candidates: vec![],
            owner: "user".into(),
            consumers: vec![],
            observed_type: "file".into(),
            mode: Some(0o600),
            baseline: None,
        },
    }
}

#[test]
fn identities_and_aliases_do_not_collapse_instances() -> TestResult {
    assert_eq!(
        id("agent", &("claude", "/profile-a"))?,
        id("agent", &("claude", "/profile-a"))?
    );
    assert_ne!(
        id("agent", &("claude", "/profile-a"))?,
        id("agent", &("claude", "/profile-b"))?
    );
    let mut report = Snapshot {
        schema_version: 1,
        home: "/fixture".into(),
        project: None,
        resources: vec![sample("b", "same"), sample("a", "same")],
        coverage: vec![],
        incomplete: false,
    };
    report.finish();
    assert_eq!(report.resources[0].id, "a");
    assert_eq!(report.select("a")?.id, "a");
    assert_eq!(
        report.select("same").err().ok_or("ambiguous")?.code,
        "ambiguous"
    );
    assert_eq!(
        report.select("alias").err().ok_or("ambiguous")?.code,
        "ambiguous"
    );
    assert_eq!(
        report.select("missing").err().ok_or("missing")?.code,
        "not_found"
    );
    report.resources.pop();
    assert_eq!(report.select("alias")?.id, "a");
    assert_eq!(report.select("same")?.id, "a");
    Ok(())
}

#[test]
fn categories_and_source_failures_are_explicit() {
    let mut value = sample("a", "a");
    assert_eq!(value.category(), "configs");
    value.data = Data::Application {
        manager: "mise".into(),
        native_id: "claude".into(),
        scope: "global".into(),
        releases: vec![],
        baseline: None,
        issues: vec![],
        recipe_ref: None,
    };
    assert_eq!(value.category(), "applications");
    value.data = Data::Agent {
        client: "claude".into(),
        profile: "mi".into(),
        config_dir: "/profile".into(),
        model: Model::default(),
        runtime: RuntimeBinding {
            selector: "unresolved".into(),
            context: "/fixture".into(),
            binary: None,
            install_ref: None,
            version: None,
            evidence: None,
        },
    };
    assert_eq!(value.category(), "agents");
    value.data = Data::Capability {
        delivery_kind: "skill".into(),
        native_id: "demo".into(),
        origin: Origin {
            repository: None,
            manager: "skills".into(),
            label: None,
            record: "/lock".into(),
            native_key: "demo".into(),
        },
        revision: Revision::default(),
        contents: vec![],
        deployments: vec![],
        configured_enabled: None,
        loaded: None,
        baseline: None,
    };
    assert_eq!(value.category(), "capabilities");
    let mut report = Snapshot {
        schema_version: 1,
        home: "/fixture".into(),
        project: None,
        resources: vec![value],
        coverage: vec![
            Coverage {
                source: "z".into(),
                state: CoverageState::Complete,
                count: 1,
                roots: vec![],
                issues: vec![],
            },
            Coverage {
                source: "a".into(),
                state: CoverageState::Absent,
                count: 0,
                roots: vec![],
                issues: vec![],
            },
        ],
        incomplete: true,
    };
    report.finish();
    assert!(!report.incomplete);
    assert_eq!(report.coverage[0].source, "a");
    report.coverage[0].state = CoverageState::Invalid;
    report.finish();
    assert!(report.incomplete);
}

#[test]
fn unsupported_path_encoding_is_not_a_silent_identifier_collision() {
    use std::os::unix::ffi::OsStringExt;
    let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
    assert!(id("config", &invalid).is_err());
    assert!(id("config", &PathBuf::from("/valid")).is_ok());
}
