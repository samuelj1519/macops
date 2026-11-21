#![cfg(test)]
use super::*;
use crate::{
    policy::{
        delivery::Profile,
        profile_fields::{ConfigProfile, Role},
    },
    test_support::TestResult,
};
fn plugins(profile: Profile, names: &[&str]) -> Request {
    Request {
        owner: Owner {
            profile: ConfigProfile::Claude(profile),
            role: Role::Source,
            intent: Intent::Plugins,
        },
        intent_sha256: "a".repeat(64),
        desired: Some(Desired::Plugins(
            names.iter().map(|s| (*s).into()).collect(),
        )),
    }
}
#[test]
fn claude_owned_plugin_fields_preserve_raw_model_and_restore_only_after_last_owner() -> TestResult {
    let target = Path::new("/home/source.json");
    let raw = br#"{ "model" : "private-\u006dodel", "env": {"TOKEN":"PRIVATE"} }"#;
    let mi = plugins(Profile::Mi, &["p@vendor"]);
    let zen = plugins(Profile::Zen, &["p@vendor"]);
    let first = plan(target, raw, None, std::slice::from_ref(&mi))?;
    let value = crate::policy::json::parse(&first.after)?;
    assert_eq!(value["enabledPlugins"]["p@vendor"], false);
    assert!(
        String::from_utf8(first.after.clone())?
            .contains(r#""model" : "private-\u006dodel", "env": {"TOKEN":"PRIVATE"}"#)
    );
    let shared = plan(
        target,
        &first.after,
        first.record.as_ref(),
        std::slice::from_ref(&zen),
    )?;
    let mut retire_mi = mi;
    retire_mi.desired = None;
    let partial = plan(target, &shared.after, shared.record.as_ref(), &[retire_mi])?;
    assert_eq!(
        crate::policy::json::parse(&partial.after)?["enabledPlugins"]["p@vendor"],
        false
    );
    let mut retire_zen = zen;
    retire_zen.desired = None;
    let retired = plan(
        target,
        &partial.after,
        partial.record.as_ref(),
        &[retire_zen],
    )?;
    assert!(
        crate::policy::json::parse(&retired.after)?
            .get("enabledPlugins")
            .is_none()
    );
    assert!(retired.record.is_none());
    Ok(())
}

#[test]
fn pstack_retirement_preserves_missing_versus_empty_deny_and_other_permission_bytes() -> TestResult
{
    for raw in [
        br#"{"model":"x","permissions":{"allow" : ["Read(\u0078)"]}}"#.as_slice(),
        br#"{"model":"x","permissions":{"allow" : ["Read(\u0078)"],"deny":[]}}"#,
    ] {
        let request = Request {
            owner: Owner {
                profile: ConfigProfile::Claude(Profile::Mi),
                role: Role::Live,
                intent: Intent::Pstack,
            },
            intent_sha256: "b".repeat(64),
            desired: Some(Desired::Pstack(vec!["Skill(pstack:deny)".into()])),
        };
        let applied = plan(
            Path::new("/home/.claude-mi/settings.json"),
            raw,
            None,
            std::slice::from_ref(&request),
        )?;
        assert_eq!(
            crate::policy::json::parse(&applied.after)?["permissions"]["deny"][0],
            "Skill(pstack:deny)"
        );
        let mut retire = request;
        retire.desired = None;
        let retired = plan(
            Path::new("/home/.claude-mi/settings.json"),
            &applied.after,
            applied.record.as_ref(),
            &[retire],
        )?;
        let before = crate::policy::json::parse(raw)?;
        assert_eq!(crate::policy::json::parse(&retired.after)?, before);
        assert!(String::from_utf8(retired.after)?.contains(r#""allow" : ["Read(\u0078)"]"#));
        assert!(retired.record.is_none());
    }
    Ok(())
}

fn pstack(entries: Vec<String>) -> Request {
    Request {
        owner: Owner {
            profile: ConfigProfile::Claude(Profile::Mi),
            role: Role::Live,
            intent: Intent::Pstack,
        },
        intent_sha256: "b".repeat(64),
        desired: Some(Desired::Pstack(entries)),
    }
}
#[test]
fn invalid_claude_intents_and_budget_overflow_never_produce_a_configuration_proposal() {
    let target = Path::new("/home/source.json");
    let good = plugins(Profile::Mi, &["p@vendor"]);
    let mut cases = vec![vec![good.clone(), good.clone()], vec![good.clone(); 10001]];
    for (profile, intent, hash) in [
        (ConfigProfile::Codex, Intent::CodexSkills, "a".repeat(64)),
        (
            ConfigProfile::Claude(Profile::Mi),
            Intent::CodexSkills,
            "a".repeat(64),
        ),
        (
            ConfigProfile::Claude(Profile::Mi),
            Intent::Plugins,
            "bad".into(),
        ),
    ] {
        let mut request = good.clone();
        request.owner.profile = profile;
        request.owner.intent = intent;
        request.intent_sha256 = hash;
        cases.push(vec![request]);
    }
    for names in [
        vec!["../escape".to_owned()],
        vec!["devnorm@vendor".to_owned()],
        (0..5001).map(|n| format!("p{n}@vendor")).collect(),
    ] {
        let mut request = good.clone();
        request.desired = Some(Desired::Plugins(names.into_iter().collect()));
        cases.push(vec![request]);
    }
    cases.push(vec![pstack(vec!["Bash(rm)".into()])]);
    cases.push(vec![pstack(vec!["Skill(pstack:tdd)".into(); 5001])]);
    let mut wrong = good.clone();
    wrong.desired = Some(Desired::Pstack(vec![]));
    cases.push(vec![wrong]);
    for requests in cases {
        assert!(plan(target, b"{}", None, &requests).is_err());
    }
    for raw in [
        b"[]".as_slice(),
        b"{",
        br#"{"enabledPlugins":null}"#,
        br#"{"enabledPlugins":{"p@vendor":"false"}}"#,
    ] {
        assert!(plan(target, raw, None, std::slice::from_ref(&good)).is_err());
    }
    assert!(plan(Path::new("relative"), b"{}", None, &[good]).is_err());
}
#[test]
fn existing_parent_deletion_and_corrupt_previous_configuration_never_recreate_user_containers()
-> TestResult {
    let target = Path::new("/home/source.json");
    let request = plugins(Profile::Mi, &["p@vendor"]);
    let first = plan(
        target,
        br#"{"enabledPlugins":{}}"#,
        None,
        std::slice::from_ref(&request),
    )?;
    assert_eq!(
        plan(
            target,
            b"{}",
            first.record.as_ref(),
            std::slice::from_ref(&request)
        )
        .err()
        .ok_or("missing parent")?
        .code,
        "profile_owned_parent_drift"
    );
    let mut record = first.record.ok_or("record")?;
    record.target = "/other/file.json".into();
    assert!(
        plan(
            target,
            &first.after,
            Some(&record),
            std::slice::from_ref(&request)
        )
        .is_err()
    );
    record.target = target.into();
    record.fields = Fields::Codex {
        skills: BTreeMap::new(),
    };
    record.parents.clear();
    assert!(plan(target, b"{}", Some(&record), std::slice::from_ref(&request)).is_err());
    record.target = "relative".into();
    assert!(plan(target, b"{}", Some(&record), &[request]).is_err());
    let pstack = pstack(vec!["Skill(pstack:tdd)".into()]);
    for raw in [
        br#"{"permissions":null}"#.as_slice(),
        br#"{"permissions":{"deny":false}}"#,
        br#"{"permissions":{"deny":[false]}}"#,
    ] {
        assert!(plan(target, raw, None, std::slice::from_ref(&pstack)).is_err());
    }
    let first = plan(
        target,
        br#"{"permissions":{"deny":[]}}"#,
        None,
        std::slice::from_ref(&pstack),
    )?;
    for raw in [b"{}".as_slice(), br#"{"permissions":{}}"#] {
        assert!(
            plan(
                target,
                raw,
                first.record.as_ref(),
                std::slice::from_ref(&pstack)
            )
            .is_err()
        );
    }
    Ok(())
}
#[test]
fn pstack_empty_claims_and_partial_policy_updates_keep_nonselected_fields_and_parent_states()
-> TestResult {
    let target = Path::new("/home/source.json");
    let empty_claim = pstack(vec![]);
    let first = plan(target, b"{}", None, std::slice::from_ref(&empty_claim))?;
    assert_eq!(first.after, b"{}");
    let request = plugins(Profile::Mi, &["p@vendor"]);
    let next = plan(
        target,
        &first.after,
        first.record.as_ref(),
        std::slice::from_ref(&request),
    )?;
    let Fields::Claude {
        pstack: Some(kept), ..
    } = &next.record.as_ref().ok_or("record")?.fields
    else {
        return Err("unselected pstack dropped".into());
    };
    assert_eq!(kept.claims.len(), 1);
    let mut retire = empty_claim;
    retire.desired = None;
    let next = plan(target, &next.after, next.record.as_ref(), &[retire])?;
    assert_eq!(
        crate::policy::json::parse(&next.after)?["enabledPlugins"]["p@vendor"],
        false
    );
    let both = plan(
        target,
        b"{}",
        None,
        &[pstack(vec!["Skill(pstack:tdd)".into()]), request],
    )?;
    let mut retire = pstack(vec!["Skill(pstack:tdd)".into()]);
    retire.desired = None;
    let retired = plan(target, &both.after, both.record.as_ref(), &[retire])?;
    assert!(
        crate::policy::json::parse(&retired.after)?
            .get("permissions")
            .is_none()
    );
    Ok(())
}

#[test]
fn owned_field_drift_and_a_large_edit_batch_cannot_return_partial_configurations() -> TestResult {
    let target = Path::new("/home/source.json");
    let p = plugins(Profile::Mi, &["p@vendor"]);
    let old = plan(
        target,
        br#"{"enabledPlugins":{"p@vendor":true}}"#,
        None,
        std::slice::from_ref(&p),
    )?;
    assert!(
        plan(
            target,
            br#"{"enabledPlugins":{}}"#,
            old.record.as_ref(),
            std::slice::from_ref(&p)
        )
        .is_err()
    );
    let stack = pstack(vec!["Skill(pstack:tdd)".into()]);
    let old = plan(target, b"{}", None, std::slice::from_ref(&stack))?;
    assert!(
        plan(
            target,
            br#"{"permissions":{"deny":["Skill(pstack:external)"]}}"#,
            old.record.as_ref(),
            std::slice::from_ref(&stack)
        )
        .is_err()
    );
    assert!(
        plan(
            target,
            br#"{"permissions":null}"#,
            old.record.as_ref(),
            std::slice::from_ref(&p)
        )
        .is_err()
    );
    let mut many = p;
    many.desired = Some(Desired::Plugins(
        (0..1001).map(|n| format!("p{n}@vendor")).collect(),
    ));
    assert!(plan(target, b"{}", None, &[many]).is_err());
    Ok(())
}
#[test]
fn disjoint_selected_and_unselected_fields_cannot_overflow_the_ownership_book() -> TestResult {
    let target = Path::new("/home/source.json");
    let original = plugins(Profile::Zen, &["p@vendor"]);
    let one = plan(
        target,
        br#"{"enabledPlugins":{"p@vendor":false}}"#,
        None,
        &[original],
    )?;
    let mut previous = one.record.ok_or("record")?;
    let Fields::Claude { plugins: old, .. } = &mut previous.fields else {
        return Err("plugins".into());
    };
    let field = old.values().next().ok_or("field")?.clone();
    *old = (0..5000)
        .map(|n| (format!("p{n}@vendor"), field.clone()))
        .collect();
    previous.validate()?;
    let raw = serde_json::to_vec(
        &serde_json::json!({"enabledPlugins":(0..10000).map(|n|(format!("p{n}@vendor"),Value::Bool(false))).collect::<Map<String,Value>>()}),
    )?;
    let mut request = plugins(Profile::Mi, &[]);
    request.desired = Some(Desired::Plugins(
        (5000..10000).map(|n| format!("p{n}@vendor")).collect(),
    ));
    assert!(plan(target, &raw, Some(&previous), &[request]).is_err());
    Ok(())
}
#[test]
fn pruning_proofs_reject_corrupt_containers_and_never_delete_an_unknown_parent() -> TestResult {
    let parents = BTreeMap::from([(
        Parent::EnabledPlugins,
        ParentRecord::capture(ParentState::Missing),
    )]);
    assert!(prune(b"{".to_vec(), &parents, false, false).is_err());
    assert!(
        prune(
            br#"{"enabledPlugins":true}"#.to_vec(),
            &parents,
            false,
            false
        )
        .is_err()
    );
    let bad = BTreeMap::from([(
        Parent::EnabledPlugins,
        ParentRecord {
            original: ParentState::PresentEmpty,
            created_by_tool: true,
        },
    )]);
    assert!(prune(b"{}".to_vec(), &bad, false, false).is_err());
    let oversized = serde_json::to_vec(
        &serde_json::json!({"model":"x".repeat(1024*1024),"enabledPlugins":{}}),
    )?;
    assert!(prune(oversized, &parents, false, false).is_err());
    let mut request = plugins(Profile::Mi, &["p@vendor"]);
    request.owner.intent = Intent::Pstack;
    assert!(
        pstack_plan(
            &Map::new(),
            None,
            &[request],
            &mut BTreeMap::new(),
            &mut vec![]
        )
        .is_err()
    );
    Ok(())
}
