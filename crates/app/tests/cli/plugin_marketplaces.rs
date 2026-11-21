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
    assert!(!f.file("state").exists());
    Ok(())
}
