#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use serde_json::json;
pub fn recipe() -> Recipe {
    Recipe {
        schema_version: 1,
        id: "alpha".into(),
        owner: "skills-cli".into(),
        cli_version: "1.9.0".into(),
        source: Source {
            repository: "owner/repo".into(),
            commit: "a".repeat(40),
            skill_directory: "skills/alpha".into(),
            member_name: "alpha".into(),
            tracking_ref: None,
        },
        install_name: "alpha".into(),
        scope: "global".into(),
        client_scope: vec!["codex".into()],
        copy: true,
        manual_only: true,
        license_files: vec![],
        derived: None,
        runner: Runner {
            node: "/usr/bin/node".into(),
            entry: "/fixture/skills/bin/cli.mjs".into(),
            sha256: Some("f".repeat(64)),
        },
    }
}
pub fn row() -> Value {
    json!({"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo.git","commit":"a".repeat(40),"ref":"a".repeat(40),"skillPath":"skills/alpha/SKILL.md","skillFolderHash":"b".repeat(40),"installMode":"copy","installedAt":"2026-11-20T00:00:00.000Z","updatedAt":"2026-11-20T00:00:00.000Z"})
}
#[test]
fn fixed_recipe_native_arguments_and_slot_identity_are_independent_of_source() -> TestResult {
    let mut r = recipe();
    validate(&r)?;
    assert_eq!(recipe_hash(&r)?.len(), 64);
    assert_eq!(skill_directory("skills/alpha/SKILL.md")?, "skills/alpha");
    assert_eq!(skill_directory("skills/alpha")?, "skills/alpha");
    assert_eq!(skill_directory("SKILL.md")?, ".");
    assert_eq!(skill_directory(".")?, ".");
    assert!(skill_directory("../escape").is_err());
    let args = add_argv(&r, None)?;
    assert_eq!(
        args[2],
        format!(
            "https://github.com/owner/repo/tree/{}/skills/alpha",
            "a".repeat(40)
        )
    );
    assert!(!args.iter().any(|s| s == "--rename"));
    r.install_name = "renamed-alpha".into();
    r.source.skill_directory = ".".into();
    let args = add_argv(&r, None)?;
    assert_eq!(&args[args.len() - 2..], ["--rename", "renamed-alpha"]);
    assert!(args[2].ends_with(&"a".repeat(40)));
    assert_eq!(
        add_argv(&r, Some(Path::new("/artifact/v2")))?[2],
        "/artifact/v2"
    );
    assert!(add_argv(&r, Some(Path::new("relative"))).is_err());
    assert_eq!(
        installation_id(Path::new("/home/.agents/skills/alpha"))?,
        installation_id(Path::new("/home/.agents/skills/alpha"))?
    );
    assert_ne!(
        installation_id(Path::new("/home/.agents/skills/alpha"))?,
        installation_id(Path::new("/other/.agents/skills/alpha"))?
    );
    assert!(installation_id(Path::new("relative")).is_err());
    assert!(installation_id(Path::new("/")).is_err());
    let mut expected = row();
    selected_after(&recipe(), &expected, None)?;
    expected["skillPath"] = json!("skills/alpha");
    selected_after(&recipe(), &expected, None)?;
    expected["sourceType"] = json!("local");
    expected["source"] = json!("/artifact/v2");
    expected["sourceUrl"] = expected["source"].clone();
    selected_after(&r, &expected, Some(Path::new("/artifact/v2")))
        .err()
        .ok_or("rename should need original name")?;
    expected["skillName"] = json!("alpha");
    selected_after(&r, &expected, Some(Path::new("/artifact/v2")))?;
    Ok(())
}
#[test]
fn recipe_and_book_reject_wrong_shapes_scope_version_and_collisions() -> TestResult {
    let good = serde_json::to_value(recipe())?;
    for (key, value) in [
        ("schemaVersion", json!(2)),
        ("owner", json!("other")),
        ("cliVersion", json!("latest")),
        ("scope", json!("project")),
        ("clientScope", json!(["claude-code"])),
        ("copy", json!(false)),
        ("id", json!("../bad")),
        ("installName", json!("Upper Name")),
        ("licenseFiles", json!(["LICENSE", "LICENSE", "LICENSE"])),
        ("runner", json!({"node":"relative","entry":"/entry"})),
        ("runner", json!({"node":"/node","entry":"relative"})),
    ] {
        let mut candidate = good.clone();
        candidate[key] = value;
        assert!(
            decode::<Recipe>(&serde_json::to_vec(&candidate)?)
                .and_then(|r| validate(&r))
                .is_err()
        );
    }
    assert!(decode::<Recipe>(br#"{"id":"a","id":"b"}"#).is_err());
    assert!(decode::<Recipe>(b"[]").is_err());
    assert_eq!(book(None)?.recipes.len(), 0);
    let bytes = serde_json::to_vec(&Book {
        schema_version: 1,
        recipes: vec![recipe()],
    })?;
    assert_eq!(book(Some(&bytes))?.recipes.len(), 1);
    for value in [
        json!({"schemaVersion":2,"recipes":[]}),
        json!({"schemaVersion":1,"recipes":[good.clone(),good.clone()]}),
        json!({"schemaVersion":1,"recipes":null}),
    ] {
        assert!(book(Some(&serde_json::to_vec(&value)?)).is_err());
    }
    Ok(())
}
#[test]
fn strict_native_schema_preserves_foreign_rows_and_rejects_corrupt_registry() -> TestResult {
    assert!(registry(None)?.is_empty());
    let good = json!({"version":3,"skills":{"alpha":row()},"dismissed":{"findSkillsPrompt":true},"lastSelectedAgents":["codex"]});
    assert_eq!(registry(Some(&serde_json::to_vec(&good)?))?["alpha"], row());
    for value in [
        json!({"version":4,"skills":{}}),
        json!({"version":3,"skills":[]}),
        json!({"version":3,"skills":null}),
        json!({"version":3,"skills":{},"future":true}),
        json!({"version":3,"skills":{},"dismissed":[]}),
        json!({"version":3,"skills":{},"dismissed":{"unknown":false}}),
        json!({"version":3,"skills":{},"lastSelectedAgents":null}),
        json!({"version":3,"skills":{},"lastSelectedAgents":["codex","codex"]}),
        json!({"version":3,"skills":{"Alpha":row()}}),
        json!({"version":3,"skills":{"alpha":[]}}),
    ] {
        assert!(registry(Some(&serde_json::to_vec(&value)?)).is_err());
    }
    assert!(registry(Some(br#"{"version":3,"skills":{"a":{},"a":{}}}"#)).is_err());
    for key in [
        "source",
        "sourceType",
        "sourceUrl",
        "skillFolderHash",
        "installedAt",
        "updatedAt",
    ] {
        let mut value = good.clone();
        value["skills"]["alpha"]
            .as_object_mut()
            .ok_or("row")?
            .remove(key);
        assert!(registry(Some(&serde_json::to_vec(&value)?)).is_err());
    }
    for (key, value) in [
        ("source", json!("")),
        ("sourceType", json!("")),
        ("ref", Value::Null),
        ("commit", json!("bad")),
        ("skillPath", json!("../bad")),
        ("future", json!("x")),
        ("installMode", json!("symlink")),
    ] {
        let mut doc = good.clone();
        doc["skills"]["alpha"][key] = value;
        assert!(registry(Some(&serde_json::to_vec(&doc)?)).is_err());
    }
    for (key, value) in [
        ("installMode", json!("other")),
        ("sourceType", json!("local")),
        ("source", json!("wrong/repo")),
        ("commit", json!("c".repeat(40))),
        ("skillPath", json!("other/SKILL.md")),
        ("sourceUrl", json!("https://other.invalid/repo")),
        ("skillFolderHash", json!("bad")),
    ] {
        let mut row = row();
        row[key] = value;
        assert!(selected_after(&recipe(), &row, None).is_err());
    }
    let mut local = row();
    local["sourceType"] = json!("local");
    local["source"] = json!("/artifact");
    local["sourceUrl"] = json!("/different");
    assert!(selected_after(&recipe(), &local, Some(Path::new("/artifact"))).is_err());
    Ok(())
}

#[test]
fn native_names_runner_versions_and_recipe_storage_reject_ambiguous_or_changed_contracts()
-> TestResult {
    for good in ["a", "a-b", "a_b", "a.b", "0"] {
        assert!(native_name(good));
    }
    for bad in [
        "",
        ".a",
        "-a",
        "a.",
        "a-",
        "A",
        "a/b",
        "a\nb",
        &"a".repeat(256),
    ] {
        assert!(!native_name(bad));
    }
    for version in ["v22.20.0", "v22.21.1", "v23.0.0", "v26.13.1\n"] {
        node_version(version)?;
    }
    for version in [
        "22.20.0",
        "v22.19.99",
        "v21.99.99",
        "v23.0",
        "vx.20.0",
        "v22.20.0.1",
    ] {
        assert!(node_version(version).is_err());
    }
    let r = recipe();
    let empty = book(None)?;
    let first = merge(empty, &r)?;
    assert_eq!(first.recipes.len(), 1);
    let mut upgrade = r.clone();
    upgrade.source.commit = "b".repeat(40);
    assert_eq!(merge(first.clone(), &upgrade)?.recipes, [upgrade]);
    let mut conflict = r.clone();
    conflict.id = "other".into();
    assert!(merge(first.clone(), &conflict).is_err());
    conflict = r.clone();
    conflict.install_name = "other".into();
    assert!(merge(first, &conflict).is_err());
    let mut r = r;
    r.runner.sha256 = None;
    assert!(
        book(Some(&serde_json::to_vec(&Book {
            schema_version: 1,
            recipes: vec![r]
        })?))
        .is_err()
    );
    let mut r = recipe();
    r.runner.sha256 = Some("bad".into());
    assert!(validate(&r).is_err());
    assert!(recipe_hash(&r).is_err());
    let mut r = recipe();
    r.source.member_name = "-flag".into();
    assert!(validate(&r).is_err());
    Ok(())
}
#[test]
fn fixed_license_and_writing_derivation_are_bound_to_one_explicit_source_and_installation()
-> TestResult {
    let mut r = recipe();
    r.license_files = vec!["LICENSE".into(), "ASSET_LICENSES.md".into()];
    validate(&r)?;
    let id = installation_id(Path::new("/home/.agents/skills/alpha"))?;
    let direct = locator(&r, &id)?;
    assert_eq!(direct.len(), 64);
    r.license_files = vec!["LICENSE".into(), "nested/LICENSE".into()];
    assert!(validate(&r).is_err());
    r.license_files = vec!["README.md".into()];
    assert!(validate(&r).is_err());
    r.license_files = vec![".".into()];
    assert!(validate(&r).is_err());
    r.license_files.clear();
    r.source.repository = "larashero3-dotcom/writing-dna-skill".into();
    r.source.skill_directory = ".".into();
    let rules = json!({"kind":"github-skill","repository":r.source.repository,"path":".","manual_only":true,
        "exclude":["skills/lieflat-less-ai-tone"],"edits":[{"path":"SKILL.md","old":"old prose","new":"new prose"}]});
    r.derived = Some(rules.clone());
    validate(&r)?;
    assert_ne!(locator(&r, &id)?, direct);
    for variant in 0..8 {
        let mut changed = r.clone();
        match variant {
            0 => changed.source.repository = "owner/repo".into(),
            1 => changed.source.skill_directory = "skills/other".into(),
            2 => changed.manual_only = false,
            3 => changed.derived.as_mut().ok_or("rules")?["repository"] = json!("owner/repo"),
            4 => changed.derived.as_mut().ok_or("rules")?["path"] = json!("other"),
            5 => changed.derived.as_mut().ok_or("rules")?["exclude"] = json!([]),
            6 => changed.derived.as_mut().ok_or("rules")?["edits"] = json!([]),
            _ => changed.derived.as_mut().ok_or("rules")?["manual_only"] = json!(false),
        }
        assert!(validate(&changed).is_err(), "variant {variant}");
    }
    r.derived.as_mut().ok_or("rules")?["edits"][0]["path"] = json!("README.md");
    assert!(validate(&r).is_err());
    r.derived = Some(rules);
    r.source.tracking_ref = Some("main".into());
    let tracking = locator(&r, &id)?;
    r.source.commit = "c".repeat(40);
    assert_eq!(locator(&r, &id)?, tracking);
    let scope = crate::test_support::journal()?.scope;
    assert_eq!(native_scope(&scope)?.len(), 64);
    Ok(())
}
#[test]
fn native_lock_rejects_missing_or_wrong_typed_evidence_and_preserves_valid_local_rows() -> TestResult
{
    let mut r = recipe();
    r.source.skill_directory = ".".into();
    let mut native = row();
    native["skillPath"] = json!("SKILL.md");
    selected_after(&r, &native, None)?;
    native["skillPath"] = json!(1);
    assert!(selected_after(&r, &native, None).is_err());
    native = row();
    native["sourceUrl"] = json!(null);
    assert!(selected_after(&recipe(), &native, None).is_err());
    native = row();
    native["skillFolderHash"] = json!(null);
    assert!(selected_after(&recipe(), &native, None).is_err());
    native = row();
    native["ref"] = json!("main");
    assert!(selected_after(&recipe(), &native, None).is_err());
    native = row();
    native["sourceType"] = json!("local");
    native["source"] = json!("/artifact");
    native["sourceUrl"] = native["source"].clone();
    selected_after(&recipe(), &native, Some(Path::new("/artifact")))?;
    native["skillFolderHash"] = json!("c".repeat(64));
    selected_after(&recipe(), &native, Some(Path::new("/artifact")))?;
    for key in ["commit", "skillPath"] {
        let mut native = row();
        native[key] = json!(null);
        assert!(registry_row(&native).is_err());
    }
    let mut historical = row();
    let fields = historical.as_object_mut().ok_or("native row")?;
    fields.remove("commit");
    fields.remove("skillPath");
    let raw = serde_json::to_vec(&json!({"version":3,"skills":{"alpha":historical}}))?;
    let observed = registry(Some(&raw))?;
    assert_eq!(observed["alpha"], historical);
    assert!(selected_after(&recipe(), &observed["alpha"], None).is_err());
    let mut native = row();
    native["source"] = json!("a\nb");
    assert!(registry_row(&native).is_err());
    let mut native = row();
    native["source"] = json!("a".repeat(4097));
    assert!(registry_row(&native).is_err());
    assert!(registry(Some(b"[]")).is_err());
    let mut many = serde_json::Map::new();
    for index in 0..5001 {
        many.insert(format!("m{index}"), row());
    }
    assert!(
        registry(Some(&serde_json::to_vec(
            &json!({"version":3,"skills":many})
        )?))
        .is_err()
    );
    Ok(())
}

#[test]
fn a_fixed_member_must_not_expand_to_native_wildcard_selection() {
    let mut recipe = recipe();
    recipe.source.member_name = "*".into();
    assert!(validate(&recipe).is_err());
    assert!(add_argv(&recipe, None).is_err());
}

#[test]
fn malformed_source_parts_and_derived_rules_fail_at_the_same_recipe_boundary_for_every_entry()
-> TestResult {
    for variant in 0..7 {
        let mut r = recipe();
        match variant {
            0 => r.source.repository = "../repo".into(),
            1 => r.source.commit = "bad".into(),
            2 => r.source.skill_directory = "../escape".into(),
            3 => r.source.member_name = ".".into(),
            4 => r.source.tracking_ref = Some("main~1".into()),
            5 => r.license_files = vec!["../LICENSE".into()],
            _ => r.owner = "another-owner".into(),
        }
        assert!(validate(&r).is_err());
        assert!(add_argv(&r, None).is_err());
        assert!(selected_after(&r, &row(), None).is_err());
        assert!(merge(book(None)?, &r).is_err());
        assert!(locator(&r, "capability:alpha").is_err());
    }
    let mut r = recipe();
    r.source.repository = "larashero3-dotcom/writing-dna-skill".into();
    r.source.skill_directory = ".".into();
    r.derived = Some(json!([]));
    assert!(validate(&r).is_err());
    let mut invalid = serde_json::to_value(recipe())?;
    invalid["owner"] = json!("another");
    assert!(
        book(Some(&serde_json::to_vec(
            &json!({"schemaVersion":1,"recipes":[invalid]})
        )?))
        .is_err()
    );
    let mut second = recipe();
    second.install_name = "other".into();
    assert!(
        book(Some(&serde_json::to_vec(&Book {
            schema_version: 1,
            recipes: vec![recipe(), second]
        })?))
        .is_err()
    );
    assert!(
        book(Some(&serde_json::to_vec(&Book {
            schema_version: 1,
            recipes: vec![recipe(); 5001]
        })?))
        .is_err()
    );
    Ok(())
}
#[test]
fn filesystem_bytes_that_cannot_be_json_paths_never_become_an_argv_locator_or_installation_identity()
-> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let non_utf8: PathBuf = std::ffi::OsString::from_vec(vec![b'/', 255]).into();
    assert!(installation_id(&non_utf8).is_err());
    assert!(installation_id(Path::new("/home/.git")).is_err());
    assert_eq!(
        installation_id(Path::new("/home/.agents/skills/alpha"))?,
        crate::resource::id(
            "capability",
            &(
                "skills-cli-install-v1",
                "global",
                Path::new("/home/.agents/skills"),
                "alpha"
            )
        )?
    );
    assert!(add_argv(&recipe(), Some(&non_utf8)).is_err());
    let mut r = recipe();
    r.runner.entry = non_utf8.clone();
    assert!(add_argv(&r, None).is_err());
    assert!(recipe_hash(&r).is_err());
    assert!(merge(book(None)?, &r).is_err());
    assert!(locator(&r, "id").is_err());
    let mut local = row();
    local["sourceType"] = json!("local");
    local["source"] = json!("/local");
    local["sourceUrl"] = json!("/local");
    assert!(selected_after(&recipe(), &local, Some(&non_utf8)).is_err());
    let mut raw = row();
    raw["skillPath"] = json!("../escape");
    assert!(selected_after(&recipe(), &raw, None).is_err());
    let mut before = Book {
        schema_version: 2,
        recipes: vec![],
    };
    assert!(merge(before.clone(), &recipe()).is_err());
    let mut existing = recipe();
    existing.id = "other".into();
    existing.install_name = "other".into();
    existing.runner.node = non_utf8;
    before.schema_version = 1;
    before.recipes.push(existing);
    assert!(merge(before, &recipe()).is_err());
    Ok(())
}
