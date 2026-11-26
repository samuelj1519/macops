use crate::{
    capability::rules::delivery::Profile,
    policy::{
        json_fields as j,
        profile::{
            codex_fields as c, deny_fields as d,
            profile_fields::{self as p, Presence},
        },
    },
    test_support::TestResult,
};
use serde_json::json;
use std::collections::BTreeMap;
#[test]
fn reviewer_probe_codex_comments() -> TestResult {
    let mut lost = 0;
    for raw in [
        "[[skills.config]]\n# user path note\npath='/demo/SKILL.md'\nenabled=false\n",
        "[[skills.config]]\npath='/demo/SKILL.md'\n# user enabled note\nenabled=false\n",
        "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=false\n# trailing user note\n",
        "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=false\n\n# other note\n[other]\ntoken='secret'\n",
    ] {
        let result = c::patch(
            raw.as_bytes(),
            &[c::Change {
                path: "/demo/SKILL.md".into(),
                enabled: Presence::Missing,
                remove_created_table: true,
            }],
        );
        eprintln!("COMMENT INPUT={raw:?} RESULT={result:?}");
        if let Ok(out) = result
            && !String::from_utf8(out)?.contains("note")
        {
            lost += 1;
        }
    }
    assert_eq!(lost, 0, "deleted user notes");
    Ok(())
}
#[test]
fn reviewer_probe_codex_preservation() -> TestResult {
    for raw in [
        "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=true\n[other]\ntoken='secret'",
        "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=true\n# token note\n[other]\ntoken = '''raw\ntext'''\n",
        "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=true\n[skills.config.user]\nkeep=3\n",
        "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=true\nuser.a=3\n",
    ] {
        let out = c::patch(
            raw.as_bytes(),
            &[c::Change {
                path: "/demo/SKILL.md".into(),
                enabled: Presence::Present(false),
                remove_created_table: false,
            }],
        )?;
        eprintln!(
            "PRESERVE INPUT={raw:?} OUTPUT={:?}",
            String::from_utf8(out)?
        );
    }
    assert!(
        c::patch(
            b"skills={config=[{path='/demo/SKILL.md',enabled=true}]}",
            &[c::Change {
                path: "/demo/SKILL.md".into(),
                enabled: Presence::Present(false),
                remove_created_table: false
            }]
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn reviewer_probe_codex_key_comments_on_toggle() -> TestResult {
    let mut lost = 0;
    for raw in [
        "[[skills.config]]\npath='/demo/SKILL.md'\n# user enabled note\nenabled=true\n[other]\ntoken='secret'\n",
        "[[skills.config]]\npath='/demo/SKILL.md'\n# user enabled note\nenabled=true\n",
    ] {
        let result = c::patch(
            raw.as_bytes(),
            &[c::Change {
                path: "/demo/SKILL.md".into(),
                enabled: Presence::Present(false),
                remove_created_table: false,
            }],
        );
        eprintln!("TOGGLE INPUT={raw:?} OUTPUT={result:?}");
        if let Ok(out) = result
            && !String::from_utf8(out)?.contains("note")
        {
            lost += 1;
        }
        let result = c::patch(
            raw.as_bytes(),
            &[c::Change {
                path: "/demo/SKILL.md".into(),
                enabled: Presence::Missing,
                remove_created_table: false,
            }],
        );
        eprintln!("REMOVE ENABLED INPUT={raw:?} OUTPUT={result:?}");
    }
    assert_eq!(lost, 0, "changing owned boolean discarded user key comment");
    Ok(())
}
#[test]
fn reviewer_probe_json_output_is_reusable() -> TestResult {
    let input = format!(
        "{{\"unowned\":[{}],\"enabledPlugins\":{{}}}}",
        vec!["0"; 19997].join(",")
    );
    assert_eq!(j::apply(input.as_bytes(), &[])?, input.as_bytes());
    let result = j::apply(
        input.as_bytes(),
        &[j::Edit {
            path: vec!["enabledPlugins".into(), "new@vendor".into()],
            value: Some(json!(false)),
        }],
    );
    assert!(
        result.is_err(),
        "over-budget after must be rejected before returning rewritten bytes"
    );
    Ok(())
}

#[test]
fn reviewer_probe_codex_protected_eof() -> TestResult {
    let input = "[[skills.config]]\npath='/demo/SKILL.md'\nenabled=true\n[other]\ntoken='secret'";
    let output = c::patch(
        input.as_bytes(),
        &[c::Change {
            path: "/demo/SKILL.md".into(),
            enabled: Presence::Present(false),
            remove_created_table: false,
        }],
    )?;
    assert!(
        output.ends_with(b"[other]\ntoken='secret'"),
        "unowned EOF changed"
    );
    Ok(())
}
#[test]
fn reviewer_probe_json_cases() -> TestResult {
    let raw=br#" \r\n{"enabl\u0065dPlugins":{"target":true},"model":"x\\\"\\u1234","secret":"\u4e2d\ud83d\ude00"} "#;
    let raw = String::from_utf8(raw.to_vec())?.replace("\\r\\n", "\r\n");
    let out = j::apply(
        raw.as_bytes(),
        &[j::Edit {
            path: vec!["enabledPlugins".into(), "target".into()],
            value: Some(json!(false)),
        }],
    )?;
    assert_eq!(String::from_utf8(out)?, raw.replace("true", "false"));
    for raw in [
        br#"{"x":1,"\u0078":2}"#.as_slice(),
        br#"{"x":{"a":1,"a":2}}"#,
    ] {
        assert!(j::apply(raw, &[]).is_err());
    }
    for remove in [
        vec!["a", "b"],
        vec!["a", "c"],
        vec!["b", "c"],
        vec!["a", "b", "c"],
    ] {
        let input = br#"{"a":1,  "b":2,\n "c":3,"protected" : "\u0031"}"#;
        let input = String::from_utf8(input.to_vec())?.replace("\\n", "\n");
        let out = j::apply(
            input.as_bytes(),
            &remove
                .into_iter()
                .map(|key| j::Edit {
                    path: vec![key.into()],
                    value: None,
                })
                .collect::<Vec<_>>(),
        )?;
        assert!(String::from_utf8(out)?.contains(r#""protected" : "\u0031""#));
    }
    assert!(
        j::apply(
            b"{}",
            &[
                j::Edit {
                    path: vec!["a".into()],
                    value: Some(json!(1))
                },
                j::Edit {
                    path: vec!["a".into()],
                    value: None
                }
            ]
        )
        .is_err()
    );
    assert!(
        j::apply(
            format!("{{\"a\":[{}]}}", vec!["0"; 20000].join(",")).as_bytes(),
            &[]
        )
        .is_err()
    );
    Ok(())
}
fn dc(profile: Profile, desired: Vec<String>) -> d::Claim {
    d::Claim {
        owner: p::Owner {
            profile: p::ConfigProfile::Claude(profile),
            role: p::Role::Source,
            intent: p::Intent::Pstack,
        },
        intent_sha256: "a".repeat(64),
        desired,
    }
}
#[test]
fn reviewer_probe_deny_anchors_multiowner() -> TestResult {
    let original = vec![
        "X".into(),
        "Skill(pstack:old)".into(),
        "X".into(),
        "Skill(pstack:old)".into(),
        "Y".into(),
    ];
    let claim = dc(Profile::Mi, vec!["Skill(pstack:new)".into()]);
    let id = claim.owner.id();
    let first = d::merge(
        &original,
        None,
        &BTreeMap::from([(id.clone(), Some(claim.clone()))]),
    )?;
    let other = dc(Profile::Zen, claim.desired.clone());
    let otherid = other.owner.id();
    let shared = d::merge(
        &first.after,
        first.record.as_ref(),
        &BTreeMap::from([(otherid.clone(), Some(other))]),
    )?;
    let partial = d::merge(
        &shared.after,
        shared.record.as_ref(),
        &BTreeMap::from([(id.clone(), None)]),
    )?;
    assert_eq!(partial.after, first.after);
    let retired = d::merge(
        &partial.after,
        partial.record.as_ref(),
        &BTreeMap::from([(otherid, None)]),
    )?;
    assert_eq!(retired.after, original);
    let changed = vec![
        "X".into(),
        "X".into(),
        "Z".into(),
        "Y".into(),
        "Skill(pstack:new)".into(),
    ];
    assert!(
        d::merge(
            &changed,
            first.record.as_ref(),
            &BTreeMap::from([(id.clone(), None)])
        )
        .is_err()
    );
    let ambiguous = vec!["X".into(), "Y".into(), "Skill(pstack:new)".into()];
    assert!(
        d::merge(
            &ambiguous,
            first.record.as_ref(),
            &BTreeMap::from([(id.clone(), None)])
        )
        .is_err()
    );
    let conflicting = dc(Profile::Zen, vec!["Skill(pstack:other)".into()]);
    assert!(
        d::merge(
            &first.after,
            first.record.as_ref(),
            &BTreeMap::from([(conflicting.owner.id(), Some(conflicting))])
        )
        .is_err()
    );
    Ok(())
}
#[test]
fn reviewer_probe_deny_output_is_reusable() {
    let current = vec!["Bash(cat)".into(); 5001];
    let claim = dc(
        Profile::Mi,
        (0..5000).map(|n| format!("Skill(pstack:s{n})")).collect(),
    );
    let id = claim.owner.id();
    assert!(
        d::merge(&current, None, &BTreeMap::from([(id, Some(claim))])).is_err(),
        "over-budget after must not be returned"
    );
}

#[test]
fn reviewer_probe_deny_record_is_valid() {
    let current = vec!["Skill(pstack:old)".into(); 5001];
    let claim = dc(Profile::Mi, vec!["Skill(pstack:new)".into()]);
    let id = claim.owner.id();
    assert!(
        d::merge(&current, None, &BTreeMap::from([(id, Some(claim))])).is_err(),
        "an invalid original record must not be emitted"
    );
}

#[test]
fn reviewer_probe_json_unowned_array_gaps() -> TestResult {
    let input = br#"{"permissions":{"deny":["Bash(cat)", "Read(\u0078)",
        "Write(y)", "Skill(pstack:old)"]}}"#;
    let output = j::apply(
        input,
        &[j::Edit {
            path: vec!["permissions".into(), "deny".into()],
            value: Some(json!([
                "Bash(cat)",
                "Read(x)",
                "Write(y)",
                "Skill(pstack:new)"
            ])),
        }],
    )?;
    let output = String::from_utf8(output)?;
    eprintln!("ARRAY GAP OUTPUT={output:?}");
    assert!(
        output.contains("\"Read(\\u0078)\",\n        \"Write(y)\""),
        "raw gap between two unowned entries changed"
    );
    Ok(())
}
#[test]
fn reviewer_probe_codex_missing_enabled_still_retires_created_table() -> TestResult {
    let input = b"[[skills.config]]\npath='/demo/SKILL.md'\n";
    let output = c::patch(
        input,
        &[c::Change {
            path: "/demo/SKILL.md".into(),
            enabled: Presence::Missing,
            remove_created_table: true,
        }],
    )?;
    eprintln!(
        "MISSING RETIRE OUTPUT={:?}",
        String::from_utf8(output.clone())?
    );
    assert!(
        !c::observe(&output)?.entries.contains_key("/demo/SKILL.md"),
        "explicit table retirement was skipped because enabled already missing"
    );
    Ok(())
}
