use crate::{
    common::{Fixture, TestResult, assert_code, write_json},
    native_skill_fixture,
};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn claude_plugin(config: &Path, market: &str, version: &str) -> TestResult {
    let payload = config
        .join("plugins/cache")
        .join(market)
        .join("widget")
        .join(version);
    let leaf = payload.join(".claude-plugin");
    fs::create_dir_all(&leaf)?;
    write_json(
        &leaf.join("plugin.json"),
        &json!({"name":"widget","version":version}),
    )?;
    let ledger = config.join("plugins/installed_plugins.json");
    let mut book: Value = if ledger.is_file() {
        serde_json::from_slice(&fs::read(&ledger)?)?
    } else {
        json!({"version":2,"plugins":{}})
    };
    book["plugins"][format!("widget@{market}")] = json!([
        {"scope":"user","version":version,"installPath":payload}
    ]);
    write_json(&ledger, &book)
}

fn rows(report: &Value) -> Option<&Vec<Value>> {
    report["data"]["results"].as_array()
}

#[test]
fn preview_resolves_exact_agent_plugin_and_separates_unsupported_and_missing_targets() -> TestResult
{
    let f = Fixture::new()?;
    let claude = f.home.join(".claude-mi");
    let codex = f.home.join(".codex");
    let cursor = f.home.join(".cursor");
    claude_plugin(&claude, "fixture", "1.0.0")?;
    write_json(
        &claude.join("settings.json"),
        &json!({"enabledPlugins":{"widget@fixture":true}}),
    )?;
    fs::create_dir(&codex)?;
    fs::create_dir(&cursor)?;
    fs::write(
        codex.join("config.toml"),
        "[plugins.'widget@fixture']\nenabled=false\n",
    )?;

    let (out, report) = f.run(&[
        "skill",
        "enable",
        "widget@fixture",
        "--agent",
        "all",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    let targets = rows(&report).ok_or("Agent rows")?;
    assert!(
        targets
            .iter()
            .any(|r| r["agent"] == "claude-mi" && r["status"] == "current")
    );
    assert!(
        targets
            .iter()
            .any(|r| r["agent"] == "codex" && r["status"] == "blocked")
    );
    assert!(
        targets
            .iter()
            .any(|r| r["agent"] == "cursor-agent" && r["status"] == "not-installed")
    );

    claude_plugin(&claude, "another", "2.0.0")?;
    let (out, result) = f.run(&[
        "skill",
        "uninstall",
        "widget",
        "--agent",
        "claude-mi",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    let row = &result["data"]["results"][0];
    assert_eq!(row["status"], "blocked");
    assert!(
        row["reason"]
            .as_str()
            .is_some_and(|s| s.contains("多份安装"))
    );
    let (out, result) = f.run(&[
        "skill",
        "enable",
        "widget@fixture",
        "--agent",
        "claude-mi",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["results"][0]["status"], "current");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn shared_ownership_and_invalid_batch_selectors_have_clear_preview_results() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    fs::create_dir(f.home.join(".cursor"))?;
    let (out, preview) = f.run(&[
        "skill",
        "enable",
        "demo",
        "--agent",
        "cursor-agent",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    let row = &preview["data"]["results"][0];
    assert_eq!(row["status"], "blocked");
    assert!(
        row["reason"]
            .as_str()
            .is_some_and(|s| s.contains("共享技能"))
    );

    for (args, code) in [
        (
            &[
                "skill", "upgrade", "demo", "--agent", "all", "--only", "demo",
            ][..],
            "skill_agent_selection",
        ),
        (
            &["skill", "upgrade", "demo", "--agent", "no-such-agent"][..],
            "skill_agent_not_unique",
        ),
    ] {
        let (out, result) = f.run(args)?;
        assert_code(&out, 3);
        assert_eq!(result["error"]["code"], code);
    }

    fs::remove_file(f.home.join(".agents/skills/demo/SKILL.md"))?;
    let (out, report) = f.run(&[
        "skill",
        "upgrade",
        "demo",
        "--agent",
        "cursor-agent",
        "--shared",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    let row = &report["data"]["results"][0];
    assert_eq!(row["status"], "blocked");
    assert!(
        row["reason"]
            .as_str()
            .is_some_and(|s| s.contains("无法确认"))
    );
    assert!(!f.file("state").exists());
    Ok(())
}
