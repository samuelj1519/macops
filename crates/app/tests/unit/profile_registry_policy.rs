#![cfg(test)]
use super::*;
use crate::{
    capability::rules::delivery::Profile,
    policy::profile::profile_fields::{Claim, Owner, ParentState, Presence, Role},
    test_support::TestResult,
};
fn bool_field(profile: ConfigProfile, intent: Intent) -> BoolField {
    let owner = Owner {
        profile,
        role: Role::Live,
        intent,
    };
    BoolField {
        original: Presence::Missing,
        applied: false,
        claims: BTreeMap::from([(
            owner.id(),
            Claim {
                owner,
                intent_sha256: "a".repeat(64),
                desired: false,
            },
        )]),
    }
}
fn claude() -> File {
    File {
        target: "/home/.claude-mi/settings.json".into(),
        parents: BTreeMap::from([(
            Parent::EnabledPlugins,
            ParentRecord::capture(ParentState::Missing),
        )]),
        fields: Fields::Claude {
            plugins: BTreeMap::from([(
                "p@vendor".into(),
                bool_field(ConfigProfile::Claude(Profile::Mi), Intent::Plugins),
            )]),
            pstack: None,
        },
    }
}
fn codex() -> File {
    File {
        target: "/home/.codex/config.toml".into(),
        parents: BTreeMap::from([
            (Parent::Skills, ParentRecord::capture(ParentState::Missing)),
            (
                Parent::SkillsConfig,
                ParentRecord::capture(ParentState::PresentEmpty),
            ),
        ]),
        fields: Fields::Codex {
            skills: BTreeMap::from([(
                "/home/skill/SKILL.md".into(),
                CodexField {
                    enabled: bool_field(ConfigProfile::Codex, Intent::CodexSkills),
                    table: ParentRecord::capture(ParentState::Missing),
                },
            )]),
        },
    }
}
fn pstack() -> File {
    let mut file = claude();
    for parent in [Parent::Permissions, Parent::PermissionsDeny] {
        file.parents
            .insert(parent, ParentRecord::capture(ParentState::Missing));
    }
    let owner = Owner {
        profile: ConfigProfile::Claude(Profile::Mi),
        role: Role::Live,
        intent: Intent::Pstack,
    };
    if let Fields::Claude { pstack, .. } = &mut file.fields {
        *pstack = Some(deny_fields::Record {
            original: vec![],
            applied: vec!["Skill(pstack:tdd)".into()],
            claims: BTreeMap::from([(
                owner.id(),
                deny_fields::Claim {
                    owner,
                    intent_sha256: "a".repeat(64),
                    desired: vec!["Skill(pstack:tdd)".into()],
                },
            )]),
        });
    }
    file
}
#[test]
fn claude_registry_rejects_missing_parents_invalid_plugin_keys_and_foreign_owner_kinds()
-> TestResult {
    claude().validate()?;
    pstack().validate()?;
    let mut cases = vec![];
    let mut f = claude();
    f.target = "relative".into();
    cases.push(f);
    let mut f = claude();
    f.parents
        .values_mut()
        .for_each(|p| p.created_by_tool = false);
    cases.push(f);
    let mut f = claude();
    f.parents.clear();
    cases.push(f);
    for parent in [Parent::Permissions, Parent::PermissionsDeny] {
        let mut f = pstack();
        f.parents.remove(&parent);
        cases.push(f);
    }
    for parent in [Parent::Skills, Parent::SkillsConfig] {
        let mut f = claude();
        f.parents
            .insert(parent, ParentRecord::capture(ParentState::Missing));
        cases.push(f);
    }
    let mut f = claude();
    if let Fields::Claude { plugins, .. } = &mut f.fields {
        let field = plugins.values().next().ok_or("field")?.clone();
        *plugins = (0..5001)
            .map(|n| (format!("p{n}@vendor"), field.clone()))
            .collect();
    }
    cases.push(f);
    let mut f = claude();
    if let Fields::Claude { plugins, .. } = &mut f.fields {
        let field = plugins.values().next().ok_or("field")?.clone();
        *plugins = BTreeMap::from([("../escape".into(), field)]);
    }
    cases.push(f);
    let mut f = claude();
    if let Fields::Claude { plugins, .. } = &mut f.fields {
        for field in plugins.values_mut() {
            field.claims.clear();
        }
    }
    cases.push(f);
    for (profile, intent) in [
        (ConfigProfile::Codex, Intent::CodexSkills),
        (ConfigProfile::Claude(Profile::Mi), Intent::Pstack),
    ] {
        let mut f = claude();
        if let Fields::Claude { plugins, .. } = &mut f.fields {
            *plugins = BTreeMap::from([("p@vendor".into(), bool_field(profile, intent))]);
        }
        cases.push(f);
    }
    let mut f = pstack();
    if let Fields::Claude {
        pstack: Some(record),
        ..
    } = &mut f.fields
    {
        record.claims.clear();
    }
    cases.push(f);
    for file in cases {
        assert!(file.validate().is_err(), "accepted {file:?}");
    }
    Ok(())
}
#[test]
fn codex_registry_rejects_foreign_fields_wrong_paths_and_inconsistent_table_responsibility()
-> TestResult {
    codex().validate()?;
    let mut cases = vec![];
    for parent in [Parent::Skills, Parent::SkillsConfig] {
        let mut f = codex();
        f.parents.remove(&parent);
        cases.push(f);
    }
    let mut f = codex();
    f.parents.insert(
        Parent::Permissions,
        ParentRecord::capture(ParentState::Missing),
    );
    cases.push(f);
    let mut f = codex();
    if let Fields::Codex { skills } = &mut f.fields {
        let field = skills.values().next().ok_or("field")?.clone();
        *skills = (0..5001)
            .map(|n| (PathBuf::from(format!("/home/{n}/SKILL.md")), field.clone()))
            .collect();
    }
    cases.push(f);
    let mut f = codex();
    if let Fields::Codex { skills } = &mut f.fields {
        let field = skills.values().next().ok_or("field")?.clone();
        *skills = BTreeMap::from([("relative".into(), field)]);
    }
    cases.push(f);
    let mut f = codex();
    if let Fields::Codex { skills } = &mut f.fields {
        for field in skills.values_mut() {
            field.enabled.claims.clear();
        }
    }
    cases.push(f);
    let mut f = codex();
    if let Fields::Codex { skills } = &mut f.fields {
        for field in skills.values_mut() {
            field.table.created_by_tool = false;
        }
    }
    cases.push(f);
    let mut f = codex();
    if let Fields::Codex { skills } = &mut f.fields {
        let foreign = bool_field(ConfigProfile::Claude(Profile::Mi), Intent::Plugins);
        for field in skills.values_mut() {
            field.enabled = foreign.clone();
        }
    }
    cases.push(f);
    for file in cases {
        assert!(file.validate().is_err(), "accepted {file:?}");
    }
    Ok(())
}
#[test]
fn registry_keys_and_budget_are_checked_before_a_snapshot_can_be_used() -> TestResult {
    let file = claude();
    validate(&BTreeMap::from([(file.target.clone(), file.clone())]))?;
    assert!(validate(&BTreeMap::from([("/wrong/file".into(), file.clone())])).is_err());
    let mut broken = file.clone();
    broken.parents.clear();
    assert!(validate(&BTreeMap::from([(broken.target.clone(), broken)])).is_err());
    assert!(
        validate(
            &(0..513)
                .map(|n| (PathBuf::from(format!("/home/{n}")), file.clone()))
                .collect()
        )
        .is_err()
    );
    Ok(())
}
