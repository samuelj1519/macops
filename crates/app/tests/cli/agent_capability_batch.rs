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
    assert_eq!(result["data"]["results"][0]["status"], "not-installed");
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

#[test]
fn short_plugin_name_does_not_operate_across_distinct_markets() -> TestResult {
    let f = Fixture::new()?;
    let mi = f.home.join(".claude-mi");
    let glm = f.home.join(".claude-glm");
    claude_plugin(&mi, "fixture-a", "1.0.0")?;
    claude_plugin(&glm, "fixture-b", "2.0.0")?;
    let command = ["skill", "uninstall", "widget", "--agent", "all"];
    for option in ["--dry-run", "--yes"] {
        let (out, report) = f.run(&[&command[..], &[option]].concat())?;
        assert_code(&out, 0);
        assert!(
            rows(&report)
                .ok_or("Agent rows")?
                .iter()
                .all(|row| row["status"] == "not-installed")
        );
    }
    let (out, report) = f.run(&[
        "skill",
        "uninstall",
        "widget@fixture-a",
        "--agent",
        "all",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    assert!(
        rows(&report)
            .ok_or("Agent rows")?
            .iter()
            .any(|row| row["agent"] == "claude-glm" && row["status"] == "not-installed")
    );
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn retry_recovers_the_same_native_skill_upgrade_without_restarting_the_runner() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    super::public_skill_runner(&f)?;
    let (out, adoption) = f.run(&["skill", "adopt", "owner/repo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        adoption["data"]["results"][0]["journal"]["phase"],
        "succeeded"
    );

    let wanted = "b".repeat(40);
    let request = [
        "skill",
        "upgrade",
        "owner/repo",
        "--revision",
        &wanted,
        "--yes",
    ];
    let (out, first) = f.run(&request)?;
    assert_code(&out, 0);
    let original = &first["data"]["results"][0]["journal"];
    assert_eq!(original["phase"], "succeeded");
    let mut pending = f.read_json("state/active.json")?;
    pending["phase"] = json!("unknown");
    write_json(&f.file("state/active.json"), &pending)?;

    let (out, recovered) = f.run(&request)?;
    assert_code(&out, 0);
    let journal = &recovered["data"]["journal"];
    assert_eq!(journal["id"], original["id"]);
    assert_eq!(journal["phase"], "succeeded");
    let log = f.home.join("native-calls.jsonl");
    assert_eq!(fs::read_to_string(log)?.lines().count(), 1);
    assert_eq!(f.read_json("state/active.json")?["phase"], "succeeded");
    Ok(())
}

#[test]
fn agent_batch_requires_a_target_and_rejects_repository_flags_for_plain_names() -> TestResult {
    let f = Fixture::new()?;
    let (out, error) = f.run(&["skill", "enable", "demo", "--agent", "all", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(error["error"]["code"], "skill_agents_missing");
    fs::create_dir(f.home.join(".claude-mi"))?;
    for (args, code) in [
        (
            &["skill", "enable", "widget@fixture"][..],
            "skill_agent_required",
        ),
        (
            &[
                "skill",
                "upgrade",
                "widget@fixture",
                "--switch-source",
                "--agent",
                "claude-mi",
                "--dry-run",
            ][..],
            "skill_agent_selection",
        ),
        (
            &[
                "skill",
                "upgrade",
                "widget@fixture",
                "--agent",
                "all",
                "--agent",
                "claude-mi",
                "--dry-run",
            ][..],
            "skill_agent_not_unique",
        ),
    ] {
        let (out, error) = f.run(args)?;
        assert_code(&out, 3);
        assert_eq!(error["error"]["code"], code);
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn shared_batch_updates_only_remaining_member_and_uninstall_preserves_the_other() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha", "beta"])?;
    super::public_skill_runner(&f)?;
    fs::create_dir(f.home.join(".codex"))?;
    let (out, adoption) = f.run(&["skill", "adopt", "owner/repo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        adoption["data"]["results"]
            .as_array()
            .ok_or("adopted members")?
            .len(),
        2
    );

    let wanted = "b".repeat(40);
    let (out, one) = f.run(&["skill", "upgrade", "alpha", "--revision", &wanted, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(one["data"]["results"][0]["journal"]["phase"], "succeeded");
    let shared = [
        "skill",
        "upgrade",
        "owner/repo",
        "--agent",
        "codex",
        "--shared",
        "--revision",
        &wanted,
        "--yes",
    ];
    for expected in ["changed", "current"] {
        let (out, report) = f.run(&shared)?;
        assert_code(&out, 0);
        assert_eq!(report["data"]["results"][0]["status"], expected);
    }
    let log = f.home.join("native-calls.jsonl");
    let payload = fs::read_to_string(&log)?;
    let records = payload
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        records
            .iter()
            .map(|r| r["name"].as_str())
            .collect::<Vec<_>>(),
        [Some("alpha"), Some("beta")]
    );

    let (out, removed) = f.run(&[
        "skill",
        "uninstall",
        "alpha",
        "--agent",
        "codex",
        "--shared",
        "--yes",
    ])?;
    assert_code(&out, 0);
    assert_eq!(removed["data"]["results"][0]["status"], "changed");
    assert!(!f.home.join(".agents/skills/alpha").exists());
    assert!(f.home.join(".agents/skills/beta/SKILL.md").is_file());
    assert_eq!(fs::read_to_string(log)?.lines().count(), 3);
    Ok(())
}

#[test]
fn agent_registration_checks_client_and_physical_directory_before_storing_binding() -> TestResult {
    let f = Fixture::new()?;
    let claude = f.home.join(".claude-mi");
    let codex = f.home.join(".codex");
    fs::create_dir(&claude)?;
    fs::create_dir(&codex)?;
    let alias = f.home.join("alias");
    std::os::unix::fs::symlink(&claude, &alias)?;
    for command in ["claude-mi", "studio", "codex"] {
        f.script(
            command,
            r#"if [ "${1-}" = "--version" ] && [ "$#" -eq 1 ]; then
printf 'fixture 1.0.0\n'
else
printf '%s\n' "$*" >> "$MACOPS_TEST_HOME/launches"
fi"#,
        )?;
    }

    for (name, location) in [
        ("all", claude.as_path()),
        ("studio", claude.as_path()),
        ("claude-mi", f.home.as_path()),
        ("claude-mi", alias.as_path()),
    ] {
        let (out, report) = f.run(&[
            "agent",
            "add",
            name,
            "--config",
            location.to_str().ok_or("fixture path")?,
            "--yes",
        ])?;
        assert_code(&out, 3);
        assert_eq!(report["error"]["code"], "agent_registration_invalid");
    }
    let (out, report) = f.run(&[
        "agent",
        "add",
        "codex",
        "--config",
        codex.to_str().ok_or("codex fixture")?,
        "--type",
        "claude",
        "--yes",
    ])?;
    assert_code(&out, 3);
    assert_eq!(report["error"]["code"], "agent_configuration_overlap");
    assert!(!f.file("state").exists());

    for (name, location) in [("claude-mi", &claude), ("codex", &codex)] {
        let (out, report) = f.run(&[
            "agent",
            "add",
            name,
            "--config",
            location.to_str().ok_or("fixture path")?,
            "--yes",
        ])?;
        assert_code(&out, 0);
        assert_eq!(report["data"]["status"], "registered");
    }
    let (_, listed) = f.run(&["agent"])?;
    let resources = listed["data"]["resources"]
        .as_array()
        .ok_or("Agent resources")?;
    assert!(
        resources
            .iter()
            .any(|r| r["name"] == "claude:mi" && r["config_dir"] == json!(claude))
    );
    assert!(resources.iter().any(|r| {
        r["name"] == "codex:default"
            && r["config_dir"] == json!(codex)
            && r["aliases"]
                .as_array()
                .is_some_and(|names| names.contains(&json!("codex")))
    }));
    assert!(!f.home.join("launches").exists());
    Ok(())
}

#[test]
fn shared_skill_name_does_not_also_select_an_unrelated_native_plugin() -> TestResult {
    let f = Fixture::new()?;
    let claude = f.home.join(".claude-mi");
    claude_plugin(&claude, "fixture", "1.0.0")?;
    let book = claude.join("plugins/installed_plugins.json");
    let before = fs::read(&book)?;
    fs::create_dir(f.home.join(".codex"))?;
    native_skill_fixture(&f, &["widget"])?;
    super::public_skill_runner(&f)?;
    let (out, report) = f.run(&[
        "skill",
        "uninstall",
        "widget",
        "--agent",
        "claude-mi",
        "--agent",
        "codex",
        "--shared",
        "--yes",
    ])?;
    assert_code(&out, 0);
    let rows = rows(&report).ok_or("batch rows")?;
    assert!(
        rows.iter()
            .any(|row| row["agent"] == "claude-mi" && row["status"] == "not-installed")
    );
    assert!(
        rows.iter()
            .any(|row| row["agent"] == "codex" && row["status"] == "changed")
    );
    assert_eq!(fs::read(book)?, before);
    assert!(!f.home.join(".agents/skills/widget").exists());
    Ok(())
}

#[test]
fn tagged_upgrade_first_records_current_shared_skill_and_preserves_requested_tag() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["widget"])?;
    super::public_skill_runner(&f)?;
    let source = "owner/repo@v2.0.0";
    let (out, incompatible) =
        f.run(&["skill", "upgrade", source, "--only", "widget", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(incompatible["error"]["code"], "source_revision_selection");
    let (out, preview) = f.run(&["skill", "upgrade", source, "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["skillAction"], "adopt");
    assert_eq!(
        preview["data"]["next"],
        format!("mo skill upgrade {source}")
    );
    assert!(!f.file("state").exists());
    let (out, adopted) = f.run(&["skill", "upgrade", source, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        adopted["data"]["results"][0]["journal"]["phase"],
        "succeeded"
    );
    assert_eq!(
        adopted["data"]["next"],
        format!("mo skill upgrade {source}")
    );
    assert!(!f.home.join("native-calls.jsonl").exists());
    let lock: Value = serde_json::from_slice(&fs::read(f.home.join(".agents/.skill-lock.json"))?)?;
    assert_eq!(lock["skills"]["widget"]["commit"], "a".repeat(40));
    Ok(())
}
