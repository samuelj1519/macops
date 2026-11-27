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

#[test]
fn app_previews_accept_name_first_syntax_and_reject_project_writes() -> TestResult {
    let f = Fixture::new()?;
    let (out, first) = f.run(&["app", "demo", "install", "--version", "2.0.0", "--dry-run"])?;
    assert_code(&out, 0);
    let (out, second) = f.run(&["app", "install", "demo", "--version", "2.0.0", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(first, second);
    assert_eq!(first["data"]["plan"]["target"], "2.0.0");

    fs::write(f.file("version"), "1.0.0")?;
    let (out, unregistered) =
        f.run(&["app", "demo", "upgrade", "--version", "2.0.0", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(unregistered["error"]["code"], "adoption_required");

    let project = f.file("project");
    fs::create_dir(&project)?;
    let (out, denied) = f.run(&[
        "app",
        "upgrade",
        "demo",
        "--version",
        "2.0.0",
        "--project",
        project.to_str().ok_or("project")?,
    ])?;
    assert_code(&out, 1);
    assert_eq!(denied["error"]["code"], "project_scope_unsupported");
    assert_eq!(fs::read_to_string(f.file("version"))?, "1.0.0");
    assert!(!f.file("calls").exists() && !f.file("state").exists());
    Ok(())
}

#[test]
fn shared_skill_prefers_existing_supported_runner_and_keeps_scope_on_preview() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["widget"])?;
    let cache = super::public_skill_runner(&f)?;
    let entry = cache.join("skills/bin/cli.mjs");
    std::os::unix::fs::symlink(&entry, f.home.join("bin/skills"))?;
    let (out, preview) = f.run(&["skill", "adopt", "widget", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["skills"], json!(["widget"]));
    assert_eq!(
        preview["data"]["directory"],
        json!(f.home.join(".agents/skills"))
    );
    assert!(!f.file("state").exists() && !f.home.join("native-calls.jsonl").exists());
    Ok(())
}

#[test]
fn refresh_of_discovered_source_reports_the_required_next_step_without_writing() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["widget"])?;
    let (out, report) = f.run(&["skill", "refresh", "source:owner/repo", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(report["error"]["code"], "native_skills_recipe_required");
    let plain = super::plain(&f, &["skill", "refresh", "source:owner/repo", "--dry-run"])?;
    assert_code(&plain, 3);
    assert!(String::from_utf8(plain.stderr)?.contains("mo skill adopt owner/repo --dry-run"));
    assert!(!f.file("state").exists() && !f.file("cache").exists());
    Ok(())
}

#[test]
fn config_target_first_preview_matches_the_action_first_form() -> TestResult {
    let f = Fixture::new()?;
    let target = f.home.join(".fixture-config");
    fs::write(&target, b"unchanged")?;
    let path = target.to_str().ok_or("config target")?;
    let (out, target_first) = f.run(&["config", path, "apply", "--dry-run"])?;
    assert_code(&out, 0);
    let (out, action_first) = f.run(&["config", "apply", path, "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(target_first, action_first);
    assert_eq!(action_first["data"]["persistentWrites"], false);
    assert_eq!(fs::read(target)?, b"unchanged");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn agent_browser_refresh_preserves_known_version_when_another_tool_probe_fails() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir(f.home.join(".claude-mi"))?;
    f.script(
        "claude",
        r#"test "$*" = --version; printf 'claude 1.2.3\n'"#,
    )?;
    f.script("codex", "exit 19")?;
    super::chezmoi_native::run_browser_pty(&f, &["agent"], "agent-refresh", &f.home, "160", "3")?;
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn declared_skill_source_requires_matching_native_repository_before_sync() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha"])?;
    write_json(
        &f.file("settings/manifests/managed-agent-assets.json"),
        &json!({
            "schema_version":1,"assets":{"alpha":{
                "kind":"github-skill","repository":"owner/repo","revision":"a".repeat(40),"path":"skills/alpha"
            }}
        }),
    )?;
    write_json(
        &f.file("settings/manifests/skill-profiles.json"),
        &json!({
            "claude":{"mi":{"links":["alpha"]}}
        }),
    )?;
    fs::create_dir_all(f.home.join(".claude-mi/skills"))?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let mut native: Value = serde_json::from_slice(&fs::read(&lock)?)?;
    let args = ["skill", "sync", "--only", "source:alpha", "--dry-run"];
    let (out, plan) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(plan["data"]["planned_changes"], 1);
    assert!(plan.to_string().contains(".claude-mi/skills/alpha"));

    native["skills"]["alpha"]["source"] = json!("other/repo");
    native["skills"]["alpha"]["sourceUrl"] = json!("https://github.com/other/repo");
    write_json(&lock, &native)?;
    let (out, mismatch) = f.run(&args)?;
    assert_code(&out, 3);
    assert_eq!(mismatch["error"]["code"], "declared_link_source_mismatch");
    assert!(!f.file("state").exists());
    assert!(!f.home.join(".claude-mi/skills/alpha").exists());
    Ok(())
}

#[test]
fn filtered_skill_inventory_reports_unverifiable_declarations_without_treating_them_as_installed()
-> TestResult {
    let f = Fixture::new()?;
    let manifest = f.file("settings/manifests/managed-agent-assets.json");
    let mut declaration = json!({"schema_version":1,"assets":{
        "alpha":{"kind":"github-skill","repository":"owner/repo","revision":"a".repeat(40),"path":"skills/alpha"},
        "future":{"kind":"future-kind"}
    }});
    write_json(&manifest, &declaration)?;
    let (out, filtered) = f.run(&["skill", "--details", "--only", "alpha"])?;
    assert_code(&out, 3);
    let entries = filtered["data"]["declared"]
        .as_array()
        .ok_or("declared assets")?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["id"], "asset:alpha");
    let (out, all) = f.run(&["skill", "--details"])?;
    assert_code(&out, 3);
    assert!(
        all["data"]["declared"]
            .as_array()
            .ok_or("declared assets")?
            .iter()
            .any(|row| row["id"] == "asset:future"
                && row["kind"] == "unsupported"
                && row["status"] == "declared")
    );

    declaration["assets"]["future"] = json!({});
    write_json(&manifest, &declaration)?;
    let (out, invalid) = f.run(&["skill", "--details"])?;
    assert_code(&out, 1);
    assert_eq!(invalid["error"]["code"], "invalid_assets");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn resource_query_refuses_a_file_in_place_of_project_directory() -> TestResult {
    let f = Fixture::new()?;
    let project = f.file("not-a-project");
    fs::write(&project, "fixture")?;
    let (out, result) = f.run(&[
        "agent",
        "--project",
        project.to_str().ok_or("project fixture")?,
    ])?;
    assert_code(&out, 1);
    assert_eq!(result["error"]["code"], "invalid_project_root");
    assert_eq!(fs::read_to_string(project)?, "fixture");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn plugin_details_use_only_the_selected_user_deployment() -> TestResult {
    let f = Fixture::new()?;
    for (name, enabled) in [("mi", true), ("glm", false)] {
        let profile = f.home.join(format!(".claude-{name}"));
        claude_plugin(&profile, "fixture", "1.0.0")?;
        write_json(
            &profile.join("settings.json"),
            &json!({"enabledPlugins":{"widget@fixture":enabled}}),
        )?;
    }
    let other = f.home.join(".claude-glm/settings.json");
    let bytes = fs::read(&other)?;
    super::chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-plugin-scope",
        &f.home,
        "160",
        "0",
    )?;
    assert_eq!(fs::read(other)?, bytes);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn plugin_details_route_configured_codex_plugin_to_its_native_guard() -> TestResult {
    let f = Fixture::new()?;
    let profile = f.home.join(".codex");
    fs::create_dir_all(profile.join("plugins/cache/fixture/widget/1.0.0"))?;
    let config = profile.join("config.toml");
    let original = "[plugins.'widget@fixture']\nenabled=true\n";
    fs::write(&config, original)?;
    super::chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-codex-remove",
        &f.home,
        "160",
        "0",
    )?;
    assert_eq!(fs::read_to_string(config)?, original);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn plugin_details_check_only_the_selected_codex_fixed_recipe() -> TestResult {
    let f = Fixture::new()?;
    let profile = f.home.join(".codex");
    let config = profile.join("config.toml");
    let body = "[plugins.'widget@fixture']\nenabled=true\n";
    fs::create_dir_all(profile.join("plugins/cache/fixture/widget/1.0.0"))?;
    fs::write(&config, body)?;
    let settings = f.home.join(".config/macops");
    fs::create_dir_all(&settings)?;
    write_json(
        &settings.join("native-plugin-recipes.json"),
        &json!({
            "schema_version":1,
            "instances":{"codex":{"home":f.home,"config_root":profile,"cwd":f.home,"binary":f.home.join("bin/codex"),"binary_sha256":"739cda86e15d30a6e2b40cf8dcbbfb988aaf48b305b27065573bb8e9fd41d002","version":"0.169.0"}},
            "recipes":{"widget":{"revision":"1","instances":["codex"],"marketplace":"fixture","plugin":"widget","source":{"type":"local_directory","root":f.home.join("market"),"member":"plugins/widget"}}}
        }),
    )?;
    super::chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-codex-fixed-refusal",
        &f.home,
        "160",
        "0",
    )?;
    assert_eq!(fs::read_to_string(config)?, body);
    assert!(!f.file("state").exists());
    Ok(())
}
