use crate::common::{Fixture, TestResult, assert_code, write_json};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::symlink, path::Path, process::Output};

fn run(fixture: &Fixture, args: &[&str]) -> Result<(Output, Value), Box<dyn std::error::Error>> {
    let out = fixture
        .roots_command()
        .args(["skills", "plugins"])
        .args(args)
        .output()?;
    let envelope = serde_json::from_slice(&out.stdout)?;
    Ok((out, envelope))
}

#[test]
fn unbound_profiles_are_listed_independently_without_native_calls() -> TestResult {
    let fixture = Fixture::new()?;
    for name in ["claude", "codex", "cursor"] {
        fixture.script(
            name,
            r#"printf 'native call\n' >> "$MACOPS_TEST_HOME/calls""#,
        )?;
    }
    let (out, envelope) = run(&fixture, &["list"])?;
    assert_code(&out, 3);
    let rows = envelope["data"]["results"]
        .as_array()
        .ok_or("profile rows")?;
    let ids: Vec<_> = rows.iter().map(|row| &row["instance"]).collect();
    assert_eq!(
        ids,
        [
            "claude:default",
            "claude:mi",
            "claude:kimi",
            "claude:glm",
            "claude:zen",
            "codex",
            "cursor",
        ]
    );
    assert!(rows.iter().all(|row| row["status"] == "unknown"));
    assert!(!fixture.home.join("calls").exists());
    assert!(!fixture.home.join(".local/state/macops").exists());
    Ok(())
}

fn write_toml(path: &Path, config: &str) -> TestResult {
    fs::create_dir_all(path.parent().ok_or("config parent")?)?;
    fs::write(path, config)?;
    Ok(())
}

fn recipe_root(f: &Fixture) -> std::path::PathBuf {
    f.home.join(".config/macops/native-plugin-recipes.json")
}

fn instance(
    f: &Fixture,
    config: &Path,
    version: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    fs::create_dir_all(config)?;
    let binary = f.script(
        "fake-client",
        r#"printf 'native\n' >> "$MACOPS_TEST_HOME/calls""#,
    )?;
    let digest = format!("{:x}", Sha256::digest(fs::read(&binary)?));
    Ok(
        json!({"home":f.home,"config_root":config,"cwd":f.home,"binary":binary,
        "binary_sha256":digest,"version":version}),
    )
}

#[test]
fn claude_bindings_keep_five_distinct_roots_and_fail_closed_on_aliases() -> TestResult {
    let f = Fixture::new()?;
    let mut instances = serde_json::Map::new();
    for (id, suffix) in [
        ("claude:default", ".claude"),
        ("claude:mi", ".claude-mi"),
        ("claude:kimi", ".claude-kimi"),
        ("claude:glm", ".claude-glm"),
        ("claude:zen", ".claude-zen"),
    ] {
        let config = f.home.join(suffix);
        instances.insert(id.into(), instance(&f, &config, "2.1.325")?);
    }
    fs::create_dir_all(recipe_root(&f).parent().ok_or("recipe root")?)?;
    let common_store = f.home.join("shared");
    fs::create_dir(&common_store)?;
    instances.get_mut("claude:mi").ok_or("mi")?["plugin_root"] = json!(common_store);
    instances.get_mut("claude:glm").ok_or("glm")?["plugin_root"] = json!(common_store);
    let book = json!({"schema_version":1,"instances":instances,"recipes":{}});
    write_json(&recipe_root(&f), &book)?;
    let (out, result) = run(&f, &["list", "--instance", "claude:mi"])?;
    assert_code(&out, 0);
    let instance = &result["data"]["results"][0];
    assert_eq!(instance["instance"], "claude:mi");
    assert_eq!(instance["shared_with"], json!(["claude:glm"]));
    assert_eq!(instance["config_root"], json!(f.home.join(".claude-mi")));
    let alias = f.home.join("config-alias");
    symlink(f.home.join(".claude-mi"), &alias)?;
    let mut book = book;
    book["instances"]["claude:zen"]["config_root"] = json!(alias);
    write_json(&recipe_root(&f), &book)?;
    let (out, result) = run(&f, &["list", "--instance", "claude:zen"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["results"][0]["status"], "unknown");
    assert!(!f.home.join("calls").exists());
    Ok(())
}

#[test]
fn codex_disabled_and_cross_source_preview_do_not_start_native() -> TestResult {
    let f = Fixture::new()?;
    let config = f.home.join(".codex");
    let source = f.home.join("market");
    let cache = config.join("plugins/cache/deep/example/1.0.0");
    let binding = instance(&f, &config, "0.169.0")?;
    fs::create_dir_all(cache.join(".codex-plugin"))?;
    fs::create_dir_all(source.join("plugins/example/.codex-plugin"))?;
    fs::create_dir_all(source.join(".agents/plugins"))?;
    fs::create_dir_all(recipe_root(&f).parent().ok_or("recipe root")?)?;
    let payload = json!({"name":"example","version":"1.0.0"});
    write_json(&cache.join(".codex-plugin/plugin.json"), &payload)?;
    write_json(
        &source.join("plugins/example/.codex-plugin/plugin.json"),
        &payload,
    )?;
    write_json(
        &source.join(".agents/plugins/marketplace.json"),
        &json!({
            "name":"deep","plugins":[{"name":"example","source":{"source":"local","path":"./plugins/example"}}]
        }),
    )?;
    let book = json!({"schema_version":1,"instances":{"codex":binding},"recipes":{"example":{
        "revision":"r1","instances":["codex"],"marketplace":"deep","plugin":"example",
        "source":{"type":"local_directory","root":source,"member":"plugins/example"}
    }}});
    write_json(&recipe_root(&f), &book)?;
    let contents = format!(
        "# custom model and unrelated settings\nmodel='made-up'\n[unknown]\nsetting='private-fixture-value'\n[marketplaces.deep]\nsource_type='local'\nsource='{}'\n[plugins.'example@deep']\nenabled=false\n",
        source.display()
    );
    write_toml(&config.join("config.toml"), &contents)?;
    let (out, result) = run(&f, &["check", "example"])?;
    assert_code(&out, 0);
    let row = &result["data"]["results"][0];
    assert_eq!(row["registration"], "present");
    assert_eq!(row["configured_enabled"], false);
    assert_eq!(
        row["payload"]["source_sha256"],
        row["payload"]["installed_sha256"]
    );
    assert_eq!(row["loaded"], "unknown");
    assert!(
        !out.stdout
            .windows(b"private-fixture-value".len())
            .any(|w| w == b"private-fixture-value")
    );
    let (out, result) = run(&f, &["upgrade", "example", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(
        result["data"]["results"][0]["reason"],
        "codex_readd_would_enable"
    );
    let different = contents.replace(source.to_str().ok_or("source")?, "/outside/never-read");
    write_toml(&config.join("config.toml"), &different)?;
    let (out, result) = run(&f, &["upgrade", "example"])?;
    assert_code(&out, 3);
    assert_eq!(
        result["data"]["results"][0]["reason"],
        "marketplace_source_change_required"
    );
    assert!(!f.home.join("calls").exists());
    assert!(!f.home.join(".local/state/macops").exists());
    assert_eq!(fs::read_to_string(config.join("config.toml"))?, different);
    Ok(())
}

#[test]
fn claude_local_source_evidence_does_not_claim_effective_enablement_or_load() -> TestResult {
    let f = Fixture::new()?;
    let config = f.home.join(".claude-zen");
    let source = f.home.join("claude-market");
    let member = source.join("plugins/example");
    let binding = instance(&f, &config, "2.1.325")?;
    fs::create_dir_all(config.join("plugins"))?;
    fs::create_dir_all(source.join(".claude-plugin"))?;
    fs::create_dir_all(member.join(".claude-plugin"))?;
    fs::create_dir_all(recipe_root(&f).parent().ok_or("recipe root")?)?;
    write_json(
        &source.join(".claude-plugin/marketplace.json"),
        &json!({"name":"fixture","plugins":[{"name":"example","source":"./plugins/example"}]}),
    )?;
    write_json(
        &member.join(".claude-plugin/plugin.json"),
        &json!({"name":"example","version":"1.0.0"}),
    )?;
    write_json(
        &config.join("plugins/known_marketplaces.json"),
        &json!({"fixture":{"source":{"source":"directory","path":source},"installLocation":source}}),
    )?;
    write_json(
        &config.join("plugins/installed_plugins.json"),
        &json!({"version":2,"plugins":{"example@fixture":[{"scope":"user","installPath":member,"version":"1.0.0"}]}}),
    )?;
    write_json(
        &config.join("settings.json"),
        &json!({"model":"invented-zen-model","enabledPlugins":{"example@fixture":true}}),
    )?;
    write_json(
        &recipe_root(&f),
        &json!({"schema_version":1,"instances":{"claude:zen":binding},"recipes":{"example":{
            "revision":"r1","instances":["claude:zen"],"marketplace":"fixture","plugin":"example",
            "source":{"type":"local_directory","root":source,"member":"plugins/example"}
        }}}),
    )?;
    let (out, result) = run(&f, &["check", "example"])?;
    assert_code(&out, 3);
    let row = &result["data"]["results"][0];
    assert_eq!(row["registration"], "present");
    assert_eq!(row["configured_enabled"], true);
    assert_eq!(row["payload"]["status"], "local_source");
    assert_eq!(row["effective_enabled"], "unknown");
    assert_eq!(row["loaded"], "unknown");
    assert_eq!(row["reason"], "claude_inplace_query_unverified");
    symlink(config.join("settings.json"), member.join("linked.json"))?;
    let (out, result) = run(&f, &["check", "example"])?;
    assert_code(&out, 3);
    assert_eq!(
        result["data"]["results"][0]["reason"],
        "plugin_payload_symlink"
    );
    assert!(!f.home.join("calls").exists());
    Ok(())
}
