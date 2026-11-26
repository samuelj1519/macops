#![cfg(test)]
use super::*;
use crate::{policy::profile::profile_fields::Role, test_support::TestResult};
use std::{ffi::OsString, os::unix::ffi::OsStringExt};
#[test]
fn codex_parent_missing_empty_table_and_empty_array_have_distinct_retirement_results() -> TestResult
{
    for raw in [
        b"model='x'\n".as_slice(),
        b"model='x'\n[skills]\n",
        b"model='x'\n[skills]\nconfig=[]\n",
    ] {
        let target = Path::new("/home/.codex/config.toml");
        let path = PathBuf::from("/home/.agents/skills/demo/SKILL.md");
        let request = Request {
            owner: Owner {
                profile: ConfigProfile::Codex,
                role: Role::Live,
                intent: Intent::CodexSkills,
            },
            intent_sha256: "a".repeat(64),
            disabled: Some(BTreeSet::from([path])),
        };
        let before = codex_fields::observe(raw)?;
        let applied = plan(target, raw, None, std::slice::from_ref(&request))?;
        let mut retire = request;
        retire.disabled = None;
        let restored = plan(target, &applied.after, applied.record.as_ref(), &[retire])?;
        let after = codex_fields::observe(&restored.after)?;
        assert_eq!(before.skills_parent, after.skills_parent);
        assert_eq!(before.config_parent, after.config_parent);
        assert!(String::from_utf8(restored.after)?.contains("model='x'"));
        assert!(restored.record.is_none());
    }
    Ok(())
}

fn request() -> Request {
    Request {
        owner: Owner {
            profile: ConfigProfile::Codex,
            role: Role::Live,
            intent: Intent::CodexSkills,
        },
        intent_sha256: "a".repeat(64),
        disabled: Some(BTreeSet::from([PathBuf::from("/home/skill/SKILL.md")])),
    }
}
#[test]
fn codex_requests_require_valid_owner_hash_unique_selection_and_bounded_absolute_paths() {
    let target = Path::new("/home/.codex/config.toml");
    let good = request();
    let mut cases = vec![vec![good.clone(), good.clone()]];
    let mut r = good.clone();
    r.owner.intent = Intent::Plugins;
    cases.push(vec![r]);
    let mut r = good.clone();
    r.owner.profile = ConfigProfile::Claude(crate::capability::rules::delivery::Profile::Mi);
    r.owner.intent = Intent::Plugins;
    cases.push(vec![r]);
    let mut r = good.clone();
    r.intent_sha256 = "bad".into();
    cases.push(vec![r]);
    let mut r = good.clone();
    r.disabled = Some(
        (0..5001)
            .map(|n| PathBuf::from(format!("/home/skill{n}/SKILL.md")))
            .collect(),
    );
    cases.push(vec![r]);
    let mut r = good.clone();
    r.disabled = Some(BTreeSet::from([PathBuf::from("relative")]));
    cases.push(vec![r]);
    let mut r = good.clone();
    r.disabled = Some(BTreeSet::from([PathBuf::from(OsString::from_vec(
        b"/home/\xff".to_vec(),
    ))]));
    cases.push(vec![r]);
    for requests in cases {
        assert!(plan(target, b"model='x'\n", None, &requests).is_err());
    }
    assert!(plan(Path::new("relative"), b"", None, &[good]).is_err());
    assert!(plan(target, b"[", None, &[]).is_err());
}
#[test]
fn codex_preserves_preexisting_tables_and_rejects_corrupt_or_foreign_previous_owners() -> TestResult
{
    let target = Path::new("/home/.codex/config.toml");
    let before = b"model='x'\n[[skills.config]]\npath='/home/skill/SKILL.md'\nextra='preserve'\n";
    assert_eq!(plan(target, before, None, &[])?.after, before);
    let r = request();
    let applied = plan(target, before, None, std::slice::from_ref(&r))?;
    let mut retire = r.clone();
    retire.disabled = None;
    let restored = plan(target, &applied.after, applied.record.as_ref(), &[retire])?;
    assert_eq!(restored.after, before);
    let mut previous = applied.record.ok_or("record")?;
    previous.target = "/other/config.toml".into();
    assert!(
        plan(
            target,
            &applied.after,
            Some(&previous),
            std::slice::from_ref(&r)
        )
        .is_err()
    );
    previous.target = target.into();
    previous.fields = Fields::Claude {
        plugins: BTreeMap::new(),
        pstack: None,
    };
    previous.parents.clear();
    assert!(plan(target, before, Some(&previous), std::slice::from_ref(&r)).is_err());
    previous.target = "relative".into();
    assert!(plan(target, before, Some(&previous), &[r]).is_err());
    Ok(())
}

#[test]
fn codex_owned_drift_and_unsafe_parent_conversion_are_rejected_before_returning_after_bytes()
-> TestResult {
    let target = Path::new("/home/.codex/config.toml");
    let r = request();
    let old = plan(
        target,
        b"[[skills.config]]\npath='/home/skill/SKILL.md'\nenabled=true\n",
        None,
        std::slice::from_ref(&r),
    )?;
    assert!(
        plan(
            target,
            b"[[skills.config]]\npath='/home/skill/SKILL.md'\n",
            old.record.as_ref(),
            std::slice::from_ref(&r)
        )
        .is_err()
    );
    assert!(
        plan(
            target,
            b"[skills]\n# user's config note\nconfig=[]\n",
            None,
            std::slice::from_ref(&r)
        )
        .is_err()
    );
    let previous = profile_registry::File {
        target: target.into(),
        parents: BTreeMap::from([
            (
                Parent::Skills,
                ParentRecord::capture(profile_fields::ParentState::Missing),
            ),
            (
                Parent::SkillsConfig,
                ParentRecord::capture(profile_fields::ParentState::Missing),
            ),
        ]),
        fields: Fields::Codex {
            skills: BTreeMap::new(),
        },
    };
    previous.validate()?;
    let mut retire = r;
    retire.disabled = None;
    assert!(
        plan(
            target,
            b"model='x'\r\n[skills]\nconfig=[]\n",
            Some(&previous),
            &[retire]
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn shared_codex_ownership_cannot_exceed_the_total_field_budget_during_partial_selection()
-> TestResult {
    let target = Path::new("/home/.codex/config.toml");
    let mut source = request().owner;
    source.role = Role::Source;
    let field = CodexField {
        enabled: profile_fields::BoolField {
            original: Presence::Present(false),
            applied: false,
            claims: BTreeMap::from([(
                source.id(),
                profile_fields::Claim {
                    owner: source,
                    intent_sha256: "a".repeat(64),
                    desired: false,
                },
            )]),
        },
        table: ParentRecord::capture(profile_fields::ParentState::PresentNonEmpty),
    };
    let previous = profile_registry::File {
        target: target.into(),
        parents: BTreeMap::from([
            (
                Parent::Skills,
                ParentRecord::capture(profile_fields::ParentState::PresentNonEmpty),
            ),
            (
                Parent::SkillsConfig,
                ParentRecord::capture(profile_fields::ParentState::PresentNonEmpty),
            ),
        ]),
        fields: Fields::Codex {
            skills: (0..5000)
                .map(|n| (PathBuf::from(format!("/s/{n}/SKILL.md")), field.clone()))
                .collect(),
        },
    };
    previous.validate()?;
    let mut raw = String::new();
    for n in 0..10000 {
        std::fmt::Write::write_fmt(
            &mut raw,
            format_args!("[[skills.config]]\npath='/s/{n}/SKILL.md'\nenabled=false\n"),
        )?;
    }
    let mut r = request();
    r.disabled = Some(
        (5000..10000)
            .map(|n| PathBuf::from(format!("/s/{n}/SKILL.md")))
            .collect(),
    );
    assert!(plan(target, raw.as_bytes(), Some(&previous), &[r]).is_err());
    Ok(())
}
