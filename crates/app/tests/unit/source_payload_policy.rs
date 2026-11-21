#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use serde_json::json;
fn payload() -> Payload {
    Payload {
        entries: BTreeMap::from([
            (
                "SKILL.md".into(),
                Entry::File {
                    mode: 0o644,
                    bytes: b"---\nname: demo\n---\nOriginal body\n".to_vec(),
                },
            ),
            ("scripts".into(), Entry::Directory { mode: 0o755 }),
            (
                "scripts/run".into(),
                Entry::File {
                    mode: 0o755,
                    bytes: vec![0, 255],
                },
            ),
        ]),
    }
}
#[test]
fn declared_rules_keep_default_license_distinct_from_empty_and_reject_unknown_transform_fields()
-> TestResult {
    let value = json!({"kind":"github-skill","repository":"Owner/Repo","path":"skills/demo","branch":"main","revision":"a".repeat(40)});
    let rules = Rules::declared(&value)?;
    assert_eq!(rules.repository.key(), "owner/repo");
    assert_eq!(rules.licenses.len(), 1);
    assert_eq!(rules.licenses[0].parts()[0].as_str(), "LICENSE");
    let mut empty = value.clone();
    empty["license_files"] = json!([]);
    assert_eq!(Rules::declared(&empty)?.licenses.len(), 0);
    let mut future = value.clone();
    future["run_before_install"] = json!("unexpected-command");
    assert!(Rules::declared(&future).is_err());
    for (field, bad) in [
        ("path", json!("../out")),
        ("license_files", json!(["../LICENSE"])),
        ("license_files", json!(["LICENSE", "LICENSE"])),
        ("exclude", json!(["."])),
        ("manual_only", json!("false")),
        ("edits", json!([{"path":"SKILL.md","old":"","new":"x"}])),
    ] {
        let mut bad_value = value.clone();
        bad_value[field] = bad;
        assert!(Rules::declared(&bad_value).is_err());
    }
    Ok(())
}
#[test]
fn bounded_transform_preserves_binary_modes_applies_manual_before_exact_text_edits_and_detects_license_conflicts()
-> TestResult {
    let mut data = payload();
    let license = Entry::File {
        mode: 0o644,
        bytes: b"legal text".to_vec(),
    };
    data.add_license("nested/LICENSE", license.clone())?;
    data.add_license("LICENSE", license)?;
    assert!(
        data.add_license(
            "other/LICENSE",
            Entry::File {
                mode: 0o644,
                bytes: b"different".to_vec()
            }
        )
        .is_err()
    );
    let rules = Rules::declared(
        &json!({"kind":"github-skill","repository":"owner/repo","path":".","license_files":[],"manual_only":true,"edits":[{"path":"SKILL.md","old":"disable-model-invocation: true","new":"disable-model-invocation: true # kept"}]}),
    )?;
    data.transform(&rules, &[])?;
    assert_eq!(
        data.entries["scripts/run"],
        Entry::File {
            mode: 0o755,
            bytes: vec![0, 255]
        }
    );
    let Entry::File { bytes, .. } = &data.entries["SKILL.md"] else {
        return Err("skill".into());
    };
    assert!(String::from_utf8_lossy(bytes).contains("true # kept"));
    assert_eq!(data.entries["agents"], Entry::Directory { mode: 0o700 });
    assert!(matches!(
        data.entries["agents/openai.yaml"],
        Entry::File { mode: 0o600, .. }
    ));
    let mut mismatch = rules.clone();
    mismatch.manual = false;
    mismatch.edits[0].old = "not present".into();
    assert!(payload().transform(&mismatch, &[]).is_err());
    let mut excludes = rules;
    excludes.manual = false;
    excludes.edits.clear();
    excludes.exclude = vec!["scripts".into()];
    let mut data = payload();
    data.transform(&excludes, &[])?;
    assert!(!data.entries.contains_key("scripts/run"));
    assert!(!excludes.excludes("scripts-old/run"));
    Ok(())
}
#[test]
fn declared_licenses_are_added_after_exclusions() -> TestResult {
    let mut data = payload();
    data.entries.insert(
        "COPYING".into(),
        Entry::File {
            mode: 0o644,
            bytes: b"legal".to_vec(),
        },
    );
    let rules = Rules::declared(
        &json!({"kind":"github-skill","repository":"owner/repo","path":".","license_files":["COPYING"],"exclude":["COPYING"]}),
    )?;
    data.transform(
        &rules,
        &[(
            "COPYING".into(),
            Entry::File {
                mode: 0o644,
                bytes: b"legal".to_vec(),
            },
        )],
    )?;
    assert!(data.entries.contains_key("COPYING"));
    Ok(())
}

#[test]
fn declared_rules_reject_malformed_documents_lists_edits_and_missing_identity_fields() -> TestResult
{
    let base =
        json!({"kind":"github-skill","repository":"owner/repo","path":".","license_files":[]});
    for (field, value) in [
        ("repository", json!(null)),
        ("repository", json!("../repo")),
        ("path", json!(null)),
        ("path", json!("../escape")),
        ("license_files", json!(false)),
        ("license_files", json!([1])),
        ("license_files", json!(vec!["LICENSE"; 129])),
        ("edits", json!(false)),
        ("edits", json!(vec![json!({}); 129])),
        ("edits", json!([1])),
        ("edits", json!([{}])),
        ("edits", json!([{"path":"x","old":"a","new":1}])),
        ("edits", json!([{"path":"../x","old":"a","new":"b"}])),
    ] {
        let mut value_doc = base.clone();
        value_doc[field] = value;
        assert!(Rules::declared(&value_doc).is_err(), "{field}");
    }
    assert!(Rules::declared(&json!([])).is_err());
    let rules = Rules::declared(
        &json!({"kind":"github-skill","repository":"owner/repo","path":".","license_files":["LICENSE"],"edits":[{"path":"SKILL.md","old":"Original","new":"replacement"}]}),
    )?;
    assert_eq!(
        rules
            .locator("asset:demo", &crate::policy::source_git::Reference::Head)?
            .len(),
        64
    );
    Ok(())
}
#[test]
fn payload_transform_rejects_missing_or_nonfile_entrypoints_licenses_and_edit_targets() -> TestResult
{
    let rules = Rules::native(Repository::parse("owner/repo")?, RelativePath::parse(".")?);
    assert!(Payload::default().validate().is_err());
    let mut empty = payload();
    empty.entries.insert(
        "SKILL.md".into(),
        Entry::File {
            mode: 0o644,
            bytes: vec![],
        },
    );
    assert!(empty.validate().is_err());
    let mut invalid_shape = payload();
    invalid_shape.entries.insert(
        "orphan/file".into(),
        Entry::File {
            mode: 0o644,
            bytes: vec![],
        },
    );
    assert!(invalid_shape.validate().is_err());
    for name in [".", "../LICENSE"] {
        assert!(
            payload()
                .add_license(
                    name,
                    Entry::File {
                        mode: 0o644,
                        bytes: vec![]
                    }
                )
                .is_err()
        );
    }
    assert!(
        payload()
            .add_license("LICENSE", Entry::Directory { mode: 0o755 })
            .is_err()
    );
    assert!(
        payload()
            .transform(
                &rules,
                &[(
                    "LICENSE".into(),
                    Entry::File {
                        mode: 0o644,
                        bytes: vec![]
                    }
                )]
            )
            .is_err()
    );
    let mut licensing = rules.clone();
    licensing.licenses = vec![RelativePath::parse("LICENSE")?];
    for path in ["COPYING", "../LICENSE"] {
        assert!(
            payload()
                .transform(
                    &licensing,
                    &[(
                        path.into(),
                        Entry::File {
                            mode: 0o644,
                            bytes: vec![]
                        }
                    )]
                )
                .is_err()
        );
    }
    let mut edit = rules;
    edit.edits = vec![Edit {
        path: "scripts".into(),
        old: "a".into(),
        new: "b".into(),
    }];
    assert!(payload().transform(&edit, &[]).is_err());
    edit.edits[0].path = "scripts/run".into();
    assert!(payload().transform(&edit, &[]).is_err());
    edit.edits[0].path = "SKILL.md".into();
    edit.edits[0].old = String::new();
    assert!(payload().transform(&edit, &[]).is_err());
    Ok(())
}
#[test]
fn metadata_transform_errors_propagate_without_partial_authority() -> TestResult {
    let rules = Rules::native(Repository::parse("owner/repo")?, RelativePath::parse(".")?);
    assert!(Payload::default().transform(&rules, &[]).is_err());
    for value in [
        json!({"path":1,"old":"x","new":"y"}),
        json!({"path":"SKILL.md","old":1,"new":"y"}),
    ] {
        assert!(
            Rules::declared(
                &json!({"kind":"github-skill","repository":"owner/repo","path":".","edits":[value]})
            )
            .is_err()
        );
    }
    let mut license = rules.clone();
    license.licenses = vec![RelativePath::parse("LICENSE")?];
    assert!(
        payload()
            .transform(
                &license,
                &[("LICENSE".into(), Entry::Directory { mode: 0o755 })]
            )
            .is_err()
    );
    let mut manual = rules;
    manual.manual = true;
    let mut bad = payload();
    bad.entries.insert(
        "SKILL.md".into(),
        Entry::File {
            mode: 0o644,
            bytes: b"no frontmatter".to_vec(),
        },
    );
    assert!(bad.transform(&manual, &[]).is_err());
    let mut bad = payload();
    bad.entries
        .insert("agents".into(), Entry::Directory { mode: 0o755 });
    bad.entries.insert(
        "agents/openai.yaml".into(),
        Entry::File {
            mode: 0o600,
            bytes: b"policy: []\n".to_vec(),
        },
    );
    assert!(bad.manual().is_err());
    Ok(())
}

#[test]
fn manual_policy_requires_real_metadata_files_and_keeps_existing_codex_mode() -> TestResult {
    let mut bad = payload();
    bad.entries.remove("SKILL.md");
    assert!(bad.manual().is_err());
    bad.entries
        .insert("SKILL.md".into(), Entry::Directory { mode: 0o755 });
    assert!(bad.manual().is_err());
    let mut bad = payload();
    bad.entries
        .insert("agents".into(), Entry::Directory { mode: 0o755 });
    bad.entries.insert(
        "agents/openai.yaml".into(),
        Entry::Directory { mode: 0o755 },
    );
    assert!(bad.manual().is_err());
    let mut good = payload();
    good.entries
        .insert("agents".into(), Entry::Directory { mode: 0o755 });
    good.entries.insert(
        "agents/openai.yaml".into(),
        Entry::File {
            mode: 0o644,
            bytes: b"name: demo\n".to_vec(),
        },
    );
    good.manual()?;
    assert!(matches!(
        good.entries.get("agents/openai.yaml"),
        Some(Entry::File { mode: 0o644, .. })
    ));
    Ok(())
}
