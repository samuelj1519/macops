#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
fn claim(profile: Profile, desired: bool) -> Claim {
    Claim {
        owner: Owner {
            profile: ConfigProfile::Claude(profile),
            role: Role::Source,
            intent: Intent::Plugins,
        },
        intent_sha256: "a".repeat(64),
        desired,
    }
}
#[test]
fn shared_field_keeps_first_original_until_the_last_selected_owner_retires() -> TestResult {
    let mi = claim(Profile::Mi, false);
    let zen = claim(Profile::Zen, false);
    let a = merge(
        &Presence::Present(true),
        None,
        &BTreeMap::from([(mi.owner.id(), Some(mi.clone()))]),
    )?;
    assert_eq!(a.after, Presence::Present(false));
    let b = merge(
        &a.after,
        a.record.as_ref(),
        &BTreeMap::from([(zen.owner.id(), Some(zen.clone()))]),
    )?;
    assert_eq!(
        b.record.as_ref().ok_or("record")?.original,
        Presence::Present(true)
    );
    assert_eq!(b.record.as_ref().ok_or("record")?.claims.len(), 2);
    let c = merge(
        &b.after,
        b.record.as_ref(),
        &BTreeMap::from([(mi.owner.id(), None)]),
    )?;
    assert_eq!(c.after, Presence::Present(false));
    assert_eq!(c.record.as_ref().ok_or("record")?.claims.len(), 1);
    let d = merge(
        &c.after,
        c.record.as_ref(),
        &BTreeMap::from([(zen.owner.id(), None)]),
    )?;
    assert_eq!(d.after, Presence::Present(true));
    assert!(d.record.is_none());
    Ok(())
}

#[test]
fn atomic_owner_updates_preserve_original_and_unselected_claims_and_reject_conflicts() -> TestResult
{
    let mi = claim(Profile::Mi, false);
    let zen = claim(Profile::Zen, false);
    let a_id = mi.owner.id();
    let b_id = zen.owner.id();
    let first = merge(
        &Presence::Missing,
        None,
        &BTreeMap::from([
            (a_id.clone(), Some(mi.clone())),
            (b_id.clone(), Some(zen.clone())),
        ]),
    )?;
    let mut mi_on = mi;
    mi_on.desired = true;
    assert_eq!(
        merge(
            &first.after,
            first.record.as_ref(),
            &BTreeMap::from([(a_id.clone(), Some(mi_on.clone()))])
        )
        .err()
        .ok_or("must reject")?
        .code,
        "profile_field_claim_conflict"
    );
    let mut zen_on = zen;
    zen_on.desired = true;
    let next = merge(
        &first.after,
        first.record.as_ref(),
        &BTreeMap::from([(a_id.clone(), Some(mi_on)), (b_id.clone(), Some(zen_on))]),
    )?;
    assert_eq!(next.after, Presence::Present(true));
    assert_eq!(
        next.record.as_ref().ok_or("record")?.original,
        Presence::Missing
    );
    let retired = merge(
        &next.after,
        next.record.as_ref(),
        &BTreeMap::from([(a_id, None), (b_id, None)]),
    )?;
    assert_eq!(retired.after, Presence::Missing);
    Ok(())
}
#[test]
fn owned_drift_invalid_claims_and_empty_record_never_rewrite_a_field() -> TestResult {
    let a = claim(Profile::Mi, false);
    let id = a.owner.id();
    let first = merge(
        &Presence::Missing,
        None,
        &BTreeMap::from([(id.clone(), Some(a.clone()))]),
    )?;
    assert_eq!(
        merge(
            &Presence::Present(true),
            first.record.as_ref(),
            &BTreeMap::from([(id.clone(), None)])
        )
        .err()
        .ok_or("must reject")?
        .code,
        "profile_owned_field_drift"
    );
    assert_eq!(
        merge(
            &Presence::Present(true),
            first.record.as_ref(),
            &BTreeMap::new()
        )?
        .after,
        Presence::Present(true)
    );
    let mut invalid_owner = a.clone();
    invalid_owner.owner.intent = Intent::CodexSkills;
    assert!(
        merge(
            &Presence::Missing,
            None,
            &BTreeMap::from([(id.clone(), Some(invalid_owner))])
        )
        .is_err()
    );
    let mut invalid_sha = a.clone();
    invalid_sha.intent_sha256 = "bad".into();
    assert!(
        merge(
            &Presence::Missing,
            None,
            &BTreeMap::from([(id.clone(), Some(invalid_sha))])
        )
        .is_err()
    );
    assert!(
        merge(
            &Presence::Missing,
            None,
            &BTreeMap::from([("wrong".into(), Some(a.clone()))])
        )
        .is_err()
    );
    assert!(
        BoolField {
            original: Presence::Missing,
            applied: false,
            claims: BTreeMap::new()
        }
        .validate()
        .is_err()
    );
    assert!(
        BoolField {
            original: Presence::Missing,
            applied: true,
            claims: BTreeMap::from([(id, a)])
        }
        .validate()
        .is_err()
    );
    assert!(
        Owner {
            profile: ConfigProfile::Codex,
            role: Role::Live,
            intent: Intent::Plugins
        }
        .validate()
        .is_err()
    );
    Owner {
        profile: ConfigProfile::Codex,
        role: Role::Live,
        intent: Intent::CodexSkills,
    }
    .validate()?;
    Ok(())
}

#[test]
fn each_parent_records_missing_empty_and_existing_nonempty_without_conflation() -> TestResult {
    for name in [
        "enabledPlugins",
        "permissions",
        "permissions.deny",
        "skills",
        "skills.config",
    ] {
        let missing = ParentRecord::capture(ParentState::Missing);
        let empty = ParentRecord::capture(ParentState::PresentEmpty);
        let existing = ParentRecord::capture(ParentState::PresentNonEmpty);
        assert_eq!(
            missing.after(ParentState::PresentEmpty, false)?,
            ParentAction::Remove,
            "{name}"
        );
        assert_eq!(
            empty.after(ParentState::PresentEmpty, false)?,
            ParentAction::Keep,
            "{name}"
        );
        assert_eq!(
            existing.after(ParentState::PresentEmpty, false)?,
            ParentAction::Keep,
            "{name}"
        );
        assert_eq!(
            missing.after(ParentState::PresentNonEmpty, false)?,
            ParentAction::Keep,
            "{name}"
        );
        assert_eq!(
            missing.after(ParentState::PresentEmpty, true)?,
            ParentAction::Keep,
            "{name}"
        );
        assert_eq!(
            missing.after(ParentState::Missing, false)?,
            ParentAction::Keep,
            "{name}"
        );
    }
    Ok(())
}

#[test]
fn malformed_previous_owners_and_parent_responsibility_cannot_gain_write_authority() -> TestResult {
    let good = claim(Profile::Mi, false);
    let key = good.owner.id();
    let invalid = BoolField {
        original: Presence::Missing,
        applied: false,
        claims: BTreeMap::from([("wrong".into(), good)]),
    };
    assert!(merge(&Presence::Missing, Some(&invalid), &BTreeMap::new()).is_err());
    let oversized: BTreeMap<_, _> = (0..10001).map(|n| (n.to_string(), None)).collect();
    assert!(merge(&Presence::Missing, None, &oversized).is_err());
    let no_original = merge(
        &Presence::Present(true),
        None,
        &BTreeMap::from([(key, None)]),
    )?;
    assert_eq!(no_original.after, Presence::Present(true));
    assert!(no_original.record.is_none());
    assert!(
        ParentRecord {
            original: ParentState::PresentEmpty,
            created_by_tool: true
        }
        .after(ParentState::PresentEmpty, false)
        .is_err()
    );
    Ok(())
}

#[test]
fn closed_owner_identifiers_are_stable_unique_and_distinguish_profile_role_and_intent() {
    let profiles: Vec<_> = Profile::ALL
        .into_iter()
        .map(ConfigProfile::Claude)
        .chain(std::iter::once(ConfigProfile::Codex))
        .collect();
    let mut keys = std::collections::BTreeSet::new();
    let mut valid = 0;
    for profile in profiles {
        for role in [Role::Live, Role::Source] {
            for intent in [Intent::Plugins, Intent::Pstack, Intent::CodexSkills] {
                let owner = Owner {
                    profile,
                    role,
                    intent,
                };
                let key = owner.id();
                assert_eq!(owner.id(), key);
                assert!(keys.insert(key));
                valid += usize::from(owner.validate().is_ok());
            }
        }
    }
    assert_eq!(keys.len(), 30);
    assert_eq!(valid, 18);
    assert_eq!(
        claim(Profile::Mi, false).owner.id(),
        "profile-policy:claude-mi:source:plugins"
    );
}
