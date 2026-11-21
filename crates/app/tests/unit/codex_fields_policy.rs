#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
#[test]
fn codex_skill_enabled_edits_preserve_models_credentials_comments_and_adjacent_tables() -> TestResult
{
    let before=b"model = 'private-model'\r\n# keep next table\r\n[[skills.config]]\r\npath = '/home/.agents/skills/demo/SKILL.md'\r\nenabled = true # own\r\nextra = 'keep'\r\n[other]\r\ntoken = 'PRIVATE_TOKEN'\r\n";
    let after = patch(
        before,
        &[Change {
            path: "/home/.agents/skills/demo/SKILL.md".into(),
            enabled: Presence::Present(false),
            remove_created_table: false,
        }],
    )?;
    let text = String::from_utf8(after)?;
    assert!(text.contains("enabled = false # own"));
    assert!(text.contains("model = 'private-model'"));
    assert!(text.contains("extra = 'keep'"));
    assert!(text.contains("[other]\r\ntoken = 'PRIVATE_TOKEN'"));
    Ok(())
}

#[test]
fn codex_created_tables_can_retire_without_losing_user_tables_or_extra_fields() -> TestResult {
    let path = "/home/.agents/skills/demo/SKILL.md";
    for raw in [
        b"model='x'\n".as_slice(),
        b"model='x'\n[skills]\n",
        b"model='x'\n[skills]\nconfig=[]\n",
    ] {
        let created = patch(
            raw,
            &[Change {
                path: path.into(),
                enabled: Presence::Present(false),
                remove_created_table: false,
            }],
        )?;
        assert_eq!(
            observe(&created)?.entries[path].enabled,
            Presence::Present(false)
        );
        let retired = patch(
            &created,
            &[Change {
                path: path.into(),
                enabled: Presence::Missing,
                remove_created_table: true,
            }],
        )?;
        assert!(!observe(&retired)?.entries.contains_key(path));
        assert!(String::from_utf8(retired)?.contains("model='x'"));
    }
    let user=b"[[skills.config]]\npath='/home/.agents/skills/demo/SKILL.md'\nenabled=false\nuser='keep'\n";
    let after = patch(
        user,
        &[Change {
            path: path.into(),
            enabled: Presence::Missing,
            remove_created_table: true,
        }],
    )?;
    assert!(observe(&after)?.entries[path].other_fields);
    assert!(String::from_utf8(after)?.contains("user='keep'"));
    Ok(())
}

#[test]
fn invalid_toml_shapes_duplicate_paths_wrong_enabled_types_and_limits_are_explicit_errors()
-> TestResult {
    for raw in [
        b"\xff".as_slice(),
        b"[",
        b"skills=3",
        b"[skills]\nconfig=[1]\n",
        b"[[skills.config]]\nenabled=false\n",
        b"[[skills.config]]\npath='a'\nenabled='false'\n",
        b"[[skills.config]]\npath='a'\n[[skills.config]]\npath='a'\n",
    ] {
        assert!(observe(raw).is_err(), "accepted {raw:?}");
    }
    assert!(observe(&vec![b' '; 1024 * 1024 + 1]).is_err());
    let change = Change {
        path: "/home/skill/SKILL.md".into(),
        enabled: Presence::Present(false),
        remove_created_table: false,
    };
    assert!(patch(b"", &[change.clone(), change.clone()]).is_err());
    assert!(
        patch(
            b"",
            &(0..5001)
                .map(|n| Change {
                    path: format!("/home/{n}/SKILL.md"),
                    ..change.clone()
                })
                .collect::<Vec<_>>()
        )
        .is_err()
    );
    assert!(patch(b"[", std::slice::from_ref(&change)).is_err());
    assert!(patch(b"model='x'\r\n[skills]\n", std::slice::from_ref(&change)).is_err());
    let raw = b"model='x'\r\n[other]\r\ntoken='private'";
    let out = patch(raw, &[change])?;
    assert!(String::from_utf8(out)?.contains("token='private'"));
    Ok(())
}

#[test]
fn mixed_codex_changes_retire_missing_paths_without_creating_unwanted_tables() -> TestResult {
    let absent = Change {
        path: "/absent/SKILL.md".into(),
        enabled: Presence::Missing,
        remove_created_table: true,
    };
    let changed = Change {
        path: "/changed/SKILL.md".into(),
        enabled: Presence::Present(false),
        remove_created_table: false,
    };
    for raw in [
        b"model='x'\n".as_slice(),
        b"[skills]\n",
        b"[skills]\nconfig=[]\n",
        b"[[skills.config]]\npath='/changed/SKILL.md'\nenabled=true\n",
    ] {
        let out = patch(raw, &[absent.clone(), changed.clone()])?;
        let observed = observe(&out)?;
        assert!(!observed.entries.contains_key(&absent.path));
        assert_eq!(
            observed.entries[&changed.path].enabled,
            Presence::Present(false)
        );
    }
    let out = patch(b"model='x'\r\n", std::slice::from_ref(&changed))?;
    assert!(out.ends_with(b"\r\n"));
    let very_long = Change {
        path: format!("/{}", "x".repeat(1024 * 1024)),
        ..changed
    };
    assert!(patch(b"", &[very_long]).is_err());
    Ok(())
}
#[test]
fn toml_postconditions_reject_bad_projections_invalid_output_and_wrong_field_values() -> TestResult
{
    let first = parse(b"model='first'\n")?;
    let second = parse(b"model='other'\n")?;
    assert!(verify_patch(&first, &second, b"model='first'\n", &[]).is_err());
    assert!(verify_patch(&first, &first, b"[", &[]).is_err());
    let change = Change {
        path: "/s/SKILL.md".into(),
        enabled: Presence::Present(false),
        remove_created_table: false,
    };
    assert!(
        verify_patch(
            &first,
            &first,
            b"[[skills.config]]\npath='/s/SKILL.md'\nenabled=true\n",
            &[change]
        )
        .is_err()
    );
    verify_patch(&first, &first, b"model='first'\n", &[])?;
    assert_eq!(
        line_endings("model='first'".into(), b"model='first'")?,
        b"model='first'"
    );
    Ok(())
}
#[test]
fn ownership_projection_keeps_unidentifiable_nodes_and_checks_empty_ast_containers() -> TestResult {
    let before = observe(b"")?;
    for raw in [
        b"skills=3\n".as_slice(),
        b"[[skills.config]]\nunknown='preserve'\n",
        b"[[skills.config]]\npath='/unselected/SKILL.md'\nenabled=true\n",
    ] {
        let mut doc = parse(raw)?;
        let original = doc.to_string();
        strip_owned(&mut doc, &[], &before);
        assert_eq!(doc.to_string(), original);
    }
    let mut doc = parse(b"[skills]\n")?;
    doc["skills"]["config"] = Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    assert_eq!(observe_doc(&doc)?.config_parent, ParentState::PresentEmpty);
    let change = Change {
        path: "/s/SKILL.md".into(),
        enabled: Presence::Present(false),
        remove_created_table: false,
    };
    for raw in [b"skills=3".as_slice(), b"[skills]\nconfig=3\n"] {
        assert!(apply_change(&mut parse(raw)?, &change).is_err());
    }
    Ok(())
}
#[test]
fn pruning_rejects_invalid_responsibility_and_preserves_parent_comments() -> TestResult {
    use crate::policy::{profile_fields::ParentRecord, profile_registry::Parent};
    let missing = ParentRecord::capture(ParentState::Missing);
    let parents = BTreeMap::from([
        (Parent::Skills, missing.clone()),
        (Parent::SkillsConfig, missing),
    ]);
    assert!(prune_parents(b"[", &parents, false).is_err());
    assert!(prune_parents(b"skills=3", &parents, false).is_err());
    for parent in [Parent::Skills, Parent::SkillsConfig] {
        let mut bad = parents.clone();
        bad.insert(
            parent,
            ParentRecord {
                original: ParentState::PresentEmpty,
                created_by_tool: true,
            },
        );
        assert!(prune_parents(b"[skills]\nconfig=[]\n", &bad, false).is_err());
    }
    let comments = b"[skills]\n# user's note\nconfig=[]\n";
    assert_eq!(prune_parents(comments, &parents, false)?, comments);
    let table_comment = b"# user's table note\n[skills]\n";
    assert_eq!(
        prune_parents(table_comment, &parents, false)?,
        table_comment
    );
    for raw in [b"".as_slice(), b"skills=3\n", b"[skills]\nconfig=[1]\n"] {
        let mut doc = parse(raw)?;
        let original = doc.to_string();
        assert!(!remove_empty_config(&mut doc));
        assert_eq!(doc.to_string(), original);
    }
    Ok(())
}

#[test]
fn reviewer_empty_config_array_inner_comments_survive_conversion() {
    let raw = b"model='x'\n[skills]\nconfig = [\n  # user's inner empty-array note\n]\n[other]\ntoken='private'\n";
    let output = patch(
        raw,
        &[Change {
            path: "/demo/SKILL.md".into(),
            enabled: Presence::Present(false),
            remove_created_table: false,
        }],
    );
    eprintln!("INNER_ARRAY_COMMENT_RESULT={output:?}");
    assert!(
        output.as_ref().map_or(true, |bytes| bytes
            .windows(b"user's inner empty-array note".len())
            .any(|w| w == b"user's inner empty-array note")),
        "successful conversion deleted user comment inside original empty array"
    );
}
#[test]
fn reviewer_empty_config_array_inner_comments_survive_pruning() -> TestResult {
    use crate::policy::{profile_fields::ParentRecord, profile_registry::Parent};
    let raw = b"model='x'\n[skills]\nconfig = [\n  # user's inner empty-array note\n]\n[other]\ntoken='private'\n";
    let missing = ParentRecord::capture(ParentState::Missing);
    let parents = BTreeMap::from([
        (Parent::Skills, missing.clone()),
        (Parent::SkillsConfig, missing),
    ]);
    let output = prune_parents(raw, &parents, false)?;
    eprintln!(
        "PRUNED_INNER_ARRAY_COMMENT_RESULT={:?}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        output
            .windows(b"user's inner empty-array note".len())
            .any(|w| w == b"user's inner empty-array note"),
        "successful pruning deleted user comment inside empty array"
    );
    Ok(())
}

#[test]
fn hash_characters_in_scalar_values_are_not_array_comments() {
    assert!(!value_has_comments(&Value::from("# literal text")));
    assert!(!value_has_comments(&Value::from(false)));
}
