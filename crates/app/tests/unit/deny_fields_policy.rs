#![cfg(test)]
use super::*;
use crate::{
    policy::{delivery::Profile, profile_fields::Role},
    test_support::TestResult,
};
fn claim() -> Claim {
    Claim {
        owner: Owner {
            profile: ConfigProfile::Claude(Profile::Mi),
            role: Role::Live,
            intent: Intent::Pstack,
        },
        intent_sha256: "a".repeat(64),
        desired: vec!["Skill(pstack:one)".into(), "Skill(pstack:two)".into()],
    }
}
#[test]
fn pstack_owned_entries_merge_without_losing_nonowned_order_and_restore_interleaving() -> TestResult
{
    let original = vec![
        "Bash(cat)".into(),
        "Skill(pstack:original)".into(),
        "Read(secret)".into(),
    ];
    let owner = claim();
    let id = owner.owner.id();
    let applied = merge(
        &original,
        None,
        &BTreeMap::from([(id.clone(), Some(owner))]),
    )?;
    assert_eq!(
        applied.after,
        vec![
            "Bash(cat)",
            "Read(secret)",
            "Skill(pstack:one)",
            "Skill(pstack:two)"
        ]
    );
    let retired = merge(
        &applied.after,
        applied.record.as_ref(),
        &BTreeMap::from([(id, None)]),
    )?;
    assert_eq!(retired.after, original);
    assert!(retired.record.is_none());
    Ok(())
}

#[test]
fn deny_records_reject_foreign_owner_shapes_disagreement_and_invalid_anchors_before_merge()
-> TestResult {
    let good = claim();
    let id = good.owner.id();
    let first = merge(
        &[],
        None,
        &BTreeMap::from([(id.clone(), Some(good.clone()))]),
    )?;
    let record = first.record.ok_or("record")?;
    let mut claims = vec![];
    let mut c = good.clone();
    c.owner.profile = ConfigProfile::Codex;
    claims.push((id.clone(), c));
    let mut c = good.clone();
    c.owner.intent = Intent::Plugins;
    claims.push((id.clone(), c));
    let mut c = good.clone();
    c.owner.profile = ConfigProfile::Codex;
    c.owner.intent = Intent::CodexSkills;
    claims.push((id.clone(), c));
    claims.push(("wrong".into(), good.clone()));
    let mut c = good.clone();
    c.intent_sha256 = "bad".into();
    claims.push((id.clone(), c));
    for desired in [
        vec!["bad".into()],
        vec!["Skill(pstack:duplicate)".into(); 2],
        vec!["Skill(pstack:item)".into(); 5001],
    ] {
        let mut c = good.clone();
        c.desired = desired;
        claims.push((id.clone(), c));
    }
    for (key, c) in claims {
        assert!(merge(&[], None, &BTreeMap::from([(key, Some(c))])).is_err());
    }
    let mut cases = vec![];
    let mut r = record.clone();
    r.claims.clear();
    cases.push(r);
    let mut r = record.clone();
    r.applied.clear();
    cases.push(r);
    let mut r = record.clone();
    r.claims = BTreeMap::from([("wrong".into(), good.clone())]);
    cases.push(r);
    for anchor in [
        Anchor {
            sha256: "bad".into(),
            occurrence: 0,
            count: 1,
        },
        Anchor {
            sha256: "a".repeat(64),
            occurrence: 0,
            count: 5001,
        },
        Anchor {
            sha256: "a".repeat(64),
            occurrence: 1,
            count: 1,
        },
    ] {
        let mut r = record.clone();
        r.original = vec![Slot {
            entry: "Skill(pstack:old)".into(),
            previous: Some(anchor),
            next: None,
        }];
        cases.push(r);
    }
    let mut r = record.clone();
    r.original = vec![Slot {
        entry: "not-owned".into(),
        previous: None,
        next: None,
    }];
    cases.push(r);
    let mut r = record.clone();
    r.claims = (0..10001).map(|n| (n.to_string(), good.clone())).collect();
    cases.push(r);
    let mut r = record;
    r.original = vec![
        Slot {
            entry: "Skill(pstack:old)".into(),
            previous: None,
            next: None
        };
        5001
    ];
    cases.push(r);
    for previous in cases {
        assert!(merge(&[], Some(&previous), &BTreeMap::new()).is_err());
    }
    Ok(())
}
#[test]
fn deny_noops_retirement_and_drift_keep_user_values_without_guessing_new_ownership() -> TestResult {
    let owner = claim();
    let id = owner.owner.id();
    let original = vec!["Skill(pstack:original)".into()];
    let first = merge(
        &original,
        None,
        &BTreeMap::from([(id.clone(), Some(owner))]),
    )?;
    assert_eq!(
        merge(&first.after, first.record.as_ref(), &BTreeMap::new())?.after,
        first.after
    );
    assert_eq!(
        merge(
            &original,
            first.record.as_ref(),
            &BTreeMap::from([(id.clone(), None)])
        )?
        .after,
        original
    );
    assert!(
        merge(
            &["Skill(pstack:external)".into()],
            first.record.as_ref(),
            &BTreeMap::from([(id.clone(), None)])
        )
        .is_err()
    );
    let noop = merge(&["Bash(cat)".into()], None, &BTreeMap::from([(id, None)]))?;
    assert_eq!(noop.after, vec!["Bash(cat)"]);
    assert!(noop.record.is_none());
    assert!(merge(&vec![String::new(); 10001], None, &BTreeMap::new()).is_err());
    assert!(
        merge(
            &[],
            None,
            &(0..10001).map(|n| (n.to_string(), None)).collect()
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn a_missing_right_anchor_blocks_restoration_before_any_owned_deny_is_returned() -> TestResult {
    let c = claim();
    let id = c.owner.id();
    let applied = merge(
        &["Skill(pstack:original)".into(), "Bash(anchor)".into()],
        None,
        &BTreeMap::from([(id.clone(), Some(c))]),
    )?;
    assert!(
        merge(
            &["Skill(pstack:one)".into(), "Skill(pstack:two)".into()],
            applied.record.as_ref(),
            &BTreeMap::from([(id, None)])
        )
        .is_err()
    );
    Ok(())
}
