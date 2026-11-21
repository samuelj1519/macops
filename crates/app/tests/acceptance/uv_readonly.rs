use super::*;
#[test]
fn uv_local_read_views_separate_same_name_roots_without_executing_any_tool() -> TestResult {
    let f = Fixture::new()?;
    let a = f.home.join("A/tools");
    let b = f.home.join("B/tools");
    let ab = f.home.join("A/bin");
    let bb = f.home.join("B/bin");
    uv_tool(&a, &ab, "demo-tool", "1.0")?;
    uv_tool(&b, &bb, "demo-tool", "2.0")?;
    for executable in ["uv", "python", "python3", "keyring"] {
        f.script(
            executable,
            r#"printf called >> "$MACOPS_TEST_HOME/native-called"; exit 99"#,
        )?;
    }
    write_json(
        &f.catalog,
        &json!({"schema_version":1,"packages":[
            {"id":"declared:a","name":"Demo Tool A","kind":"cli","official_url":"https://example.org/a","provider":{"type":"uv","package":"Demo_Tool","binding":{"tool_dir":a,"bin_dir":ab}}},
            {"id":"declared:b","name":"Demo Tool B","kind":"cli","official_url":"https://example.org/b","provider":{"type":"uv","package":"demo-tool","binding":{"tool_dir":b,"bin_dir":bb}}}
        ]}),
    )?;
    let (_, v) = f.run(&["list", "--kind", "applications"])?;
    let rows = v["data"]["resources"].as_array().ok_or("resources")?;
    let uv: Vec<_> = rows.iter().filter(|r| r["manager"] == "uv").collect();
    assert_eq!(uv.len(), 2, "{v}");
    assert_ne!(uv[0]["id"], uv[1]["id"]);
    assert!(uv.iter().all(|r| r["status"] == "present"));
    for row in uv {
        let id = row["id"].as_str().ok_or("id")?;
        let (_, v) = f.run(&["info", id])?;
        assert_eq!(v["data"]["resource"]["id"], id);
    }
    let (_, v) = f.run(&["doctor", "declared:a"])?;
    assert_eq!(v["data"][0]["observation"]["status"], "uv-tool");
    let (_, v) = f.run(&["discover", "uv"])?;
    assert!(v["data"]["packages"].as_array().is_some());
    assert!(!f.home.join("native-called").exists());
    assert!(!a.join(".lock").exists());
    assert!(!b.join(".lock").exists());
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    assert_eq!(
        fs::read_to_string(a.join("demo-tool/pyvenv.cfg"))?,
        "home = /synthetic-unexecuted-python\n"
    );
    Ok(())
}

#[test]
fn uv_all_five_data_leaf_canaries_are_not_opened_by_compiled_cli() -> TestResult {
    let f = Fixture::new()?;
    let root = f.home.join("tools");
    let bin = f.home.join("uv-bin");
    uv_tool(&root, &bin, "demo-tool", "1.0")?;
    let canary = f.file("synthetic-credential-canary");
    fs::write(&canary, "SYNTHETIC-CREDENTIAL")?;
    write_json(
        &f.catalog,
        &json!({"schema_version":1,"packages":[{"id":"uv:demo","name":"demo-tool","kind":"cli","official_url":"https://example.org/demo","provider":{"type":"uv","package":"demo-tool","binding":{"tool_dir":root,"bin_dir":bin}}}]}),
    )?;
    // Runtime acceptance supplies the dylib explicitly. Ordinary Cargo tests
    // still assert Unknown and zero state writes, without claiming access trace.
    let library = std::env::var_os("MACOPS_UV_PROBE_LIBRARY");
    for relative in [
        "demo-tool/uv-receipt.toml",
        "demo-tool/pyvenv.cfg",
        "demo-tool/lib/python3.12/site-packages/demo_tool-1.0.dist-info/METADATA",
        "demo-tool/bin/demo-tool",
        ".lock",
    ] {
        let path = root.join(relative);
        let original = fs::read(&path).ok();
        if path.exists() {
            fs::remove_file(&path)?;
        }
        symlink(&canary, &path)?;
        let log = f.file("open-canary.log");
        if log.exists() {
            fs::remove_file(&log)?;
        }
        let mut command = f.command();
        if let Some(library) = &library {
            command
                .env("DYLD_INSERT_LIBRARIES", library)
                .env("MACOPS_UV_CANARY", &canary)
                .env("MACOPS_UV_OPEN_LOG", &log);
        }
        let output = command.args(["doctor", "uv:demo"]).output()?;
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(
            value["data"][0]["observation"]["status"], "unknown",
            "{relative}: {value}"
        );
        if library.is_some() {
            assert_eq!(fs::read(&log)?, b"", "{relative} opened external canary");
        }
        assert!(!f.file("state").exists());
        assert!(!f.file("cache").exists());
        fs::remove_file(&path)?;
        if let Some(bytes) = original {
            fs::write(path, bytes)?;
        }
    }
    Ok(())
}

fn uv_declaration(id: &str, root: &std::path::Path, bin: &std::path::Path) -> serde_json::Value {
    json!({"id":id,"name":"demo-tool","kind":"cli","official_url":"https://example.org/demo","provider":{"type":"uv","package":"Demo_Tool","binding":{"tool_dir":root,"bin_dir":bin}}})
}

fn discovery_row(
    value: &serde_json::Value,
    id: &str,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    value["data"]["packages"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == id))
        .cloned()
        .ok_or_else(|| "discovery row missing".into())
}

fn assert_discovery_drift(
    f: &Fixture,
    id: &str,
    root: &std::path::Path,
    bin: &std::path::Path,
    other_root: &std::path::Path,
    other_bin: &std::path::Path,
    declared: bool,
) -> TestResult {
    let (_, changed) = f.run(&["discover", "uv"])?;
    let row = discovery_row(&changed, id)?;
    assert_eq!(row["status"], "unknown", "{declared}: {changed}");
    assert_eq!(row["management"], "adopted");
    assert_eq!(row["baseline"]["comparison"], "unknown");
    assert_eq!(row["releases"], json!([]));
    let reports = changed["data"]["coverage"].as_array().ok_or("coverage")?;
    assert!(
        reports
            .iter()
            .any(|r| r["roots"] == json!([root, bin]) && r["state"] == "invalid")
    );
    assert!(
        reports
            .iter()
            .any(|r| r["roots"] == json!([other_root, other_bin]) && r["state"] == "complete")
    );
    assert!(
        changed["data"]["packages"]
            .as_array()
            .ok_or("packages")?
            .iter()
            .any(|row| row["scope"] == other_root.display().to_string()
                && row["status"] == "present")
    );
    let (out, list) = f.run(&["list", "--kind", "applications"])?;
    assert_code(&out, 3);
    assert!(
        list["data"]["resources"]
            .as_array()
            .ok_or("resources")?
            .iter()
            .any(|row| row["id"] == id && row["status"] == "unknown")
    );
    let (out, doctor) = f.run(&["doctor", if declared { "owner:a" } else { id }])?;
    assert_code(&out, 3);
    assert_eq!(doctor["data"][0]["observation"]["status"], "unknown");
    let (out, _) = f.run(&["--dry-run", "adopt", id])?;
    assert_code(&out, 3);
    Ok(())
}

#[test]
fn uv_discovery_preserves_adopted_root_and_bin_identity_for_all_owners() -> TestResult {
    for declared in [false, true] {
        for change in [
            "root-replaced",
            "bin-replaced",
            "root-missing",
            "bin-missing",
            "tool-missing",
        ] {
            let f = Fixture::new()?;
            let root = f.home.join(".local/share/uv/tools");
            let bin = f.home.join(".local/bin");
            let other_root = f.home.join("B/tools");
            let other_bin = f.home.join("B/bin");
            uv_tool(&root, &bin, "demo-tool", "1.0")?;
            uv_tool(&other_root, &other_bin, "demo-tool", "2.0")?;
            let mut packages = vec![uv_declaration("owner:b", &other_root, &other_bin)];
            if declared {
                packages.push(uv_declaration("owner:a", &root, &bin));
            }
            write_json(&f.catalog, &json!({"schema_version":1,"packages":packages}))?;
            let (_, before) = f.run(&["discover", "uv"])?;
            let rows = before["data"]["packages"].as_array().ok_or("packages")?;
            let id = rows
                .iter()
                .find(|row| row["scope"] == root.display().to_string())
                .and_then(|row| row["id"].as_str())
                .ok_or("id")?;
            assert_eq!(discovery_row(&before, id)?["status"], "present");
            assert_eq!(discovery_row(&before, id)?["management"], "native");
            assert!(!f.file("state").exists());
            let (out, _) = f.run(&["adopt", id, "--yes"])?;
            assert_code(&out, 0);
            let receipts = fs::read(f.file("state/receipts.json"))?;
            let (_, stable) = f.run(&["discover", "uv"])?;
            let row = discovery_row(&stable, id)?;
            assert_eq!(row["status"], "present");
            assert_eq!(row["management"], "adopted");
            assert_eq!(row["baseline"]["comparison"], "same");
            let old = f.home.join("old-namespace");
            match change {
                "root-replaced" | "root-missing" => fs::rename(&root, &old)?,
                "bin-replaced" | "bin-missing" => fs::rename(&bin, &old)?,
                _ => fs::rename(root.join("demo-tool"), &old)?,
            }
            if change == "root-replaced" {
                fs::create_dir(&root)?;
                fs::rename(old.join("demo-tool"), root.join("demo-tool"))?;
            } else if change == "bin-replaced" {
                fs::create_dir(&bin)?;
                fs::rename(old.join("demo-tool"), bin.join("demo-tool"))?;
            }
            assert_discovery_drift(&f, id, &root, &bin, &other_root, &other_bin, declared)?;
            assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
            assert!(!f.file("cache").exists());
            assert!(!root.join(".lock").exists());
        }
    }
    Ok(())
}

#[test]
fn uv_observed_adopt_is_zero_authority_and_never_mutates_another_root() -> TestResult {
    let f = Fixture::new()?;
    let a = f.home.join(".local/share/uv/tools");
    let ab = f.home.join(".local/bin");
    let b = f.home.join("B/tools");
    let bb = f.home.join("B/bin");
    uv_tool(&a, &ab, "demo-tool", "1.0")?;
    uv_tool(&b, &bb, "demo-tool", "2.0")?;
    f.script(
        "uv",
        r#"printf invoked > "$MACOPS_TEST_HOME/uv-called"; exit 99"#,
    )?;
    write_json(
        &f.catalog,
        &json!({"schema_version":1,"packages":[uv_declaration("owner:b",&b,&bb)]}),
    )?;
    let (_, v) = f.run(&["list", "--kind", "applications"])?;
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "uv" && r["scope"] == a.display().to_string())
        .ok_or("A")?;
    let id = resource["id"].as_str().ok_or("id")?;
    let (out, v) = f.run(&["--dry-run", "adopt", id])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["plan"]["package"]["id"], id);
    assert_eq!(v["data"]["plan"]["command"], serde_json::Value::Null);
    assert_eq!(v["data"]["plan"]["target"], serde_json::Value::Null);
    assert!(!f.file("state").exists());
    let (out, _) = f.run(&["adopt", id])?;
    assert_code(&out, 3);
    assert!(!f.file("state").exists());
    let (out, v) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    let receipt = f.read_json("state/receipts.json")?;
    let record = &receipt["installations"][id];
    assert_eq!(record["package"]["provider"]["type"], "uv-observed");
    assert!(record["package"]["provider"].get("authority").is_none());
    assert_eq!(record["query"]["kind"], "uv-tool-local-v1");
    let (_, v) = f.run(&["info", id])?;
    assert_eq!(v["data"]["resource"]["baseline"]["comparison"], "same");
    for action in ["install", "upgrade", "uninstall", "update"] {
        let mut args = vec![action, id];
        if action == "install" || action == "upgrade" {
            args.extend(["--version", "9.0"]);
        }
        let (out, value) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(
            value["error"]["code"], "uv_write_blocked_review",
            "{action}: {value}"
        );
    }
    assert_eq!(f.read_json("state/receipts.json")?, receipt);
    assert_eq!(
        fs::read_to_string(
            b.join("demo-tool/lib/python3.12/site-packages/demo_tool-2.0.dist-info/METADATA")
        )?,
        "Name: demo-tool\nVersion: 2.0\n"
    );
    assert!(!f.home.join("uv-called").exists());
    assert!(!a.join(".lock").exists());
    assert!(!b.join(".lock").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}
#[test]
fn uv_unique_declared_owner_is_reused_and_both_entry_forms_block_collisions() -> TestResult {
    let f = Fixture::new()?;
    let root = f.home.join("tools");
    let bin = f.home.join("uv-bin");
    uv_tool(&root, &bin, "demo-tool", "1.0")?;
    let one = uv_declaration("owner:one", &root, &bin);
    write_json(&f.catalog, &json!({"schema_version":1,"packages":[one]}))?;
    let (_, v) = f.run(&["list", "--kind", "applications"])?;
    let id = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "uv")
        .and_then(|r| r["id"].as_str())
        .ok_or("id")?;
    for key in [id, "owner:one"] {
        let (out, v) = f.run(&["--dry-run", "adopt", key])?;
        assert_code(&out, 0);
        assert_eq!(v["data"]["plan"]["package"]["id"], "owner:one");
    }
    let (out, _) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let (out, value) = f.run(&["update", id])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "uv_write_blocked_review");
    write_json(
        &f.catalog,
        &json!({"schema_version":1,"packages":[uv_declaration("owner:two",&root,&bin)]}),
    )?;
    for key in [id, "owner:two"] {
        let (out, v) = f.run(&["--dry-run", "adopt", key])?;
        assert_code(&out, 3);
        assert_eq!(v["error"]["code"], "owner_conflict", "{v}");
    }
    let receipts = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipts["installations"]
            .as_object()
            .ok_or("installations")?
            .len(),
        1
    );
    assert!(receipts["installations"].get("owner:one").is_some());
    Ok(())
}

#[test]
fn uv_adopt_recovery_uses_original_operation_and_legacy_uv_recovery_cannot_query() -> TestResult {
    let f = Fixture::new()?;
    let root = f.home.join(".local/share/uv/tools");
    let bin = f.home.join(".local/bin");
    uv_tool(&root, &bin, "demo-tool", "1.0")?;
    f.script(
        "uv",
        r#"printf called > "$MACOPS_TEST_HOME/uv-called"; exit 99"#,
    )?;
    write_json(&f.catalog, &json!({"schema_version":1,"packages":[]}))?;
    let (_, v) = f.run(&["list", "--kind", "applications"])?;
    let id = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "uv")
        .and_then(|r| r["id"].as_str())
        .ok_or("id")?;
    let (out, v) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let operation = v["data"]["id"].as_str().ok_or("op")?;
    let before = fs::read(f.file("state/receipts.json"))?;
    let mut journal = v["data"].clone();
    journal["phase"] = json!("prepared");
    write_json(&f.file("state/active.json"), &journal)?;
    write_json(
        &f.file(&format!("state/operations/{operation}.json")),
        &journal,
    )?;
    for _ in 0..2 {
        let (out, value) = f.run(&["recover", operation])?;
        assert_code(&out, 0);
        assert_eq!(value["data"]["phase"], "succeeded");
        assert_eq!(value["data"]["id"], operation);
    }
    assert_eq!(fs::read(f.file("state/receipts.json"))?, before);
    assert!(!f.home.join("uv-called").exists());
    assert!(!root.join(".lock").exists());
    Ok(())
}
