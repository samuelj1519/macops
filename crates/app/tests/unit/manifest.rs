use super::*;
use crate::test_support::TestResult;

fn entry() -> Entry {
    Entry {
        id: "npm:fixture".into(),
        kind: Kind::Cli,
        channel: Channel::Npm,
        recipe_sha256: "a".repeat(64),
        scope_sha256: Some("b".repeat(64)),
        version: Some("1.0.0".into()),
    }
}

#[test]
fn manifest_plan_compares_explicit_intentions_and_existing_authority() -> TestResult {
    let base = entry();
    let request = Manifest {
        schema_version: 1,
        entries: vec![base.clone()],
    };
    validate(&request)?;
    assert_eq!(
        plan(
            &request,
            &BTreeMap::new(),
            &BTreeSet::new(),
            &"b".repeat(64)
        )[0]
        .status,
        Status::Unregistered
    );
    for (case, expected) in [
        ("aligned", Status::Aligned),
        ("channel", Status::RecipeChanged),
        ("kind", Status::RecipeChanged),
        ("fingerprint", Status::RecipeChanged),
        ("scope", Status::ScopeChanged),
        ("unseen scope", Status::NeedsNativeObservation),
        ("unseen version", Status::NeedsNativeObservation),
        ("version", Status::VersionDifference),
    ] {
        let mut existing = base.clone();
        match case {
            "aligned" => (),
            "channel" => existing.channel = Channel::Cargo,
            "kind" => existing.kind = Kind::App,
            "fingerprint" => existing.recipe_sha256 = "c".repeat(64),
            "scope" => existing.scope_sha256 = Some("c".repeat(64)),
            "unseen scope" => existing.scope_sha256 = None,
            "unseen version" => existing.version = None,
            "version" => existing.version = Some("0.5.0".into()),
            _ => return Err("test input".into()),
        }
        let local = BTreeMap::from([(existing.id.clone(), existing)]);
        let rows = plan(
            &request,
            &local,
            &BTreeSet::from([base.id.clone()]),
            &"b".repeat(64),
        );
        assert_eq!(rows[0].status, expected, "{case}");
    }
    Ok(())
}

#[test]
fn manifest_rejects_unknown_wire_versions_duplicate_ids_and_unsafe_values() {
    let valid = entry();
    for case in [
        "empty",
        "oversize",
        "control",
        "digest",
        "scope",
        "version",
        "duplicate",
        "count",
        "schema",
    ] {
        let mut bad = valid.clone();
        match case {
            "empty" => bad.id.clear(),
            "oversize" => bad.id = "X".repeat(513),
            "control" => bad.id = "bad\nentry".into(),
            "digest" => bad.recipe_sha256 = "not a digest".into(),
            "scope" => bad.scope_sha256 = Some("not a digest".into()),
            "version" => bad.version = Some("version / with spaces".into()),
            _ => (),
        }
        let entries = match case {
            "duplicate" => vec![bad.clone(), bad],
            "count" => vec![bad; MAX_ENTRIES + 1],
            _ => vec![bad],
        };
        let manifest = Manifest {
            schema_version: if case == "schema" { 2 } else { 1 },
            entries,
        };
        assert!(validate(&manifest).is_err(), "{case}");
    }
    for bad in ["", "-1", "1.0.0 && command", &"a".repeat(129)] {
        assert!(!valid_version(bad));
    }
    assert!(valid_version("1.0.0_2"));
}

#[test]
fn manifest_labels_declaration_kinds_without_exporting_raw_recipes() -> TestResult {
    use serde_json::json;
    for (value, expected) in [
        (json!({"type":"brew","package":"jq"}), Channel::BrewFormula),
        (
            json!({"type":"brew","package":"firefox","cask":true}),
            Channel::BrewCask,
        ),
        (json!({"type":"mise","package":"node"}), Channel::Mise),
        (
            json!({"type":"npm","package":"tool","prefix":"/fixture"}),
            Channel::Npm,
        ),
        (
            json!({"type":"npm-observed","name":"tool","prefix":"/fixture"}),
            Channel::Npm,
        ),
        (json!({"type":"bun","package":"tool"}), Channel::Bun),
        (
            json!({"type":"cargo","package":"tool","binary":"tool"}),
            Channel::Cargo,
        ),
        (json!({"type":"uv","package":"tool"}), Channel::Uv),
        (
            json!({"type":"config-observed","target":"/fixture/.profile","authority":"untracked"}),
            Channel::Chezmoi,
        ),
        (
            json!({"type":"capability-observed","capability":"skills-cli","native_id":"sample",
                "manager":"skills","record":"/fixture/record","payload":"/fixture/payload"}),
            Channel::Capability,
        ),
        (json!({"type":"macops"}), Channel::Other),
    ] {
        assert_eq!(channel(&serde_json::from_value(value)?), expected);
    }
    let formula = Provider::BrewFormula {
        query: Box::new(crate::test_support::brew::query()?),
        recipe_ref: None,
    };
    assert_eq!(channel(&formula), Channel::BrewFormula);
    Ok(())
}

#[test]
fn a_manifest_entry_without_scope_or_version_intent_leaves_local_values_open() {
    let current = entry();
    let mut requested = current.clone();
    requested.version = None;
    requested.scope_sha256 = None;
    let local = BTreeMap::from([(current.id.clone(), current)]);
    let manifest = Manifest {
        schema_version: 1,
        entries: vec![requested],
    };
    assert_eq!(
        plan(&manifest, &local, &BTreeSet::new(), &"b".repeat(64))[0].status,
        Status::Aligned
    );
}

#[test]
fn npm_replay_requires_a_complete_canonical_semver() {
    for good in ["0.0.0", "1.2.3", "1.2.3-rc.1", "1.2.3-0", "1.2.3-beta-x"] {
        assert!(concrete_npm_version(good), "{good}");
    }
    for bad in [
        "1",
        "1.2",
        "1.2.3+build.01",
        "1.2.3-beta-x+v.1",
        "1.x",
        "1.2.x",
        "latest",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3.4",
        "1.2.3-beta.01",
        "1.2.3-",
        "1.2.3+",
        "1.2.3+counter/last",
        "1.2.3-pre+build+other",
        "9007199254740992.0.0",
        "9999999999999999999999.0.0",
        "v1.2.3",
        ">=1.2.3",
        "1.2.3-rc.*",
        "1.2.3-pre..x",
    ] {
        assert!(!concrete_npm_version(bad), "{bad}");
    }
}

#[test]
fn replay_eligibility_uses_current_scope_and_approved_registry_authority() -> TestResult {
    let base = entry();
    let mut manifest = Manifest {
        schema_version: 1,
        entries: vec![base.clone()],
    };
    let local = BTreeMap::from([(base.id.clone(), base.clone())]);
    let ids = BTreeSet::from([base.id]);
    let expected_scope = "b".repeat(64);
    assert_eq!(
        plan(&manifest, &local, &ids, &expected_scope)[0].replay,
        Replay::VerifyNativeState
    );
    assert_eq!(
        plan(&manifest, &local, &ids, &"a".repeat(64))[0].status,
        Status::ScopeChanged
    );
    assert_eq!(
        plan(&manifest, &local, &ids, &"a".repeat(64))[0].replay,
        Replay::Inconsistent
    );
    assert_eq!(
        plan(&manifest, &local, &BTreeSet::new(), &expected_scope)[0].replay,
        Replay::ReadOnly
    );
    manifest.entries[0].version = Some("1.x".into());
    assert_eq!(
        plan(&manifest, &local, &ids, &expected_scope)[0].replay,
        Replay::NeedsIntent
    );
    manifest.entries[0].version = Some("1.2.3".into());
    manifest.entries[0].scope_sha256 = None;
    assert_eq!(
        plan(&manifest, &local, &ids, &expected_scope)[0].replay,
        Replay::NeedsIntent
    );
    for (authority, allowed) in [
        (serde_json::Value::Null, false),
        (serde_json::json!({"allow_remove":true}), false),
        (
            serde_json::json!({"allow_remove":false,"registry":{"url":"https://registry.npmjs.org"}}),
            true,
        ),
    ] {
        let provider: Provider = serde_json::from_value(serde_json::json!({
            "type":"npm","package":"demo","prefix":"/fixture","authority":authority
        }))?;
        assert_eq!(supports_replay(&provider), allowed);
    }
    Ok(())
}

#[test]
fn a_legacy_version_without_scope_needs_native_observation_even_when_it_is_the_requested_version() {
    let mut old = entry();
    old.scope_sha256 = None;
    let wanted = Manifest {
        schema_version: 1,
        entries: vec![old.clone()],
    };
    let registered = BTreeSet::from([old.id.clone()]);
    let current = BTreeMap::from([(old.id.clone(), old)]);
    let state = plan(&wanted, &current, &registered, &"b".repeat(64));
    assert_eq!(state[0].status, Status::NeedsNativeObservation);
    assert_eq!(state[0].replay, Replay::NeedsIntent);
}
