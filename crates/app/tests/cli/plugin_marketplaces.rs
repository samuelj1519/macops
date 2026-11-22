use super::{Fixture, TestResult, assert_code, write_json};
use serde_json::json;
use std::fs;

#[test]
fn marketplace_listing_observes_local_catalogs_per_harness_profile() -> TestResult {
    let f = Fixture::new()?;
    let codex = f.home.join(".codex");
    let codex_catalog = codex.join(".tmp/marketplaces/fixture-codex/.agents/plugins");
    fs::create_dir_all(&codex_catalog)?;
    fs::write(
        codex.join("config.toml"),
        "[marketplaces.fixture-codex]\nsource_type = 'git'\nsource = 'https://github.com/vendor/codex-fixture.git'\n",
    )?;
    write_json(
        &codex_catalog.join("marketplace.json"),
        &json!({"name":"fixture-codex","plugins":[{"name":"one"},{"name":"two"}]}),
    )?;
    for profile in ["mi", "glm"] {
        let root = f.home.join(format!(".claude-{profile}/plugins"));
        let catalog = root.join("marketplaces/team");
        fs::create_dir_all(catalog.join(".claude-plugin"))?;
        write_json(
            &root.join("known_marketplaces.json"),
            &json!({"team":{"source":{"source":"github","repo":format!("vendor/{profile}")},"installLocation":catalog}}),
        )?;
        write_json(
            &catalog.join(".claude-plugin/marketplace.json"),
            &json!({"name":"team","plugins":[{"name":format!("{profile}-only") }]}),
        )?;
    }
    f.script("codex", "touch \"$MACOPS_TEST_HOME/codex-called\"; exit 88")?;
    f.script(
        "claude",
        "touch \"$MACOPS_TEST_HOME/claude-called\"; exit 88",
    )?;
    let (out, data) = f.run(&["plugins"])?;
    assert_code(&out, 0);
    let profiles = data["data"]["profiles"].as_array().ok_or("profiles")?;
    let markets = |id: &str| {
        profiles
            .iter()
            .find(|profile| profile["profile"] == id)
            .map(|profile| &profile["marketplaces"][0])
            .ok_or("profile missing")
    };
    assert_eq!(markets("codex:default")?["plugins"], json!(["one", "two"]));
    assert_eq!(
        markets("codex:default")?["repository"],
        "https://github.com/vendor/codex-fixture"
    );
    assert_eq!(markets("claude:mi")?["plugins"], json!(["mi-only"]));
    assert_eq!(markets("claude:glm")?["plugins"], json!(["glm-only"]));
    let (out, selected) = f.run(&["plugins", "claude:mi"])?;
    assert_code(&out, 0);
    assert_eq!(
        selected["data"]["profiles"]
            .as_array()
            .ok_or("profiles")?
            .len(),
        1
    );
    assert_eq!(selected["data"]["profiles"][0]["profile"], "claude:mi");
    assert!(!f.home.join("codex-called").exists());
    assert!(!f.home.join("claude-called").exists());
    assert!(!f.file("state").exists());
    super::chezmoi_native::run_browser_pty(&f, &["plugins"], "market-browser", &f.home, "76", "0")?;
    super::chezmoi_native::run_browser_pty(
        &f,
        &["plugins", "--json"],
        "market-json",
        &f.home,
        "76",
        "0",
    )?;
    assert!(!f.home.join("codex-called").exists());
    assert!(!f.home.join("claude-called").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn marketplace_listing_preserves_known_rows_and_rejects_conflicting_catalog_identity() -> TestResult
{
    let f = Fixture::new()?;
    let root = f.home.join(".claude/plugins");
    let catalog = root.join("marketplaces/team");
    fs::create_dir_all(catalog.join(".claude-plugin"))?;
    write_json(
        &root.join("known_marketplaces.json"),
        &json!({"team":{"source":{"source":"git","url":concat!("https://TOKEN", "@github.com/vendor/fixture")},"installLocation":catalog}}),
    )?;
    write_json(
        &catalog.join(".claude-plugin/marketplace.json"),
        &json!({"name":"other-team","plugins":[{"name":"wrong-plugin"}]}),
    )?;
    let (out, data) = f.run(&["plugins", "claude"])?;
    assert_code(&out, 3);
    let market = &data["data"]["profiles"][0]["marketplaces"][0];
    assert_eq!(market["name"], "team");
    assert_eq!(market["issue"], "catalog_unknown");
    assert!(market["repository"].is_null());
    assert!(market["plugins"].is_null());
    assert!(!data.to_string().contains("TOKEN"));
    assert!(!data.to_string().contains("wrong-plugin"));
    let (out, missing) = f.run(&["plugins", "claude:unknown"])?;
    assert_code(&out, 3);
    assert_eq!(missing["error"]["code"], "marketplace_profile_unknown");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn cursor_marketplace_query_requires_an_explicit_online_request() -> TestResult {
    let f = Fixture::new()?;
    f.script(
        "cursor-agent",
        r#"
        test "$HOME" = "$MACOPS_TEST_HOME"
        test "$*" = "plugin marketplace list --format json"
        printf 'called\n' >> "$MACOPS_TEST_HOME/query-calls"
        printf '%s' '{"marketplaces":[{"name":"team","scope":"user","gitUrl":"https://github.com/vendor/cursor.git","credential":"SYNTHETIC_SECRET"}]}'"#,
    )?;
    let (_, data) = f.run(&["plugins", "cursor"])?;
    assert_eq!(
        data["data"]["profiles"][0]["status"],
        "account_query_available"
    );
    let (_, data) = f.run(&["plugins", "cursor", "--online", "--dry-run"])?;
    assert_eq!(data["data"]["profiles"][0]["status"], "dry_run");
    assert!(!f.home.join("query-calls").exists());
    let (out, data) = f.run(&["plugins", "cursor", "--online"])?;
    assert_code(&out, 0);
    let market = &data["data"]["profiles"][0]["marketplaces"][0];
    assert_eq!(market["repository"], "https://github.com/vendor/cursor");
    assert_eq!(market["scope"], "user");
    assert!(market["plugins"].is_null());
    assert!(!data.to_string().contains("SYNTHETIC_SECRET"));
    assert_eq!(fs::read_to_string(f.home.join("query-calls"))?, "called\n");
    super::chezmoi_native::run_browser_pty(
        &f,
        &["plugins", "cursor"],
        "market-entry",
        &f.home,
        "76",
        "0",
    )?;
    assert_eq!(fs::read_to_string(f.home.join("query-calls"))?, "called\n");
    super::chezmoi_native::run_browser_pty(
        &f,
        &["plugins", "cursor", "--online"],
        "market-online",
        &f.home,
        "76",
        "0",
    )?;
    assert_eq!(
        fs::read_to_string(f.home.join("query-calls"))?,
        "called\ncalled\n"
    );
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn cursor_marketplace_query_reports_login_and_unrecognized_shapes_without_native_output()
-> TestResult {
    let f = Fixture::new()?;
    let (out, invalid_host) = f.run(&["plugins", "claude", "--online"])?;
    assert_code(&out, 1);
    assert_eq!(
        invalid_host["error"]["code"],
        "marketplace_online_unsupported"
    );
    f.script(
        "cursor-agent",
        "printf '%s' 'Authentication required: SYNTHETIC_SECRET' >&2; exit 1",
    )?;
    let (out, data) = f.run(&["plugins", "cursor", "--online"])?;
    assert_code(&out, 3);
    assert_eq!(
        data["data"]["profiles"][0]["reason"],
        "cursor_marketplace_login_required"
    );
    assert!(!data.to_string().contains("SYNTHETIC_SECRET"));
    f.script("cursor-agent", "printf '%s' '[]'")?;
    let (out, data) = f.run(&["plugins", "cursor", "--online"])?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["profiles"][0]["marketplaces"], json!([]));
    f.script(
        "cursor-agent",
        "printf '%s' '{\"unexpected\":\"SYNTHETIC_SECRET\"}'",
    )?;
    let (out, data) = f.run(&["plugins", "cursor", "--online"])?;
    assert_code(&out, 3);
    assert_eq!(
        data["data"]["profiles"][0]["reason"],
        "marketplace_metadata_unknown"
    );
    assert!(!data.to_string().contains("SYNTHETIC_SECRET"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn marketplace_listing_rejects_ambiguous_profiles_and_isolates_unrelated_roots() -> TestResult {
    let f = Fixture::new()?;
    for name in [
        ".claude",
        ".claude-default",
        ".claude-mi",
        ".claude-test\nFORGED",
    ] {
        fs::create_dir(f.home.join(name))?;
    }
    std::os::unix::fs::symlink(".kimi", f.home.join(".kimi"))?;
    let (out, data) = f.run(&["plugins", "claude:mi"])?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["profiles"][0]["profile"], "claude:mi");
    let (out, data) = f.run(&["plugins", "claude"])?;
    assert_code(&out, 3);
    assert_eq!(
        data["data"]["profiles"][0]["reason"],
        "marketplace_profile_ambiguous"
    );
    assert!(!data.to_string().contains("FORGED"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn marketplace_human_output_explains_native_routes_and_partial_results() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir_all(f.home.join(".claude-mi/plugins"))?;
    fs::write(
        f.home.join(".claude-mi/plugins/known_marketplaces.json"),
        "{malformed",
    )?;
    let codex = f.home.join(".codex");
    let catalog = codex.join(".tmp/marketplaces/fixture/.agents/plugins");
    fs::create_dir_all(&catalog)?;
    write_json(
        &catalog.join("marketplace.json"),
        &json!({"name":"fixture","plugins":[{"name":"fixture-member"}]}),
    )?;
    fs::write(
        codex.join("config.toml"),
        "[marketplaces.fixture]\nsource_type = 'git'\nsource = 'https://github.com/vendor/fixture.git'\n",
    )?;
    let base = f.command();
    let output = std::process::Command::new(base.get_program())
        .args(base.get_args().filter(|arg| *arg != "--json"))
        .args(["plugins"])
        .output()?;
    assert_code(&output, 3);
    let text = String::from_utf8(output.stdout)?;
    for line in [
        "codex:default  ·  /plugins",
        "fixture  ·  https://github.com/vendor/fixture",
        "fixture-member",
        "claude:mi  ·  /plugin",
        "市场状态未知",
        "查询账户市场  mo plugins cursor --online",
    ] {
        assert!(text.contains(line), "{text}");
    }
    super::chezmoi_native::run_browser_pty(
        &f,
        &["plugins", "claude:mi"],
        "market-partial",
        &f.home,
        "76",
        "3",
    )?;
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn codex_adoption_keeps_native_files_and_reuses_the_profile_resource_identity() -> TestResult {
    let f = Fixture::new()?;
    let root = f.home.join(".codex");
    let cache = root.join("plugins/cache/market/fixture/1.0.0");
    fs::create_dir_all(cache.join(".codex-plugin"))?;
    fs::create_dir(cache.join(".git"))?;
    fs::write(cache.join(".git/HEAD"), "ref: refs/heads/main\n")?;
    let manifest = cache.join(".codex-plugin/plugin.json");
    fs::write(&manifest, r#"{"name":"fixture","version":"1.0.0"}"#)?;
    let record = root.join("config.toml");
    let raw = b"model = 'example'\n[plugins.'fixture@market']\nenabled=false\n";
    fs::write(&record, raw)?;
    f.script("codex", "touch \"$MACOPS_TEST_HOME/codex-called\"; exit 19")?;
    let (out, found) = f.run(&["list", "--kind", "capabilities"])?;
    assert_code(&out, 0);
    let id = found["data"]["resources"][0]["id"]
        .as_str()
        .ok_or("inventory identity")?;
    let (out, result) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["phase"], "succeeded");
    assert_eq!(result["data"]["plan"]["package"]["id"], id);
    assert_eq!(
        result["data"]["plan"]["package"]["provider"]["capability"],
        "codex-plugin"
    );
    assert_eq!(fs::read(&record)?, raw);
    assert!(!f.home.join("codex-called").exists());
    fs::write(
        cache.join(".git/HEAD"),
        "native Git administrative change\n",
    )?;
    let (out, same) = f.run(&["list", "--kind", "capabilities"])?;
    assert_code(&out, 0);
    assert_eq!(same["data"]["resources"][0]["management"], "adopted");
    assert_eq!(
        same["data"]["resources"][0]["baseline"]["comparison"],
        "same"
    );
    fs::write(
        &manifest,
        r#"{"name":"fixture","version":"1.0.0","description":"modified content"}"#,
    )?;
    let (_, drift) = f.run(&["list", "--kind", "capabilities"])?;
    assert_eq!(
        drift["data"]["resources"][0]["baseline"]["comparison"],
        "different"
    );
    fs::create_dir(root.join("plugins/cache/market/fixture/2.0.0"))?;
    let (_, multiple) = f.run(&["list", "--kind", "capabilities"])?;
    assert_eq!(
        multiple["data"]["resources"][0]["baseline"]["comparison"],
        "unknown"
    );
    assert!(!f.home.join("codex-called").exists());
    Ok(())
}

#[test]
fn codex_adoption_refuses_an_ambiguous_or_unbound_cache_without_writing() -> TestResult {
    let f = Fixture::new()?;
    let codex = f.home.join(".codex");
    let cache = codex.join("plugins/cache/market/fixture");
    fs::create_dir_all(cache.join("1.0.0"))?;
    fs::create_dir(cache.join("2.0.0"))?;
    fs::write(
        codex.join("config.toml"),
        "[plugins.'fixture@market']\nenabled=true\n",
    )?;
    let (out, ambiguous) = f.run(&["adopt", "plugin:fixture@market", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(ambiguous["error"]["code"], "codex_plugin_cache_unproven");
    fs::remove_dir(cache.join("2.0.0"))?;
    let (out, missing) = f.run(&["adopt", "plugin:fixture@market", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(missing["error"]["code"], "codex_plugin_manifest_missing");
    let available = cache.join("1.0.0");
    fs::create_dir(available.join(".codex-plugin"))?;
    let manifest = available.join(".codex-plugin/plugin.json");
    for contents in [
        "malformed",
        r#"{"name":"another","version":"1.0.0"}"#,
        r#"{"name":"fixture","version":"2.0.0"}"#,
    ] {
        fs::write(&manifest, contents)?;
        let (out, invalid) = f.run(&["adopt", "plugin:fixture@market", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(invalid["error"]["code"], "codex_plugin_manifest_mismatch");
    }
    fs::write(&manifest, r#"{"name":"fixture","version":"1.0.0"}"#)?;
    for schema in ["1.0.0", "9.0.0"] {
        let portable = json!({"$schema":format!("https://agent-plugins.org/schemas/{schema}/plugin.schema.json"),
            "name":"other", "version":"1.0.0"});
        write_json(&available.join("plugin.json"), &portable)?;
        let (out, invalid) = f.run(&["adopt", "plugin:fixture@market", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(invalid["error"]["code"], "codex_plugin_manifest_mismatch");
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn codex_adoption_remains_present_when_the_profile_is_no_longer_selected() -> TestResult {
    let f = Fixture::new()?;
    let profile = f.home.join("alternate-codex");
    let payload = profile.join("plugins/cache/market/fixture/1.0.0/.codex-plugin");
    fs::create_dir_all(&payload)?;
    fs::write(
        payload.join("plugin.json"),
        r#"{"name":"fixture","version":"1.0.0"}"#,
    )?;
    fs::write(
        profile.join("config.toml"),
        "[plugins.'fixture@market']\nenabled=true\n",
    )?;
    let adopted = f
        .roots_command()
        .args([
            "--state-dir",
            f.file("state").to_str().ok_or("state path")?,
            "adopt",
            "plugin:fixture@market",
            "--yes",
        ])
        .env("CODEX_HOME", &profile)
        .output()?;
    assert_code(&adopted, 0);
    let output = f
        .roots_command()
        .args([
            "--state-dir",
            f.file("state").to_str().ok_or("state path")?,
            "list",
            "--kind",
            "capabilities",
        ])
        .output()?;
    assert_code(&output, 0);
    let inventory: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let row = &inventory["data"]["resources"][0];
    assert_eq!(row["management"], "adopted");
    assert_eq!(row["baseline"]["comparison"], "same");
    assert_eq!(row["status"], "present");
    assert_eq!(row["contents"][0]["status"], "present");
    assert_eq!(row["deployments"], json!([]));
    assert!(row["loaded"].is_null());
    Ok(())
}

#[test]
fn codex_adoption_deduplicates_profile_aliases_but_rejects_redirected_plugin_caches() -> TestResult
{
    let f = Fixture::new()?;
    let root = f.home.join(".codex");
    let plugins = root.join("plugins/cache/market");
    let manifest = plugins.join("fixture/1.0.0/.codex-plugin/plugin.json");
    fs::create_dir_all(manifest.parent().ok_or("manifest parent")?)?;
    fs::write(&manifest, r#"{"name":"fixture","version":"1.0.0"}"#)?;
    fs::write(
        root.join("config.toml"),
        "[plugins.'fixture@market']\nenabled=true\n[plugins.'other@market']\nenabled=false\n",
    )?;
    std::os::unix::fs::symlink(plugins.join("fixture"), plugins.join("other"))?;
    let (out, redirected) = f.run(&["adopt", "plugin:other@market", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(redirected["error"]["code"], "codex_plugin_cache_rebound");
    assert!(!f.file("state").exists());
    let (out, _) = f.run(&["adopt", "plugin:fixture@market", "--yes"])?;
    assert_code(&out, 0);
    let alias = f.home.join("alternate-codex");
    std::os::unix::fs::symlink(&root, &alias)?;
    let query = || {
        let mut command = f.roots_command();
        command
            .args(["--state-dir", f.file("state").to_str().ok_or("state path")?])
            .env("CODEX_HOME", &alias);
        Ok::<_, Box<dyn std::error::Error>>(command)
    };
    let out = query()?.args(["list", "--kind", "capabilities"]).output()?;
    assert_code(&out, 0);
    let result: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let alias_record = json!(alias.join("config.toml"));
    let row = result["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|row| row["name"] == "fixture@market" && row["origin"]["record"] == alias_record)
        .ok_or("alias resource")?;
    let id = row["id"].as_str().ok_or("alias id")?;
    let bytes = fs::read(f.file("state/receipts.json"))?;
    let out = query()?.args(["adopt", id, "--yes"]).output()?;
    assert_code(&out, 3);
    let result: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(result["error"]["code"], "owner_conflict");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, bytes);
    Ok(())
}

#[test]
fn unregistered_isolated_codex_profile_cannot_be_adopted_through_another_state_root() -> TestResult
{
    let f = Fixture::new()?;
    let profile = f.home.join("state-a/codex-profile.Isolated/home/.codex");
    let payload = profile.join("plugins/cache/market/item/1.0.0/.codex-plugin");
    fs::create_dir_all(&payload)?;
    fs::write(
        payload.join("plugin.json"),
        br#"{"name":"item","version":"1.0.0"}"#,
    )?;
    fs::write(
        profile.join("config.toml"),
        "[plugins.'item@market']\nenabled = true\n",
    )?;
    let query = |path: &std::path::Path| {
        let mut command = f.roots_command();
        command
            .env("CODEX_HOME", path)
            .arg("--state-dir")
            .arg(f.file("state-b"));
        command
    };
    let ordinary = f.home.join("ordinary-config");
    fs::create_dir_all(&ordinary)?;
    fs::copy(profile.join("config.toml"), ordinary.join("config.toml"))?;
    let out = query(&ordinary)
        .args(["list", "--kind", "capabilities"])
        .output()?;
    assert_code(&out, 0);
    let selected: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(selected["data"]["resources"][0]["name"], "item@market");
    let out = query(&profile)
        .args(["list", "--kind", "capabilities"])
        .output()?;
    assert_code(&out, 0);
    let hidden: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(hidden["data"]["resources"], json!([]));
    let out = query(&profile)
        .args(["adopt", "plugin:item@market", "--yes"])
        .output()?;
    assert_ne!(out.status.code(), Some(0));
    assert!(!f.file("state-b").join("receipts.json").exists());
    std::os::unix::fs::symlink(profile, f.home.join(".codex"))?;
    let (out, hidden) = f.run(&["list", "--kind", "capabilities"])?;
    assert_code(&out, 0);
    assert_eq!(hidden["data"]["resources"], json!([]));
    Ok(())
}
