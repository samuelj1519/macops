#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
#[test]
fn change_id_and_commit_revision_validation_are_safe_and_stable() -> TestResult {
    assert_eq!(parse_change_id("CASE-1._")?, "CASE-1._");
    for bad in ["", ".", "..", "../outside", "a/b", "space value"] {
        assert!(parse_change_id(bad).is_err());
    }
    assert!(parse_change_id(&"a".repeat(129)).is_err());
    assert_eq!(revision(&"A".repeat(40))?, "a".repeat(40));
    assert!(revision("short").is_err());
    assert!(revision(&"g".repeat(40)).is_err());
    Ok(())
}

#[test]
fn selectors_match_exact_ids_aliases_and_plugin_groups_without_matching_other_kinds() {
    use crate::resource::{Management, Origin, Revision, State};
    let mut resource = Resource {
        id: "capability:one".into(),
        name: "pstack@market".into(),
        aliases: vec!["plugin:pstack@market".into()],
        management: Management::Native,
        status: State::Present,
        data: Data::Capability {
            delivery_kind: "plugin".into(),
            native_id: "pstack@market".into(),
            origin: Origin {
                repository: None,
                manager: "claude-plugin".into(),
                label: None,
                record: "/home/registry".into(),
                native_key: "pstack@market".into(),
            },
            revision: Revision::default(),
            contents: vec![],
            deployments: vec![],
            configured_enabled: None,
            loaded: None,
            baseline: None,
        },
    };
    for key in [
        "capability:one",
        "pstack@market",
        "plugin:pstack@market",
        "pstack",
    ] {
        assert!(matches_resource(&resource, key));
    }
    assert!(!matches_resource(&resource, "other"));
    if let Data::Capability { native_id, .. } = &mut resource.data {
        *native_id = "standalone".into();
    }
    assert!(!matches_resource(&resource, "pstack"));
    resource.data = Data::Application {
        manager: "brew".into(),
        native_id: "pstack@market".into(),
        scope: "/home".into(),
        releases: vec![],
        recipe_ref: None,
        baseline: None,
        issues: vec![],
    };
    assert!(!matches_resource(&resource, "pstack"));
}
