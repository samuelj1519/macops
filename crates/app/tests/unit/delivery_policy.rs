#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::os::unix::ffi::OsStringExt;

#[test]
fn link_text_ownership_uses_exact_original_bytes_instead_of_path_equivalence() -> TestResult {
    let owned = LinkText::managed("demo")?;
    assert_eq!(owned.as_str(), "../../.agents/skills/demo");
    assert!(owned.owned_by("demo"));
    for text in [
        "../../.agents//skills/demo",
        "../../.agents/skills/./demo",
        "../../.agents/skills/demo/",
        "/h/.agents/skills/demo",
        "../../.agents/skills/other",
    ] {
        let foreign = LinkText::from_bytes(text.as_bytes())?;
        assert!(!foreign.owned_by("demo"));
        assert_ne!(foreign, owned);
        assert_eq!(foreign.as_str().as_bytes(), text.as_bytes());
    }
    let encoded = serde_json::to_vec(&owned)?;
    assert_eq!(serde_json::from_slice::<LinkText>(&encoded)?, owned);
    Ok(())
}

#[test]
fn link_text_rejects_unrepresentable_records_and_managed_path_components() -> TestResult {
    for bytes in [b"".as_slice(), b"bad\0value", &[0xff], &vec![b'a'; 4097]] {
        assert!(LinkText::from_bytes(bytes).is_err());
    }
    for name in [
        "",
        ".",
        "..",
        "../demo",
        "a/b",
        "a\\b",
        "a\nb",
        "a\tb",
        &"a".repeat(256),
    ] {
        assert!(LinkText::managed(name).is_err());
        assert!(!LinkText::from_bytes(b"foreign")?.owned_by(name));
    }
    for encoded in [b"[]".as_slice(), b"true", b"null", b"\"bad\\u0000value\""] {
        assert!(serde_json::from_slice::<LinkText>(encoded).is_err());
    }
    assert!(LinkText::managed("中文-skill")?.owned_by("中文-skill"));
    Ok(())
}

#[test]
fn profile_rules_preserve_explicit_empty_all_default_and_pending_configuration_intents()
-> TestResult {
    let parsed = profiles(br#"{"_comment":"policy","claude":{"mi":{"links":[],"pstack_allow":["tdd"]},"kimi":{"links":["tdd"],"plugins_off":["pstack@cursor-plugins"]},"zen":"all"},"codex":{"disable_agents_skills":["mail"]}}"#)?;
    assert!(
        matches!(&parsed.rules[&Profile::Mi].links, LinkRule::Selected(names) if names.is_empty())
    );
    assert!(
        matches!(&parsed.rules[&Profile::Kimi].links, LinkRule::Selected(names) if names.contains("tdd"))
    );
    assert_eq!(parsed.rules[&Profile::Zen].links, LinkRule::All);
    assert_eq!(parsed.rules[&Profile::Glm].links, LinkRule::All);
    assert!(!parsed.declared.contains(&Profile::Glm));
    assert!(
        parsed.rules[&Profile::Mi]
            .pstack_allow
            .as_ref()
            .is_some_and(|v| v.contains("tdd"))
    );
    assert!(
        parsed.rules[&Profile::Kimi]
            .plugins_off
            .as_ref()
            .is_some_and(|v| v.contains("pstack@cursor-plugins"))
    );
    assert!(
        parsed
            .codex_disabled
            .as_ref()
            .is_some_and(|v| v.contains("mail"))
    );
    assert_eq!(
        Profile::Mi.root(Path::new("/home"))?,
        Path::new("/home/.claude-mi")
    );
    Ok(())
}

#[test]
fn profile_schema_rejects_unknown_profiles_positional_objects_nulls_duplicates_and_bad_names()
-> TestResult {
    assert_eq!(profiles(b"{}")?, Profiles::default());
    let all=profiles(r#"{"claude":{"_comment":"metadata","mi":{},"kimi":"all","glm":{"links":[]},"zen":{"links":["中文"]}}}"#.as_bytes())?;
    assert_eq!(all.declared.len(), 4);
    assert_eq!(all.pending_config_policies(), Vec::<String>::new());
    for profile in Profile::ALL {
        assert_eq!(Profile::parse(profile.name())?, profile);
    }
    assert!(Profile::Mi.root(Path::new("relative")).is_err());
    for bad in [
        b"not-json".as_slice(),
        b"[]",
        br#"{"extra":1}"#,
        br#"{"claude":null}"#,
        br#"{"codex":null}"#,
        br#"{"claude":{"default":"all"}}"#,
        br#"{"claude":{"mi":"none"}}"#,
        br#"{"claude":{"mi":[]}}"#,
        br#"{"claude":{"_comment":{}}}"#,
        br#"{"claude":{"mi":{"links":null}}}"#,
        br#"{"claude":{"mi":{"links":["demo","demo"]}}}"#,
        br#"{"claude":{"mi":{"links":["../demo"]}}}"#,
        br#"{"claude":{"mi":{"plugins_off":["bad"]}}}"#,
        br#"{"claude":{"mi":{"pstack_allow":["a/b"]}}}"#,
        br#"{"codex":{"disable_agents_skills":["bad/name"]}}"#,
        br#"{"claude":{"mi":{},"mi":{}}}"#,
        br#"{"codex":{"disable_agents_skills":null}}"#,
    ] {
        assert_eq!(
            profiles(bad).err().ok_or("must reject")?.code,
            "invalid_skill_profiles"
        );
    }
    assert!(profiles(&vec![b' '; 1024 * 1024 + 1]).is_err());
    let requested=profiles(br#"{"claude":{"mi":{"plugins_off":[],"pstack_allow":[]}},"codex":{"disable_agents_skills":[]}}"#)?;
    assert_eq!(
        requested.pending_config_policies(),
        vec![
            "claude:mi:plugins_off",
            "claude:mi:pstack_allow",
            "codex:disable_agents_skills"
        ]
    );
    Ok(())
}

#[test]
fn link_selection_is_explicit_and_never_silently_ignores_revision_or_overlapping_arguments()
-> TestResult {
    assert_eq!(
        selection(Some("tdd"), &[], false, None, None)?,
        Selection::Explicit(vec!["tdd".into()])
    );
    assert_eq!(
        selection(None, &["a".into(), "b".into()], false, None, Some("CASE-1"))?,
        Selection::Explicit(vec!["a".into(), "b".into()])
    );
    assert_eq!(selection(None, &[], true, None, None)?, Selection::All);
    for (name, only, all) in [
        (None, vec![], false),
        (Some("a"), vec!["b".into()], false),
        (Some("a"), vec![], true),
        (None, vec!["b".into()], true),
    ] {
        assert!(selection(name, &only, all, None, None).is_err());
    }
    assert!(selection(Some("a"), &[], false, Some(&"a".repeat(40)), None).is_err());
    assert!(selection(Some("a"), &[], false, Some("bad"), None).is_err());
    assert!(selection(Some("a"), &[], false, None, Some("../invalid")).is_err());
    assert!(selection(None, &["a".into(), "a".into()], false, None, None).is_err());
    Ok(())
}

#[test]
fn source_groups_expand_explicitly_without_turning_ambiguous_or_overlapping_names_into_authority()
-> TestResult {
    let units = vec![
        LinkUnit {
            id: "unit:a".into(),
            name: "alpha".into(),
            aliases: BTreeSet::from(["skill:alpha".into()]),
        },
        LinkUnit {
            id: "unit:b".into(),
            name: "beta".into(),
            aliases: BTreeSet::new(),
        },
    ];
    let groups = vec![SourceGroup {
        id: "source:group".into(),
        aliases: BTreeSet::from(["group".into()]),
        members: BTreeSet::from(["unit:a".into(), "unit:b".into()]),
    }];
    assert_eq!(
        resolve(&Selection::Explicit(vec!["alpha".into()]), &units, &groups)?,
        BTreeSet::from(["unit:a".into()])
    );
    assert_eq!(
        resolve(&Selection::Explicit(vec!["group".into()]), &units, &groups)?,
        BTreeSet::from(["unit:a".into(), "unit:b".into()])
    );
    assert_eq!(
        resolve(&Selection::All, &units, &groups)?,
        BTreeSet::from(["unit:a".into(), "unit:b".into()])
    );
    assert!(
        resolve(
            &Selection::Explicit(vec!["group".into(), "skill:alpha".into()]),
            &units,
            &groups
        )
        .is_err()
    );
    assert!(
        resolve(
            &Selection::Explicit(vec!["missing".into()]),
            &units,
            &groups
        )
        .is_err()
    );
    let mut ambiguous = groups.clone();
    ambiguous[0].aliases.insert("alpha".into());
    assert!(
        resolve(
            &Selection::Explicit(vec!["alpha".into()]),
            &units,
            &ambiguous
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn change_kinds_keep_existing_wire_values_and_never_convert_links_into_metadata() -> TestResult {
    use crate::capability::rules::invocation::MetadataKind;
    for old in [MetadataKind::Frontmatter, MetadataKind::CodexPolicy] {
        let bytes = serde_json::to_vec(&old)?;
        let next = serde_json::from_slice::<ChangeKind>(&bytes)?;
        assert_eq!(next, ChangeKind::from(old));
        assert_eq!(next.metadata()?, old);
        assert_eq!(serde_json::to_vec(&next)?, bytes);
    }
    assert_eq!(
        serde_json::to_string(&ChangeKind::ManagedLink)?,
        "\"managed-link\""
    );
    assert!(ChangeKind::ManagedLink.metadata().is_err());
    assert!(serde_json::from_slice::<ChangeKind>(b"\"codex_policy\"").is_err());
    Ok(())
}

#[test]
fn native_release_copies_are_preserved_separately_from_standalone_github_sources() -> TestResult {
    let facts=asset_facts(br#"{"schema_version":1,"assets":{"docs":{"kind":"github-skill","repository":"owner/docs"},"devnorm":{"kind":"local-release-plugin","copy_skills":["devnorm","devnorm-tdd"],"claude_profiles":["default","mi"]},"local":{"kind":"local-plugin"}}}"#)?;
    assert_eq!(facts.standalone, BTreeSet::from(["docs".into()]));
    assert_eq!(
        facts.copies[&Profile::Mi],
        BTreeSet::from(["devnorm".into(), "devnorm-tdd".into()])
    );
    assert!(!facts.copies.contains_key(&Profile::Zen));
    let fallback=asset_facts(br#"{"schema_version":1,"assets":{"root":{"kind":"local-release-plugin","name":"renamed","claude_profiles":["zen"]},"fallback":{"kind":"local-release-plugin","claude_profiles":["mi"]}}}"#)?;
    assert!(fallback.copies[&Profile::Zen].contains("renamed"));
    assert!(fallback.copies[&Profile::Mi].contains("fallback"));
    for bad in [b"[]".as_slice(),br#"{"schema_version":2,"assets":{}}"#,br#"{"schema_version":1,"assets":[]}"#,br#"{"schema_version":1,"assets":{"x":[]}}"#,br#"{"schema_version":1,"assets":{"x":{"kind":"local-release-plugin","copy_skills":null}}}"#,br#"{"schema_version":1,"assets":{"x":{"kind":"local-release-plugin","claude_profiles":["foreign"]}}}"#] {assert!(asset_facts(bad).is_err());}
    Ok(())
}

#[test]
fn selector_index_rejects_duplicate_identity_incomplete_groups_and_resource_budgets() {
    let unit = LinkUnit {
        id: "id:a".into(),
        name: "alpha".into(),
        aliases: BTreeSet::new(),
    };
    let empty = SourceGroup {
        id: "source:empty".into(),
        aliases: BTreeSet::new(),
        members: BTreeSet::new(),
    };
    assert!(
        resolve(
            &Selection::Explicit(vec!["source:empty".into()]),
            std::slice::from_ref(&unit),
            std::slice::from_ref(&empty)
        )
        .is_err()
    );
    for (units, groups) in [
        (vec![unit.clone(), unit.clone()], vec![]),
        (
            vec![LinkUnit {
                id: String::new(),
                ..unit.clone()
            }],
            vec![],
        ),
        (
            vec![LinkUnit {
                name: "../bad".into(),
                ..unit.clone()
            }],
            vec![],
        ),
        (
            vec![LinkUnit {
                aliases: BTreeSet::from(["bad\nkey".into()]),
                ..unit.clone()
            }],
            vec![],
        ),
        (
            vec![unit.clone()],
            vec![SourceGroup {
                members: BTreeSet::from(["missing".into()]),
                ..empty.clone()
            }],
        ),
        (vec![unit.clone()], vec![empty.clone(), empty.clone()]),
        (
            vec![unit.clone()],
            vec![SourceGroup {
                aliases: BTreeSet::from(["bad\nkey".into()]),
                ..empty.clone()
            }],
        ),
        (vec![unit.clone(); 5001], vec![]),
        (vec![unit.clone()], vec![empty; 5001]),
        (
            vec![LinkUnit {
                aliases: (0..20001).map(|n| format!("key-{n}")).collect(),
                ..unit
            }],
            vec![],
        ),
    ] {
        assert!(resolve(&Selection::All, &units, &groups).is_err());
    }
    for keys in [
        vec![String::new()],
        vec!["a\nb".into()],
        vec!["a".repeat(513)],
        vec!["x".into(); 5001],
    ] {
        assert!(selection(None, &keys, false, None, None).is_err());
    }
}
#[test]
fn target_authorization_excludes_default_cursor_other_users_and_protected_names() -> TestResult {
    for path in [
        "relative",
        "/other/.claude-mi/skills/demo",
        "/h/.claude/skills/demo",
        "/h/.cursor/skills/demo",
        "/h/.claude-foreign/skills/demo",
        "/h/.claude-mi/plugins/demo",
        "/h/.claude-mi/skills/devnorm",
    ] {
        assert!(link_target_parts(Path::new("/h"), Path::new(path)).is_err());
    }
    assert!(link_target(Path::new("/h"), Profile::Mi, "bad/name").is_err());
    assert!(link_target(Path::new("relative"), Profile::Mi, "demo").is_err());
    let path = PathBuf::from(std::ffi::OsString::from_vec(
        b"/h/.claude-mi/skills/\xff".to_vec(),
    ));
    assert!(link_target_parts(Path::new("/h"), &path).is_err());
    assert_eq!(github_repository("owner/name.git")?, "owner/name.git");
    assert_eq!(
        github_repository("https://github.com/owner/name")?,
        "owner/name"
    );
    for bad in [
        "missing-slash",
        "owner/",
        "owner/repo/extra",
        "owner/bad?query",
    ] {
        assert!(github_repository(bad).is_err());
    }
    Ok(())
}
#[test]
fn asset_source_and_profile_names_are_validated_before_copy_exclusions_are_used() -> TestResult {
    for bad in [
        b"invalid".as_slice(),br#"{"schema_version":1,"assets":{"bad/name":{"kind":"local-plugin"}}}"#,
        br#"{"schema_version":1,"assets":{"x":{}}}"#,br#"{"schema_version":1,"assets":{"x":{"kind":"github-skill"}}}"#,
        br#"{"schema_version":1,"assets":{"x":{"kind":"github-skill","repository":"bad"}}}"#,
        br#"{"schema_version":1,"assets":{"x":{"kind":"local-release-plugin","name":false}}}"#,
        br#"{"schema_version":1,"assets":{"x":{"kind":"local-release-plugin","copy_skills":["bad/name"]}}}"#,
        br#"{"schema_version":1,"assets":{"x":{"kind":"local-release-plugin","claude_profiles":null}}}"#,
    ] {assert!(asset_facts(bad).is_err());}
    let many = serde_json::json!({"schema_version":1,"assets":(0..5001).map(|n|(format!("a{n}"),serde_json::json!({"kind":"local-plugin"}))).collect::<serde_json::Map<_,_>>()});
    assert!(asset_facts(&serde_json::to_vec(&many)?).is_err());
    let rules = serde_json::json!({"claude":{"mi":{"links":(0..5001).map(|n|format!("a{n}")).collect::<Vec<_>>()}}});
    assert!(profiles(&serde_json::to_vec(&rules)?).is_err());
    Ok(())
}
