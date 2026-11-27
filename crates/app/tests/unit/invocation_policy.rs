#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn invocation_transform_changes_only_the_requested_policy_and_preserves_body() -> TestResult {
    let skill =
        b"---\nname: example\ndescription: preserved\n---\nBody with code and whitespace.\n";
    assert_eq!(transform(MetadataKind::Frontmatter, Some(skill))?,
        b"---\nname: example\ndescription: preserved\ndisable-model-invocation: true\n---\nBody with code and whitespace.\n");
    assert_eq!(
        transform(MetadataKind::CodexPolicy, None)?,
        b"policy:\n  allow_implicit_invocation: false\n"
    );
    Ok(())
}

#[test]
fn metadata_updates_preserve_quotes_comments_crlf_and_nested_values_and_are_idempotent()
-> TestResult {
    for scalar in ["false", "no", "off", "0", "yes", "on", "1"] {
        let input = format!(
            "---\r\nname: example\r\n'disable-model-invocation': {scalar} # keep\r\n---\r\nbody\r\n"
        );
        let expected =
            "---\r\nname: example\r\n'disable-model-invocation': true # keep\r\n---\r\nbody\r\n";
        let output = transform(MetadataKind::Frontmatter, Some(input.as_bytes()))?;
        assert_eq!(output, expected.as_bytes());
        assert_eq!(transform(MetadataKind::Frontmatter, Some(&output))?, output);
    }
    let codex=b"interface: {display_name: Example}\npolicy: # block\n    allow_implicit_invocation: true # keep\n    other: preserved\ndependencies: {tools: []}\n";
    let expected=b"interface: {display_name: Example}\npolicy: # block\n    allow_implicit_invocation: false # keep\n    other: preserved\ndependencies: {tools: []}\n";
    assert_eq!(transform(MetadataKind::CodexPolicy, Some(codex))?, expected);
    assert_eq!(
        transform(MetadataKind::CodexPolicy, Some(expected))?,
        expected
    );
    let nested = b"policy:\n  nested:\n    allow_implicit_invocation: true\nother: kept\n";
    assert_eq!(transform(MetadataKind::CodexPolicy,Some(nested))?,b"policy:\n  nested:\n    allow_implicit_invocation: true\n  allow_implicit_invocation: false\nother: kept\n");
    assert_eq!(
        transform(MetadataKind::CodexPolicy, Some(b"# comment"))?,
        b"# comment\npolicy:\n  allow_implicit_invocation: false\n"
    );
    Ok(())
}

#[test]
fn unsupported_metadata_cannot_modify_unrelated_yaml_or_markdown() -> TestResult {
    assert!(transform(MetadataKind::Frontmatter, None).is_err());
    for input in [
        b"no header".as_slice(),
        b"---",
        b"---\nname: x\n",
        b"---\n[]\n---\n",
        b"\xff",
    ] {
        assert!(transform(MetadataKind::Frontmatter, Some(input)).is_err());
    }
    for body in [
        "disable-model-invocation: null\n",
        "disable-model-invocation: [false]\n",
        "disable-model-invocation: 'false'\n",
        "? disable-model-invocation\n: false\n",
    ] {
        let input = format!("---\n{body}---\nbody");
        assert!(transform(MetadataKind::Frontmatter, Some(input.as_bytes())).is_err());
    }
    for input in [
        "policy: null\n",
        "policy: {allow_implicit_invocation: true}\n",
        "policy:\n\tallow_implicit_invocation: true\n",
        "policy:\n  allow_implicit_invocation: 'false'\n",
        "? policy\n: {allow_implicit_invocation: true}\n",
    ] {
        assert!(transform(MetadataKind::CodexPolicy, Some(input.as_bytes())).is_err());
    }
    assert!(
        transform(
            MetadataKind::CodexPolicy,
            Some(&vec![b' '; 1024 * 1024 + 1])
        )
        .is_err()
    );
    assert_eq!(
        transform(
            MetadataKind::CodexPolicy,
            Some(b"policy: {allow_implicit_invocation: false}\n")
        )?,
        b"policy: {allow_implicit_invocation: false}\n"
    );
    Ok(())
}

#[test]
fn metadata_permissions_exclude_all_devnorm_components_and_keep_cursor_read_only() -> TestResult {
    let home = Path::new("/h");
    for (path, kind, access) in [
        (
            "/h/.agents/skills/foo/SKILL.md",
            MetadataKind::Frontmatter,
            MetadataAccess::Managed,
        ),
        (
            "/h/.agents/skills/foo/agents/openai.yaml",
            MetadataKind::CodexPolicy,
            MetadataAccess::Managed,
        ),
        (
            "/h/.cursor/skills/foo/SKILL.md",
            MetadataKind::Frontmatter,
            MetadataAccess::ReadOnly,
        ),
        (
            "/h/.claude-mi/plugins/cache/market/pstack/version/skills/foo/SKILL.md",
            MetadataKind::Frontmatter,
            MetadataAccess::Managed,
        ),
        (
            "/h/.codex/plugins/cache/market/pstack/version/skills/foo/agents/openai.yaml",
            MetadataKind::CodexPolicy,
            MetadataAccess::Managed,
        ),
        (
            "/h/.claude/plugins/cache/market/pstack/version/commands/review.MD",
            MetadataKind::Frontmatter,
            MetadataAccess::Managed,
        ),
        (
            "/h/.cursor/plugins/cache/market/pstack/version/skills/foo/SKILL.md",
            MetadataKind::Frontmatter,
            MetadataAccess::ReadOnly,
        ),
    ] {
        assert_eq!(metadata_access(home, Path::new(path), kind)?, access);
    }
    for path in [
        "relative",
        "/outside/SKILL.md",
        "/h/.agents/skills/DevNorm-extra/SKILL.md",
        "/h/.codex/plugins/cache/market/devnorm/version/skills/foo/SKILL.md",
        "/h/.agents/releases/foo/SKILL.md",
        "/h/.agents/skills/foo/arbitrary/SKILL.md",
        "/h/.agents/skills/foo/bad.md",
        "/h/.claude/plugins/cache/market/pstack/version/commands/review.txt",
        "/h/.agents/skills/../foo/SKILL.md",
    ] {
        assert!(metadata_access(home, Path::new(path), MetadataKind::Frontmatter).is_err());
    }
    Ok(())
}

fn row() -> Result<OverlayRow> {
    let original = b"---\nname: sample\n---\nbody\n";
    let applied = transform(MetadataKind::Frontmatter, Some(original))?;
    Ok(OverlayRow {
        kind: MetadataKind::Frontmatter,
        label: "sample".into(),
        owner: "owner".into(),
        original: Some(STANDARD.encode(original)),
        original_hash: Some(hash(original)),
        applied_hash: hash(&applied),
        original_mode: Some(0o644),
        applied_mode: 0o644,
    })
}
#[test]
fn overlay_original_bytes_are_validated_and_never_rebuilt_from_the_current_file() -> TestResult {
    let home = Path::new("/h");
    let row = row()?;
    let (original, applied) = row.bytes()?;
    assert!(original.is_some());
    assert_ne!(original.as_deref(), Some(applied.as_slice()));
    let mut book = OverlayRecord {
        schema_version: 1,
        profile_policy_hash: None,
        profiles: BTreeMap::new(),
        policy_hash: Some("a".repeat(64)),
        files: BTreeMap::from([("/h/.agents/skills/sample/SKILL.md".into(), row.clone())]),
    };
    assert_eq!(record(home, &serde_json::to_vec(&book)?)?, book);
    for case in 0..8 {
        let mut broken = row.clone();
        match case {
            0 => broken.original = Some("%%%".into()),
            1 => broken.original_hash = Some("b".repeat(64)),
            2 => broken.original_mode = None,
            3 => broken.original_mode = Some(0o10000),
            4 => broken.applied_mode = 0o10000,
            5 => broken.applied_hash = "b".repeat(64),
            6 => broken.original = None,
            _ => broken.original_hash = None,
        }
        assert!(broken.bytes().is_err());
    }
    let mut no_original = OverlayRow {
        kind: MetadataKind::CodexPolicy,
        label: "sample".into(),
        owner: "local".into(),
        original: None,
        original_hash: None,
        applied_hash: hash(b"policy:\n  allow_implicit_invocation: false\n"),
        original_mode: None,
        applied_mode: 0o600,
    };
    assert_eq!(no_original.bytes()?.0, None);
    no_original.applied_mode = 0o10000;
    assert!(no_original.bytes().is_err());
    book.schema_version = 2;
    assert_eq!(record(home, &serde_json::to_vec(&book)?)?, book);
    book.schema_version = 3;
    assert!(record(home, &serde_json::to_vec(&book)?).is_err());
    book.schema_version = 1;
    book.policy_hash = Some("bad".into());
    assert!(record(home, &serde_json::to_vec(&book)?).is_err());
    Ok(())
}

#[test]
fn invocation_declarations_are_explicit_and_do_not_expand_to_protected_or_duplicate_names()
-> TestResult {
    let valid=br#"{"schema_version":1,"standalone_manual_only":["sample"],"plugins":{"pstack":{"manual_only":["review"],"mirror_upstream_manual":true}},"mirror_standalone_upstream_manual":true,"notes":[]}"#;
    let parsed = declaration(valid)?;
    assert_eq!(parsed.standalone_manual_only, vec!["sample"]);
    for input in [br#"{"schema_version":2,"standalone_manual_only":[]}"#.as_slice(),
        br#"{"schema_version":1,"standalone_manual_only":["devnorm-helper"]}"#,
        br#"{"schema_version":1,"standalone_manual_only":["sample","sample"]}"#,
        br#"{"schema_version":1,"standalone_manual_only":[],"plugins":{"pstack":[true,[]]}}"#,
        br#"{"schema_version":1,"standalone_manual_only":[],"plugins":{"devnorm":{"manual_only":[]}}}"#] {
        assert!(declaration(input).is_err());
    }
    Ok(())
}

#[test]
fn policy_proposals_preserve_originals_reapply_only_known_states_and_never_resurrect_retired_files()
-> TestResult {
    let original = b"---\nname: sample\n---\nbody\n";
    let first = propose(
        MetadataKind::Frontmatter,
        "sample",
        "source",
        Some(original),
        Some(0o644),
        None,
    )?;
    assert!(first.changed);
    let row = first.row.as_ref().ok_or("row")?;
    let applied = first.after.as_deref().ok_or("after")?;
    let repeated = propose(
        MetadataKind::Frontmatter,
        "sample",
        "source",
        Some(applied),
        Some(0o644),
        Some(row),
    )?;
    assert!(!repeated.changed);
    assert_eq!(repeated.row.as_ref().ok_or("row")?.original, row.original);
    let reapplied = propose(
        MetadataKind::Frontmatter,
        "sample",
        "source",
        Some(original),
        Some(0o644),
        Some(row),
    )?;
    assert!(reapplied.changed);
    assert_eq!(reapplied.row.as_ref().ok_or("row")?.original, row.original);
    assert!(
        propose(
            MetadataKind::Frontmatter,
            "sample",
            "source",
            Some(b"externally edited"),
            Some(0o644),
            Some(row)
        )
        .is_err()
    );
    assert!(
        propose(
            MetadataKind::CodexPolicy,
            "sample",
            "source",
            Some(applied),
            Some(0o644),
            Some(row)
        )
        .is_err()
    );
    let removed = retire(row, Some(applied), Some(0o644))?;
    assert_eq!(removed.after.as_deref(), Some(original.as_slice()));
    assert!(removed.changed);
    let already = retire(row, Some(original), Some(0o644))?;
    assert!(!already.changed);
    let gone = retire(row, None, None)?;
    assert!(!gone.changed);
    assert_eq!(gone.after, None);
    assert!(retire(row, Some(b"external"), Some(0o644)).is_err());
    assert!(
        propose(
            MetadataKind::Frontmatter,
            "sample",
            "source",
            Some(original),
            None,
            None
        )
        .is_err()
    );
    let preexisting = propose(
        MetadataKind::Frontmatter,
        "sample",
        "source",
        Some(applied),
        Some(0o644),
        None,
    )?;
    assert!(!preexisting.changed);
    assert_eq!(preexisting.row, None);
    let created = propose(
        MetadataKind::CodexPolicy,
        "sample",
        "source",
        None,
        None,
        None,
    )?;
    assert!(created.changed);
    assert_eq!(created.after_mode, Some(0o600));
    Ok(())
}

#[test]
fn metadata_line_boundaries_and_manual_detection_reject_ambiguous_forms() -> TestResult {
    for malformed in ["", "---\n- sequence\n---\n", "---\na: [\n---\n"] {
        assert!(manual_frontmatter(malformed.as_bytes()).is_err());
    }
    assert!(manual_frontmatter(b"\xff").is_err());
    assert!(!manual_frontmatter(b"---\nname: example\n---\n")?);
    for (flag, expected) in [
        ("true", true),
        ("false", false),
        ("1", true),
        ("0", false),
        ("yes", true),
        ("no", false),
        ("on", true),
        ("off", false),
    ] {
        assert_eq!(
            manual_frontmatter(format!("---\ndisable-model-invocation: {flag}\n---\n").as_bytes())?,
            expected
        );
    }
    for flag in ["2", "[true]", "'not-boolean'"] {
        assert!(
            manual_frontmatter(format!("---\ndisable-model-invocation: {flag}\n---\n").as_bytes())
                .is_err()
        );
    }
    assert!(
        replace_flag(
            "disable-model-invocation: illegal\n",
            "disable-model-invocation",
            "",
            "true"
        )
        .is_err()
    );
    assert!(
        replace_flag(
            "disable-model-invocation: false extra\n",
            "disable-model-invocation",
            "",
            "true"
        )
        .is_err()
    );
    assert!(
        replace_flag(
            "# no indentation\n",
            "allow_implicit_invocation",
            "  ",
            "false"
        )?
        .is_none()
    );
    assert!(policy_indent(&["plain-root: value\n"]).is_err());
    assert!(policy_indent(&["\tchild: value\n"]).is_err());
    assert_eq!(policy_indent(&["# comment\n", "\n"])?, "  ");
    let codex = b"policy: # keep\n# unindented comment\n\n  other: value\n";
    assert_eq!(transform(MetadataKind::CodexPolicy,Some(codex))?,b"policy: # keep\n# unindented comment\n\n  other: value\n  allow_implicit_invocation: false\n");
    assert!(transform(MetadataKind::Frontmatter, Some(b"")).is_err());
    // Line-oriented edits must never change scalar content that only resembles a metadata key.
    let ambiguous=b"---\ndescription: \"line\ndisable-model-invocation: false\n\"\ndisable-model-invocation: false\n---\nbody\n";
    assert!(transform(MetadataKind::Frontmatter, Some(ambiguous)).is_err());
    let ambiguous_policy=b"description: \"line\npolicy:\n  allow_implicit_invocation: true\n\"\npolicy:\n  allow_implicit_invocation: true\n";
    assert!(transform(MetadataKind::CodexPolicy, Some(ambiguous_policy)).is_err());
    Ok(())
}

#[test]
fn declaration_and_overlay_boundaries_reject_corruption_without_printing_original_data()
-> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let home = Path::new("/h");
    let valid = row()?;
    let mut book = OverlayRecord {
        schema_version: 1,
        profile_policy_hash: None,
        profiles: BTreeMap::new(),
        policy_hash: None,
        files: BTreeMap::from([("/h/.agents/skills/sample/SKILL.md".into(), valid.clone())]),
    };
    for value in [
        "",
        "a--b",
        "Uppercase",
        "bad/name",
        "a-",
        "devnorm",
        &"a".repeat(129),
    ] {
        assert!(
            declaration(&serde_json::to_vec(
                &serde_json::json!({"schema_version":1,"standalone_manual_only":[value]})
            )?)
            .is_err()
        );
    }
    assert!(declaration(b"{").is_err());
    assert!(declaration(br#"{"schema_version":"SECRET","standalone_manual_only":[]}"#).is_err());
    assert!(declaration(&serde_json::to_vec(&serde_json::json!({"schema_version":1,"standalone_manual_only":[],"plugins":{"pstack":{"manual_only":["review","review"]}}}))?).is_err());
    assert!(record(home, b"{").is_err());
    assert!(record(home, br#"{"schema_version":1,"files":{"/h/path":[]}}"#).is_err());
    for case in 0..5 {
        let mut changed = valid.clone();
        match case {
            0 => changed.label = String::new(),
            1 => changed.owner = String::new(),
            2 => changed.label = "line\n".into(),
            3 => changed.owner = "x".repeat(257),
            _ => changed.owner = "tab\t".into(),
        }
        book.files
            .insert("/h/.agents/skills/sample/SKILL.md".into(), changed);
        assert!(record(home, &serde_json::to_vec(&book)?).is_err());
    }
    book.files = BTreeMap::from([("/outside/SKILL.md".into(), valid.clone())]);
    assert!(record(home, &serde_json::to_vec(&book)?).is_err());
    let mut broken = valid;
    broken.applied_hash = "bad".into();
    book.files = BTreeMap::from([("/h/.agents/skills/sample/SKILL.md".into(), broken.clone())]);
    assert!(record(home, &serde_json::to_vec(&book)?).is_err());
    assert!(
        propose(
            MetadataKind::Frontmatter,
            "sample",
            "source",
            Some(b"x"),
            Some(0o644),
            Some(&broken)
        )
        .is_err()
    );
    assert!(
        propose(
            MetadataKind::Frontmatter,
            "sample",
            "source",
            None,
            None,
            None
        )
        .is_err()
    );
    assert!(retire(&broken, Some(b"x"), None).is_err());
    assert!(
        propose(
            MetadataKind::CodexPolicy,
            "sample",
            "source",
            Some(b""),
            Some(0o10000),
            None
        )
        .is_err()
    );
    assert!(
        metadata_access(
            home,
            Path::new("/h/.agents/skills/bad\0/SKILL.md"),
            MetadataKind::Frontmatter
        )
        .is_err()
    );
    let bytes = std::ffi::OsString::from_vec(b"/h/.agents/skills/\xff/SKILL.md".to_vec());
    assert!(metadata_access(home, Path::new(&bytes), MetadataKind::Frontmatter).is_err());
    Ok(())
}

#[test]
fn quoted_keys_and_incomplete_lines_keep_the_scalar_edit_boundary_precise() -> TestResult {
    let double = b"---\n\"disable-model-invocation\": false\n---\nbody";
    assert_eq!(
        transform(MetadataKind::Frontmatter, Some(double))?,
        b"---\n\"disable-model-invocation\": true\n---\nbody"
    );
    assert!(key_prefix("\"unclosed: false", "disable-model-invocation", "").is_none());
    assert!(
        transform(
            MetadataKind::Frontmatter,
            Some(b"---\ndisable-model-invocation: !!bool false\n---\n")
        )
        .is_err()
    );
    assert!(
        transform(
            MetadataKind::CodexPolicy,
            Some(b"policy:\n  \"allow_implicit_invocat\\u0069on\": true\n")
        )
        .is_err()
    );
    assert_eq!(
        transform(MetadataKind::CodexPolicy, Some(b"policy:\n  other: kept"))?,
        b"policy:\n  other: kept\n  allow_implicit_invocation: false\n"
    );
    let mut broken = row()?;
    broken.original = Some(STANDARD.encode(b"not a header"));
    broken.original_hash = Some(hash(b"not a header"));
    assert!(broken.bytes().is_err());
    let quoted =
        b"policy:\n  description: \"line\n\tquoted content\"\n  allow_implicit_invocation: true\n";
    assert!(transform(MetadataKind::CodexPolicy, Some(quoted)).is_err());
    Ok(())
}

#[test]
fn source_document_end_and_quoted_policy_lookalikes_cannot_escape_semantic_validation() {
    for input in [
        "policy:\n  allow_implicit_invocation: !!bool true\n",
        "interface: {display_name: Example}\n...\n",
        "policy:\n  description: \"line\n  allow_implicit_invocation: true\n  end\"\n  allow_implicit_invocation: true\n",
        "policy:\n  description: \"line\n  allow_implicit_invocation: true\n  end\"\n  \"allow_implicit_invocat\\u0069on\": true\n",
    ] {
        assert!(transform(MetadataKind::CodexPolicy, Some(input.as_bytes())).is_err());
    }
    assert!(
        transform(
            MetadataKind::Frontmatter,
            Some(b"---\nname: example\n...\n---\nbody\n")
        )
        .is_err()
    );
}

#[test]
fn profile_overlay_hashes_and_nested_registry_are_validated_with_legacy_metadata() -> TestResult {
    let home = Path::new("/home");
    for hash in [serde_json::Value::Null, serde_json::json!("a".repeat(64))] {
        let value = serde_json::json!({"schema_version":2,"policy_hash":null,"files":{},"profiles":{},"profile_policy_hash":hash});
        assert_eq!(
            record(home, &serde_json::to_vec(&value)?)?.schema_version,
            2
        );
    }
    assert!(record(home,br#"{"schema_version":2,"policy_hash":null,"files":{},"profiles":{},"profile_policy_hash":"bad"}"#).is_err());
    assert!(record(home,br#"{"schema_version":2,"policy_hash":null,"files":{},"profiles":{"/wrong":{"target":"relative","parents":{},"fields":{"format":"claude","plugins":{},"pstack":null}}}}"#).is_err());
    Ok(())
}

#[test]
fn skill_name_requires_valid_frontmatter_and_a_string_name() -> TestResult {
    assert_eq!(
        skill_name(b"---\nname: fixture\ndescription: example\n---\nBody\n")?,
        "fixture"
    );
    for invalid in [
        &b"\xff"[..],
        b"body without a header",
        b"---\nname: [\n---\n",
        b"---\n- fixture\n---\n",
        b"---\ndescription: missing name\n---\n",
        b"---\nname: 42\n---\n",
    ] {
        assert!(skill_name(invalid).is_err());
    }
    Ok(())
}
