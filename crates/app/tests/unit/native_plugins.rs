use super::*;
use crate::native_plugins_test_fixture::{Check, Fixture, error};
use crate::test_support::TestResult;
use std::fs;

#[test]
fn registry_requires_bounded_unique_bindings_and_path_normalized_sources() -> TestResult {
    let fixture = Fixture::new()?;
    let book = fixture.book();
    let parse = |value| -> Check<Registry> { Ok(Registry::parse(&serde_json::to_vec(&value)?)?) };
    let original: Registry = parse(book.clone())?;
    assert_eq!(original.recipes["example"].native_id(), "example@market");

    for (pointer, replacement) in [
        ("/schema_version", json!(2)),
        ("/instances/codex/cwd", json!("relative")),
        ("/instances/codex/binary", json!("/")),
        (
            "/instances/codex/home",
            json!(format!("/{}", vec!["level"; 33].join("/"))),
        ),
        ("/instances/codex/binary_sha256", json!("g".repeat(64))),
        ("/instances/codex/binary_sha256", json!("a".repeat(63))),
        ("/instances/codex/version", json!("-v")),
        ("/recipes/example/marketplace", json!("market/name")),
        ("/recipes/example/plugin", json!("ü")),
        ("/recipes/example/plugin", json!("a".repeat(129))),
        ("/recipes/example/plugin", json!("")),
        ("/recipes/example/instances", json!([])),
        ("/recipes/example/instances", json!(["codex", "codex"])),
        ("/recipes/example/instances", json!(["claude:unregistered"])),
        ("/recipes/example/source/member", json!("../outside")),
        ("/recipes/example/source/member", json!("./ambiguous")),
        ("/recipes/example/source/member", json!("x//y")),
        ("/recipes/example/source/member", json!("x\\y")),
        ("/recipes/example/source/member", json!("x\ny")),
        ("/recipes/example/source/member", json!("a".repeat(513))),
        (
            "/recipes/example/source/member",
            json!(vec!["dir"; 17].join("/")),
        ),
        ("/recipes/example/source/member", json!("")),
    ] {
        let mut wrong = book.clone();
        *wrong.pointer_mut(pointer).ok_or("fixture JSON pointer")? = replacement;
        error(
            Registry::parse(&serde_json::to_vec(&wrong)?),
            "invalid_native_plugin_recipes",
        );
    }
    let mut wrong = book.clone();
    wrong["instances"]["codex"]["plugin_root"] = json!(fixture.config);
    error(
        Registry::parse(&serde_json::to_vec(&wrong)?),
        "invalid_native_plugin_recipes",
    );
    let mut wrong = book.clone();
    wrong["instances"]["mystery"] = fixture.instance_json();
    error(
        Registry::parse(&serde_json::to_vec(&wrong)?),
        "invalid_native_plugin_recipes",
    );
    let mut wrong = book;
    for n in 0..64 {
        wrong["recipes"][format!("extra-{n}")] = fixture.recipe_json();
    }
    error(
        Registry::parse(&serde_json::to_vec(&wrong)?),
        "invalid_native_plugin_recipes",
    );
    error(Registry::parse(b"[]"), "invalid_native_plugin_recipes");
    error(
        Registry::parse(br#"{"schema_version":1,"instances":{},"recipes":{},"recipes":{}}"#),
        "invalid_native_plugin_recipes",
    );
    Ok(())
}

#[test]
fn source_registry_keeps_git_and_accepted_release_provenance_distinct() -> TestResult {
    let fixture = Fixture::new()?;
    let mut book = fixture.book();
    book["recipes"]["example"]["source"] =
        json!({"type":"native_git","repo":"accepted-owner/project"});
    let registry = Registry::parse(&serde_json::to_vec(&book)?)?;
    assert!(
        matches!(&registry.recipes["example"].source, Source::NativeGit { repo } if repo == "accepted-owner/project")
    );
    for invalid in ["no-owner", "owner/repo/extra", "/repo", "owner/-leading"] {
        book["recipes"]["example"]["source"]["repo"] = json!(invalid);
        error(
            Registry::parse(&serde_json::to_vec(&book)?),
            "invalid_native_plugin_recipes",
        );
    }
    book["recipes"]["example"]["source"] = json!({
        "type":"accepted_release","root":fixture.source,"member":"plugins/example","artifact":"a".repeat(64),"candidate":"1".repeat(64)
    });
    let registry = Registry::parse(&serde_json::to_vec(&book)?)?;
    assert!(
        matches!(&registry.recipes["example"].source,Source::AcceptedRelease { root, member, .. }
        if *root==fixture.source && member==Path::new("plugins/example"))
    );
    book["recipes"]["example"]["source"]["candidate"] = json!("unsealed");
    error(
        Registry::parse(&serde_json::to_vec(&book)?),
        "invalid_native_plugin_recipes",
    );
    Ok(())
}

#[test]
fn each_binding_failure_retains_its_own_reason_and_does_not_run_the_client() -> TestResult {
    let fixture = Fixture::new()?;
    let ctx = fixture.context()?;
    let list = |id: &str| NativePlugins::List {
        instance: Some(id.into()),
    };
    let mut book = fixture.book();
    let declaration = &mut book["instances"];
    declaration["claude:mi"] = fixture.instance_json();
    declaration["claude:mi"]["version"] = json!("2.1.323");
    declaration["claude:mi"]["home"] = json!(fixture.root());
    declaration["claude:kimi"] = fixture.instance_json();
    declaration["claude:kimi"]["version"] = json!("2.1.100");
    declaration["claude:glm"] = fixture.instance_json();
    declaration["claude:glm"]["version"] = json!("2.1.325");
    declaration["claude:glm"]["cwd"] = json!(fixture.root().join("missing"));
    declaration["claude:zen"] = fixture.instance_json();
    declaration["claude:zen"]["version"] = json!("2.1.316");
    declaration["claude:zen"]["config_root"] = json!(fixture.root().join("absent"));
    declaration["codex"]["binary_sha256"] = json!("0".repeat(64));
    declaration["cursor"] = fixture.instance_json();
    fixture.write_book(&book)?;
    let rows = run(&ctx, &NativePlugins::List { instance: None }, false, false)?;
    for (row, (id, reason)) in rows["results"].as_array().ok_or("rows")?.iter().zip([
        ("claude:default", "instance_not_bound"),
        ("claude:mi", "native_plugin_home_changed"),
        ("claude:kimi", "native_plugin_file_contract_unsupported"),
        ("claude:glm", "native_plugin_cwd_missing"),
        ("claude:zen", "native_plugin_config_root_missing"),
        ("codex", "native_plugin_binary_changed"),
        ("cursor", "cursor_plugin_interactive_only"),
    ]) {
        assert_eq!(row["instance"], id);
        assert_eq!(row["reason"], reason);
    }
    error(
        run(&ctx, &list("made-up"), false, false),
        "unknown_native_plugin_instance",
    );
    error(
        run(
            &ctx,
            &NativePlugins::Check {
                recipe: "missing".into(),
            },
            false,
            false,
        ),
        "native_plugin_recipe_missing",
    );
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn multiple_recipe_instances_do_not_inherit_registration_from_shared_profile() -> TestResult {
    let fixture = Fixture::new()?;
    let ctx = fixture.context()?;
    let mut book = fixture.book();
    for id in ["claude:mi", "claude:zen", "claude:glm"] {
        book["instances"][id] = fixture.instance_json();
        book["instances"][id]["version"] = json!("2.1.325");
    }
    book["instances"]["claude:glm"]["version"] = json!("2.1.300");
    book["instances"]["claude:kimi"] = fixture.instance_json();
    book["instances"]["claude:kimi"]["version"] = json!("2.1.325");
    book["instances"]["claude:kimi"]["config_root"] = json!(fixture.root().join("disjoint"));
    book["recipes"]["example"]["instances"] = json!([
        "claude:mi",
        "claude:kimi",
        "claude:zen",
        "claude:glm",
        "claude:default"
    ]);
    fixture.write_book(&book)?;
    let value = run(
        &ctx,
        &NativePlugins::Upgrade {
            recipe: "example".into(),
        },
        true,
        false,
    )?;
    let rows = value["results"].as_array().ok_or("rows")?;
    assert_eq!(rows[0]["reason"], "shared_native_storage_unsupported");
    assert_eq!(rows[0]["registration"], "unknown");
    assert_eq!(rows[1]["reason"], "native_plugin_config_root_missing");
    assert_eq!(rows[2]["reason"], "shared_native_storage_unsupported");
    assert_eq!(rows[3]["reason"], "native_plugin_file_contract_unsupported");
    assert_eq!(rows[4]["reason"], "instance_not_bound");
    assert_eq!(value["persistentWrites"], false);

    book["instances"] = json!({"codex":fixture.instance_json()});
    book["recipes"]["example"]["instances"] = json!(["codex"]);
    fixture.write_book(&book)?;
    fixture.installed()?;
    let value = run(
        &ctx,
        &NativePlugins::Upgrade {
            recipe: "example".into(),
        },
        true,
        false,
    )?;
    assert_eq!(value["results"][0]["reason"], "native_contract_required");
    fs::write(fixture.codex_file(), b"invalid")?;
    let value = run(
        &ctx,
        &NativePlugins::Check {
            recipe: "example".into(),
        },
        false,
        false,
    )?;
    assert_eq!(
        value["results"][0]["reason"],
        "invalid_native_plugin_metadata"
    );
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn declared_profiles_keep_configured_enablement_and_installed_records_independent() -> TestResult {
    use crate::native_plugins_test_fixture::write_json;

    let fixture = Fixture::new()?;
    let ctx = fixture.context()?;
    let mut book = fixture.book();
    book["instances"] = json!({});
    book["recipes"]["example"]["instances"] = json!(["claude:mi", "claude:zen"]);
    book["recipes"]["example"]["source"] = json!({"type":"native_git","repo":"fixture/market"});
    let mut settings_before = vec![];
    for (profile, enabled) in [("claude:mi", false), ("claude:zen", true)] {
        let config_root = fixture.home.join(format!(".{}", profile.replace(':', "-")));
        let mut instance = fixture.instance_json();
        instance["config_root"] = json!(config_root);
        instance["version"] = json!("2.1.325");
        book["instances"][profile] = instance;

        let settings = config_root.join("settings.json");
        write_json(
            &settings,
            &json!({"model":format!("fixture-{profile}"),"enabledPlugins":{"example@market":enabled}}),
        )?;
        settings_before.push((settings.clone(), fs::read(&settings)?));
        let plugins = config_root.join("plugins");
        write_json(
            &plugins.join("known_marketplaces.json"),
            &json!({"market":{"source":{"source":"github","repo":"fixture/market"}}}),
        )?;
        let registrations = if enabled {
            json!({})
        } else {
            json!({"example@market":[{"scope":"user","installPath":fixture.source_member(),"version":"1.2.3"}]})
        };
        write_json(
            &plugins.join("installed_plugins.json"),
            &json!({"version":2,"plugins":registrations}),
        )?;
    }
    fixture.write_book(&book)?;

    let result = run(
        &ctx,
        &NativePlugins::Check {
            recipe: "example".into(),
        },
        false,
        false,
    )?;
    let rows = result["results"].as_array().ok_or("profile rows")?;
    assert_eq!(rows.len(), 2);
    let installed = &rows[0];
    assert_eq!(installed["instance"], "claude:mi");
    assert_eq!(installed["registration"], "present");
    assert_eq!(installed["configured_enabled"], false);
    assert_eq!(installed["version"], "1.2.3");
    assert_eq!(installed["reason"], "native_git_payload_unproven");
    let absent = &rows[1];
    assert_eq!(absent["instance"], "claude:zen");
    assert_eq!(absent["registration"], "absent");
    assert_eq!(absent["configured_enabled"], true);
    assert!(absent["version"].is_null());
    assert_eq!(absent["reason"], "native_plugin_not_registered");
    for row in rows {
        assert_eq!(row["marketplace_registered"], true);
        assert_eq!(row["effective_enabled"], "unknown");
        assert_eq!(row["loaded"], "unknown");
    }
    for (path, before) in settings_before {
        assert_eq!(fs::read(path)?, before);
    }
    assert_eq!(result["persistentWrites"], false);
    assert!(!fixture.state.exists());
    Ok(())
}

#[test]
fn list_and_check_report_shared_plugin_storage_when_a_sibling_uses_it_as_its_config_root()
-> TestResult {
    let fixture = Fixture::new()?;
    let ctx = fixture.context()?;
    fixture.installed()?;
    let mut book = fixture.book();
    let mut claude = fixture.instance_json();
    claude["version"] = json!("2.1.325");
    claude["config_root"] = json!(fixture.config.join("plugins"));
    book["instances"]["claude:mi"] = claude;
    fixture.write_book(&book)?;

    for (id, other) in [("codex", "claude:mi"), ("claude:mi", "codex")] {
        let result = run(
            &ctx,
            &NativePlugins::List {
                instance: Some(id.into()),
            },
            false,
            false,
        )?;
        assert_eq!(result["results"][0]["instance"], id);
        assert_eq!(result["results"][0]["status"], "bound");
        assert_eq!(result["results"][0]["shared_with"], json!([other]));
    }

    let check = NativePlugins::Check {
        recipe: "example".into(),
    };
    let result = run(&ctx, &check, false, false)?;
    let row = &result["results"][0];
    assert_eq!(row["registration"], "present");
    assert_eq!(row["reason"], "shared_native_storage_unsupported");
    assert_eq!(row["loaded"], "unknown");

    let independent = fixture.home.join(".claude-mi");
    fs::create_dir(&independent)?;
    book["instances"]["claude:mi"]["config_root"] = json!(independent);
    fixture.write_book(&book)?;
    let result = run(&ctx, &check, false, false)?;
    let row = &result["results"][0];
    assert_eq!(row["registration"], "present");
    assert_eq!(row["status"], "observed");
    assert_eq!(row["effective_enabled"], "unknown");
    assert_eq!(row["loaded"], "unknown");
    assert_eq!(result["persistentWrites"], false);
    assert!(!fixture.state.exists());
    Ok(())
}
