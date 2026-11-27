#[path = "cli/agent_capability_batch.rs"]
mod agent_capability_batch;
#[path = "cli/ai_native.rs"]
mod ai_native;
#[path = "cli/brew_native.rs"]
mod brew_native;
#[path = "cli/chezmoi_native.rs"]
mod chezmoi_native;
#[path = "cli/chezmoi_source.rs"]
mod chezmoi_source;
mod common;
#[path = "cli/migration.rs"]
mod migration;
#[path = "cli/native_plugins.rs"]
mod native_plugins;
#[path = "cli/native_routing.rs"]
mod native_routing;
#[path = "cli/plugin_marketplaces.rs"]
mod plugin_marketplaces;
use common::{Fixture, TestResult, assert_code, uv_tool, write_json};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
};

fn npm_manifest(prefix: &std::path::Path, name: &str, version: &str) -> TestResult {
    let module = prefix.join("lib/node_modules").join(name);
    fs::create_dir_all(&module)?;
    write_json(
        &module.join("package.json"),
        &json!({"name":name,"version":version}),
    )?;
    Ok(())
}

fn native_skill_fixture(f: &Fixture, names: &[&str]) -> TestResult {
    let mut skills = serde_json::Map::new();
    for name in names {
        let folder = f.home.join(".agents/skills").join(name);
        fs::create_dir_all(&folder)?;
        fs::write(
            folder.join("SKILL.md"),
            format!("---\nname: {name}\n---\nbody\n"),
        )?;
        skills.insert((*name).into(),json!({"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo","skillPath":format!("skills/{name}"),"skillFolderHash":"a".repeat(40),"commit":"a".repeat(40),"ref":"a".repeat(40),"installMode":"copy","installedAt":"2026-11-20T00:00:00.000Z","updatedAt":"2026-11-20T00:00:00.000Z"}));
    }
    write_json(
        &f.home.join(".agents/.skill-lock.json"),
        &json!({"version":3,"skills":skills}),
    )?;
    Ok(())
}

#[test]
fn capability_agent_batch_changes_one_shared_installation_and_converges_on_retry() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    public_skill_runner(&f)?;
    fs::create_dir(f.home.join(".codex"))?;
    fs::create_dir(f.home.join(".cursor"))?;
    let (out, preview) = f.run(&[
        "skill",
        "upgrade",
        "owner/repo",
        "--agent",
        "codex",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["results"][0]["status"], "blocked");
    assert!(!f.file("state").exists());

    let revision = "b".repeat(40);
    let args = [
        "skill",
        "upgrade",
        "owner/repo",
        "--agent",
        "codex",
        "--agent",
        "cursor-agent",
        "--shared",
        "--revision",
        revision.as_str(),
        "--yes",
    ];
    let (out, changed) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(changed["data"]["results"][0]["status"], "changed");
    assert_eq!(changed["data"]["results"][1]["status"], "changed");
    assert_eq!(
        fs::read_to_string(f.home.join("native-calls.jsonl"))?
            .lines()
            .count(),
        1
    );

    let active = f.file("state/active.json");
    let mut recovering = f.read_json("state/active.json")?;
    recovering["phase"] = json!("unknown");
    write_json(&active, &recovering)?;
    let (out, current) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(current["data"]["results"][0]["status"], "current");
    assert_eq!(current["data"]["results"][1]["status"], "current");
    assert_eq!(
        fs::read_to_string(f.home.join("native-calls.jsonl"))?
            .lines()
            .count(),
        1
    );
    Ok(())
}

#[test]
fn capability_agent_picker_explains_shared_scope_before_cancel() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    public_skill_runner(&f)?;
    fs::create_dir(f.home.join(".codex"))?;
    fs::create_dir(f.home.join(".cursor"))?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let before = fs::read(&lock)?;
    for scenario in ["skill-agent-picker-cancel", "skill-agent-picker-all-cancel"] {
        chezmoi_native::run_browser_pty(
            &f,
            &["skill", "upgrade", "owner/repo"],
            scenario,
            &f.home,
            "100",
            "3",
        )?;
    }
    assert_eq!(fs::read(lock)?, before);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn custom_agent_registration_is_idempotent_and_selectable_by_command_name() -> TestResult {
    let f = Fixture::new()?;
    f.script("studio", "exit 97")?;
    let config = f.file("studio-config");
    fs::create_dir(&config)?;
    let path = config.to_str().ok_or("fixture path")?;
    let args = [
        "agent", "add", "studio", "--type", "claude", "--config", path,
    ];
    let (out, plan) = f.run(&[&args[..], &["--dry-run"]].concat())?;
    assert_code(&out, 0);
    assert_eq!(plan["data"]["status"], "preview");
    assert!(!f.file("state").exists());
    let command = [&args[..], &["--yes"]].concat();
    for status in ["registered", "already-registered"] {
        let (out, result) = f.run(&command)?;
        assert_code(&out, 0);
        assert_eq!(result["data"]["status"], status);
    }
    let (_, inventory) = f.run(&["agent"])?;
    let resources = inventory["data"]["resources"]
        .as_array()
        .ok_or("Agent inventory")?;
    let registered = resources
        .iter()
        .find(|row| row["config_dir"] == path)
        .ok_or("registered Agent")?;
    assert_eq!(registered["aliases"][0], "studio");
    let (out, list) = f.run(&["skill", "enable", "demo", "--agent", "studio", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(list["data"]["results"][0]["agent"], "studio");
    assert_eq!(list["data"]["results"][0]["directory"], path);

    f.script("nested", "exit 97")?;
    let nested = f.home.join(".claude/nested");
    fs::create_dir_all(&nested)?;
    let before = fs::read(f.file("state/agent-profiles.json"))?;
    let (out, blocked) = f.run(&[
        "agent",
        "add",
        "nested",
        "--type",
        "claude",
        "--config",
        nested.to_str().ok_or("nested")?,
        "--yes",
    ])?;
    assert_code(&out, 3);
    assert_eq!(blocked["error"]["code"], "agent_configuration_overlap");
    assert_eq!(fs::read(f.file("state/agent-profiles.json"))?, before);
    Ok(())
}

#[test]
fn native_agent_enable_rechecks_the_installed_payload_even_if_already_enabled() -> TestResult {
    let f = Fixture::new()?;
    let root = f.home.join(".claude-mi");
    let payload = root.join("plugins/cache/fixture-market/widget/1.0.0");
    let manifest = payload.join(".claude-plugin/plugin.json");
    fs::create_dir_all(manifest.parent().ok_or("plugin path")?)?;
    write_json(&manifest, &json!({"name":"widget","version":"1.0.0"}))?;
    write_json(
        &root.join("plugins/installed_plugins.json"),
        &json!({
            "version":2,"plugins":{"widget@fixture-market":[{"scope":"user","version":"1.0.0","installPath":payload}]}
        }),
    )?;
    write_json(
        &root.join("settings.json"),
        &json!({"enabledPlugins":{"widget@fixture-market":true}}),
    )?;
    let args = [
        "skill",
        "enable",
        "widget@fixture-market",
        "--agent",
        "claude-mi",
        "--yes",
    ];
    let (out, report) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(report["data"]["results"][0]["status"], "current");
    fs::remove_file(manifest)?;
    let (out, report) = f.run(&args)?;
    assert_code(&out, 3);
    assert_eq!(report["data"]["results"][0]["status"], "blocked");
    assert!(!f.file("state").exists());
    Ok(())
}

fn plain(f: &Fixture, args: &[&str]) -> std::io::Result<std::process::Output> {
    let fixture = f.command();
    std::process::Command::new(fixture.get_program())
        .args(fixture.get_args().filter(|arg| *arg != "--json"))
        .envs(
            fixture
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        )
        .args(args)
        .output()
}

#[test]
fn recommend_opens_only_matching_local_inventory_when_no_install_plan_exists() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-alpha", "fixture-beta"])?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let original = fs::read(&lock)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "fixture-alpha"],
        "recommend-local-maintenance",
        &f.home,
        "102",
        "0",
    )?;
    assert_eq!(fs::read(lock)?, original);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_help_prioritizes_examples_and_explains_selection() -> TestResult {
    let f = Fixture::new()?;
    for (command, example, parameter) in [
        ("install", "mo recommend owner/repo", "--recipe"),
        ("adopt", "mo skill adopt NAME --dry-run", "唯一安装 ID"),
        (
            "refresh",
            "mo skill refresh source:owner/repo",
            "--revision",
        ),
        ("outdated", "mo skill outdated NAME", "--only"),
        (
            "upgrade",
            "mo skill upgrade source:owner/repo --dry-run",
            "--all",
        ),
        ("uninstall", "mo skill uninstall NAME --dry-run", "唯一 ID"),
        ("doctor", "mo skill doctor NAME", "--only"),
    ] {
        let out = plain(&f, &["skill", command, "--help"])?;
        assert_code(&out, 0);
        let text = String::from_utf8(out.stdout)?;
        let before_options = text.split_once("Options:").ok_or("options")?.0;
        assert!(before_options.contains(example), "{text}");
        assert!(text.contains(parameter), "{text}");
    }
    let out = plain(&f, &["skill", "--help"])?;
    assert_code(&out, 0);
    let text = String::from_utf8(out.stdout)?;
    assert!(text.contains("mo skill refresh --help") && text.contains("mo skill outdated"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_install_positional_repository_checks_runtime_without_writes() -> TestResult {
    let f = Fixture::new()?;
    for repo in [
        "owner/repo",
        "https://github.com/owner/repo",
        "github.com/owner/repo",
    ] {
        let args = ["skill", "install", repo];
        let out = plain(&f, &args)?;
        assert_code(&out, 3);
        let stderr = String::from_utf8(out.stderr)?;
        assert!(
            stderr.contains("npx --yes skills@1.9.0 --version"),
            "{stderr}"
        );
        let (out, json) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(json["error"]["code"], "skills_cli_unavailable");
    }
    let out = plain(&f, &["skill", "install"])?;
    assert_code(&out, 2);
    assert!(String::from_utf8(out.stderr)?.contains("mo skill install owner/repo"));
    for args in [
        ["app", "install", "owner/repo", "--unrelated"],
        ["skill", "install", "--invalid", "owner/repo"],
    ] {
        let out = plain(&f, &args)?;
        assert_code(&out, 2);
        assert!(!String::from_utf8(out.stderr)?.contains("mo recommend owner/repo"));
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_upgrade_distinguishes_installed_source_without_a_recipe_from_missing_source() -> TestResult
{
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["example", "extra"])?;
    for selector in ["owner/repo", "source:owner/repo", "skill:owner/repo"] {
        let args = ["skill", "upgrade", selector, "--dry-run"];
        let out = plain(&f, &args)?;
        assert_code(&out, 3);
        let text = String::from_utf8(out.stderr)?;
        assert!(
            text.contains("Skills CLI") && text.contains("npx --yes skills@1.9.0 --version"),
            "{text}"
        );
        let (out, result) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(result["error"]["code"], "skills_cli_unavailable");
    }
    for args in [
        &["skill", "upgrade", "other/repo", "--dry-run"][..],
        &["skill", "upgrade", "nothing", "--dry-run"][..],
        &[
            "skill",
            "upgrade",
            "--only",
            "owner/repo",
            "--only",
            "other/repo",
            "--dry-run",
        ][..],
    ] {
        let (out, result) = f.run(args)?;
        assert_code(&out, 3);
        assert_eq!(
            result["error"]["code"],
            "source_selection_ambiguous_or_missing"
        );
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_adoption_by_repository_uses_local_evidence_without_running_the_installer() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["example", "extra"])?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let before = fs::read(&lock)?;
    let cache = public_skill_runner(&f)?;
    let outside = f.file("outside-runner");
    fs::write(&outside, b"external fixture")?;
    symlink(&outside, cache.join("outside"))?;
    let (out, rejected) = f.run(&["skill", "adopt", "owner/repo", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(rejected["error"]["code"], "ai_payload_changed");
    assert!(!f.file("state").exists());
    fs::remove_file(cache.join("outside"))?;
    let (out, preview) = f.run(&["skill", "upgrade", "owner/repo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["skills"], json!(["example", "extra"]));
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert!(!f.file("state").exists());
    let (out, adopted) = f.run(&["skill", "adopt", "owner/repo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        adopted["data"]["results"]
            .as_array()
            .ok_or("results")?
            .len(),
        2
    );
    assert_eq!(adopted["data"]["results"][0]["nativeInvoked"], false);
    assert_eq!(fs::read(&lock)?, before);
    let (out, next) = f.run(&["skill", "upgrade", "owner/repo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(next["data"]["results"][0]["status"], "fixed-version");
    Ok(())
}

#[test]
fn a_link_in_another_skill_does_not_block_adoption_or_update() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo", "translation"])?;
    public_skill_runner(&f)?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let mut rows: serde_json::Value = serde_json::from_slice(&fs::read(&lock)?)?;
    rows["skills"]["translation"]["source"] = json!("another/kit");
    rows["skills"]["translation"]["sourceUrl"] = json!("https://github.com/another/kit");
    write_json(&lock, &rows)?;
    let original = fs::read(&lock)?;
    let link = f.home.join(".agents/skills/translation/SKILL.localized.md");
    let destination = PathBuf::from("/Users").join("macops-fixture/job/localization.md");
    symlink(&destination, &link)?;

    let (out, preview) = f.run(&["skill", "upgrade", "owner/repo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["skills"], json!(["demo"]));
    assert_eq!(out.stderr, [] as [u8; 0]);
    assert!(!f.file("state").exists());

    let (out, result) = f.run(&["skill", "upgrade", "owner/repo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["results"][0]["nativeInvoked"], false);
    assert_eq!(fs::read(&lock)?, original);
    assert!(!f.home.join("native-calls.jsonl").exists());

    let (out, upgraded) = f.run(&[
        "skill",
        "upgrade",
        "owner/repo",
        "--revision",
        &"b".repeat(40),
        "--yes",
    ])?;
    assert_code(&out, 0);
    assert_eq!(
        upgraded["data"]["results"][0]["journal"]["phase"],
        "succeeded"
    );
    let after: serde_json::Value = serde_json::from_slice(&fs::read(lock)?)?;
    assert_eq!(
        after["skills"]["translation"],
        rows["skills"]["translation"]
    );
    assert_eq!(fs::read_link(link)?, destination);
    Ok(())
}

#[test]
fn skill_preflight_explains_an_excluded_agent_directory_before_confirmation() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    public_skill_runner(&f)?;
    let profile = f.home.join(".claude-blocked");
    let destination = PathBuf::from("/Users").join("macops-fixture/job");
    symlink(&destination, &profile)?;

    for args in [
        &["skill", "upgrade", "owner/repo", "--dry-run"][..],
        &["skill", "upgrade", "owner/repo"][..],
    ] {
        let out = plain(&f, args)?;
        assert_code(&out, 1);
        let text = String::from_utf8(out.stderr)?;
        assert!(text.contains("技能操作尚未开始"));
        assert!(text.contains(".claude-blocked"));
        assert!(text.contains(destination.to_str().ok_or("fixture path")?));
        assert!(text.contains("正确用法"));
        assert!(!text.contains("确认操作"));
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_action_panels_and_picker_work_with_and_without_color() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    public_skill_runner(&f)?;
    for scenario in ["skill-adopt-style", "skill-adopt-no-color"] {
        chezmoi_native::run_browser_pty(
            &f,
            &["skill", "upgrade", "owner/repo"],
            scenario,
            &f.home,
            "68",
            "3",
        )?;
        assert!(!f.file("state").exists());
    }
    Ok(())
}

fn public_skill_runner(f: &Fixture) -> Result<PathBuf, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let cache = f.home.join(".npm/_npx/fixture/node_modules");
    let entry = cache.join("skills/bin/cli.mjs");
    let node = f
        .home
        .join(".local/share/mise/installs/node/latest/bin/node");
    fs::create_dir_all(entry.parent().ok_or("entry parent")?)?;
    fs::create_dir_all(node.parent().ok_or("node parent")?)?;
    fs::write(&entry, include_bytes!("fixtures/native-skills/cli.py"))?;
    fs::write(&node, b"#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'v26.13.1\n'; exit 0; fi\nexec /usr/bin/python3 \"$@\"\n")?;
    fs::set_permissions(&node, fs::Permissions::from_mode(0o700))?;
    write_json(
        &cache.join("skills/package.json"),
        &json!({"name":"skills","version":"1.9.0","bin":{"skills":"./bin/cli.mjs"}}),
    )?;
    Ok(cache)
}

#[test]
fn explicit_skill_upgrade_pins_the_new_version_and_checks_local_presence() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["example"])?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let mut native: serde_json::Value = serde_json::from_slice(&fs::read(&lock)?)?;
    native["skills"]["example"]["ref"] = json!("HEAD");
    write_json(&lock, &native)?;
    public_skill_runner(&f)?;
    let (out, adopted) = f.run(&["skill", "adopt", "owner/repo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(adopted["data"]["results"][0]["nativeInvoked"], false);
    let commit = "b".repeat(40);
    let (out, result) = f.run(&[
        "skill",
        "upgrade",
        "owner/repo",
        "--revision",
        &commit,
        "--yes",
    ])?;
    assert_code(&out, 0);
    assert_eq!(
        result["data"]["results"][0]["journal"]["phase"],
        "succeeded"
    );
    let (out, result) = f.run(&["skill", "upgrade", "owner/repo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["results"][0]["status"], "fixed-version");
    let record: serde_json::Value = serde_json::from_slice(&fs::read(&lock)?)?;
    assert_eq!(record["skills"]["example"]["commit"], commit);
    assert_eq!(
        fs::read_to_string(f.home.join("native-calls.jsonl"))?
            .lines()
            .count(),
        1
    );
    let (out, _) = f.run(&["skill", "uninstall", "example", "--yes"])?;
    assert_code(&out, 0);
    let (out, gone) = f.run(&["skill", "upgrade", "owner/repo", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(gone["error"]["code"], "skill_not_installed");
    Ok(())
}

#[test]
fn skill_install_recipe_errors_distinguish_github_from_a_local_file() -> TestResult {
    let f = Fixture::new()?;
    for source in [
        "samuelj1519/devnorm-kit",
        "https://github.com/samuelj1519/devnorm-kit",
        "github.com/samuelj1519/devnorm-kit",
    ] {
        let args = ["skill", "install", "--recipe", source];
        let out = plain(&f, &args)?;
        assert_code(&out, 1);
        assert_eq!(out.stdout, [] as [u8; 0]);
        let text = String::from_utf8(out.stderr)?;
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("错误表现") && lines[0].contains(source));
        assert!(lines[1].starts_with("错误原因") && lines[1].contains("GitHub 仓库"));
        assert!(
            lines[2].starts_with("正确用法")
                && lines[2].contains("mo recommend samuelj1519/devnorm-kit")
        );
        assert!(!text.contains("错误编号"));
        let (out, json) = f.run(&args)?;
        assert_code(&out, 1);
        assert_eq!(json["error"]["code"], "absolute_path_required");
    }

    for (args, example) in [
        (
            &["skill", "install", "--recipe", "recipes/devnorm.json"][..],
            "mo skill install --recipe",
        ),
        (
            &[
                "skill",
                "adopt",
                "devnorm",
                "--recipe",
                "recipes/devnorm.json",
            ][..],
            "mo skill adopt NAME --recipe",
        ),
    ] {
        let out = plain(&f, args)?;
        assert_code(&out, 1);
        let text = String::from_utf8(out.stderr)?;
        assert!(
            text.contains("以 / 开头") && text.contains(example),
            "{text}"
        );
        assert!(!text.contains("mo recommend"));
    }
    let out = plain(&f, &["skill", "install", "--recipe=-not-a-user/local"])?;
    assert_code(&out, 1);
    assert!(!String::from_utf8(out.stderr)?.contains("mo recommend -not-a-user"));

    let url = "https://github.com/example/kit/blob/main/recipe.json";
    let out = plain(&f, &["skill", "install", "--recipe", url])?;
    assert_code(&out, 1);
    assert!(String::from_utf8(out.stderr)?.contains("收到的是网络链接"));

    let missing = f.file("missing.json");
    let out = plain(
        &f,
        &[
            "skill",
            "install",
            "--recipe",
            missing.to_str().ok_or("fixture path")?,
        ],
    )?;
    assert_code(&out, 3);
    let text = String::from_utf8(out.stderr)?;
    assert!(text.contains("本机没有找到配方文件") && text.contains("确认配方 JSON 文件存在"));
    assert!(!f.file("state").exists());
    fs::create_dir(f.file("state"))?;
    let pending = f.file("state/active.json");
    fs::write(&pending, "synthetic unreadable recovery record")?;
    let (out, json) = f.run(&[
        "skill",
        "install",
        "--recipe",
        "github.com/samuelj1519/devnorm-kit",
    ])?;
    assert_code(&out, 1);
    assert_eq!(json["error"]["code"], "absolute_path_required");
    assert_eq!(fs::read(pending)?, b"synthetic unreadable recovery record");

    Ok(())
}

#[test]
fn main_journeys_show_the_selected_scope_without_executing_previews() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-skill"])?;
    let preview_args = ["app", "install", "demo", "--version", "1.0.0", "--dry-run"];
    let preview = plain(&f, &preview_args)?;
    assert_code(&preview, 0);
    let text = String::from_utf8(preview.stdout)?;
    assert!(text.contains("操作预演（未执行写入）") && text.contains("计划版本 1.0.0"));
    assert!(text.contains("fixture:demo") && !text.contains("\"command\""));
    let (out, raw) = f.run(&preview_args)?;
    assert_code(&out, 0);
    assert_eq!(raw["data"]["plan"]["target"], "1.0.0");
    assert!(raw["data"]["plan"]["command"].is_object());

    for (args, code, expected) in [
        (&["config", "list"][..], 3, "mo config enroll"),
        (&["config", "list", "--dry-run"][..], 0, "仅使用缓存"),
        (&["agent", "credential", "list"][..], 0, "档案凭据状态"),
        (&["dirs", "--roots"][..], 0, "状态"),
        (&["skill", "resources"][..], 0, "fixture-skill"),
    ] {
        let result = plain(&f, args)?;
        assert_code(&result, code);
        let stdout = String::from_utf8(result.stdout)?;
        assert!(
            stdout.contains(expected) && stdout.contains("--json"),
            "{args:?}: {stdout}"
        );
    }
    let (out, manifest) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let file = f.file("manifest.json");
    write_json(&file, &manifest["data"]["manifest"])?;
    let result = plain(
        &f,
        &["manifest", "plan", file.to_str().ok_or("manifest path")?],
    )?;
    assert_code(&result, 0);
    assert!(String::from_utf8(result.stdout)?.contains("清单差异与重放条件"));
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());

    let (_, installed) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    let id = installed["data"]["id"].as_str().ok_or("operation ID")?;
    for key in [id, "fixture:demo"] {
        let (out, response) = f.run(&["history", key])?;
        assert_code(&out, 0);
        assert_eq!(response["data"][0]["id"], id);
        let result = plain(&f, &["history", key])?;
        assert_code(&result, 0);
        let text = String::from_utf8(result.stdout)?;
        assert!(text.contains("操作历史") && text.contains(id));
    }
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn skill_inventory_and_browser_identify_agent_deployments() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-skill"])?;
    fs::create_dir_all(f.home.join(".codex/skills"))?;
    symlink(
        "../../.agents/skills/fixture-skill",
        f.home.join(".codex/skills/fixture-skill"),
    )?;
    fs::write(
        f.home.join(".codex/config.toml"),
        "[mcp_servers.fixture]\ncommand = 'demo'\n",
    )?;
    let (_, legacy) = f.run(&["skill", "resources"])?;
    assert!(
        legacy["data"]["resources"]
            .as_array()
            .ok_or("legacy resources")?
            .iter()
            .any(|row| row["delivery_kind"] == "mcp")
    );
    let (original, value) = f.run(&["skill"])?;
    assert_code(&original, 0);
    let rows = value["data"]["resources"].as_array().ok_or("resources")?;
    let skills: Vec<_> = rows
        .iter()
        .filter(|row| row["delivery_kind"] == "skill")
        .collect();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0]["name"], "fixture-skill");
    assert_eq!(
        skills[0]["deployments"]
            .as_array()
            .ok_or("deployments")?
            .len(),
        1
    );
    assert_eq!(
        skills[0]["deployments"][0]["path"],
        json!(f.home.join(".agents/skills/fixture-skill"))
    );
    assert!(skills[0]["loaded"].is_null());
    chezmoi_native::run_browser_pty(&f, &["skill"], "skill", &f.home, "72", "0")?;
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_browser_shows_installed_locations_and_shared_profile_visibility() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-skill"])?;
    for (root, name) in [
        (".claude-mi", "fixture-skill"),
        (".claude", "claude-import"),
        (".claude-glm", "glm-only"),
        (".codex", "codex-import"),
        (".cursor", "cursor-only"),
    ] {
        let folder = f.home.join(root).join("skills");
        fs::create_dir_all(&folder)?;
        if name == "fixture-skill" {
            symlink(
                f.home.join(".agents/skills/fixture-skill"),
                folder.join(name),
            )?;
        } else {
            let skill = folder.join(name);
            fs::create_dir(&skill)?;
            fs::write(skill.join("SKILL.md"), format!("# {name}\n"))?;
        }
    }
    let (out, v) = f.run(&["skill"])?;
    assert_code(&out, 0);
    let rows = v["data"]["resources"].as_array().ok_or("resources")?;
    let shared = rows
        .iter()
        .find(|row| row["name"] == "fixture-skill")
        .ok_or("shared skill")?;
    assert_eq!(
        shared["contents"][0]["path"],
        json!(f.home.join(".agents/skills/fixture-skill"))
    );
    assert!(shared["loaded"].is_null());
    for scenario in [
        "skill-visibility",
        "skill-claude-import",
        "skill-codex-import",
        "skill-location-groups",
    ] {
        chezmoi_native::run_browser_pty(&f, &["skill"], scenario, &f.home, "140", "0")?;
    }
    chezmoi_native::run_browser_pty(&f, &["skill"], "skill-location-groups", &f.home, "68", "0")?;
    chezmoi_native::run_browser_pty(
        &f,
        &["skill"],
        "skill-short-location-groups",
        &f.home,
        "42",
        "0",
    )?;
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn capability_browser_shows_native_plugin_configuration_per_profile() -> TestResult {
    let f = Fixture::new()?;
    for (profile, enabled, label) in [
        ("mi", Some(true), "已启用"),
        ("glm", Some(false), "已停用"),
        ("other", None, "未知"),
    ] {
        let root = f.home.join(format!(".claude-{profile}"));
        let payload = root.join("plugins/cache/official/review/1.0");
        fs::create_dir_all(&payload)?;
        if let Some(enabled) = enabled {
            write_json(
                &root.join("settings.json"),
                &json!({"enabledPlugins":{"review@official":enabled}}),
            )?;
        }
        write_json(
            &root.join("plugins/installed_plugins.json"),
            &json!({"version":2,"plugins":{"review@official":[{"scope":"user","installPath":payload,"version":"1.0"}]}}),
        )?;
        chezmoi_native::run_browser_pty(
            &f,
            &["app"],
            "plugin-visibility",
            std::path::Path::new(&format!("claude:{profile} {label}")),
            "95",
            "0",
        )?;
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_browser_keeps_known_rows_when_cursor_compatibility_root_is_invalid() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-skill"])?;
    fs::create_dir(f.home.join(".cursor"))?;
    fs::create_dir(f.home.join(".codex"))?;
    fs::write(f.home.join(".codex/skills"), "this should be a directory")?;
    let (out, v) = f.run(&["skill"])?;
    assert_code(&out, 3);
    assert!(
        v["data"]["coverage"]
            .as_array()
            .ok_or("coverage")?
            .iter()
            .any(|row| row["source"] == "skill-deployments" && row["state"] == "invalid")
    );
    assert_eq!(v["data"]["resources"][0]["name"], "fixture-skill");
    chezmoi_native::run_browser_pty(&f, &["skill"], "skill-partial", &f.home, "140", "3")?;
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn browsers_share_application_identities_and_keep_claude_profiles_separate() -> TestResult {
    let f = Fixture::new()?;
    for (profile, model) in [("mi", "fixture-mi-model"), ("glm", "fixture-glm-model")] {
        let root = f.home.join(format!(".claude-{profile}"));
        fs::create_dir_all(&root)?;
        write_json(&root.join("settings.json"), &json!({"model":model}))?;
    }
    for command in [["app", "resources"], ["agent", "resources"]] {
        let (_, snapshot) = f.run(&command)?;
        assert!(
            snapshot["data"]["resources"].as_array().is_some(),
            "{snapshot}"
        );
    }
    let (_, agents) = f.run(&["agent", "resources"])?;
    let rows = agents["data"]["resources"].as_array().ok_or("agents")?;
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0]["id"], rows[1]["id"]);
    assert!(rows.iter().any(|row| {
        row["model"]["selected"] == "model-mi" || row["model"]["selected"] == "fixture-mi-model"
    }));
    assert!(rows.iter().any(|row| {
        row["model"]["selected"] == "model-glm" || row["model"]["selected"] == "fixture-glm-model"
    }));
    let (_, items) = f.run(&["agent"])?;
    let items = items["data"]["resources"].as_array().ok_or("items")?;
    let keys = |rows: &[serde_json::Value]| {
        rows.iter()
            .filter_map(|row| row["id"].as_str().map(str::to_owned))
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(keys(rows), keys(items));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn skill_browser_can_show_discovered_skills_and_refuses_unrelated_config_scope() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-skill"])?;
    let own = f.home.join(".agents/skills/fixture-loose");
    fs::create_dir_all(&own)?;
    fs::write(
        own.join("SKILL.md"),
        "---\nname: fixture-loose\n---\nfixture\n",
    )?;
    chezmoi_native::run_browser_pty(&f, &["skill"], "skills-discovered", &f.home, "72", "0")?;
    let (out, v) = f.run(&[
        "skill",
        "--chezmoi-source",
        "/private/tmp/not-a-skill-scope",
    ])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "config_scope_unsupported");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn list_opens_a_unified_inventory_in_the_terminal_and_keeps_structured_queries() -> TestResult {
    let f = Fixture::new()?;
    let loose = f.home.join(".agents/skills/fixture-loose");
    fs::create_dir_all(&loose)?;
    fs::write(
        loose.join("SKILL.md"),
        "---\nname: fixture-loose\n---\nbody\n",
    )?;
    let (out, json) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    assert!(json["data"]["resources"].is_array(), "{json}");
    chezmoi_native::run_browser_pty(&f, &["app"], "unified-list", &f.home, "100", "0")?;
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn app_versions_remain_visible_in_a_narrow_terminal_and_probe_only_the_selected_cli() -> TestResult
{
    let f = Fixture::new()?;
    write_json(
        &f.home.join("mise-data"),
        &json!({
            "fixture-tool":[{"version":"3.1.4","install_path":f.home.join("tool/3.1.4"),"active":false}],
            "fixture-mise":[{"version":"5.0.0","install_path":f.home.join("bin/mise"),"active":true}]
        }),
    )?;
    let manager = f.script(
        "mise",
        r#"case "$1" in
ls) /bin/cat "$MACOPS_TEST_HOME/mise-data" ;;
where|which) exit 8 ;;
*) touch "$MACOPS_TEST_HOME/invoked-shim"; exit 9 ;;
esac"#,
    )?;
    let bin = f.home.join(".local/bin");
    fs::create_dir_all(&bin)?;
    let claude = f.script(
        "claude",
        r#"test "$1" = --version
printf 'claude 8.7.6\n'
printf 'called\n' >> "$MACOPS_TEST_HOME/version-probe""#,
    )?;
    let unrelated = f.script(
        "unrelated",
        r#"touch "$MACOPS_TEST_HOME/invoked-unrelated""#,
    )?;
    fs::rename(claude, bin.join("claude"))?;
    fs::rename(unrelated, bin.join("unrelated"))?;
    symlink(manager, bin.join("mimo"))?;
    let alternate_mise = f.home.join("alternate/mise");
    fs::create_dir_all(alternate_mise.parent().ok_or("parent")?)?;
    fs::rename(
        f.script(
            "alternate-mise",
            r#"touch "$MACOPS_TEST_HOME/invoked-shim"; printf 'mise 9.0.0\n'"#,
        )?,
        &alternate_mise,
    )?;
    symlink(alternate_mise, bin.join("dsh"))?;
    let (out, value) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    let mimo = value["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|item| item["name"] == "mimo")
        .ok_or("expected mimo entry")?;
    assert_eq!(mimo["releases"], json!([]));
    chezmoi_native::run_browser_pty(&f, &["app"], "app-version-evidence", &f.home, "48", "0")?;
    assert_eq!(
        fs::read_to_string(f.home.join("version-probe"))?,
        "called\n"
    );
    assert!(!f.home.join("invoked-unrelated").exists());
    assert!(!f.home.join("invoked-shim").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn inventory_adopts_the_confirmed_instance_without_reinstalling() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-existing");
    npm_manifest(&prefix, "@scope/demo", "1.2.3")?;
    write_authorized_npm_catalog(&f, &prefix, true, true)?;
    let path = prefix.join("lib/node_modules/@scope/demo/package.json");
    let original = fs::read(&path)?;
    f.script(
        "npm",
        r#"case "$1" in
prefix) printf '%s' "$MACOPS_TEST_HOME/npm-existing" ;;
view) printf '"2.0.0"\n' ;;
uninstall)
 spec=
 for arg; do spec=$arg; done
 [ "$4" = "$MACOPS_TEST_HOME/npm-existing" ] && [ "$spec" = '@scope/demo' ]
 /bin/rm "$4/lib/node_modules/@scope/demo/package.json"
 /bin/rmdir "$4/lib/node_modules/@scope/demo" ;;
*) printf 'unexpected' >> "$MACOPS_TEST_HOME/native-called"; exit 8 ;;
esac"#,
    )?;
    let (_, registry) = f.run(&["app", "resources"])?;
    let items = registry["data"]["resources"]
        .as_array()
        .ok_or("resources")?;
    let item = items
        .iter()
        .find(|row| row["recipe_ref"] == "npm:scoped-demo")
        .ok_or("npm instance")?;
    let name = std::path::Path::new(item["name"].as_str().ok_or("name")?);
    let source = f.home.to_str().ok_or("source")?;
    chezmoi_native::run_browser_pty(
        &f,
        &["app", "--chezmoi-source", source],
        "inventory-reject-scope",
        name,
        "110",
        "0",
    )?;
    assert!(!f.file("state").exists());
    chezmoi_native::run_browser_pty(&f, &["app"], "inventory-adopt-cancel", name, "110", "0")?;
    assert!(!f.file("state").exists());
    chezmoi_native::run_browser_pty(&f, &["app"], "inventory-adopt", name, "110", "0")?;
    assert!(f.read_json("state/receipts.json")?["installations"]["npm:scoped-demo"].is_object());
    assert_eq!(fs::read(&path)?, original);
    chezmoi_native::run_browser_pty(&f, &["app"], "inventory-upgrade-cancel", name, "110", "0")?;
    assert_eq!(fs::read(&path)?, original);
    let active = fs::read(f.file("state/active.json"))?;
    let receipts = fs::read(f.file("state/receipts.json"))?;
    chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "inventory-upgrade-migration",
        name,
        "110",
        "0",
    )?;
    assert_eq!(fs::read(&path)?, original);
    assert_eq!(fs::read(f.file("state/active.json"))?, active);
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
    assert!(!f.home.join("native-called").exists());
    let marker = f.file("state/migration-active.json");
    assert_eq!(fs::read_to_string(&marker)?, "interrupted migration");
    fs::remove_file(marker)?;
    chezmoi_native::run_browser_pty(&f, &["app"], "inventory-remove", name, "110", "0")?;
    assert!(!path.exists());
    assert!(!f.home.join("native-called").exists());
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn adopt_picker_selects_one_installation_and_exact_id_never_uses_names() -> TestResult {
    let f = Fixture::new()?;
    let left = f.home.join("picker-left");
    let right = f.home.join("picker-right");
    npm_manifest(&left, "@scope/demo", "1.2.3")?;
    npm_manifest(&right, "@scope/demo", "2.0.0")?;
    write_authorized_npm_catalog(&f, &left, true, false)?;
    let mut catalog = f.read_json("catalog.json")?;
    for (id, prefix) in [
        ("npm:other-scope", &right),
        ("npm:missing", &f.home.join("picker-missing")),
    ] {
        let mut package = catalog["packages"][0].clone();
        package["id"] = json!(id);
        package["provider"]["prefix"] = json!(prefix);
        catalog["packages"]
            .as_array_mut()
            .ok_or("packages")?
            .push(package);
    }
    write_json(&f.catalog, &catalog)?;
    f.script(
        "npm",
        r#"case "$1" in
prefix) printf '%s' "$MACOPS_TEST_HOME/picker-left" ;;
*) printf 'unexpected' >> "$MACOPS_TEST_HOME/native-called"; exit 8 ;;
esac"#,
    )?;
    for (args, code, error) in [
        (vec!["app", "adopt"], 3, "adopt_target_required"),
        (vec!["app", "adopt", "--yes"], 3, "adopt_target_required"),
        (
            vec!["app", "adopt", "--dry-run", "@scope/demo"],
            3,
            "ambiguous",
        ),
        (
            vec!["app", "adopt", "--dry-run", "--id", "scoped-demo"],
            3,
            "not_found",
        ),
        (
            vec!["app", "adopt", "@scope/demo", "--id", "npm:other-scope"],
            2,
            "usage",
        ),
        (vec!["app", "adopt", "--bind-recipe"], 2, "usage"),
    ] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, code);
        assert_eq!(value["error"]["code"], error, "{value}");
        assert!(!f.file("state/receipts.json").exists());
    }
    let (out, value) = f.run(&["app", "adopt", "--dry-run", "--id", "npm:other-scope"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["plan"]["package"]["id"], "npm:other-scope");
    assert_eq!(value["data"]["persistentWrites"], false);
    let (_, listed) = f.run(&["app", "resources"])?;
    let id = listed["data"]["resources"]
        .as_array()
        .ok_or("apps")?
        .iter()
        .find(|item| item["recipe_ref"] == "npm:other-scope")
        .and_then(|item| item["id"].as_str())
        .ok_or("visible id")?;
    let (out, value) = f.run(&["app", "adopt", "--dry-run", "--id", id])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["plan"]["package"]["id"], "npm:other-scope");
    chezmoi_native::run_browser_pty(
        &f,
        &["app", "adopt"],
        "adopt-picker-cancel",
        &right,
        "110",
        "0",
    )?;
    assert!(!f.file("state/receipts.json").exists());
    chezmoi_native::run_browser_pty(
        &f,
        &["app", "adopt", "scoped-demo"],
        "adopt-picker-select",
        &right,
        "110",
        "0",
    )?;
    let receipts = f.read_json("state/receipts.json")?;
    let installed = receipts["installations"]
        .as_object()
        .ok_or("installations")?;
    assert_eq!(installed.len(), 1);
    assert!(installed.contains_key("npm:other-scope"));
    assert!(!f.home.join("native-called").exists());
    assert!(
        left.join("lib/node_modules/@scope/demo/package.json")
            .is_file()
    );
    assert!(
        right
            .join("lib/node_modules/@scope/demo/package.json")
            .is_file()
    );
    chezmoi_native::run_browser_pty(
        &f,
        &["app", "adopt"],
        "adopt-picker-confirmation-cancel",
        &left,
        "100",
        "3",
    )?;
    assert_eq!(f.read_json("state/receipts.json")?, receipts);
    Ok(())
}

#[test]
fn adopt_picker_keeps_the_selected_inventory_identity_when_a_catalog_id_collides() -> TestResult {
    let f = Fixture::new()?;
    let left = f.home.join("picker-left");
    let right = f.home.join("picker-right");
    npm_manifest(&left, "@scope/demo", "1.2.3")?;
    npm_manifest(&right, "@scope/demo", "2.0.0")?;
    write_authorized_npm_catalog(&f, &left, true, false)?;
    f.script("npm", "printf '%s' \"$MACOPS_TEST_HOME/picker-left\"")?;
    let (_, listed) = f.run(&["app", "resources"])?;
    let left_id = listed["data"]["resources"]
        .as_array()
        .ok_or("apps")?
        .iter()
        .find(|item| item["recipe_ref"] == "npm:scoped-demo")
        .and_then(|item| item["id"].as_str())
        .ok_or("left inventory ID")?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["id"] = json!("scoped-demo");
    let mut other = catalog["packages"][0].clone();
    other["id"] = json!(left_id);
    other["provider"]["prefix"] = json!(right);
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(other);
    write_json(&f.catalog, &catalog)?;
    for (args, error) in [
        (
            vec!["app", "adopt", "--dry-run", "--id", left_id],
            "adopt_id_collision",
        ),
        (
            vec!["app", "adopt", "--dry-run", "scoped-demo"],
            "ambiguous",
        ),
    ] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], error, "{value}");
        assert!(!f.file("state/receipts.json").exists());
    }
    chezmoi_native::run_browser_pty(
        &f,
        &["app", "adopt", "scoped-demo"],
        "adopt-picker-select",
        &left,
        "110",
        "0",
    )?;
    let receipts = f.read_json("state/receipts.json")?;
    let installs = receipts["installations"].as_object().ok_or("receipts")?;
    assert_eq!(installs.len(), 1);
    assert_eq!(
        installs["scoped-demo"]["package"]["provider"]["prefix"],
        json!(left)
    );
    Ok(())
}

#[test]
fn root_help_prioritizes_daily_tasks_and_advanced_help_keeps_recovery() -> TestResult {
    let command = env!("CARGO_BIN_EXE_macops");
    for args in [vec![], vec!["--help"], vec!["-h"], vec!["help"]] {
        let output = std::process::Command::new(command).args(args).output()?;
        assert_code(&output, 0);
        let help = String::from_utf8(output.stdout)?;
        assert!(help.contains("mo recommend <目标>"), "{help}");
        assert!(help.contains("mo help advanced"), "{help}");
        assert!(!help.contains("--resolve-failed"), "{help}");
        assert!(!help.contains("--chezmoi-source"), "{help}");
        for item in [
            "mo [命令] [参数]",
            "mo help <命令>",
            "mo app [应用]",
            "mo config",
            "mo skill",
            "mo agent",
            "recover",
            "--json",
            "--dry-run",
            "--yes",
        ] {
            assert!(help.contains(item), "{help}");
        }
    }
    let detailed = std::process::Command::new(command)
        .args(["help", "advanced"])
        .output()?;
    assert_code(&detailed, 0);
    let help = String::from_utf8(detailed.stdout)?;
    assert!(
        help.contains("recover") && help.contains("--chezmoi-source"),
        "{help}"
    );
    for args in [["help", "skill", "market"], ["skill", "market", "--help"]] {
        let output = std::process::Command::new(command).args(args).output()?;
        assert_code(&output, 0);
        let help = String::from_utf8(output.stdout)?;
        assert!(
            help.contains("mo skill market")
                && help.contains("--online")
                && help.contains("claude:mi"),
            "{help}"
        );
    }
    for (category, example) in [
        ("app", "mo app adopt NAME"),
        ("config", "mo config apply PATH --dry-run"),
        ("agent", "mo agent launch"),
        ("skill", "mo skill plugins --help"),
    ] {
        let output = std::process::Command::new(command)
            .args(["help", category])
            .output()?;
        assert_code(&output, 0);
        let help = String::from_utf8(output.stdout)?;
        assert!(help.contains(example), "{help}");
        assert!(!help.contains("纳管"), "{help}");
    }
    Ok(())
}

#[test]
fn short_option_names_are_shown_and_the_old_spellings_still_work() -> TestResult {
    let f = Fixture::new()?;
    for (args, name, legacy) in [
        (&["--help"][..], "--timeout <SECONDS>", "--timeout-secs"),
        (
            &["agent", "upgrade", "--help"][..],
            "--local",
            "--force-live",
        ),
        (
            &["agent", "install", "--help"][..],
            "--local",
            "--force-live",
        ),
        (
            &["recover", "--help"][..],
            "--mark-failed",
            "--resolve-failed",
        ),
    ] {
        let out = plain(&f, args)?;
        assert_code(&out, 0);
        let help = String::from_utf8(out.stdout)?;
        assert!(help.contains(name) && !help.contains(legacy), "{help}");
    }
    for option in ["--timeout", "--timeout-secs"] {
        let (out, preview) = f.run(&[
            "app",
            "install",
            "demo",
            "--version",
            "1.0.0",
            "--dry-run",
            option,
            "30",
        ])?;
        assert_code(&out, 0);
        assert_eq!(preview["data"]["plan"]["timeout_secs"], 30);
    }
    let wrong = plain(&f, &["app", "install", "demo", "--timeout", "0"])?;
    assert_code(&wrong, 2);
    let text = String::from_utf8(wrong.stderr)?;
    assert!(text.contains("运行 mo --help"));
    assert!(text.contains("--timeout"));
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn typed_details_and_target_first_actions_keep_global_options() -> TestResult {
    let f = Fixture::new()?;
    let (out, detail) = f.run(&["app", "app:demo", "--json"])?;
    assert_code(&out, 3);
    assert_eq!(detail["data"]["package"]["id"], "fixture:demo");
    assert_eq!(detail["data"]["observation"]["status"], "missing");

    let (out, result) = f.run(&[
        "app",
        "app:demo",
        "install",
        "--version",
        "1.0.0",
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["plan"]["package"]["id"], "fixture:demo");
    assert_eq!(result["data"]["plan"]["target"], "1.0.0");

    let (out, invalid) = f.run(&["app", "demo", "--unknown"])?;
    assert_code(&out, 2);
    assert_eq!(invalid["error"]["code"], "usage");

    let project = f.home.join("project");
    fs::create_dir(&project)?;
    let project = project.to_str().ok_or("project")?;
    let (out, blocked) = f.run(&[
        "app",
        "install",
        "demo",
        "--version",
        "1.0.0",
        "--project",
        project,
    ])?;
    assert_code(&out, 1);
    assert_eq!(blocked["error"]["code"], "project_scope_unsupported");

    let (out, explicit) = f.run(&["search", "app:demo"])?;
    assert_code(&out, 0);
    let (out, bare) = f.run(&["app:demo"])?;
    assert_code(&out, 0);
    assert_eq!(bare, explicit);
    assert_eq!(bare["data"]["resource"]["recipe_ref"], "fixture:demo");

    let (out, agent) = f.run(&["agent", "agent:codex"])?;
    assert_code(&out, 0);
    assert_eq!(agent["data"]["tool"]["tool"], "codex");
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn project_scope_filters_all_four_resource_lists_and_rejects_other_routes() -> TestResult {
    let f = Fixture::new()?;
    let project = f.home.join("project");
    fs::create_dir(&project)?;
    let path = project.to_str().ok_or("project")?;
    for noun in ["app", "config", "skill", "agent"] {
        let (out, value) = f.run(&[noun, "--project", path])?;
        assert_code(&out, 0);
        assert_eq!(value["data"]["project"], json!(project));
        assert!(value["data"]["resources"].is_array());
        let (out, explicit) = f.run(&[noun, "resources", "--project", path])?;
        assert_code(&out, 0);
        assert_eq!(value, explicit);
    }
    for (noun, action) in [
        ("app", "upgrade"),
        ("config", "enroll"),
        ("skill", "market"),
        ("agent", "upgrade"),
    ] {
        let (out, value) = f.run(&[noun, action, "--project", path])?;
        assert_code(&out, 1);
        assert_eq!(value["error"]["code"], "project_scope_unsupported");
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn app_upgrade_refreshes_then_updates_only_eligible_installed_apps() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    let mut missing = catalog["packages"][0].clone();
    missing["id"] = json!("fixture:other");
    missing["name"] = json!("other");
    missing["provider"]["query"]["args"][1] = json!(f.file("missing-version"));
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(missing);
    write_json(&f.catalog, &catalog)?;

    let (out, preview) = f.run(&["app", "upgrade", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["mode"], "dry-run");
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());

    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let (out, updated) = f.run(&["app", "upgrade", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(updated["data"]["results"][0]["status"], "upgraded");
    assert_eq!(updated["data"]["results"][1]["status"], "not_installed");
    assert_eq!(fs::read_to_string(f.file("version"))?, "2.0.0");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\nupgrade\n");
    assert!(f.current_index()?.exists());
    assert!(!f.file("missing-version").exists());
    Ok(())
}

#[test]
fn app_upgrade_respects_explicit_version_requirement_in_batch_mode() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["exact_version_required"] = json!(true);
    write_json(&f.catalog, &catalog)?;
    assert_code(
        &f.run(&["app", "install", "demo", "--version", "1.0.0"])?.0,
        0,
    );
    let (out, result) = f.run(&["app", "upgrade", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        result["data"]["results"][0]["status"],
        "explicit_version_required"
    );
    assert_eq!(fs::read_to_string(f.file("version"))?, "1.0.0");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn app_upgrade_does_not_reuse_a_stale_version_when_refresh_fails() -> TestResult {
    let f = Fixture::new()?;
    let available = f.script(
        "available",
        r#"[ ! -f "$MACOPS_TEST_HOME/fail-refresh" ] || exit 7
printf '{"version":"2.0.0"}\n'"#,
    )?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"]["available"] = json!({"program":available,"args":[]});
    write_json(&f.catalog, &catalog)?;
    assert_code(
        &f.run(&["app", "install", "demo", "--version", "1.0.0"])?.0,
        0,
    );
    assert_code(&f.run(&["app", "refresh", "demo"])?.0, 0);
    fs::write(f.home.join("fail-refresh"), "")?;
    let (out, result) = f.run(&["app", "upgrade", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["results"][0]["status"], "unknown");
    assert!(result["data"]["results"][0]["error"]["code"].is_string());
    assert_eq!(fs::read_to_string(f.file("version"))?, "1.0.0");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn batch_upgrade_stops_after_an_unknown_original_operation() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    let mut other = catalog["packages"][0].clone();
    other["id"] = json!("fixture:other");
    other["name"] = json!("other");
    for operation in ["query", "install", "upgrade", "uninstall"] {
        other["provider"][operation]["args"][1] = json!(f.file("other-version"));
        if operation != "query" {
            other["provider"][operation]["args"][2] = json!(f.file("other-calls"));
        }
    }
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(other);
    catalog["packages"][0]["provider"]["upgrade"]["program"] = json!(f.script(
        "fail-upgrade",
        r#"printf attempted > "$MACOPS_TEST_HOME/upgrade-attempt"; exit 8"#,
    )?);
    write_json(&f.catalog, &catalog)?;
    for name in ["demo", "other"] {
        assert_code(
            &f.run(&["app", "install", name, "--version", "1.0.0"])?.0,
            0,
        );
    }

    let mut unsupported = catalog.clone();
    unsupported["packages"][0]["provider"]["upgrade"] = serde_json::Value::Null;
    write_json(&f.catalog, &unsupported)?;
    let (out, result) = f.run(&["app", "upgrade", "--yes"])?;
    assert_code(&out, 3);
    assert!(
        result["data"]["results"][0]["error"]["code"].is_string(),
        "{result}"
    );
    assert_eq!(result["data"]["unattempted"], json!(["fixture:other"]));
    assert!(!f.home.join("upgrade-attempt").exists());
    assert_eq!(fs::read_to_string(f.file("other-calls"))?, "install\n");
    write_json(&f.catalog, &catalog)?;

    let (out, result) = f.run(&["app", "upgrade", "--yes"])?;
    assert_code(&out, 3);
    let rows = result["data"]["results"].as_array().ok_or("results")?;
    assert_eq!(rows[0]["status"], "unknown");
    assert_eq!(rows[0]["operation"]["phase"], "unknown");
    assert_eq!(rows[1]["id"], "fixture:other");
    assert_eq!(result["data"]["unattempted"], json!(["fixture:other"]));
    assert!(f.home.join("upgrade-attempt").exists());
    assert_eq!(fs::read_to_string(f.file("other-version"))?, "1.0.0");
    assert_eq!(fs::read_to_string(f.file("other-calls"))?, "install\n");

    let (out, recovery) = f.run(&["recover"])?;
    assert_code(&out, 0);
    assert_eq!(
        recovery["data"]["pending"][0]["id"],
        rows[0]["operation"]["id"]
    );
    assert_eq!(recovery["data"]["persistentWrites"], false);
    Ok(())
}

#[test]
fn search_preserves_same_name_matches_across_types() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    let (out, result) = f.run(&["demo"])?;
    assert_code(&out, 0);
    let rows = result["data"]["candidates"]
        .as_array()
        .ok_or("candidates")?;
    assert_eq!(rows.len(), 2, "{result}");
    for (qualifier, category) in [("app:demo", "applications"), ("skill:demo", "capabilities")] {
        let expected = rows
            .iter()
            .find(|row| row["category"] == category)
            .ok_or("category")?;
        let (out, detail) = f.run(&["search", qualifier])?;
        assert_code(&out, 0);
        assert_eq!(detail["data"]["resource"]["id"], expected["id"]);
    }
    let (out, missing) = f.run(&["search", "config:missing"])?;
    assert_code(&out, 3);
    assert_eq!(missing["error"]["code"], "not_found");
    let (out, invalid) = f.run(&["recover", "--resolve-failed"])?;
    assert_code(&out, 2);
    assert_eq!(invalid["error"]["code"], "usage");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn app_upgrade_uses_brew_cask_native_version_resolution() -> TestResult {
    let f = Fixture::new()?;
    f.script("brew", r#"case "$1" in
info) printf '{"casks":[{"token":"demo","installed":null,"version":"%s"}],"formulae":[]}\n' "$(/bin/cat "$MACOPS_TEST_HOME/latest")" ;;
list) if [ -f "$MACOPS_TEST_HOME/version" ]; then printf 'demo %s\n' "$(/bin/cat "$MACOPS_TEST_HOME/version")"; fi ;;
install|upgrade) /bin/cat "$MACOPS_TEST_HOME/latest" > "$MACOPS_TEST_HOME/version"; printf '%s\n' "$1" >> "$MACOPS_TEST_HOME/calls" ;;
*) exit 8 ;;
esac"#)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["kind"] = json!("app");
    catalog["packages"][0]["provider"] = json!({"type":"brew","package":"demo","cask":true});
    write_json(&f.catalog, &catalog)?;
    fs::write(f.home.join("latest"), "1.0.0")?;
    assert_code(&f.run(&["app", "install", "demo"])?.0, 0);
    fs::write(f.home.join("latest"), "2.0.0")?;
    let (out, result) = f.run(&["app", "upgrade", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["results"][0]["status"], "upgraded");
    assert_eq!(fs::read_to_string(f.home.join("version"))?, "2.0.0");
    assert_eq!(
        fs::read_to_string(f.home.join("calls"))?,
        "install\nupgrade\n"
    );
    Ok(())
}

#[test]
fn github_skill_selectors_work_without_adding_public_aliases() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["devnorm-kit"])?;
    let path = f.home.join(".agents/.skill-lock.json");
    let mut lock: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    lock["skills"]["devnorm-kit"]["source"] = json!("samuelj1519/devnorm-kit");
    lock["skills"]["devnorm-kit"]["sourceUrl"] =
        json!("https://github.com/samuelj1519/devnorm-kit");
    write_json(&path, &lock)?;
    for args in [
        ["skill", "skill:samuelj1519/devnorm-kit"],
        ["search", "skill:SamuelJ1519/Devnorm-Kit"],
    ] {
        let (out, response) = f.run(&args)?;
        assert_code(&out, 0);
        assert!(
            response["data"]["package"]["name"] == "devnorm-kit"
                || response["data"]["resource"]["name"] == "devnorm-kit"
        );
    }
    let (out, doctor) = f.run(&["skill", "doctor", "skill:samuelj1519/devnorm-kit"])?;
    assert_code(&out, 0);
    assert_eq!(doctor["data"]["resources"][0]["name"], "devnorm-kit");
    assert_eq!(
        doctor["data"]["resources"][0]["origin"]["label"],
        "https://github.com"
    );
    assert!(
        !doctor["data"]["resources"][0]["aliases"]
            .to_string()
            .contains("samuelj1519")
    );
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn recommend_separates_pending_recovery_consent_from_install_confirmation() -> TestResult {
    for scenario in ["recommend-already-blocked", "recommend-blocked-after-menu"] {
        let f = Fixture::new()?;
        let (out, installed) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
        assert_code(&out, 0);
        let mut pending = installed["data"].clone();
        pending["phase"] = json!("unknown");
        fs::remove_file(f.file("version"))?;
        let active = f.file("state/active.json");
        let seed = f.file("pending-fixture.json");
        write_json(&seed, &pending)?;
        let original = fs::read(&seed)?;
        if scenario == "recommend-already-blocked" {
            fs::write(&active, &original)?;
        } else {
            fs::remove_file(&active)?;
        }

        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
        write_json(&f.catalog, &catalog)?;
        f.script(
            "mise",
            r#"printf unexpected > "$MACOPS_TEST_HOME/mise-called"; exit 8"#,
        )?;
        let receipts = fs::read(f.file("state/receipts.json"))?;

        chezmoi_native::run_browser_pty(&f, &["recommend", "demo"], scenario, &seed, "110", "3")?;
        let still_pending: serde_json::Value = serde_json::from_slice(&fs::read(&active)?)?;
        assert_eq!(still_pending["id"], pending["id"]);
        assert_eq!(still_pending["phase"], "unknown");
        assert_eq!(still_pending["plan_sha256"], pending["plan_sha256"]);
        let blocked_record = fs::read(&active)?;
        assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
        assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
        assert!(!f.home.join("mise-called").exists());

        let (out, recommendation) = f.run(&["recommend", "demo"])?;
        assert_code(&out, 0);
        assert_eq!(recommendation["data"]["known"][0]["id"], "fixture:demo");

        let plain_recover = |args: &[&str]| {
            let command = f.command();
            std::process::Command::new(command.get_program())
                .args(command.get_args().filter(|arg| *arg != "--json"))
                .envs(
                    command
                        .get_envs()
                        .filter_map(|(name, value)| value.map(|value| (name, value))),
                )
                .arg("recover")
                .args(args)
                .output()
        };
        let plain = plain_recover(&[])?;
        assert_code(&plain, 0);
        let text = String::from_utf8(plain.stdout)?;
        assert!(text.contains("还有这些操作要检查"));
        assert!(text.contains(pending["id"].as_str().ok_or("operation ID")?));
        assert!(text.contains("demo · 结果还没确认"));
        let id = pending["id"].as_str().ok_or("operation ID")?;
        assert!(text.contains(&format!("mo recover {id} --dry-run")));
        let (out, recovery) = f.run(&["recover"])?;
        assert_code(&out, 0);
        assert_eq!(recovery["data"]["pending"][0]["id"], pending["id"]);
        let preview = plain_recover(&[pending["id"].as_str().ok_or("operation ID")?, "--dry-run"])?;
        assert_code(&preview, 0);
        let preview = String::from_utf8(preview.stdout)?;
        assert!(preview.contains("核对预演（只读）"));
        assert!(preview.contains("原目标 demo · fixture:demo"));
        assert!(preview.contains("计划版本 1.0.0"));
        assert!(preview.contains(f.file("state").to_str().ok_or("state directory")?));
        assert_eq!(fs::read(f.file("state/active.json"))?, blocked_record);
        assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
        assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    }
    Ok(())
}

#[test]
fn recommend_accepts_repository_subjects_without_a_second_argument() -> TestResult {
    let f = Fixture::new()?;
    for subject in ["owner/example", "https://github.com/owner/example/"] {
        let (out, response) = f.run(&["--dry-run", "recommend", subject])?;
        assert_code(&out, 0);
        let data = &response["data"];
        assert_eq!(data["repository"], "owner/example", "{data}");
        assert_eq!(data["evidenceStatus"], "dry-run");
        assert_eq!(data["canExecute"], false);
        assert_eq!(data["persistentWrites"], false);
    }
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn setup_without_a_terminal_requires_an_explicit_terminal_session() -> TestResult {
    let f = Fixture::new()?;
    let base = f.command();
    let out = std::process::Command::new(base.get_program())
        .args(base.get_args().filter(|arg| *arg != "--json"))
        .args(["recommend", "--setup"])
        .output()?;
    assert_code(&out, 3);
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains("model_setup_terminal_required"), "{stderr}");
    assert!(stderr.contains("mo recommend --setup"));
    let preview = std::process::Command::new(base.get_program())
        .args(base.get_args().filter(|arg| *arg != "--json"))
        .args(["recommend", "--setup", "--dry-run"])
        .output()?;
    assert_code(&preview, 0);
    assert!(String::from_utf8(preview.stdout)?.contains("local"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn setup_reports_local_mode_and_cancels_without_creating_state() -> TestResult {
    let f = Fixture::new()?;
    let (out, data) = f.run(&["recommend", "--setup"])?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["mode"], "local", "{data}");
    let fixture = f.command();
    let noninteractive = std::process::Command::new(fixture.get_program())
        .args(fixture.get_args().filter(|arg| *arg != "--json"))
        .args(["recommend", "--setup"])
        .output()?;
    assert_code(&noninteractive, 3);
    assert!(String::from_utf8(noninteractive.stderr)?.contains("model_setup_terminal_required"));
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-cancel",
        &f.home,
        "100",
        "0",
    )?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-hide-key",
        &f.home,
        "100",
        "0",
    )?;
    assert!(!f.file("state").exists());
    assert!(!f.home.join(".config/macops/recommend-model.json").exists());
    Ok(())
}

#[test]
fn setup_metadata_and_pty_actions_use_only_the_isolated_credential_scope() -> TestResult {
    let f = Fixture::new()?;
    let config = f.home.join(".config/macops");
    std::fs::create_dir_all(&config)?;
    let profile_file = config.join("recommend-model.json");
    let mut profile = json!({
        "schemaVersion":1,
        "enabled":true,
        "endpoint":"https://models.example.test/v1/chat/completions",
        "model":"fixture",
        "account":format!("setup-{}", "a".repeat(64))
    });
    write_json(&profile_file, &profile)?;
    let (out, response) = f.run(&["recommend", "--setup"])?;
    assert_code(&out, 0);
    assert_eq!(
        response["data"],
        json!({"mode":"remote","endpoint":profile["endpoint"],"model":"fixture"})
    );
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-saved-isolated",
        &f.home,
        "100",
        "3",
    )?;
    let external = f.file("external-fixture.keychain-db");
    std::fs::write(&external, b"fixture")?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-external-isolated",
        &external,
        "100",
        "3",
    )?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&profile_file)?)?,
        profile
    );
    profile["enabled"] = json!(false);
    write_json(&profile_file, &profile)?;
    let (out, paused) = f.run(&["--dry-run", "recommend", "--setup"])?;
    assert_code(&out, 0);
    assert_eq!(paused["data"]["mode"], "local");
    assert_eq!(paused["data"]["model"], "fixture");
    assert!(!f.file("state").exists());
    assert!(!config.join("recommend-model-pending.json").exists());
    Ok(())
}

#[test]
fn explicit_home_works_without_ambient_home_and_ignores_ambient_scope() -> TestResult {
    let f = Fixture::new()?;
    let xdg = f.file("ambient-config");
    for args in [&["dirs", "--roots"][..], &["recommend", "--setup"][..]] {
        let output = f
            .roots_command()
            .env_remove("HOME")
            .env("XDG_CONFIG_HOME", &xdg)
            .env("CODEX_HOME", "relative-ambient-codex")
            .arg("--home")
            .arg(&f.home)
            .args(args)
            .output()?;
        assert_code(&output, 0);
        let response: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        if args[0] == "dirs" {
            assert_eq!(response["data"]["home"], json!(f.home));
            assert_eq!(
                response["data"]["config"],
                json!(f.home.join(".config/macops"))
            );
        } else {
            assert_eq!(response["data"]["mode"], "local");
        }
    }
    assert!(!xdg.exists());
    assert!(!f.home.join(".config/macops").exists());
    Ok(())
}

#[test]
fn setup_masked_input_can_cancel_or_reject_isolated_network_before_publishing() -> TestResult {
    let f = Fixture::new()?;
    for (scenario, status) in [
        ("setup-key-escape", "0"),
        ("setup-key-interrupt", "0"),
        ("setup-new-isolated", "3"),
    ] {
        chezmoi_native::run_browser_pty(
            &f,
            &["recommend", "--setup"],
            scenario,
            &f.home,
            "100",
            status,
        )?;
        assert!(!f.file("state").exists());
        assert!(!f.home.join(".config/macops").exists());
    }
    Ok(())
}

#[test]
fn setup_reports_a_pending_fixed_slot_from_the_real_preferences_entry_point() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new()?;
    let config = f.home.join(".config/macops");
    fs::create_dir_all(&config)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    let profile_file = config.join("recommend-model.json");
    let pending_file = config.join("recommend-model-pending.json");
    let first = json!({"schemaVersion":2,"enabled":true,"model":"original",
        "endpoint":"https://example.test/chat/completions","account":format!("setup-{}", "a".repeat(64))});
    let mut next = first.clone();
    next["account"] = json!(format!("setup-{}", "b".repeat(64)));
    next["model"] = json!("replacement");
    write_json(&profile_file, &first)?;
    write_json(
        &pending_file,
        &json!({"profile":next,"previous":first,"added":false,"tagged":true}),
    )?;
    let original_profile = fs::read(&profile_file)?;
    let original_pending = fs::read(&pending_file)?;
    for scenario in [
        "setup-single-pending-edit",
        "setup-single-pending-confirm",
        "setup-single-pending-retain",
    ] {
        chezmoi_native::run_browser_pty(
            &f,
            &["recommend", "--setup"],
            scenario,
            &f.home,
            "100",
            "3",
        )?;
        assert_eq!(fs::read(&profile_file)?, original_profile);
        assert_eq!(fs::read(&pending_file)?, original_pending);
    }
    fs::remove_file(&pending_file)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-single-invalid-model",
        &f.home,
        "100",
        "3",
    )?;
    assert_eq!(fs::read(&profile_file)?, original_profile);
    fs::remove_file(&profile_file)?;
    let mut first_attempt: serde_json::Value = serde_json::from_slice(&original_pending)?;
    first_attempt["previous"] = serde_json::Value::Null;
    write_json(&pending_file, &first_attempt)?;
    let first_pending = fs::read(&pending_file)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-single-pending-confirm",
        &f.home,
        "100",
        "3",
    )?;
    assert!(!profile_file.exists());
    assert_eq!(fs::read(&pending_file)?, first_pending);
    fs::write(&profile_file, &original_profile)?;
    fs::remove_file(&pending_file)?;
    let (out, installed) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut active = installed["data"].clone();
    active["phase"] = json!("unknown");
    let state = f.file("state/active.json");
    write_json(&state, &active)?;
    let active_before = fs::read(&state)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-key-escape",
        &f.home,
        "100",
        "0",
    )?;
    assert_eq!(fs::read(&profile_file)?, original_profile);
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-pause-saved",
        &f.home,
        "100",
        "0",
    )?;
    assert_eq!(fs::read(&state)?, active_before);
    let setup_bytes = fs::read(&profile_file)?;
    let saved = serde_json::from_slice::<serde_json::Value>(&setup_bytes)?;
    assert_eq!(saved["enabled"], false);
    assert_eq!(saved["account"], first["account"]);
    let (out, refreshed) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(refreshed["data"]["results"][0]["status"], "updated");
    let settled = f.read_json("state/active.json")?;
    assert_eq!(settled["id"], active["id"]);
    assert_eq!(settled["phase"], "succeeded");
    assert_eq!(fs::read(&profile_file)?, setup_bytes);
    assert!(!pending_file.exists());
    Ok(())
}

#[test]
fn setup_does_not_resolve_maintenance_state_or_migration() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new()?;
    fs::write(f.file("state"), "unavailable maintenance path")?;
    let (out, setup) = f.run(&["recommend", "--setup"])?;
    assert_code(&out, 0);
    assert_eq!(setup["data"]["mode"], "local");
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-recover-clean",
        &f.home,
        "100",
        "0",
    )?;
    assert!(!f.home.join(".config/macops").exists());
    let invalid_config = f.file("config-file");
    fs::write(&invalid_config, "not a config directory")?;
    let (out, invalid) = f.run(&[
        "--config-dir",
        invalid_config.to_str().ok_or("invalid config")?,
        "recommend",
        "--setup",
    ])?;
    assert_code(&out, 1);
    assert_eq!(invalid["error"]["code"], "root_not_directory");

    let old = f.home.join(".local/state/macops");
    fs::create_dir_all(&old)?;
    let redirected = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("new-maintenance-state"))
        .args(["recommend", "--setup"])
        .output()?;
    assert_code(&redirected, 0);
    let marker = old.join("migration-active.json");
    fs::write(&marker, "unresolved migration")?;
    let read_setup = f.roots_command().args(["recommend", "--setup"]).output()?;
    assert_code(&read_setup, 0);
    let config = f.home.join(".config/macops");
    fs::create_dir_all(&config)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    let profile = config.join("recommend-model.json");
    write_json(
        &profile,
        &json!({"schemaVersion":2,"enabled":true,"model":"fixture",
        "endpoint":"https://example.test/chat/completions","account":format!("setup-{}", "a".repeat(64))}),
    )?;
    fs::remove_file(f.file("state"))?;
    fs::create_dir(f.file("state"))?;
    fs::write(
        f.file("state/migration-active.json"),
        "unresolved migration",
    )?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-pause-saved",
        &f.home,
        "100",
        "0",
    )?;
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(profile)?)?;
    assert_eq!(saved["enabled"], false);
    assert_eq!(
        fs::read_to_string(f.file("state/migration-active.json"))?,
        "unresolved migration"
    );
    let maintenance = f.roots_command().arg("history").output()?;
    assert_code(&maintenance, 3);
    assert_eq!(fs::read_to_string(marker)?, "unresolved migration");
    assert!(!f.file("new-maintenance-state").exists());
    Ok(())
}

#[test]
fn setup_resolves_its_explicit_xdg_and_default_configuration_paths() -> TestResult {
    let f = Fixture::new()?;
    let xdg = f.file("xdg-config");
    let home_default = f.home.join(".config/macops");
    for (directory, model) in [
        (&xdg.join("macops"), "xdg-model"),
        (&home_default, "default-model"),
    ] {
        fs::create_dir_all(directory)?;
        write_json(
            &directory.join("recommend-model.json"),
            &json!({"schemaVersion":2,"enabled":true,
            "endpoint":"https://example.test/chat/completions","model":model,"account":format!("setup-{}", "a".repeat(64))}),
        )?;
    }
    for (variable, explicit, model) in [
        (xdg.as_path(), None, "xdg-model"),
        (xdg.as_path(), Some(home_default.as_path()), "default-model"),
        (
            std::path::Path::new("relative/ignored"),
            None,
            "default-model",
        ),
    ] {
        let mut run = f.roots_command();
        run.env("XDG_CONFIG_HOME", variable);
        if let Some(config) = explicit {
            run.arg("--config-dir").arg(config);
        }
        let out = run.args(["recommend", "--setup"]).output()?;
        assert_code(&out, 0);
        let reply: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(reply["data"]["model"], model);
        assert_eq!(
            reply["warnings"].as_array().ok_or("warnings")?.is_empty(),
            variable.is_absolute()
        );
    }
    let bad_base = f
        .roots_command()
        .env("XDG_CONFIG_HOME", "/Users/example/job/config")
        .args(["recommend", "--setup"])
        .output()?;
    assert_code(&bad_base, 1);
    let envelope: serde_json::Value = serde_json::from_slice(&bad_base.stdout)?;
    assert_eq!(envelope["error"]["code"], "invalid_scope_path");
    let bad_home = f
        .roots_command()
        .args(["--home", "relative-home", "recommend", "--setup"])
        .output()?;
    assert_code(&bad_home, 1);
    let envelope: serde_json::Value = serde_json::from_slice(&bad_home.stdout)?;
    assert_eq!(envelope["error"]["code"], "absolute_path_required");
    let missing_home = f
        .roots_command()
        .env_remove("HOME")
        .args(["recommend", "--setup"])
        .output()?;
    assert_code(&missing_home, 1);
    let envelope: serde_json::Value = serde_json::from_slice(&missing_home.stdout)?;
    assert_eq!(envelope["error"]["code"], "home_missing");
    let ambiguous_config = f
        .roots_command()
        .arg("--config-dir")
        .arg(f.home.join("../other-config"))
        .args(["recommend", "--setup"])
        .output()?;
    assert_code(&ambiguous_config, 1);
    let envelope: serde_json::Value = serde_json::from_slice(&ambiguous_config.stdout)?;
    assert_eq!(envelope["error"]["code"], "parent_path_forbidden");
    assert!(!f.home.join(".local/state/macops").exists());
    assert!(!home_default.join("recommend-model.lock").exists());
    Ok(())
}

#[test]
fn setup_write_lock_is_shared_by_config_roots_across_processes() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new()?;
    let config = f.file("alternate-config");
    fs::create_dir(&config)?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    let profile = config.join("recommend-model.json");
    write_json(
        &profile,
        &json!({"schemaVersion":2,"enabled":true,"model":"fixture",
        "endpoint":"https://example.test/chat/completions","account":format!("setup-{}", "a".repeat(64))}),
    )?;
    let before = fs::read(&profile)?;
    let lease = f.home.join(".config/macops/recommend-model.lock");
    fs::create_dir_all(&lease)?;
    fs::set_permissions(&lease, fs::Permissions::from_mode(0o700))?;
    let args = [
        "--config-dir",
        config.to_str().ok_or("config path")?,
        "recommend",
        "--setup",
    ];
    chezmoi_native::run_browser_pty(&f, &args, "setup-write-busy", &lease, "100", "3")?;
    assert_eq!(fs::read(&profile)?, before);
    chezmoi_native::run_browser_pty(&f, &args, "setup-pause-saved", &f.home, "100", "0")?;
    let after: serde_json::Value = serde_json::from_slice(&fs::read(&profile)?)?;
    assert_eq!(after["enabled"], false);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn setup_pauses_saved_profile_and_recovery_keeps_its_identity() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let f = Fixture::new()?;
    let config = f.home.join(".config/macops");
    std::fs::create_dir_all(&config)?;
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700))?;
    let profile_file = config.join("recommend-model.json");
    let mut profile = json!({
        "schemaVersion":1,
        "enabled":true,
        "endpoint":"https://models.example.test/v1/chat/completions",
        "model":"fixture",
        "account":format!("setup-{}", "b".repeat(64))
    });
    write_json(&profile_file, &profile)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-pause-saved",
        &f.home,
        "100",
        "0",
    )?;
    profile["enabled"] = json!(false);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&profile_file)?)?,
        profile
    );
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "--setup"],
        "setup-recover-clean",
        &f.home,
        "100",
        "0",
    )?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(&profile_file)?)?,
        profile
    );
    assert!(!config.join("recommend-model-pending.json").exists());
    Ok(())
}

#[test]
fn recommendation_only_displays_unverified_command_recipes() -> TestResult {
    let f = Fixture::new()?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "demo"],
        "recommend-advice-only",
        &f.home,
        "100",
        "0",
    )?;
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());
    Ok(())
}

fn fixed_native_recipe(
    f: &Fixture,
    name: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let lock = f.home.join(".agents/.skill-lock.json");
    let mut native: serde_json::Value = serde_json::from_slice(&fs::read(&lock)?)?;
    let row = native["skills"][name].as_object_mut().ok_or("row")?;
    let commit = row
        .get("commit")
        .or_else(|| row.get("ref"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        .to_owned();
    for field in ["installedAt", "updatedAt"] {
        row.entry(field)
            .or_insert(json!("2026-11-20T00:00:00.000Z"));
    }
    row.entry("installMode").or_insert(json!("copy"));
    let repository = row["source"].as_str().ok_or("repository")?.to_owned();
    let directory = row
        .get("skillPath")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("skills/demo")
        .trim_end_matches("/SKILL.md")
        .to_owned();
    write_json(&lock, &native)?;
    let entry = f.file("native-runner/skills/bin/cli.mjs");
    fs::create_dir_all(entry.parent().ok_or("bin")?)?;
    fs::write(
        &entry,
        b"// Synthetic fixed entry; adoption never executes this file.\n",
    )?;
    fs::write(
        f.file("native-runner/skills/package.json"),
        br#"{"name":"skills","version":"1.9.0"}"#,
    )?;
    let node = f.script(
        "fixed-native-node",
        r#"if [ "$1" = --version ];then printf 'v26.13.1\n';exit 0;fi
if [ "$2" = --version ];then printf '1.9.0\n';exit 0;fi
printf 'unexpected-native-call' > "$MACOPS_TEST_HOME/cap-poison";exit 97"#,
    )?;
    let recipe = f.file(&format!("{name}-native-recipe.json"));
    write_json(
        &recipe,
        &json!({"schemaVersion":1,"id":name,"owner":"skills-cli","cliVersion":"1.9.0",
        "source":{"repository":repository,"commit":commit,"skillDirectory":directory,"memberName":name,"trackingRef":null},
        "installName":name,"scope":"global","clientScope":["codex"],"copy":true,"manualOnly":false,
        "licenseFiles":[],"derived":null,"runner":{"node":node,"entry":entry}}),
    )?;
    Ok(recipe)
}

fn slow_installer(f: &Fixture) -> TestResult {
    f.script("demo",r#"case "$1" in
query) if [ -f "$2" ]; then printf '{"status":"present","version":"%s"}\n' "$(/bin/cat "$2")"; else printf '{"status":"missing"}\n'; fi ;;
install) printf '%s' "$4" > "$2"; printf 'install\n' >> "$3"; /bin/sleep 30 ;;
*) exit 7 ;;
esac"#)?;
    Ok(())
}

fn wait_for_call(f: &Fixture) -> TestResult {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match fs::read_to_string(f.file("calls")) {
            Ok(value) if value == "install\n" => return Ok(()),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        if std::time::Instant::now() > deadline {
            return Err("installer never published a complete call record".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn call_wait_observes_a_complete_record_after_empty_and_partial_publication() -> TestResult {
    let f = Fixture::new()?;
    fs::write(f.file("calls"), b"")?;
    let path = f.file("calls");
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        fs::write(&path, b"install")?;
        std::thread::sleep(std::time::Duration::from_millis(200));
        fs::write(path, b"install\n")
    });
    let waited = wait_for_call(&f);
    let observed = fs::read_to_string(f.file("calls"));
    writer.join().map_err(|_| "fixture writer panicked")??;
    waited?;
    assert_eq!(observed?, "install\n");
    Ok(())
}

#[test]
fn empty_call_file_without_a_writer_is_never_a_completion_signal() -> TestResult {
    let f = Fixture::new()?;
    fs::write(f.file("calls"), b"")?;
    assert!(wait_for_call(&f).is_err());
    assert_eq!(fs::read_to_string(f.file("calls"))?, "");
    Ok(())
}

#[test]
fn timeout_stays_unknown_then_recovers_using_frozen_recipe() -> TestResult {
    let f = Fixture::new()?;
    slow_installer(&f)?;
    let (out, v) = f.run(&[
        "--timeout-secs",
        "1",
        "app",
        "install",
        "demo",
        "--version",
        "1.0.0",
    ])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "unknown");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    let active_before = fs::read(f.file("state/active.json"))?;
    let (out, v) = f.run(&["app", "refresh", "demo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["mode"], "dry-run");
    assert_eq!(fs::read(f.file("state/active.json"))?, active_before);
    let journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?;
    fs::remove_file(&f.catalog)?;
    let (out, v) = f.run(&["recover", id])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn auto_recovery_settles_only_before_writes_and_never_repeats_the_same_install() -> TestResult {
    let f = Fixture::new()?;
    let install = ["app", "install", "demo", "--version", "1.0.0"];
    let (out, data) = f.run(&install)?;
    assert_code(&out, 0);
    let mut original = data["data"].clone();
    original["phase"] = json!("unknown");
    let active = f.file("state/active.json");
    write_json(&active, &original)?;
    let before = fs::read(&active)?;
    assert_code(&f.run(&["history"])?.0, 0);
    assert_code(&f.run(&["app", "refresh", "demo", "--dry-run"])?.0, 0);
    assert_eq!(fs::read(&active)?, before);

    let settings = f.file("settings");
    let other_settings = f.file("other-settings");
    fs::create_dir_all(other_settings.join("manifests"))?;
    let command = f.command();
    let other_scope = || {
        let mut other = std::process::Command::new(command.get_program());
        other.args(command.get_args().map(|arg| {
            if arg == settings.as_os_str() {
                other_settings.as_os_str()
            } else {
                arg
            }
        }));
        other.envs(
            command
                .get_envs()
                .filter_map(|(name, value)| value.map(|value| (name, value))),
        );
        other
    };
    let output = other_scope().args(install).output()?;
    assert_code(&output, 3);
    let blocked: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(blocked["error"]["code"], "recovery_required");
    let message = blocked["error"]["message"]
        .as_str()
        .ok_or("error message")?;
    assert!(message.contains("上一次的目录选项"));
    assert!(!message.contains(original["id"].as_str().ok_or("operation ID")?));
    assert_eq!(fs::read(&active)?, before);

    let id = original["id"].as_str().ok_or("operation ID")?;
    let history = f.file(&format!("state/operations/{id}.json"));
    let history_before = fs::read(&history)?;
    for extra in [&[][..], &["--mark-failed", "--yes"][..]] {
        let output = other_scope().args(["recover", id]).args(extra).output()?;
        assert_code(&output, 3);
        let blocked: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(blocked["error"]["code"], "recovery_scope_changed");
        assert_eq!(fs::read(&active)?, before);
        assert_eq!(fs::read(&history)?, history_before);
    }

    let (out, blocked) = f.run(&["agent", "resume", "OP-unrelated", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(blocked["error"]["code"], "recovery_required");
    assert_eq!(fs::read(&active)?, before);

    let (out, deferred) = f.run(&["app", "uninstall", "unselected"])?;
    assert_code(&out, 3);
    assert_eq!(deferred["error"]["code"], "confirmation_required");
    assert_eq!(fs::read(&active)?, before);
    assert_eq!(fs::read(&history)?, history_before);

    let (out, picker) = f.run(&["app", "adopt"])?;
    assert_code(&out, 3);
    assert_eq!(picker["error"]["code"], "adopt_target_required");
    assert_eq!(fs::read(&active)?, before);

    let (out, checked) = f.run(&install)?;
    assert_code(&out, 0);
    assert_eq!(checked["data"]["id"], original["id"]);
    assert_eq!(checked["data"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");

    write_json(&active, &original)?;
    let (out, changed_version) = f.run(&["app", "install", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(changed_version["error"]["code"], "already_installed");
    assert_eq!(f.read_json("state/active.json")?["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");

    write_json(&active, &original)?;
    let (out, refreshed) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(refreshed["data"]["results"][0]["version"], "2.0.0");
    let settled = f.read_json("state/active.json")?;
    assert_eq!(settled["id"], original["id"]);
    assert_eq!(settled["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");

    write_json(&active, &original)?;
    let mut catalog = f.read_json("catalog.json")?;
    let mut duplicate = catalog["packages"][0].clone();
    duplicate["id"] = json!("fixture:another-demo");
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(duplicate);
    write_json(&f.catalog, &catalog)?;
    let (out, ambiguous) = f.run(&install)?;
    assert_code(&out, 3);
    assert_eq!(ambiguous["error"]["code"], "operation_checked");
    assert_eq!(f.read_json("state/active.json")?["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn uninstall_retry_uses_the_verified_original_even_when_the_recipe_was_removed() -> TestResult {
    let f = Fixture::new()?;
    assert_code(
        &f.run(&["app", "install", "demo", "--version", "1.0.0"])?.0,
        0,
    );
    let uninstall = ["app", "uninstall", "demo", "--yes"];
    let (out, first) = f.run(&uninstall)?;
    assert_code(&out, 0);
    let mut pending = first["data"].clone();
    pending["phase"] = json!("unknown");
    write_json(&f.file("state/active.json"), &pending)?;
    write_json(&f.catalog, &json!({"schema_version":1,"packages":[]}))?;

    let (out, checked) = f.run(&uninstall)?;
    assert_code(&out, 0);
    assert_eq!(checked["data"]["id"], pending["id"]);
    assert_eq!(checked["data"]["phase"], "succeeded");
    assert!(!f.file("version").exists());
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\nuninstall\n");
    Ok(())
}

#[test]
fn native_installer_progress_precedes_completion_and_json_is_clean() -> TestResult {
    for scenario in ["native-progress", "native-progress-json"] {
        let f = Fixture::new()?;
        let release = f.home.join("release-progress");
        f.script("demo", r#"
case "$1" in
query)
  if [ -f "$2" ]; then printf '{"status":"present","version":"%s"}\n' "$(/bin/cat "$2")"; else printf '{"status":"missing"}\n'; fi ;;
install)
  printf 'fixture stdout progress\n\344'
  printf 'fixture stderr progress\n' >&2
  /usr/bin/touch "$MACOPS_TEST_HOME/ready-progress"
  while [ ! -f "$MACOPS_TEST_HOME/continue-progress" ]; do /bin/sleep 0.05; done
  printf '\270\255\033\007 split text\n'
  while [ ! -f "$MACOPS_TEST_HOME/release-progress" ]; do /bin/sleep 0.05; done
  printf '%s' "$4" > "$2"
  printf 'install\n' >> "$3" ;;
*) exit 9 ;;
esac"#)?;
        let mut args = vec!["app", "install", "demo", "--version", "1.0.0"];
        if scenario == "native-progress-json" {
            args.insert(0, "--json");
        }
        chezmoi_native::run_browser_pty(&f, &args, scenario, &release, "100", "0")?;
        assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    }
    Ok(())
}

#[test]
fn killed_parent_leaves_live_group_blocking_other_writes() -> TestResult {
    use std::process::Stdio;
    let f = Fixture::new()?;
    slow_installer(&f)?;
    let mut parent = f
        .command()
        .args(["app", "install", "demo", "--version", "1.0.0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_for_call(&f)?;
    let journal = f.read_json("state/active.json")?;
    let group = i32::try_from(journal["process_group"].as_u64().ok_or("group")?)?;
    let parent_id = i32::try_from(parent.id())?;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(parent_id),
        nix::sys::signal::Signal::SIGKILL,
    )?;
    parent.wait()?;
    let id = journal["id"].as_str().ok_or("id")?;
    let (out, v) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "operation_in_flight");
    let (out, v) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "recovery_required");
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(group),
        nix::sys::signal::Signal::SIGKILL,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    loop {
        let (out, v) = f.run(&["recover", id])?;
        if out.status.success() {
            assert_eq!(v["data"]["phase"], "succeeded");
            break;
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("group did not exit: {v}").into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn valid_worker_journal_without_gate_never_executes() -> TestResult {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
    };
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?.to_owned();
    journal["phase"] = json!("running");
    journal["process_group"] = json!("PGID_TOKEN");
    let template = f.file("journal-template.json");
    write_json(&template, &journal)?;
    let path = f.file(&format!("state/operations/{id}.json"));
    fs::remove_file(f.file("version"))?;
    fs::remove_file(f.file("calls"))?;
    let out=Command::new("/bin/sh").arg("-c").arg("/usr/bin/sed \"s/\\\"PGID_TOKEN\\\"/$$/\" \"$1\" > \"$2\"; exec \"$3\" __worker \"$2\"").arg("gate-test").arg(&template).arg(&path).arg(env!("CARGO_BIN_EXE_macops")).process_group(0).stdin(Stdio::null()).output()?;
    assert_code(&out, 1);
    assert!(!f.file("calls").exists());
    assert!(!f.file("version").exists());
    Ok(())
}

#[test]
fn corrupt_active_journal_blocks_writes() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir_all(f.file("state"))?;
    fs::write(f.file("state/active.json"), "[]")?;
    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 1);
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn nonzero_backend_and_unverified_success_do_not_create_receipt() -> TestResult {
    let f = Fixture::new()?;
    f.script(
        "demo",
        r#"case "$1" in query) printf '{"status":"missing"}\n';; install) exit 0;; esac"#,
    )?;
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "unknown");
    assert!(!f.file("state/receipts.json").exists());
    let journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?;
    let (out, _) = f.run(&["recover", id, "--resolve-failed", "--yes"])?;
    assert_code(&out, 3);
    f.script(
        "demo",
        r#"case "$1" in query) printf '{"status":"missing"}\n';; install) exit 9;; esac"#,
    )?;
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "unknown");
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn native_channels_are_planned_and_verified_through_fake_managers() -> TestResult {
    for manager in ["brew", "mise", "npm", "uv", "bun", "cargo"] {
        let f = Fixture::new()?;
        let native_version = f.home.join("bin/native-version");
        fs::write(&native_version, "1.0.0")?;
        if manager == "npm" {
            npm_manifest(&f.home.join("npm-global"), "demo", "1.0.0")?;
        }
        for binary in [manager, "npm"] {
            f.script(binary,r#"base=$(/usr/bin/dirname "$0")
v=$(/bin/cat "$base/native-version")
case "$1" in
ls) if [ "$0" = "$base/mise" ]; then printf '{"demo":[{"version":"%s","active":true}]}' "$v"; else exit 98; fi ;;
info) printf '{"formulae":[{"name":"demo","installed":[{"version":"%s"}],"versions":{"stable":"2.0.0"}}],"casks":[]}' "$v" ;;
list) printf 'demo %s\n' "$v" ;;
prefix) printf '%s' "$MACOPS_TEST_HOME/npm-global" ;;
latest) printf '2.0.0' ;;
view) printf '"2.0.0"' ;;
pm) printf '/fixture/global node_modules (1)\n└── demo@%s\n' "$v" ;;
tool) if [ "$2" = list ]; then printf 'demo v%s\n' "$v"; else printf '2.0.0' > "$base/native-version"; fi ;;
install) if [ "${2:-}" = --list ]; then printf 'demo v%s:\n    demo\n' "$v"; else
 if [ "$0" = "$base/npm" ]; then printf '{"name":"demo","version":"2.0.0"}' > "$4/lib/node_modules/demo/package.json"; fi
 printf '2.0.0' > "$base/native-version"; fi ;;
use|upgrade|add) printf '2.0.0' > "$base/native-version" ;;
*) exit 9 ;;
esac"#)?;
        }
        let mut catalog = f.read_json("catalog.json")?;
        let provider = match manager {
            "brew" => json!({"type":"brew","package":"demo"}),
            "mise" => json!({"type":"mise","package":"demo"}),
            "npm" => json!({"type":"npm","package":"demo","prefix":f.home.join("npm-global")}),
            "uv" => json!({"type":"uv","package":"demo"}),
            "bun" => json!({"type":"bun","package":"demo"}),
            "cargo" => json!({"type":"cargo","package":"demo","binary":"demo"}),
            _ => return Err("unexpected manager".into()),
        };
        catalog["packages"][0]["provider"] = provider;
        write_json(&f.catalog, &catalog)?;
        if manager == "brew" {
            for verb in ["adopt", "upgrade", "uninstall"] {
                let (out, value) = f.run(&["app", verb, "fixture:demo", "--yes"])?;
                assert_code(&out, 3);
                assert_eq!(value["error"]["code"], "brew_context_binding_required");
            }
            assert_eq!(fs::read_to_string(&native_version)?, "1.0.0");
            assert!(!f.file("state/receipts.json").exists());
            continue;
        }
        if manager == "uv" {
            let (out, value) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
            assert_code(&out, 3);
            assert!(value.to_string().contains("legacy_uv_query_unbound"));
            let (out, _) = f.run(&["app", "upgrade", "fixture:demo", "--version", "2.0.0"])?;
            assert_code(&out, 3);
            assert_eq!(fs::read_to_string(&native_version)?, "1.0.0");
            assert!(!f.file("state/receipts.json").exists());
            continue;
        }
        let (out, _) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
        assert_code(&out, 0);
        let args = if manager == "brew" {
            vec!["app", "upgrade", "fixture:demo"]
        } else {
            vec!["app", "upgrade", "fixture:demo", "--version", "2.0.0"]
        };
        let (out, v) = f.run(&args)?;
        assert_code(&out, 0);
        assert_eq!(v["data"]["phase"], "succeeded", "{manager}: {v}");
        assert_eq!(fs::read_to_string(&native_version)?, "2.0.0");
    }
    Ok(())
}

#[test]
fn recover_rebuilds_missing_or_lagging_history_from_active() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?.to_owned();
    journal["phase"] = json!("prepared");
    journal["process_group"] = serde_json::Value::Null;
    write_json(&f.file("state/active.json"), &journal)?;
    fs::remove_file(f.file(&format!("state/operations/{id}.json")))?;
    let original = fs::read(f.file("state/active.json"))?;
    let (out, listing) = f.run(&["recover"])?;
    assert_code(&out, 0);
    assert_eq!(listing["data"]["pending"], json!([journal]));
    assert_eq!(listing["data"]["persistentWrites"], false);
    assert_eq!(fs::read(f.file("state/active.json"))?, original);
    assert!(!f.file(&format!("state/operations/{id}.json")).exists());
    fs::remove_file(f.file("version"))?;
    fs::remove_file(f.file("state/receipts.json"))?;
    let (out, v) = f.run(&["recover", &id])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "failed");
    assert!(f.file(&format!("state/operations/{id}.json")).exists());
    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut active = f.read_json("state/active.json")?;
    let id = active["id"].as_str().ok_or("id")?.to_owned();
    active["phase"] = json!("running");
    let mut history = active.clone();
    history["phase"] = json!("prepared");
    history["process_group"] = serde_json::Value::Null;
    write_json(&f.file("state/active.json"), &active)?;
    write_json(&f.file(&format!("state/operations/{id}.json")), &history)?;
    let calls = fs::read(f.file("calls"))?;
    let (out, v) = f.run(&["recover", &id])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(calls, fs::read(f.file("calls"))?);
    Ok(())
}

#[test]
fn adopt_receipt_commit_and_old_terminal_recovery_are_consistent() -> TestResult {
    let f = Fixture::new()?;
    fs::write(f.file("version"), "1.0.0")?;
    let (out, _) = f.run(&["app", "adopt", "demo", "--yes"])?;
    assert_code(&out, 0);
    let mut journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?.to_owned();
    journal["phase"] = json!("prepared");
    write_json(&f.file("state/active.json"), &journal)?;
    fs::remove_file(f.file(&format!("state/operations/{id}.json")))?;
    let (out, v) = f.run(&["recover", &id])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    let (out, _) = f.run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    let latest = f.read_json("state/active.json")?;
    let (out, _) = f.run(&["recover", &id])?;
    assert_code(&out, 0);
    assert_eq!(latest, f.read_json("state/active.json")?);
    Ok(())
}

#[test]
fn query_and_installer_receive_frozen_home() -> TestResult {
    let f = Fixture::new()?;
    f.script("demo",r#"printf '%s' "$HOME" > "$MACOPS_TEST_HOME/observed-home"
printf '%s' "${UV_TOOL_DIR:-UNSET}" > "$MACOPS_TEST_HOME/observed-uv"
case "$1" in
query) if [ -f "$2" ]; then printf '{"status":"present","version":"1.0.0"}'; else printf '{"status":"missing"}'; fi ;;
install) printf '1.0.0' > "$2" ;;
esac"#)?;
    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_to_string(f.home.join("observed-home"))?,
        f.home.display().to_string()
    );
    assert_eq!(fs::read_to_string(f.home.join("observed-uv"))?, "UNSET");
    Ok(())
}

#[test]
fn garbage_native_output_blocks_install_and_unrelated_brew_identity_is_unknown() -> TestResult {
    for manager in ["uv", "cargo", "brew"] {
        let f = Fixture::new()?;
        let script = if manager == "brew" {
            "printf '%s' '{\"formulae\":[{\"name\":\"unrelated\",\"installed\":[]}],\"casks\":[]}'"
        } else {
            "printf 'backend schema changed - NOT AN INVENTORY'"
        };
        f.script(manager, script)?;
        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"] = if manager == "cargo" {
            json!({"type":manager,"package":"demo","binary":"demo"})
        } else {
            json!({"type":manager,"package":"demo"})
        };
        write_json(&f.catalog, &catalog)?;
        let args = if manager == "brew" {
            vec!["app", "install", "demo"]
        } else {
            vec!["app", "install", "demo", "--version", "1.0.0"]
        };
        let (out, v) = f.run(&args)?;
        assert_code(&out, 3);
        if manager == "uv" {
            assert_eq!(v["error"]["code"], "uv_write_blocked_review");
        } else if manager == "brew" {
            assert_eq!(v["error"]["code"], "brew_context_binding_required");
        } else {
            assert_eq!(v["error"]["code"], "observation_required");
        }
        assert!(!f.file("state").exists());
        assert!(!f.file("calls").exists());
    }
    Ok(())
}

#[test]
fn native_npm_absence_is_accepted_but_errors_are_not() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] =
        json!({"type":"npm","package":"demo","prefix":f.home.join("npm")});
    write_json(&f.catalog, &catalog)?;
    fs::create_dir(f.home.join("npm"))?;
    f.script("npm", "exit 98")?;
    let (out, v) = f.run(&["app", "doctor", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["observation"]["status"], "missing");
    fs::create_dir_all(f.home.join("npm/lib/node_modules/demo"))?;
    let (out, v) = f.run(&["app", "doctor", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["observation"]["status"], "unknown");
    Ok(())
}

#[test]
fn fast_oversized_write_never_commits_receipt() -> TestResult {
    let f = Fixture::new()?;
    f.script("demo",r#"case "$1" in
query) if [ -f "$2" ]; then printf '{"status":"present","version":"1.0.0"}'; else printf '{"status":"missing"}'; fi ;;
install) printf 1.0.0 > "$2"; /bin/dd if=/dev/zero bs=1048576 count=5 2>/dev/null ;;
esac"#)?;
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(v["data"]["phase"], "unknown");
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn app_info_unknown_matches_doctor_exit_contract() -> TestResult {
    let f = Fixture::new()?;
    f.script("demo", "printf 'invalid-json'")?;
    let (out, v) = f.run(&["app", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["status"], "attention");
    assert_eq!(v["data"]["observation"]["status"], "unknown");
    Ok(())
}

#[test]
fn native_read_selection_is_preserved_without_running_the_old_backend() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo", "other"])?;
    let directory = f.home.join(".local/bin");
    fs::create_dir_all(&directory)?;
    let script = f.script(
        "legacy",
        r#"printf 'called' > "$MACOPS_TEST_HOME/last-args";exit 97"#,
    )?;
    fs::copy(script, directory.join("mo"))?;
    for args in [
        vec!["skill", "doctor", "demo"],
        vec!["skill", "--only", "demo"],
        vec!["skill", "doctor", "--only", "demo"],
    ] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, 0);
        assert_eq!(value["data"]["backend"], "rust");
        let resources = value["data"]["resources"].as_array().ok_or("resources")?;
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["native_id"], "demo");
        assert!(!f.home.join("last-args").exists());
    }
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn fifo_state_is_rejected_without_waiting_for_a_writer() -> TestResult {
    use std::process::{Command, Stdio};
    let f = Fixture::new()?;
    fs::create_dir_all(f.file("state"))?;
    assert!(
        Command::new("/usr/bin/mkfifo")
            .arg(f.file("state/active.json"))
            .status()?
            .success()
    );
    let mut child = f
        .command()
        .args(["app", "install", "demo", "--version", "1.0.0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait()? {
            assert_eq!(status.code(), Some(1));
            break;
        }
        if std::time::Instant::now() > end {
            child.kill()?;
            child.wait()?;
            return Err("FIFO input blocked instead of failing".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn native_ai_oversized_query_is_attention_not_success() -> TestResult {
    let f = Fixture::new()?;
    let directory = f.home.join(".local/bin");
    fs::create_dir_all(&directory)?;
    let source = f.script(
        "codex",
        "/bin/dd if=/dev/zero bs=1048576 count=5 2>/dev/null",
    )?;
    assert!(source.is_file());
    let (out, v) = f.run(&["agent"])?;
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(v["status"], "attention");
    assert_eq!(v["data"]["incomplete"], true);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn npm_missing_query_reaches_exact_scoped_install() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] =
        json!({"type":"npm","package":"demo","prefix":f.home.join("npm-prefix")});
    write_json(&f.catalog, &catalog)?;
    f.script(
        "npm",
        r#"case "$1" in
ls) exit 98 ;;
prefix) printf '%s' "$MACOPS_TEST_HOME/npm-prefix" ;;
install)
 [ "$2" = --global ] && [ "$3" = --prefix ] && [ "$4" = "$HOME/npm-prefix" ]
 /bin/mkdir -p "$4/lib/node_modules/demo"
 printf '{"name":"demo","version":"%s"}' "${5##*@}" > "$4/lib/node_modules/demo/package.json"
 printf '%s
' "$5" >> "$MACOPS_TEST_HOME/installed" ;;
uninstall)
 [ "$4" = "$HOME/npm-prefix" ] && [ "$5" = demo ]
 /bin/rm "$4/lib/node_modules/demo/package.json"
 /bin/rmdir "$4/lib/node_modules/demo" ;;
*) exit 8 ;;
esac"#,
    )?;
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(
        fs::read_to_string(f.home.join("installed"))?,
        "demo@1.0.0\n"
    );
    assert_eq!(
        v["data"]["plan"]["query"]["namespace"]["anchor"]["kind"],
        "create-prefix"
    );
    let receipt = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipt["installations"]["fixture:demo"]["query"]["namespace"]["anchor"]["kind"],
        "existing-prefix"
    );
    let (out, _) = f.run(&["app", "upgrade", "fixture:demo", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    let (out, _) = f.run(&["app", "uninstall", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_to_string(f.home.join("installed"))?,
        "demo@1.0.0\ndemo@2.0.0\n"
    );
    assert_code(
        &f.run(&["app", "install", "demo", "--version", "3.0.0"])?.0,
        0,
    );
    chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-app-uninstall",
        std::path::Path::new("demo"),
        "160",
        "0",
    )?;
    assert!(!f.home.join("npm-prefix/lib/node_modules/demo").exists());
    assert!(
        f.read_json("state/receipts.json")?["installations"]
            .get("fixture:demo")
            .is_none()
    );
    Ok(())
}

#[test]
fn changed_scope_cannot_upgrade_a_different_installation() -> TestResult {
    let f = Fixture::new()?;
    let mut command = f.command();
    let out = command
        .env("HOME", &f.home)
        .env("UV_TOOL_DIR", f.home.join("scope-a"))
        .args(["app", "install", "demo", "--version", "1.0.0"])
        .output()?;
    assert_code(&out, 0);
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("UV_TOOL_DIR", f.home.join("scope-b"))
        .args(["app", "upgrade", "demo", "--version", "2.0.0"])
        .output()?;
    assert_code(&out, 3);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["error"]["code"], "manager_scope_changed");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn conflicting_frozen_plan_is_not_merged_as_a_history_repair() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut first = f.read_json("state/active.json")?;
    let id = first["id"].as_str().ok_or("id")?.to_owned();
    let (out, _) = f.run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    let mut different = f.read_json("state/active.json")?;
    first["phase"] = json!("unknown");
    different["id"] = json!(id);
    write_json(&f.file("state/active.json"), &first)?;
    write_json(&f.file(&format!("state/operations/{id}.json")), &different)?;
    let calls = fs::read(f.file("calls"))?;
    let (out, v) = f.run(&["recover", &id])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "operation_conflict");
    assert_eq!(calls, fs::read(f.file("calls"))?);
    Ok(())
}

#[test]
fn proxy_symlink_preserves_the_invocation_name() -> TestResult {
    let f = Fixture::new()?;
    f.script("proxy",r#"case "$0" in */cargo) printf 'cargo 1.0.0';; *) printf 'wrong invocation';exit 9;; esac"#)?;
    symlink("proxy", f.home.join("bin/cargo"))?;
    let (out, v) = f.run(&["app", "doctor", "manager:cargo"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"][0]["observation"]["version"], "cargo 1.0.0");
    Ok(())
}

#[test]
fn mise_target_array_and_ambiguous_active_versions_are_distinguished() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"aqua:openai/codex"});
    write_json(&f.catalog, &catalog)?;
    f.script("mise", r#"printf '[{"version":"1.2.3","active":true}]'"#)?;
    let (out, v) = f.run(&["app", "doctor", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"][0]["observation"]["version"], "1.2.3");
    f.script(
        "mise",
        r#"printf '[{"version":"1.2.3","active":true},{"version":"2.0.0","active":true}]'"#,
    )?;
    let (out, v) = f.run(&["app", "doctor", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["observation"]["status"], "unknown");
    Ok(())
}

#[test]
fn unbound_brew_declaration_never_queries_or_blesses_missing() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"brew","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    f.script(
        "brew",
        "printf invoked >> \"$MACOPS_TEST_HOME/calls\"; exit 1",
    )?;
    for command in ["doctor", "refresh", "install", "upgrade", "uninstall"] {
        let (out, value) = f.run(&["app", command, "demo", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "brew_context_binding_required");
    }
    let (out, preview) = f.run(&["app", "upgrade", "demo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["nativeRuns"], 0);
    assert!(!f.home.join("calls").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn bun_uninitialized_empty_scope_can_install_without_init() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"bun","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    f.script("bun",r#"case "$1" in
pm) if [ -f "$MACOPS_TEST_HOME/bun-installed" ]; then printf '/fixture/global node_modules (1)\n└── demo@1.0.0\n';else printf 'error: No package.json was found for directory "%s/.bun/install/global"\n' "$HOME" >&2;exit 1;fi ;;
add) [ "$2" = --global ] && [ "$3" = demo@1.0.0 ];printf installed > "$MACOPS_TEST_HOME/bun-installed" ;;
*) exit 7 ;;
esac"#)?;
    let (out, v) = f.run(&["app", "doctor", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["observation"]["status"], "missing");
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    Ok(())
}

#[test]
fn metadata_refresh_never_overwrites_unowned_cache_files() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir_all(f.file("cache"))?;
    let bytes = b"{\"schema_version\":1,\"entries\":{}}";
    fs::write(f.file("cache/updates.json"), bytes)?;
    let (out, v) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    assert!(v["data"]["index"].is_string());
    assert_eq!(fs::read(f.file("cache/updates.json"))?, bytes);
    let (out, v) = f.run(&["cleanup", "macops:cache", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["review"], json!(["updates.json"]));
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(fs::read(f.file("cache/updates.json"))?, bytes);
    Ok(())
}

#[test]
fn generic_mise_write_cannot_bypass_chezmoi_source_owner() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    let source = f
        .home
        .join(".local/share/chezmoi/home/dot_config/mise/private_config.toml");
    fs::create_dir_all(source.parent().ok_or("parent")?)?;
    fs::write(&source, "[tools]\ndemo='1.0.0'\n")?;
    f.script("mise",r#"case "$1" in ls) printf '[{"version":"1.0.0","active":true}]';; *) printf forbidden > "$MACOPS_TEST_HOME/should-not-exist";; esac"#)?;
    let (out, _) = f.run(&["app", "adopt", "demo", "--yes"])?;
    assert_code(&out, 0);
    let (out, v) = f.run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "configuration_owner_requires_review");
    assert!(!f.home.join("should-not-exist").exists());
    assert_eq!(fs::read_to_string(source)?, "[tools]\ndemo='1.0.0'\n");
    Ok(())
}

#[test]
fn bun_corrupt_or_duplicate_inventory_never_proves_absence() -> TestResult {
    for payload in [
        "node_modules INVENTORY WAS CORRUPTED",
        "/fixture/global node_modules (1)",
        "/fixture/global node_modules (0)\n└── demo@1.0.0",
        "/fixture/global node_modules (2)\n├── demo@1.0.0\n└── demo@2.0.0",
    ] {
        let f = Fixture::new()?;
        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"] = json!({"type":"bun","package":"demo"});
        write_json(&f.catalog, &catalog)?;
        let text = f.home.join("inventory.txt");
        fs::write(&text, payload)?;
        f.script("bun",r#"if [ "$1" = pm ]; then /bin/cat "$MACOPS_TEST_HOME/inventory.txt";else printf BAD > "$MACOPS_TEST_HOME/mutation";fi"#)?;
        let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
        assert_code(&out, 3);
        assert_eq!(v["error"]["code"], "observation_required");
        assert!(!f.home.join("mutation").exists());
        assert!(!f.file("state").exists());
    }
    Ok(())
}

#[test]
fn outdated_only_keeps_the_original_read_filter() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo", "other"])?;
    let directory = f.home.join(".local/bin");
    fs::create_dir_all(&directory)?;
    let source = f.script(
        "mo-read",
        r#"printf 'called' > "$MACOPS_TEST_HOME/read-args";exit 97"#,
    )?;
    fs::copy(source, directory.join("mo"))?;
    let (out, value) = f.run(&["skill", "outdated", "--only", "demo"])?;
    assert_code(&out, 3);
    let resources = value["data"]["resources"].as_array().ok_or("resources")?;
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["native_id"], "demo");
    assert_eq!(value["data"]["cache"]["reason"], "index_missing");
    assert!(!f.home.join("read-args").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn identical_refresh_reuses_content_and_borrowed_blob_is_not_gc_owned() -> TestResult {
    let f = Fixture::new()?;
    let (out, first) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let index = f.current_index()?;
    let bytes = fs::read(&index)?;
    let (out, second) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(first["data"]["index"], second["data"]["index"]);
    assert_eq!(second["data"]["created"], false);
    assert_eq!(second["data"]["owned"], true);
    fs::remove_file(f.file("state/version-indexes.json"))?;
    let (out, borrowed) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(borrowed["data"]["created"], false);
    assert_eq!(borrowed["data"]["owned"], false);
    let (out, preview) = f.run(&["cleanup", "macops:cache", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["candidates"], json!([]));
    let (out, _) = f.run(&["cleanup", "macops:cache", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(fs::read(index)?, bytes);
    Ok(())
}

#[test]
fn index_publication_preserves_file_created_during_network_query() -> TestResult {
    use std::process::Stdio;
    let f = Fixture::new()?;
    f.script("demo",r#"case "$1" in
available) printf ready > "$MACOPS_TEST_HOME/ready";while [ ! -f "$MACOPS_TEST_HOME/release" ];do /bin/sleep 0.01;done;printf '{"version":"2.0.0"}' ;;
query) printf '{"status":"missing"}' ;;
esac"#)?;
    let mut child = f
        .command()
        .args(["app", "refresh", "demo"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !f.home.join("ready").exists() {
        if std::time::Instant::now() > until {
            child.kill()?;
            child.wait()?;
            return Err("query did not enter barrier".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    fs::write(f.file("cache/updates.json"), "foreign user document")?;
    fs::write(f.home.join("release"), "go")?;
    let out = child.wait_with_output()?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_to_string(f.file("cache/updates.json"))?,
        "foreign user document"
    );
    assert!(f.current_index()?.exists());
    Ok(())
}

#[test]
fn cache_marker_change_blocks_reads_and_gc_without_claiming_other_data() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let index = f.current_index()?;
    fs::write(f.file("cache/indexes/.macops-scope.json"), "{}")?;
    for args in [
        vec!["app", "outdated", "demo"],
        vec!["cleanup", "macops:cache", "--yes"],
    ] {
        let (out, _) = f.run(&args)?;
        assert_code(&out, 1);
        assert!(index.exists());
    }
    Ok(())
}

fn wait_marker(path: &std::path::Path) -> TestResult {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !path.exists() {
        if std::time::Instant::now() > end {
            return Err("barrier was not reached".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}

fn metadata_barrier(f: &Fixture, version: &str) -> TestResult {
    let body = format!(
        r#"case "$1" in
available) printf ready > "$MACOPS_TEST_HOME/ready";while [ ! -f "$MACOPS_TEST_HOME/release" ];do /bin/sleep 0.01;done;printf '{{"version":"{version}"}}' ;;
query) printf '{{"status":"missing"}}' ;;
esac"#
    );
    f.script("demo", &body)?;
    Ok(())
}

#[test]
fn foreign_content_at_digest_name_is_never_replaced_or_claimed() -> TestResult {
    use std::process::Stdio;
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let path = f.current_index()?;
    fs::remove_file(&path)?;
    fs::remove_file(f.file("state/version-indexes.json"))?;
    metadata_barrier(&f, "2.0.0")?;
    let child = f
        .command()
        .args(["app", "refresh", "demo"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_marker(&f.home.join("ready"))?;
    fs::write(&path, "foreign content at the expected digest name")?;
    fs::write(f.home.join("release"), "go")?;
    let out = child.wait_with_output()?;
    assert_code(&out, 3);
    let result: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(result["error"]["code"], "cache_collision");
    assert_eq!(
        fs::read_to_string(path)?,
        "foreign content at the expected digest name"
    );
    assert!(!f.file("state/version-indexes.json").exists());
    Ok(())
}

#[test]
fn root_exchange_during_query_cannot_redirect_publication() -> TestResult {
    use std::process::Stdio;
    let f = Fixture::new()?;
    metadata_barrier(&f, "2.0.0")?;
    let child = f
        .command()
        .args(["app", "refresh", "demo"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_marker(&f.home.join("ready"))?;
    fs::rename(f.file("cache"), f.file("old-cache"))?;
    fs::create_dir(f.file("cache"))?;
    fs::write(f.file("cache/user-file"), "foreign")?;
    fs::write(f.home.join("release"), "go")?;
    let out = child.wait_with_output()?;
    assert_code(&out, 3);
    assert_eq!(fs::read_to_string(f.file("cache/user-file"))?, "foreign");
    assert!(!f.file("state/version-indexes.json").exists());
    Ok(())
}

#[test]
fn failed_book_commit_leaves_orphan_borrowed_not_owned() -> TestResult {
    use std::{os::unix::fs::PermissionsExt, process::Stdio};
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let before = fs::read(f.file("state/version-indexes.json"))?;
    metadata_barrier(&f, "3.0.0")?;
    let child = f
        .command()
        .args(["app", "refresh", "demo"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_marker(&f.home.join("ready"))?;
    fs::set_permissions(f.file("state"), fs::Permissions::from_mode(0o500))?;
    fs::write(f.home.join("release"), "go")?;
    let output = child.wait_with_output();
    fs::set_permissions(f.file("state"), fs::Permissions::from_mode(0o700))?;
    let out = output?;
    assert_code(&out, 1);
    assert_eq!(before, fs::read(f.file("state/version-indexes.json"))?);
    let (out, v) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["created"], false);
    assert_eq!(v["data"]["owned"], false);
    let orphan = f.current_index()?;
    let (out, _) = f.run(&["cleanup", "macops:cache", "--yes"])?;
    assert_code(&out, 0);
    assert!(orphan.exists());
    Ok(())
}

#[test]
fn partial_metadata_refresh_preserves_other_source_times() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    let mut other = catalog["packages"][0].clone();
    other["id"] = json!("fixture:other");
    other["name"] = json!("other");
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(other);
    write_json(&f.catalog, &catalog)?;
    for name in ["demo", "other"] {
        let (out, _) = f.run(&["app", "refresh", name])?;
        assert_code(&out, 0);
    }
    let mut book = f.read_json("state/version-indexes.json")?;
    let times = book["scopes"]
        .as_object_mut()
        .and_then(|s| s.values_mut().next())
        .ok_or("scope")?;
    times["queried_at"]["fixture:demo"] = json!(1);
    times["queried_at"]["fixture:other"] = json!(2);
    write_json(&f.file("state/version-indexes.json"), &book)?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let after = f.read_json("state/version-indexes.json")?;
    let scope = after["scopes"]
        .as_object()
        .and_then(|s| s.values().next())
        .ok_or("scope")?;
    assert!(
        scope["queried_at"]["fixture:demo"]
            .as_u64()
            .is_some_and(|v| v > 1)
    );
    assert_eq!(scope["queried_at"]["fixture:other"], 2);
    f.script("demo", "exit 9")?;
    let before = fs::read(f.file("state/version-indexes.json"))?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(before, fs::read(f.file("state/version-indexes.json"))?);
    Ok(())
}

#[test]
fn cleanup_does_not_recreate_the_retired_ownership_file() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    fs::write(
        f.file("cache/indexes/ownership.json"),
        "foreign user document",
    )?;
    let (out, preview) = f.run(&["cleanup", "macops:cache", "--dry-run"])?;
    assert_code(&out, 0);
    assert!(
        preview["data"]["review"]
            .as_array()
            .is_some_and(|r| r.contains(&json!("indexes/ownership.json")))
    );
    let (out, _) = f.run(&["cleanup", "macops:cache", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_to_string(f.file("cache/indexes/ownership.json"))?,
        "foreign user document"
    );
    Ok(())
}

#[test]
fn existing_legacy_home_app_paths_do_not_break_the_catalogue() -> TestResult {
    let f = Fixture::new()?;
    let manifest = f.file("settings/manifests/mac-apps.tsv");
    let body = "scope\tpath\tbundle_id\tversion\towner\tsource\nuser_top_level\t~//Applications/Demo.app\tcom.demo\t1\tvendor\tlegacy\ncask_non_app_artifact\tnot_applicable\tunknown\tunknown\thomebrew_cask\thomebrew_cask\n";
    fs::write(&manifest, body)?;
    let (out, v) = f.run(&["app", "com.demo"])?;
    assert_code(&out, 3);
    assert_eq!(
        v["data"]["package"]["provider"]["path"],
        f.home.join("Applications/Demo.app").display().to_string()
    );
    assert_eq!(v["data"]["observation"]["status"], "missing");
    assert_eq!(fs::read_to_string(manifest)?, body);
    Ok(())
}

#[test]
fn preview_never_resolves_from_another_scope_index() -> TestResult {
    let f = Fixture::new()?;
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("UV_TOOL_DIR", f.home.join("scope-a"))
        .args(["app", "refresh", "demo"])
        .output()?;
    assert_code(&out, 0);
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("UV_TOOL_DIR", f.home.join("scope-b"))
        .args(["app", "install", "demo", "--dry-run"])
        .output()?;
    assert_code(&out, 0);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["data"]["plan"]["target"], serde_json::Value::Null);
    assert_eq!(value["data"]["plan"]["command"], serde_json::Value::Null);
    assert!(
        value["data"]["plan"]["warnings"]
            .as_array()
            .is_some_and(|w| !w.is_empty())
    );
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn install_upgrade_uninstall_and_history_use_actual_artifact() -> TestResult {
    let f = Fixture::new()?;
    let (out, plan) = f.run(&["app", "install", "demo", "--version", "1.0.0", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(plan["data"]["persistentWrites"], false);
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());
    let (out, installed) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(installed["data"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("version"))?, "1.0.0");
    let receipt = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipt["installations"]["fixture:demo"]["observation"]["version"],
        "1.0.0"
    );
    let (out, _) = f.run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(fs::read_to_string(f.file("version"))?, "2.0.0");
    let (out, _) = f.run(&["app", "uninstall", "demo", "--yes"])?;
    assert_code(&out, 0);
    assert!(!f.file("version").exists());
    let receipts = f.read_json("state/receipts.json")?;
    assert_eq!(receipts["installations"], json!({}));
    let (out, history) = f.run(&["history"])?;
    assert_code(&out, 0);
    assert_eq!(history["data"].as_array().map(Vec::len), Some(3));
    assert_eq!(
        fs::read_to_string(f.file("calls"))?,
        "install\nupgrade\nuninstall\n"
    );
    Ok(())
}

#[test]
fn adopt_requires_present_and_preserves_installation() -> TestResult {
    let f = Fixture::new()?;
    let (out, v) = f.run(&["app", "adopt", "demo", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "not_installed");
    fs::write(f.file("version"), "0.5.0")?;
    let (out, v) = f.run(&["app", "upgrade", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "adoption_required");
    let (out, _) = f.run(&["app", "adopt", "demo", "--yes"])?;
    assert_code(&out, 0);
    assert!(!f.file("calls").exists());
    fs::write(f.file("version"), "external")?;
    let (out, v) = f.run(&["app", "uninstall", "demo", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "installation_drift");
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn strict_manifests_distinguish_bad_header_and_empty_list() -> TestResult {
    let f = Fixture::new()?;
    let manifest = f.file("settings/manifests/mac-apps.tsv");
    fs::write(&manifest, "bad\nrow\n")?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "invalid_manifest");
    fs::write(&manifest, "path\tbundle_id\tversion\towner\tsource\n")?;
    let (out, _) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    fs::write(
        &manifest,
        "path\tbundle_id\tversion\towner\tsource\nonly-one-column\n",
    )?;
    let (out, _) = f.run(&["app", "resources"])?;
    assert_code(&out, 1);
    Ok(())
}

#[test]
fn conda_wrong_field_type_is_not_a_healthy_environment() -> TestResult {
    let f = Fixture::new()?;
    f.script("conda", "printf '%s' '{\"envs\":\"abc\"}'")?;
    let (out, v) = f.run(&["app", "discover", "conda"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "invalid_json");
    f.script("conda", "printf '%s' '{\"envs\":[\"/synthetic/env\"]}'")?;
    let (out, v) = f.run(&["app", "discover", "conda"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["data"]["envs"], json!(["/synthetic/env"]));
    Ok(())
}

#[test]
fn service_string_false_is_not_running_and_identity_is_checked() -> TestResult {
    let f = Fixture::new()?;
    f.script(
        "brew",
        "printf '%s' '[{\"name\":\"postgresql@16\",\"running\":\"false\"}]'",
    )?;
    let (out, _) = f.run(&["app", "service", "list"])?;
    assert_code(&out, 1);
    f.script(
        "brew",
        "printf '%s' '[{\"name\":\"postgresql@16\",\"running\":false}]'",
    )?;
    let (out, v) = f.run(&["app", "service", "list"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["status"], "stopped");
    let (out, _) = f.run(&["app", "service", "check"])?;
    assert_code(&out, 3);
    f.script(
        "brew",
        "printf '%s' '[{\"name\":\"unrelated\",\"running\":true}]'",
    )?;
    let (out, v) = f.run(&["app", "service", "list"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "service_identity_mismatch");
    Ok(())
}

#[test]
fn update_outdated_and_owned_cleanup_leave_unknown_files() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "refresh", "demo", "--dry-run"])?;
    assert_code(&out, 0);
    assert!(!f.file("cache").exists());
    assert!(!f.file("state").exists());
    let (out, _) = f.run(&["app", "outdated", "demo"])?;
    assert_code(&out, 3);
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let index = f.current_index()?;
    assert!(index.is_file());
    fs::write(f.file("cache/keep"), "unknown-user-file")?;
    let before = fs::read(&index)?;
    let (out, _) = f.run(&["cleanup", "macops:cache", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(before, fs::read(&index)?);
    let (out, _) = f.run(&["cleanup", "macops:cache", "--yes"])?;
    assert_code(&out, 0);
    assert!(!&index.exists());
    assert_eq!(
        fs::read_to_string(f.file("cache/keep"))?,
        "unknown-user-file"
    );
    Ok(())
}

#[test]
fn changed_cache_and_symlink_are_never_deleted() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["app", "refresh", "demo"])?;
    assert_code(&out, 0);
    let index = f.current_index()?;
    fs::write(&index, "user changed")?;
    let (out, v) = f.run(&["cleanup", "macops:cache", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "cache_modified");
    fs::remove_file(&index)?;
    fs::write(f.file("outside"), "keep")?;
    symlink(f.file("outside"), &index)?;
    let (out, _) = f.run(&["cleanup", "macops:cache", "--yes"])?;
    assert_code(&out, 1);
    assert_eq!(fs::read_to_string(f.file("outside"))?, "keep");
    Ok(())
}

#[test]
fn unknown_options_and_ambiguous_objects_cannot_execute() -> TestResult {
    let f = Fixture::new()?;
    for args in [
        vec!["app", "install", "demo", "--apply"],
        vec!["app", "install"],
        vec!["agent", "credential", "get"],
        vec!["app", "install", "demo", "--dry"],
    ] {
        let (out, v) = f.run(&args)?;
        assert_code(&out, 2);
        assert_eq!(v["error"]["code"], "usage");
    }
    let mut catalog = f.read_json("catalog.json")?;
    let mut other = catalog["packages"][0].clone();
    other["id"] = json!("other:demo");
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(other);
    write_json(&f.catalog, &catalog)?;
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "ambiguous");
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn exclusion_happens_before_reading_link_targets() -> TestResult {
    let f = Fixture::new()?;
    let path = f.file("excluded");
    symlink("/Users/example/job/nonexistent", &path)?;
    let out = f.command().arg("--catalog").arg(path).arg("app").output()?;
    assert_code(&out, 2);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_macops"))
        .args(["--json", "--catalog"])
        .arg(f.file("excluded"))
        .arg("app")
        .output()?;
    assert_code(&out, 1);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["error"]["code"], "excluded_path");
    Ok(())
}

#[test]
fn no_gate_means_worker_does_not_read_or_execute() -> TestResult {
    let f = Fixture::new()?;
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_macops"))
        .arg("__worker")
        .arg(f.file("not-a-journal"))
        .stdin(std::process::Stdio::null())
        .output()?;
    assert_code(&out, 1);
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn own_roots_follow_xdg_without_initializing_directories() -> TestResult {
    let f = Fixture::new()?;
    let out = common::empty_environment_command()
        .env("HOME", &f.home)
        .env("XDG_CONFIG_HOME", f.file("config-base"))
        .env("XDG_DATA_HOME", f.file("data-base"))
        .env("XDG_STATE_HOME", f.file("state-base"))
        .env("XDG_CACHE_HOME", f.file("cache-base"))
        .args(["--json", "dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(
        v["data"]["state"],
        f.file("state-base/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["config"],
        f.file("config-base/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["data"],
        f.file("data-base/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["cache"],
        f.file("cache-base/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["catalog"],
        f.file("config-base/macops/catalog.json")
            .display()
            .to_string()
    );
    assert!(!f.file("state-base").exists());
    assert!(!f.file("cache-base").exists());
    assert!(!f.file("config-base").exists());
    assert!(!f.file("data-base").exists());
    Ok(())
}

#[test]
fn own_roots_preserve_defaults_cli_priority_and_isolated_home() -> TestResult {
    let f = Fixture::new()?;
    let out = f.roots_command().args(["dirs", "--roots"]).output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(
        v["data"]["state"],
        f.home.join(".local/state/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["cache"],
        f.home.join(".cache/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["config"],
        f.home.join(".config/macops").display().to_string()
    );
    assert_eq!(
        v["data"]["data"],
        f.home.join(".local/share/macops").display().to_string()
    );
    assert!(!f.home.join(".local").exists());
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("xdg-state"))
        .env("XDG_CACHE_HOME", f.file("xdg-cache"))
        .env("XDG_CONFIG_HOME", f.file("xdg-config"))
        .env("XDG_DATA_HOME", f.file("xdg-data"))
        .arg("--state-dir")
        .arg(f.file("chosen-state"))
        .arg("--cache-dir")
        .arg(f.file("chosen-cache"))
        .arg("--config-dir")
        .arg(f.file("chosen-config"))
        .arg("--data-dir")
        .arg(f.file("chosen-data"))
        .arg("--catalog")
        .arg(f.catalog.clone())
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    for (field, path) in [
        ("state", "chosen-state"),
        ("cache", "chosen-cache"),
        ("config", "chosen-config"),
        ("data", "chosen-data"),
    ] {
        assert_eq!(v["data"][field], f.file(path).display().to_string());
        assert!(!f.file(path).exists());
    }
    assert_eq!(v["data"]["catalog"], f.catalog.display().to_string());
    let isolated = f.file("isolated-home");
    fs::create_dir(&isolated)?;
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("host-state"))
        .env("XDG_CACHE_HOME", "/Users/example/job/private")
        .arg("--home")
        .arg(&isolated)
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(
        v["data"]["state"],
        isolated.join(".local/state/macops").display().to_string()
    );
    assert!(!isolated.join(".local").exists());
    Ok(())
}

#[test]
fn invalid_xdg_values_are_diagnosed_without_weakening_absolute_path_checks() -> TestResult {
    let f = Fixture::new()?;
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", "relative")
        .env("XDG_CONFIG_HOME", "")
        .env("XDG_CACHE_HOME", "relative-cache")
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["warnings"].as_array().ok_or("warnings")?.len(), 2);
    assert_eq!(
        v["data"]["state"],
        f.home.join(".local/state/macops").display().to_string()
    );
    for value in ["/Users/example/job/blocked", "/safe/../elsewhere"] {
        let out = f
            .roots_command()
            .env("XDG_CACHE_HOME", value)
            .args(["dirs", "--roots"])
            .output()?;
        assert_code(&out, 1);
    }
    let out = f
        .roots_command()
        .env("UV_TOOL_DIR", "relative")
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 1);
    for option in [
        "--state-dir",
        "--cache-dir",
        "--config-dir",
        "--data-dir",
        "--catalog",
    ] {
        let out = f
            .roots_command()
            .args([option, "relative", "dirs", "--roots"])
            .output()?;
        assert_code(&out, 1);
    }
    let out = f
        .roots_command()
        .args(["dirs", "demo", "--roots"])
        .output()?;
    assert_code(&out, 2);
    Ok(())
}

#[test]
fn existing_default_state_cannot_be_silently_bypassed_using_xdg() -> TestResult {
    let f = Fixture::new()?;
    let old = f.home.join(".local/state/macops");
    fs::create_dir_all(&old)?;
    fs::write(old.join("active.json"), "unknown operation bytes")?;
    for args in [
        vec!["dirs", "--roots"],
        vec!["app", "install", "demo", "--version", "2"],
    ] {
        let out = f
            .roots_command()
            .env("XDG_STATE_HOME", f.file("new-state"))
            .args(args)
            .output()?;
        assert_code(&out, 3);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(v["error"]["code"], "state_layout_migration_required");
    }
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("new-state"))
        .env("XDG_DATA_HOME", "relative")
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 3);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["warnings"].as_array().ok_or("warnings")?.len(), 1);
    assert!(!f.file("new-state").exists());
    assert_eq!(
        fs::read_to_string(old.join("active.json"))?,
        "unknown operation bytes"
    );
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("new-state"))
        .arg("--state-dir")
        .arg(&old)
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.home.join(".local/state/./"))
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    fs::remove_file(old.join("active.json"))?;
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("new-state"))
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 3);
    Ok(())
}

#[test]
fn roots_query_rejects_bad_types_links_and_overlap_but_ignores_catalog_contents() -> TestResult {
    let f = Fixture::new()?;
    fs::write(&f.catalog, "broken JSON")?;
    let out = f
        .roots_command()
        .arg("--catalog")
        .arg(&f.catalog)
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    let file = f.file("root-file");
    fs::write(&file, "not a directory")?;
    for option in ["--config-dir", "--data-dir", "--state-dir", "--cache-dir"] {
        let out = f
            .roots_command()
            .arg(option)
            .arg(&file)
            .args(["dirs", "--roots"])
            .output()?;
        assert_code(&out, 1);
    }
    let excluded = f.file("excluded-root");
    symlink("/Users/example/job/nonexistent", &excluded)?;
    let looping = f.file("looping-root");
    symlink(&looping, &looping)?;
    for path in [&excluded, &looping, &file.join("subdir")] {
        let out = f
            .roots_command()
            .arg("--cache-dir")
            .arg(path)
            .args(["dirs", "--roots"])
            .output()?;
        assert_code(&out, 1);
    }
    let out = f
        .roots_command()
        .arg("--state-dir")
        .arg(f.file("overlap"))
        .arg("--cache-dir")
        .arg(f.file("overlap/cache"))
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 1);
    let old = f.home.join(".local/state/macops");
    fs::create_dir_all(old.parent().ok_or("parent")?)?;
    fs::write(&old, "not a state directory")?;
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("new-state"))
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 1);
    assert!(!f.file("new-state").exists());
    Ok(())
}

#[test]
fn relative_xdg_has_text_diagnostic_and_does_not_create_defaults() -> TestResult {
    let f = Fixture::new()?;
    let out = common::empty_environment_command()
        .env("HOME", &f.home)
        .env("XDG_STATE_HOME", "relative")
        .args(["dirs", "--roots"])
        .output()?;
    assert_code(&out, 0);
    assert!(String::from_utf8(out.stderr)?.contains("XDG_STATE_HOME"));
    assert!(String::from_utf8(out.stdout)?.contains(&f.home.display().to_string()));
    assert!(!f.home.join(".local").exists());
    Ok(())
}

#[test]
fn explicit_old_state_recovers_using_original_xdg_without_repeating_install() -> TestResult {
    let f = Fixture::new()?;
    let old = f.home.join(".local/state/macops");
    f.script("demo", r#"case "$1" in
query) printf '%s' "${XDG_DATA_HOME-unset}" > "$MACOPS_TEST_HOME/query-scope"; if [ -f "$2" ]; then printf '{"status":"present","version":"%s"}\n' "$(/bin/cat "$2")"; else printf '{"status":"missing"}\n'; fi ;;
install) printf '%s' "$4" > "$2"; printf 'install\n' >> "$3"; /bin/sleep 30 ;;
*) exit 7 ;;
esac"#)?;
    let out = f
        .roots_command()
        .env("XDG_DATA_HOME", f.file("original-data"))
        .arg("--state-dir")
        .arg(&old)
        .arg("--catalog")
        .arg(&f.catalog)
        .args([
            "--timeout-secs",
            "1",
            "app",
            "install",
            "demo",
            "--version",
            "1.0.0",
        ])
        .output()?;
    assert_code(&out, 3);
    let journal: serde_json::Value = serde_json::from_slice(&fs::read(old.join("active.json"))?)?;
    let id = journal["id"].as_str().ok_or("id")?;
    assert_eq!(journal["phase"], "unknown");
    fs::remove_file(f.home.join("query-scope"))?;
    let out = f
        .roots_command()
        .env("XDG_STATE_HOME", f.file("new-state"))
        .env("XDG_DATA_HOME", f.file("hostile-data"))
        .arg("--state-dir")
        .arg(&old)
        .args(["recover", id])
        .output()?;
    assert_code(&out, 0);
    let result: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(result["data"]["phase"], "succeeded");
    assert_eq!(
        fs::read_to_string(f.home.join("query-scope"))?,
        f.file("original-data").display().to_string()
    );
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    assert!(!f.file("new-state").exists());
    assert!(!f.file("hostile-data").exists());
    Ok(())
}

#[test]
fn native_credential_routes_do_not_require_or_invoke_legacy_mo() -> TestResult {
    let f = Fixture::new()?;
    let (out, v) = f.run(&["agent", "credential", "add", "mi", "fixture"])?;
    assert_code(&out, 0);
    assert_eq!(
        v["data"]["command"]["args"],
        json!(["credential", "add", "mi", "fixture"])
    );
    assert_eq!(v["data"]["account"], "claude-mi@fixture");
    assert_eq!(v["data"]["executed"], false);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn native_credential_probe_discards_output_and_nonzero_does_not_prove_absence() -> TestResult {
    let f = Fixture::new()?;
    let settings = f.home.join(".claude-mi/settings.json");
    fs::create_dir_all(settings.parent().ok_or("profile parent")?)?;
    write_json(
        &settings,
        &json!({"apiKeyHelper":format!("{} claude-mi@fixture", f.home.join(".local/libexec/claude-key-helper").display())}),
    )?;
    f.script("security", r#"printf '%s\n' "$@" > "$MACOPS_TEST_HOME/security-args"
printf 'SYNTHETIC_SECRET_DO_NOT_CAPTURE'
printf 'SYNTHETIC_SECRET_DO_NOT_CAPTURE' >&2
if [ -f "$MACOPS_TEST_HOME/probe-code" ]; then exit "$(/bin/cat "$MACOPS_TEST_HOME/probe-code")"; fi"#)?;
    let out = f
        .command()
        .env("TMPDIR", f.file("missing-temp-root"))
        .args(["agent", "credential", "check", "mi"])
        .output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["data"][0]["status"], "present");
    assert_eq!(
        fs::read_to_string(f.home.join("security-args"))?,
        "find-generic-password\n-s\nlocal.claude-profile\n-a\nclaude-mi@fixture\n"
    );
    assert!(!String::from_utf8(out.stdout)?.contains("SYNTHETIC_SECRET"));
    assert!(!String::from_utf8(out.stderr)?.contains("SYNTHETIC_SECRET"));
    assert!(!f.file("missing-temp-root").exists());
    for code in ["1", "44"] {
        fs::write(f.home.join("probe-code"), code)?;
        let (out, v) = f.run(&["agent", "credential", "check", "mi"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"][0]["status"], "unknown");
        assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    }
    f.script("security", "/bin/sleep 2")?;
    let (out, v) = f.run(&["--timeout-secs", "1", "agent", "credential", "check", "mi"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["status"], "unknown");
    let out = f
        .command()
        .env("PATH", f.home.join("empty-bin"))
        .args(["agent", "credential", "check", "mi"])
        .output()?;
    assert_code(&out, 3);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["data"][0]["reason"], "manager_missing");
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn native_credential_validation_retains_safe_selection_and_no_secret_parameters() -> TestResult {
    let f = Fixture::new()?;
    let (out, v) = f.run(&["agent", "credential", "list"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"].as_array().ok_or("profiles")?.len(), 4);
    let (out, v) = f.run(&["agent", "credential", "check"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["reason"], "settings_missing");
    for profile in ["glm", "kimi", "mi", "zen"] {
        let (out, v) = f.run(&["agent", "credential", "activate", profile, "safe_alias-1"])?;
        assert_code(&out, 0);
        assert_eq!(
            v["data"]["command"]["args"],
            json!(["credential", "activate", profile, "safe_alias-1"])
        );
        assert_eq!(v["data"]["executed"], false);
    }
    for args in [
        vec!["agent", "credential", "check", "unknown"],
        vec!["agent", "credential", "add", "unknown", "fixture"],
        vec!["agent", "credential", "add", "mi", "a;bad"],
        vec![
            "agent",
            "credential",
            "add",
            "mi",
            "fixture",
            "secret-value",
        ],
        vec!["agent", "credential", "get"],
    ] {
        let (out, _) = f.run(&args)?;
        assert_code(&out, 2);
    }
    let settings = f.home.join(".claude-mi/settings.json");
    fs::create_dir_all(settings.parent().ok_or("parent")?)?;
    for body in [
        "broken JSON",
        "{}",
        "{\"apiKeyHelper\":\"/wrong claude-mi@fixture\"}",
    ] {
        fs::write(&settings, body)?;
        let (out, v) = f.run(&["agent", "credential", "check", "mi"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"][0]["reason"], "invalid_profile_settings");
    }
    fs::remove_file(&settings)?;
    symlink(f.catalog.clone(), &settings)?;
    let (out, v) = f.run(&["agent", "credential", "check", "mi"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["reason"], "invalid_profile_settings");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn native_service_start_stop_noop_and_preview_do_not_create_install_receipts() -> TestResult {
    let f = Fixture::new()?;
    let user = String::from_utf8(
        std::process::Command::new("/usr/bin/id")
            .arg("-un")
            .output()?
            .stdout,
    )?
    .trim()
    .to_owned();
    let stopped = json!([{"name":"postgresql@16","running":false,"loaded":false,"registered":false,"user":null}]);
    write_json(&f.home.join("service-state"), &stopped)?;
    write_json(&f.home.join("stop-state"), &stopped)?;
    write_json(
        &f.home.join("start-state"),
        &json!([{"name":"postgresql@16","running":true,"loaded":true,"registered":true,"user":user}]),
    )?;
    f.script("brew",r#"case "$1:$2" in
services:info) /bin/cat "$MACOPS_TEST_HOME/service-state" ;;
services:start|services:stop) printf '%s\n' "$2" >> "$MACOPS_TEST_HOME/service-calls"; /bin/cp "$MACOPS_TEST_HOME/$2-state" "$MACOPS_TEST_HOME/service-state" ;;
*) exit 8 ;;
esac"#)?;
    let (out, v) = f.run(&["app", "app:postgresql", "start", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(
        v["data"]["plan"]["command"]["args"],
        json!(["services", "start", "postgresql@16"])
    );
    assert!(!f.file("state").exists());
    assert!(!f.home.join("service-calls").exists());
    let (out, v) = f.run(&["app", "start", "postgresql"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "confirmation_required");
    assert!(!f.file("state").exists());
    let (out, v) = f.run(&["app", "start", "postgresql", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(v["data"]["plan"]["action"], "start");
    assert!(!f.file("state/receipts.json").exists());
    let before = fs::read(f.file("state/active.json"))?;
    let (out, v) = f.run(&["app", "start", "postgresql"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["mode"], "no-op");
    assert_eq!(before, fs::read(f.file("state/active.json"))?);
    let (out, v) = f.run(&["app", "stop", "postgresql", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(
        v["data"]["plan"]["command"]["args"],
        json!(["services", "stop", "postgresql@16"])
    );
    assert_eq!(
        fs::read_to_string(f.home.join("service-calls"))?,
        "start\nstop\n"
    );
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

fn service_user() -> Result<String, Box<dyn std::error::Error>> {
    Ok(String::from_utf8(
        std::process::Command::new("/usr/bin/id")
            .arg("-un")
            .output()?
            .stdout,
    )?
    .trim()
    .to_owned())
}

fn service_row(
    running: bool,
    loaded: bool,
    registered: bool,
    user: Option<&str>,
) -> serde_json::Value {
    json!([{"name":"postgresql@16","running":running,"loaded":loaded,"registered":registered,"user":user}])
}

#[test]
fn service_preview_covers_all_states_without_treating_loaded_as_registered() -> TestResult {
    let user = service_user()?;
    for running in [false, true] {
        for loaded in [false, true] {
            for registered in [false, true] {
                let f = Fixture::new()?;
                write_json(
                    &f.home.join("service-state"),
                    &service_row(running, loaded, registered, Some(&user)),
                )?;
                f.script("brew",r#"if [ "$2" = info ]; then /bin/cat "$MACOPS_TEST_HOME/service-state"; else printf 'control\n' >> "$MACOPS_TEST_HOME/service-calls"; fi"#)?;
                for (verb, wanted) in [("start", true), ("stop", false)] {
                    let (out, v) = f.run(&["app", verb, "postgresql", "--dry-run"])?;
                    assert_code(&out, 0);
                    if running == wanted && loaded == wanted && registered == wanted {
                        assert_eq!(v["data"]["changed"], false);
                    } else {
                        assert_eq!(
                            v["data"]["plan"]["command"]["args"],
                            json!(["services", verb, "postgresql@16"])
                        );
                    }
                }
                assert!(!f.file("state").exists());
                assert!(!f.home.join("service-calls").exists());
            }
        }
    }
    Ok(())
}

#[test]
fn running_without_registration_is_not_a_successful_noop_or_successful_start() -> TestResult {
    let f = Fixture::new()?;
    write_json(
        &f.home.join("service-state"),
        &service_row(true, true, false, Some(&service_user()?)),
    )?;
    f.script("brew",r#"if [ "$2" = info ]; then /bin/cat "$MACOPS_TEST_HOME/service-state"; else printf '%s\n' "$2" >> "$MACOPS_TEST_HOME/service-calls"; fi"#)?;
    let (out, v) = f.run(&["app", "start", "postgresql", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "unknown");
    assert_eq!(fs::read_to_string(f.home.join("service-calls"))?, "start\n");
    let (out, v) = f.run(&["app", "start", "postgresql", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "recovery_required");
    assert_eq!(fs::read_to_string(f.home.join("service-calls"))?, "start\n");
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn service_bad_output_unproven_owners_and_secret_diagnostics_cannot_execute() -> TestResult {
    let f = Fixture::new()?;
    f.script("brew",r#"printf 'SYNTHETIC_SECRET_STDERR' >&2
if [ "$2" = info ]; then /bin/cat "$MACOPS_TEST_HOME/service-state"; else printf 'control\n' >> "$MACOPS_TEST_HOME/service-calls"; fi"#)?;
    let mut bad = service_row(false, false, false, None);
    bad[0]["running"] = json!("SYNTHETIC_SECRET_VALUE");
    for body in [
        "not JSON".to_owned(),
        "[]".into(),
        bad.to_string(),
        service_row(true, true, true, Some("root")).to_string(),
        service_row(true, true, false, None).to_string(),
        service_row(false, false, true, Some("another-user")).to_string(),
    ] {
        fs::write(f.home.join("service-state"), body)?;
        let (out, v) = f.run(&["app", "start", "postgresql", "--yes"])?;
        assert_code(&out, 3);
        assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
        assert!(!String::from_utf8(out.stderr)?.contains("SYNTHETIC_SECRET"));
        assert!(!f.file("state").exists());
        assert!(!f.home.join("service-calls").exists());
    }
    let mut valid = service_row(true, true, true, Some(&service_user()?));
    valid[0]["command"] = json!("SYNTHETIC_SECRET_COMMAND");
    write_json(&f.home.join("service-state"), &valid)?;
    let (out, v) = f.run(&["app", "start", "postgresql", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["mode"], "no-op");
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    f.script("brew", "printf 'SYNTHETIC_SECRET_STDERR' >&2; exit 2")?;
    let (out, v) = f.run(&["app", "service", "list"])?;
    assert_code(&out, 1);
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn service_timeout_retains_frozen_actor_scope_and_recovers_without_repeating_control() -> TestResult
{
    use sha2::Digest;
    let f = Fixture::new()?;
    write_json(
        &f.home.join("service-state"),
        &service_row(false, false, false, None),
    )?;
    write_json(
        &f.home.join("start-state"),
        &service_row(true, true, true, Some(&service_user()?)),
    )?;
    f.script("brew",r#"case "$2" in
info) printf '%s' "${HOMEBREW_USER_CONFIG_HOME-unset}" > "$MACOPS_TEST_HOME/query-env"; /bin/cat "$MACOPS_TEST_HOME/service-state" ;;
start) printf 'start\n' >> "$MACOPS_TEST_HOME/service-calls"; /bin/cp "$MACOPS_TEST_HOME/start-state" "$MACOPS_TEST_HOME/service-state"; /bin/sleep 30 ;;
stop) printf 'stop\n' >> "$MACOPS_TEST_HOME/service-calls" ;;
*) exit 8 ;;
esac"#)?;
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("HOMEBREW_USER_CONFIG_HOME", f.file("scope-before"))
        .args(["--timeout-secs", "1", "app", "start", "postgresql", "--yes"])
        .output()?;
    assert_code(&out, 3);
    let journal = f.read_json("state/active.json")?;
    assert_eq!(journal["phase"], "unknown");
    let id = journal["id"].as_str().ok_or("id")?;
    let (out, v) = f.run(&["app", "stop", "postgresql", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "recovery_required");
    let mut wrong = journal.clone();
    wrong["scope"]["service_identity"]["uid"] = json!(
        journal["scope"]["service_identity"]["uid"]
            .as_u64()
            .ok_or("uid")?
            + 1000
    );
    wrong["scope_sha256"] = json!(format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(&wrong["scope"])?)
    ));
    write_json(&f.file("state/active.json"), &wrong)?;
    write_json(&f.file(&format!("state/operations/{id}.json")), &wrong)?;
    let (out, v) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "service_identity_changed");
    assert_eq!(wrong, f.read_json("state/active.json")?);
    write_json(&f.file("state/active.json"), &journal)?;
    write_json(&f.file(&format!("state/operations/{id}.json")), &journal)?;
    fs::remove_file(f.home.join("query-env"))?;
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("HOMEBREW_USER_CONFIG_HOME", f.file("scope-after"))
        .args(["recover", id])
        .output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(
        fs::read_to_string(f.home.join("query-env"))?,
        f.file("scope-before").display().to_string()
    );
    assert_eq!(fs::read_to_string(f.home.join("service-calls"))?, "start\n");
    assert!(!f.file("state/receipts.json").exists());
    assert!(!f.file("scope-before").exists());
    assert!(!f.file("scope-after").exists());
    Ok(())
}

#[test]
fn slow_query_must_not_hide_a_recipe_changed_before_execution() -> TestResult {
    let f = Fixture::new()?;
    let mut changed = f.read_json("catalog.json")?;
    changed["packages"][0]["name"] = json!("changed-during-query");
    write_json(&f.home.join("changed-catalog"), &changed)?;
    f.script("demo",r#"case "$1" in
query) if [ -f "$MACOPS_TEST_HOME/queried" ]; then /bin/cp "$MACOPS_TEST_HOME/changed-catalog" "$MACOPS_TEST_HOME/../catalog.json"; else printf 'queried' > "$MACOPS_TEST_HOME/queried"; fi; printf '{"status":"missing"}\n' ;;
install) printf 'install\n' >> "$3" ;;
*) exit 8 ;;
esac"#)?;
    let (out, v) = f.run(&["app", "install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "plan_stale");
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn unified_agent_inventory_keeps_custom_profiles_separate_without_running_agents() -> TestResult {
    let f = Fixture::new()?;
    for (name, model) in [("mi", "model-mi"), ("glm", "model-glm")] {
        let root = f.home.join(format!(".claude-{name}"));
        fs::create_dir_all(&root)?;
        write_json(
            &root.join("settings.json"),
            &json!({"model":model,"env":{"ANTHROPIC_AUTH_TOKEN":"SYNTHETIC_SECRET"}}),
        )?;
    }
    f.script(
        "claude",
        "printf 'BAD' > \"$MACOPS_TEST_HOME/agent-executed\"; exit 9",
    )?;
    f.script(
        "security",
        "printf 'BAD' > \"$MACOPS_TEST_HOME/keychain-executed\"; exit 9",
    )?;
    let (out, v) = f.run(&["agent", "resources"])?;
    assert_code(&out, 0);
    let entries = v["data"]["resources"]
        .as_array()
        .ok_or("unified resources")?;
    assert_eq!(entries.len(), 2);
    assert_ne!(entries[0]["id"], entries[1]["id"]);
    assert!(entries.iter().any(|e| e["model"]["selected"] == "model-mi"));
    assert!(
        entries
            .iter()
            .any(|e| e["model"]["selected"] == "model-glm")
    );
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    assert!(!f.home.join("agent-executed").exists());
    assert!(!f.home.join("keychain-executed").exists());
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn unified_capabilities_ignore_npx_cache_and_keep_source_and_placements_distinct() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir_all(f.home.join(".npm/_npx/ephemeral/node_modules/skills"))?;
    f.script(
        "npx",
        "printf 'BAD' > \"$MACOPS_TEST_HOME/npx-executed\"; exit 9",
    )?;
    let (out, v) = f.run(&["skill", "resources"])?;
    assert_code(&out, 0);
    assert_eq!(
        v["data"]["resources"].as_array().ok_or("resources")?.len(),
        0
    );
    let canonical = f.home.join(".agents/skills/demo");
    fs::create_dir_all(&canonical)?;
    fs::write(canonical.join("SKILL.md"), "native content")?;
    write_json(
        &f.home.join(".agents/.skill-lock.json"),
        &json!({"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo","ref":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}),
    )?;
    for profile in ["mi", "glm"] {
        fs::create_dir_all(f.home.join(format!(".claude-{profile}/skills")))?;
    }
    symlink(&canonical, f.home.join(".claude-mi/skills/demo"))?;
    let copied = f.home.join(".claude-glm/skills/demo");
    fs::create_dir(&copied)?;
    fs::write(copied.join("SKILL.md"), "untracked copy")?;
    let (out, v) = f.run(&["skill", "resources"])?;
    assert_code(&out, 0);
    let resources = v["data"]["resources"].as_array().ok_or("resources")?;
    assert_eq!(resources.len(), 2);
    assert_ne!(resources[0]["id"], resources[1]["id"]);
    let native = resources
        .iter()
        .find(|r| r["management"] == "native")
        .ok_or("native record")?;
    assert_eq!(
        native["deployments"].as_array().ok_or("deployments")?.len(),
        1
    );
    assert_eq!(
        native["deployments"][0]["path"],
        f.home.join(".claude-mi/skills/demo").display().to_string()
    );
    assert_eq!(native["loaded"], serde_json::Value::Null);
    let (out, v) = f.run(&["skill", "skill:demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "ambiguous");
    write_json(
        &f.home.join(".agents/.skill-lock.json"),
        &json!({"version":1,"skills":{}}),
    )?;
    let (out, v) = f.run(&["skill", "resources"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["incomplete"], true);
    assert!(!f.home.join("npx-executed").exists());
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn unified_agent_resolution_uses_profile_and_project_context_without_launching_helper() -> TestResult
{
    let f = Fixture::new()?;
    let helper = f.home.join(".local/bin/claudectl");
    fs::create_dir_all(helper.parent().ok_or("parent")?)?;
    fs::write(
        &helper,
        "claude_resolved_bin() {\n  mise which claude\n}\nprintf 'BAD' > invoked-helper\n",
    )?;
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755))?;
    let release1 = f.home.join("releases/1.0.0");
    let release2 = f.home.join("releases/2.0.0");
    fs::create_dir_all(&release1)?;
    fs::create_dir_all(&release2)?;
    let executable = f.script(
        "fixture-claude",
        r#"touch "$MACOPS_TEST_HOME/invoked-claude"; exit 9"#,
    )?;
    for release in [&release1, &release2] {
        fs::copy(&executable, release.join("claude"))?;
    }
    write_json(
        &f.home.join("mise-data"),
        &json!({"claude":[{"version":"1.0.0","install_path":release1,"active":true},{"version":"2.0.0","install_path":release2,"active":false}]}),
    )?;
    f.script("mise",r#"case "$1" in
ls) /bin/cat "$MACOPS_TEST_HOME/mise-data" ;;
which) printf '%s|%s|%s\n' "$(/bin/pwd)" "$PWD" "${CLAUDE_CONFIG_DIR-unset}" >> "$MACOPS_TEST_HOME/query-context"; case "$(/bin/pwd)" in */project-two) printf '%s/releases/2.0.0/claude\n' "$MACOPS_TEST_HOME" ;; *) printf '%s/releases/1.0.0/claude\n' "$MACOPS_TEST_HOME" ;; esac ;;
*) exit 8 ;;
esac"#)?;
    for profile in ["mi", "glm", "kimi", "zen"] {
        let dir = f.home.join(format!(".claude-{profile}"));
        fs::create_dir_all(&dir)?;
        write_json(&dir.join("settings.json"), &json!({"model":profile}))?;
        let wrapper = f.home.join(format!(".local/bin/claude-{profile}"));
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nset -eu\nexec {} run {profile} \"$@\"\n",
                helper.display()
            ),
        )?;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))?;
        symlink(wrapper, f.home.join(format!("bin/claude-{profile}")))?;
    }
    let p1 = f.file("project-one");
    let p2 = f.file("project-two");
    fs::create_dir(&p1)?;
    fs::create_dir(&p2)?;
    for (project, version) in [(&p1, "1.0.0"), (&p2, "2.0.0")] {
        let (out, v) = f.run(&["agent", "--project", project.to_str().ok_or("project")?])?;
        assert_code(&out, 0);
        let profiles = v["data"]["resources"].as_array().ok_or("profiles")?;
        assert_eq!(profiles.len(), 4);
        assert!(profiles.iter().all(|r| {
            r["runtime"]["binary"]
                == f.home
                    .join(format!("releases/{version}/claude"))
                    .display()
                    .to_string()
        }));
        assert!(
            profiles
                .iter()
                .all(|r| r["runtime"]["install_ref"].is_string())
        );
        assert!(profiles.iter().all(|r| r["runtime"]["version"] == version));
    }
    let contexts = fs::read_to_string(f.home.join("query-context"))?;
    for line in contexts.lines() {
        let values: Vec<_> = line.split('|').collect();
        assert_eq!(values[0], values[1]);
        assert!(values[2].contains(".claude-"));
    }
    fs::remove_file(f.home.join("bin/claude-mi"))?;
    let selected = f.script("claude-mi", "exit 9")?;
    fs::remove_file(release2.join("claude"))?;
    let (out, stale) = f.run(&["agent", "--project", p2.to_str().ok_or("project")?])?;
    assert_code(&out, 0);
    let profiles = stale["data"]["resources"].as_array().ok_or("profiles")?;
    for profile in profiles {
        let runtime = &profile["runtime"];
        assert!(runtime["version"].is_null());
        assert!(runtime["install_ref"].is_null());
        if profile["name"] == "claude:mi" {
            assert_eq!(runtime["binary"], selected.display().to_string());
        } else {
            assert!(runtime["binary"].is_null());
        }
    }
    assert!(!f.file("invoked-helper").exists());
    assert!(!f.home.join("invoked-claude").exists());
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn default_agent_version_follows_the_project_selected_mise_shim_release() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir(f.home.join(".codex"))?;
    let shims = f.home.join(".local/share/mise/shims");
    let release = f.home.join("codex-2.0.0");
    let release_alias = f.home.join("selected-codex");
    fs::create_dir_all(&shims)?;
    fs::create_dir(&release)?;
    symlink(&release, &release_alias)?;
    let command = f.script(
        "codex",
        r#"touch "$MACOPS_TEST_HOME/invoked-codex"; exit 9"#,
    )?;
    fs::copy(&command, release.join("codex"))?;
    write_json(
        &f.home.join("mise-data"),
        &json!({"codex":[
            {"version":"1.0.0","install_path":f.home.join("codex-1.0.0"),"active":true},
            {"version":"2.0.0","install_path":release_alias,"active":false}
        ]}),
    )?;
    let manager = f.script(
        "mise",
        r#"case "$1" in
ls) /bin/cat "$MACOPS_TEST_HOME/mise-data" ;;
which) test "$2" = codex; printf '%s/codex-2.0.0/codex\n' "$MACOPS_TEST_HOME" ;;
*) exit 8 ;;
esac"#,
    )?;
    symlink(manager, shims.join("codex"))?;
    let out = f
        .command()
        .env(
            "PATH",
            format!(
                "{}:{}:/usr/bin:/bin",
                shims.display(),
                f.home.join("bin").display()
            ),
        )
        .env("MISE_DATA_DIR", f.home.join(".local/share/mise"))
        .args(["agent", "resources"])
        .output()?;
    assert_code(&out, 0);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let profiles = value["data"]["resources"].as_array().ok_or("profiles")?;
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["name"], "codex:default");
    assert_eq!(profiles[0]["runtime"]["version"], "2.0.0");
    assert!(!f.home.join("invoked-codex").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn agent_refresh_shows_the_probed_version_only_on_the_matching_profile() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir(f.home.join(".claude"))?;
    fs::create_dir(f.home.join(".claude-mi"))?;
    let probed = f.script(
        "original-claude",
        r#"test "$1" = --version
printf 'claude 8.7.6\n'
printf 'called\n' >> "$MACOPS_TEST_HOME/version-probe""#,
    )?;
    symlink(probed, f.home.join("bin/claude"))?;
    f.script(
        "claude-mi",
        r#"touch "$MACOPS_TEST_HOME/invoked-wrapper"; exit 9"#,
    )?;
    f.script(
        "cursor-agent",
        r#"ln -sf "$MACOPS_TEST_HOME/bin/claude-mi" "$MACOPS_TEST_HOME/bin/claude"
printf 'cursor-agent 4.5.6\n'"#,
    )?;
    chezmoi_native::run_browser_pty(&f, &["agent"], "agent-profile-version", &f.home, "90", "0")?;
    assert_eq!(
        fs::read_to_string(f.home.join("version-probe"))?,
        "called\n"
    );
    assert!(!f.home.join("invoked-wrapper").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn unified_software_keeps_manager_prefixes_versions_and_bundles_distinct() -> TestResult {
    let f = Fixture::new()?;
    let brew = f.home.join("native/brew");
    let npm = f.home.join("native/npm");
    let uv = f.home.join(".local/share/uv/tools");
    write_json(
        &f.home.join("brew-data"),
        &json!({"formulae":[{"name":"dup","full_name":"tap/dup","installed":[{"version":"1.0.0"},{"version":"2.0.0"}],"linked_keg":"2.0.0"}],"casks":[{"token":"app","installed":"3.0.0","version":"9.9.9"}]}),
    )?;
    fs::write(f.home.join("brew-prefix"), brew.display().to_string())?;
    f.script("brew",r#"case "$1" in --prefix) /bin/cat "$MACOPS_TEST_HOME/brew-prefix" ;; info) /bin/cat "$MACOPS_TEST_HOME/brew-data" ;; *) exit 8 ;; esac"#)?;
    brew_native::register_discovery_fixture(&f, &f.home.join("native/brew"))?;
    npm_manifest(&npm, "dup", "4.0.0")?;
    fs::write(f.home.join("npm-prefix"), npm.display().to_string())?;
    f.script("npm",r#"case "$1" in prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;; ls) exit 98 ;; *) exit 8 ;; esac"#)?;
    uv_tool(&uv, &f.home.join(".local/bin"), "uv-app", "1.0.0")?;
    f.script(
        "uv",
        "printf invoked > \"$MACOPS_TEST_HOME/uv-called\"; exit 99",
    )?;
    f.script("cargo", "printf 'cargo-app v1.0.0:\n    cargo-exe\n'")?;
    f.script("bun",r#"printf '%s node_modules (2)\n├── bun-app@1.0.0\n└── @scope/bun@2.0.0\n' "$MACOPS_TEST_HOME/native/bun""#)?;
    f.script(
        "rustup",
        "printf 'stable-host (active, default)\n1.100.0-host\n'",
    )?;
    f.script(
        "conda",
        r#"printf '{"envs":["%s/native/conda"]}' "$MACOPS_TEST_HOME""#,
    )?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"].as_array_mut().ok_or("packages")?.push(json!({"id":"declared:other-prefix","name":"dup","kind":"cli","official_url":"https://example.org/pkg","provider":{"type":"npm","package":"dup","prefix":f.home.join("other-prefix")}}));
    write_json(&f.catalog, &catalog)?;
    let manifest = f.file("settings/manifests/mac-apps.tsv");
    fs::write(
        &manifest,
        "path\tbundle_id\tversion\towner\tsource\n/Applications/One.app\tcom.same\t1\tvendor\tmanual\n/Applications/Two.app\tcom.same\t2\tvendor\tmanual\n",
    )?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 3);
    let items = v["data"]["resources"].as_array().ok_or("resources")?;
    let duplicates: Vec<_> = items.iter().filter(|r| r["name"] == "dup").collect();
    assert_eq!(duplicates.len(), 3);
    assert_eq!(
        duplicates
            .iter()
            .map(|r| r["id"].as_str().ok_or("id"))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()?
            .len(),
        3
    );
    let keg = items
        .iter()
        .find(|r| r["manager"] == "brew")
        .ok_or("brew")?;
    assert_eq!(keg["releases"].as_array().ok_or("releases")?.len(), 2);
    let cask = items
        .iter()
        .find(|r| r["manager"] == "brew-cask")
        .ok_or("cask")?;
    assert_eq!(cask["releases"][0]["version"], "3.0.0");
    let bundles: Vec<_> = items
        .iter()
        .filter(|r| r["native_id"] == "com.same")
        .collect();
    assert_eq!(bundles.len(), 2);
    assert_ne!(bundles[0]["id"], bundles[1]["id"]);
    for manager in [
        "uv",
        "cargo",
        "bun",
        "rustup-toolchain",
        "conda-environment",
    ] {
        assert!(
            items.iter().any(|r| r["manager"] == manager),
            "missing {manager}"
        );
    }
    let (out, v) = f.run(&["app", "dup"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "ambiguous");
    for action in ["adopt", "upgrade", "uninstall"] {
        let (out, v) = f.run(&["app", action, "dup", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(v["error"]["code"], "ambiguous");
    }
    assert!(!f.home.join("uv-called").exists());
    assert!(!uv.join(".lock").exists());
    assert!(!f.file("state/write.lock").exists());
    assert_eq!(fs::read_dir(f.file("state"))?.count(), 1);
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn unified_plugin_and_mcp_records_do_not_infer_loaded_state_from_cache() -> TestResult {
    let f = Fixture::new()?;
    let project = f.file("project");
    fs::create_dir(&project)?;
    let codex = f.home.join(".codex");
    fs::create_dir(&codex)?;
    fs::write(
        codex.join("config.toml"),
        "api_key='SYNTHETIC_SECRET'\n[plugins.'demo@market']\nenabled=true\n[mcp_servers.code]\ncommand='npx'\nargs=['--token','SYNTHETIC_SECRET']\n[mcp_servers.remote]\nurl='https://SYNTHETIC_SECRET'\n",
    )?;
    fs::create_dir_all(codex.join("plugins/cache/market/demo/old"))?;
    fs::create_dir_all(codex.join("plugins/cache/market/demo/new"))?;
    let claude = f.home.join(".claude-mi");
    fs::create_dir_all(claude.join("plugins"))?;
    write_json(
        &claude.join("settings.json"),
        &json!({"enabledPlugins":{"demo@market":true},"env":{"ANTHROPIC_API_KEY":"SYNTHETIC_SECRET"}}),
    )?;
    let cache = f.home.join("plugin-payload");
    fs::create_dir(&cache)?;
    write_json(
        &claude.join("plugins/installed_plugins.json"),
        &json!({"version":2,"plugins":{"demo@market":[{"scope":"user","installPath":cache,"version":"1.0.0","mcpServers":{"SECRET":"SYNTHETIC_SECRET"}},{"scope":"project","projectPath":project,"installPath":cache,"version":"2.0.0"}]}}),
    )?;
    write_json(
        &claude.join(".claude.json"),
        &json!({"mcpServers":{"global":{"command":"npx","args":["SYNTHETIC_SECRET"]}},"projects":{project.display().to_string():{"mcpServers":{"local":{"url":"https://SYNTHETIC_SECRET"}}}}}),
    )?;
    write_json(
        &project.join(".mcp.json"),
        &json!({"mcpServers":{"project":{"command":"uvx","env":{"TOKEN":"SYNTHETIC_SECRET"}}}}),
    )?;
    let (out, v) = f.run(&["skill", "--project", project.to_str().ok_or("project")?])?;
    assert_code(&out, 0);
    let items = v["data"]["resources"].as_array().ok_or("resources")?;
    assert!(items.iter().all(|r| r["loaded"].is_null()));
    let codex = items
        .iter()
        .find(|r| r["origin"]["manager"] == "codex-config")
        .ok_or("codex")?;
    assert_eq!(codex["status"], "declared");
    assert_eq!(codex["contents"].as_array().ok_or("contents")?.len(), 2);
    let plugins: Vec<_> = items
        .iter()
        .filter(|r| r["origin"]["manager"] == "claude-plugin")
        .collect();
    assert_eq!(plugins.len(), 2);
    assert_ne!(plugins[0]["id"], plugins[1]["id"]);
    for name in ["global", "local", "project", "code", "remote"] {
        assert!(
            items
                .iter()
                .any(|r| r["delivery_kind"] == "mcp" && r["name"] == name),
            "missing {name}"
        );
    }
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn unified_native_failures_and_invalid_sources_are_partial_without_raw_diagnostics() -> TestResult {
    let f = Fixture::new()?;
    for name in [
        "mise", "brew", "npm", "uv", "cargo", "bun", "rustup", "conda", "chezmoi",
    ] {
        f.script(name, "printf 'SYNTHETIC_SECRET' >&2; exit 9")?;
    }
    let profile = f.home.join(".claude-bad");
    fs::create_dir_all(profile.join("plugins"))?;
    fs::write(
        profile.join("settings.json"),
        "{\"model\":{\"bad\":\"SYNTHETIC_SECRET\"}}",
    )?;
    fs::write(
        profile.join("plugins/installed_plugins.json"),
        "{\"version\":99,\"plugins\":{}}",
    )?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["incomplete"], true);
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    assert!(
        v["data"]["coverage"]
            .as_array()
            .ok_or("coverage")?
            .iter()
            .any(|c| c["state"] == "unavailable")
    );
    let (out, v) = f.run(&["app", "--kind", "not-supported"])?;
    assert_code(&out, 2);
    assert_eq!(v["error"]["code"], "usage");
    let (out, _) = f.run(&["app", "--project", "/Users/example/job/forbidden"])?;
    assert_code(&out, 1);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn unified_profile_errors_propagate_to_all_dependent_views_and_excluded_roots() -> TestResult {
    let f = Fixture::new()?;
    let profile = f.home.join(".claude-bad");
    fs::create_dir(&profile)?;
    fs::write(profile.join("settings.json"), r#"{"model":{}}"#)?;
    for noun in ["agent", "skill", "config"] {
        let (out, v) = f.run(&[noun, "resources"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["incomplete"], true);
        assert!(
            v["data"]["coverage"]
                .as_array()
                .ok_or("coverage")?
                .iter()
                .any(|c| c["source"] == "harness-profiles" && c["state"] == "invalid")
        );
    }
    fs::remove_file(profile.join("settings.json"))?;
    fs::remove_dir(&profile)?;
    symlink("/Users/example/job/forbidden", &profile)?;
    let (out, v) = f.run(&["skill", "resources"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["incomplete"], true);
    let (out, _) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    Ok(())
}

#[test]
fn unified_skill_lock_cannot_turn_an_empty_directory_into_a_payload() -> TestResult {
    let f = Fixture::new()?;
    let skill = f.home.join(".agents/skills/empty");
    fs::create_dir_all(&skill)?;
    write_json(
        &f.home.join(".agents/.skill-lock.json"),
        &json!({"version":3,"skills":{"empty":{"source":"owner/repo","sourceType":"github"}}}),
    )?;
    let (out, v) = f.run(&["skill", "resources"])?;
    assert_code(&out, 0);
    let items = v["data"]["resources"].as_array().ok_or("resources")?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "missing");
    assert_eq!(items[0]["contents"][0]["status"], "missing");
    fs::create_dir(skill.join("SKILL.md"))?;
    let (out, v) = f.run(&["skill", "resources"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["resources"][0]["status"], "unknown");
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn discovered_npm_instance_can_be_adopted_without_gaining_software_write_commands() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    let module = prefix.join("lib/node_modules/native-demo");
    fs::create_dir_all(&module)?;
    write_json(
        &module.join("package.json"),
        &json!({"name":"native-demo","version":"1.0.0","_resolved":"https://registry.example/?token=SYNTHETIC_SECRET"}),
    )?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    f.script(
        "npm",
        r#"case "$1" in
prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;;
*) printf 'MUTATION\n' >> "$MACOPS_TEST_HOME/npm-mutated"; exit 9 ;;
esac"#,
    )?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "npm")
        .ok_or("npm resource")?;
    let id = resource["id"].as_str().ok_or("id")?.to_owned();
    let (out, v) = f.run(&["app", "adopt", &id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    let receipt = f.read_json("state/receipts.json")?;
    assert!(receipt["installations"][&id].is_object());
    assert_eq!(
        receipt["installations"][&id]["observation"]["status"],
        "native-package"
    );
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    assert_eq!(
        v["data"]["resources"]
            .as_array()
            .ok_or("resources")?
            .iter()
            .filter(|r| r["id"] == id)
            .count(),
        1
    );
    assert_eq!(
        v["data"]["resources"]
            .as_array()
            .ok_or("resources")?
            .iter()
            .filter(|r| r["name"] == "native-demo")
            .count(),
        1
    );
    let (out, v) = f.run(&["app", "upgrade", &id, "--version", "2.0.0"])?;
    assert_code(&out, 1);
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    assert!(!f.home.join("npm-mutated").exists());
    Ok(())
}

#[test]
fn unified_npm_id_routes_existing_recipe_without_rekeying_or_touching_another_prefix() -> TestResult
{
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    let other = f.home.join("other-prefix/untouched");
    fs::create_dir_all(other.parent().ok_or("parent")?)?;
    fs::write(&other, "other installation bytes")?;
    fs::create_dir_all(prefix.join("lib/node_modules/demo"))?;
    fs::write(prefix.join("version"), "1.0.0")?;
    npm_manifest(&prefix, "demo", "1.0.0")?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    f.script("npm", r#"case "$1" in
prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;;
ls) exit 98 ;;
install) printf '%s' "${5##*@}" > "$4/version"; printf '{"name":"demo","version":"%s"}' "${5##*@}" > "$4/lib/node_modules/demo/package.json"; printf '%s
' "$*" >> "$MACOPS_TEST_HOME/writes" ;;
uninstall) /bin/rm "$4/version" "$4/lib/node_modules/demo/package.json"; /bin/rmdir "$4/lib/node_modules/demo"; printf '%s
' "$*" >> "$MACOPS_TEST_HOME/writes" ;;
*) exit 9 ;;
esac"#)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":prefix});
    write_json(&f.catalog, &catalog)?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    let rows: Vec<_> = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .filter(|r| r["manager"] == "npm")
        .collect();
    assert_eq!(rows.len(), 1);
    let id = rows[0]["id"].as_str().ok_or("id")?;
    assert_eq!(rows[0]["recipe_ref"], "fixture:demo");
    let (out, _) = f.run(&["app", "adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let receipts = f.read_json("state/receipts.json")?;
    assert!(receipts["installations"]["fixture:demo"].is_object());
    assert!(receipts["installations"][id].is_null());
    assert!(!f.home.join("writes").exists());
    let (out, v) = f.run(&["app", "upgrade", id, "--version", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["plan"]["package"]["id"], "fixture:demo");
    assert_eq!(fs::read_to_string(prefix.join("version"))?, "2.0.0");
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["id"] == id)
        .ok_or("stable id")?;
    assert_eq!(resource["management"], "adopted");
    assert_eq!(resource["releases"][0]["version"], "2.0.0");
    let (out, _) = f.run(&["app", "uninstall", id, "--yes"])?;
    assert_code(&out, 0);
    assert!(!prefix.join("version").exists());
    assert_eq!(fs::read_to_string(&other)?, "other installation bytes");
    let log = fs::read_to_string(f.home.join("writes"))?;
    assert_eq!(log.lines().count(), 2);
    assert!(
        log.lines()
            .all(|line| line.contains(&format!("--prefix {}", prefix.display())))
    );
    assert!(
        f.read_json("state/receipts.json")?["installations"]
            .as_object()
            .ok_or("receipts")?
            .is_empty()
    );
    Ok(())
}

#[test]
fn internal_npm_probe_emits_only_bare_sanitized_observation() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    fs::create_dir_all(prefix.join("lib/node_modules/demo"))?;
    let npm = f.home.join("bin/removed-npm");
    let manifest = prefix.join("lib/node_modules/demo/package.json");
    write_json(
        &manifest,
        &json!({"name":"demo","version":"1.0.0","_resolved":"SYNTHETIC_SECRET"}),
    )?;
    let args = [
        "__npm_observe",
        "--npm",
        npm.to_str().ok_or("npm")?,
        "--prefix",
        prefix.to_str().ok_or("prefix")?,
        "--name",
        "demo",
    ];
    let mut command = f.command();
    command.env("HOME", &f.home);
    let out = command.args(args).output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["status"], "native-package");
    assert!(v.get("data").is_none());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SYNTHETIC_SECRET"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("SYNTHETIC_SECRET"));
    fs::write(&manifest, b"SYNTHETIC_SECRET")?;
    let out = f.command().env("HOME", &f.home).args(args).output()?;
    assert_code(&out, 0);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(v["status"], "unknown");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SYNTHETIC_SECRET"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn npm_owners_are_resolved_across_old_keys_and_multiple_owners_block_writes() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    fs::create_dir_all(prefix.join("lib/node_modules/demo"))?;
    npm_manifest(&prefix, "demo", "1.0.0")?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    f.script("npm", r#"case "$1" in prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;; ls) exit 98 ;; *) printf MUTATION > "$MACOPS_TEST_HOME/mutated"; exit 9 ;; esac"#)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":prefix});
    write_json(&f.catalog, &catalog)?;
    let (out, _) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    catalog["packages"] = json!([]);
    write_json(&f.catalog, &catalog)?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "npm")
        .ok_or("npm")?;
    let id = resource["id"].as_str().ok_or("id")?;
    let (out, _) = f.run(&["app", "adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let mut receipts = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipts["installations"]
            .as_object()
            .ok_or("receipts")?
            .len(),
        1
    );
    let mut other = receipts["installations"]["fixture:demo"].clone();
    other["package"]["id"] = json!("old:other-owner");
    receipts["installations"]["old:other-owner"] = other;
    write_json(&f.file("state/receipts.json"), &receipts)?;
    let before = fs::read(f.file("state/receipts.json"))?;
    let active = fs::read(f.file("state/active.json"))?;
    for key in [id, "fixture:demo", "old:other-owner"] {
        let (out, v) = f.run(&["app", "adopt", key, "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(v["error"]["code"], "owner_conflict");
    }
    assert_eq!(fs::read(f.file("state/receipts.json"))?, before);
    assert_eq!(fs::read(f.file("state/active.json"))?, active);
    assert!(!f.home.join("mutated").exists());
    Ok(())
}

#[test]
fn discovered_npm_probe_preserves_proxy_invocation_name() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    fs::create_dir_all(prefix.join("lib/node_modules/native-demo"))?;
    npm_manifest(&prefix, "native-demo", "1.0.0")?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    let proxy = f.script(
        "multi-call",
        r#"[ "${0##*/}" = npm ] || exit 7
case "$1" in prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;; ls) exit 98 ;; *) exit 9 ;; esac"#,
    )?;
    std::os::unix::fs::symlink(proxy, f.home.join("bin/npm"))?;
    let (out, v) = f.run(&["app", "adopt", "native-demo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    Ok(())
}

#[test]
fn conflicting_npm_declarations_cannot_silently_fall_back_to_a_new_observer() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    fs::create_dir_all(prefix.join("lib/node_modules/demo"))?;
    npm_manifest(&prefix, "demo", "1.0.0")?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    f.script("npm", r#"case "$1" in prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;; ls) exit 98 ;; *) exit 9 ;; esac"#)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":prefix});
    let mut other = catalog["packages"][0].clone();
    other["id"] = json!("declared:other");
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(other);
    write_json(&f.catalog, &catalog)?;
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 3);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "npm")
        .ok_or("npm")?;
    assert_eq!(resource["status"], "unknown");
    let (out, v) = f.run(&[
        "app",
        "adopt",
        resource["id"].as_str().ok_or("id")?,
        "--yes",
    ])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "owner_conflict");
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn duplicate_native_keys_and_generic_native_observations_cannot_be_adopted_as_writers() -> TestResult
{
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    fs::create_dir_all(prefix.join("lib/node_modules/native-demo"))?;
    npm_manifest(&prefix, "native-demo", "1.0.0")?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    fs::write(
        prefix.join("lib/node_modules/native-demo/package.json"),
        r#"{"name":"native-demo","version":"1.0.0","version":"2.0.0"}"#,
    )?;
    f.script(
        "npm",
        r#"case "$1" in prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;; *) exit 98 ;; esac"#,
    )?;
    let (out, v) = f.run(&["app", "adopt", "native-demo", "--yes"])?;
    assert_code(&out, 3);
    assert!(v["error"]["code"].is_string());
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 3);
    assert!(
        v["data"]["coverage"]
            .as_array()
            .ok_or("coverage")?
            .iter()
            .any(|c| c["source"] == "npm" && c["state"] == "invalid")
    );
    assert!(
        !v["data"]["resources"]
            .as_array()
            .ok_or("resources")?
            .iter()
            .any(|r| r["manager"] == "npm" && r["status"] == "present")
    );
    assert!(!f.file("state/receipts.json").exists());
    let program = f.script("generic-native", r#"case "$1" in query) printf '{"status":"native-package","version":"1.0.0","source_fingerprint":"same","linked":false}' ;; *) printf MUTATION > "$MACOPS_TEST_HOME/mutated" ;; esac"#)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"command","query":{"program":program,"args":["query"]},"install":{"program":program,"args":["write"]},"upgrade":{"program":program,"args":["write"]},"uninstall":{"program":program,"args":["write"]},"synchronous":true});
    write_json(&f.catalog, &catalog)?;
    for action in ["adopt", "upgrade", "uninstall"] {
        let (out, v) = f.run(&["app", action, "fixture:demo", "--yes"])?;
        assert_code(&out, 1);
        assert_eq!(v["error"]["code"], "operation_unsupported");
    }
    assert!(!f.home.join("mutated").exists());
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn adopted_native_baseline_drift_is_visible_without_rewriting_receipts() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-root");
    fs::create_dir_all(prefix.join("lib/node_modules/native-demo"))?;
    npm_manifest(&prefix, "native-demo", "1.0.0")?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    f.script(
        "npm",
        r#"case "$1" in prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;; *) exit 98 ;; esac"#,
    )?;
    let (out, _) = f.run(&["app", "adopt", "native-demo", "--yes"])?;
    assert_code(&out, 0);
    let receipt = fs::read(f.file("state/receipts.json"))?;
    let (out, v) = f.run(&["app", "native-demo"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["resource"]["baseline"]["comparison"], "same");
    npm_manifest(&prefix, "native-demo", "2.0.0")?;
    let (out, v) = f.run(&["app", "native-demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["resource"]["baseline"]["comparison"], "different");
    assert_eq!(
        v["data"]["resource"]["baseline"]["receipt"]["version"],
        "1.0.0"
    );
    assert_eq!(
        v["data"]["resource"]["baseline"]["current"]["version"],
        "2.0.0"
    );
    let (out, v) = f.run(&["app", "resources"])?;
    assert_code(&out, 3);
    assert!(
        v["data"]["coverage"]
            .as_array()
            .ok_or("coverage")?
            .iter()
            .any(|c| c["source"] == "installation-baselines" && c["state"] == "invalid")
    );
    let receipts = f.read_json("state/receipts.json")?;
    let id = receipts["installations"]
        .as_object()
        .ok_or("receipts")?
        .keys()
        .next()
        .ok_or("key")?;
    let (out, v) = f.run(&["app", "doctor", id])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["baseline"]["comparison"], "different");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipt);
    Ok(())
}

#[test]
fn config_candidate_queries_never_execute_hooks_templates_externals_or_chezmoi() -> TestResult {
    let f = Fixture::new()?;
    let root = f.home.join(".local/share/chezmoi");
    fs::create_dir_all(&root)?;
    let marker = f.home.join("MUST_NOT_RUN");
    let executable = format!(
        "{{{{ output \"/usr/bin/touch\" \"{}\" }}}}",
        marker.display()
    );
    fs::write(root.join("dot_safe.tmpl"), &executable)?;
    fs::write(root.join(".chezmoiignore"), &executable)?;
    fs::write(
        root.join(".chezmoiexternal.toml"),
        "['other']\ntype='archive'\nurl='http://127.0.0.1:9/MUST_NOT_FETCH'\n",
    )?;
    let config = f.home.join(".config/chezmoi/chezmoi.toml");
    fs::create_dir_all(config.parent().ok_or("parent")?)?;
    fs::write(
        &config,
        format!(
            "[hooks.read-source-state.pre]\ncommand='/usr/bin/touch'\nargs=['{}']\n",
            marker.display()
        ),
    )?;
    f.script(
        "chezmoi",
        "printf CALLED > \"$MACOPS_TEST_HOME/chezmoi-called\"; exit 9",
    )?;
    let before = fs::read(&config)?;
    for (args, expected) in [(["config", "list"], 3), (["config", "resources"], 0)] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, expected);
        if expected == 3 {
            assert_eq!(value["data"]["coverage"], "unresolved");
        } else {
            assert!(
                value["data"]["coverage"]
                    .as_array()
                    .ok_or("coverage")?
                    .iter()
                    .any(|c| c["source"] == "chezmoi-native-cache" && c["state"] == "absent")
            );
        }
        assert!(!value.to_string().contains("output "));
    }
    assert!(!marker.exists());
    assert!(!f.home.join("chezmoi-called").exists());
    assert_eq!(fs::read(&config)?, before);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn npm_install_recovery_uses_committed_identity_and_preserves_receipt_bytes() -> TestResult {
    for replace in [false, true] {
        let f = Fixture::new()?;
        let prefix = f.home.join("new-npm-prefix");
        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":prefix});
        write_json(&f.catalog, &catalog)?;
        f.script(
            "npm",
            r#"[ "$1" = install ] || exit 98
/bin/mkdir -p "$4/lib/node_modules/demo"
printf '%s' '{"name":"demo","version":"1.0.0"}' > "$4/lib/node_modules/demo/package.json"
printf 'install\n' >> "$MACOPS_TEST_HOME/write-calls"
"#,
        )?;
        let (out, result) = f.run(&["app", "install", "fixture:demo", "--version", "1.0.0"])?;
        assert_code(&out, 0);
        let receipt = fs::read(f.file("state/receipts.json"))?;
        let mut journal = result["data"].clone();
        let id = journal["id"].as_str().ok_or("operation id")?.to_owned();
        journal["phase"] = json!("unknown");
        write_json(&f.file("state/active.json"), &journal)?;
        write_json(&f.file(&format!("state/operations/{id}.json")), &journal)?;
        if replace {
            fs::rename(&prefix, f.home.join("relocated"))?;
            npm_manifest(&prefix, "demo", "1.0.0")?;
        }
        fs::remove_file(f.home.join("bin/npm"))?;
        let (out, result) = f.run(&["recover", &id])?;
        assert_code(&out, if replace { 3 } else { 0 });
        assert_eq!(
            result["data"]["phase"],
            if replace { "unknown" } else { "succeeded" }
        );
        assert_eq!(fs::read(f.file("state/receipts.json"))?, receipt);
        assert_eq!(fs::read_to_string(f.home.join("write-calls"))?, "install\n");
    }
    Ok(())
}

#[test]
fn npm_known_prefix_queries_do_not_launch_ls_or_require_an_npm_binary() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-local");
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":prefix});
    write_json(&f.catalog, &catalog)?;
    npm_manifest(&prefix, "demo", "1.0.0")?;
    npm_manifest(&prefix, "@scope/other", "2.0.0")?;
    f.script(
        "npm",
        r#"case "$1" in prefix) printf '%s' "$MACOPS_TEST_HOME/npm-local" ;;
*) printf '%s\n' "$*" >> "$MACOPS_TEST_HOME/forbidden-npm"; exit 98 ;;
esac"#,
    )?;
    for args in [
        vec!["app", "discover", "npm"],
        vec!["app", "discover"],
        vec!["app", "discover", "npm"],
        vec!["app", "discover", "npm"],
        vec!["app", "resources"],
        vec!["app", "doctor", "fixture:demo"],
    ] {
        let (out, value) = f.run(&args)?;
        assert!(
            matches!(out.status.code(), Some(0 | 3)),
            "{args:?}: {value}"
        );
        assert!(value.to_string().contains("1.0.0"), "{args:?}: {value}");
        assert!(!f.home.join("forbidden-npm").exists());
    }
    fs::remove_file(f.home.join("bin/npm"))?;
    let (out, value) = f.run(&["app", "adopt", "@scope/other", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        value["data"]["plan"]["package"]["provider"]["type"],
        "npm-observed"
    );
    assert_eq!(value["data"]["plan"]["before"]["version"], "2.0.0");
    Ok(())
}

#[test]
fn npm_receipt_discovery_keeps_frozen_prefix_and_truthful_provenance() -> TestResult {
    let f = Fixture::new()?;
    let original = f.home.join("original");
    let replacement = f.home.join("replacement");
    let alias = f.home.join("alias");
    npm_manifest(&original, "demo", "1.0.0")?;
    npm_manifest(&replacement, "demo", "2.0.0")?;
    symlink(&original, &alias)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":alias});
    write_json(&f.catalog, &catalog)?;
    let (out, _) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    let baseline = fs::read(f.file("state/receipts.json"))?;
    catalog["packages"] = json!([]);
    write_json(&f.catalog, &catalog)?;
    fs::remove_file(&alias)?;
    symlink(&replacement, &alias)?;
    let (out, result) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(
        result["data"]["namespaces"],
        json!([{"prefix":original,"sources":["receipt"],"count":1}])
    );
    assert_eq!(
        result["data"]["packages"][0]["releases"][0]["version"],
        "1.0.0"
    );
    let (out, result) = f.run(&["app", "doctor", "fixture:demo"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"][0]["observation"]["status"], "unknown");
    let (out, result) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(result["error"]["code"], "manager_scope_changed");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, baseline);
    Ok(())
}

#[test]
fn npm_inventory_keeps_invalid_root_errors_and_accepts_maximum_scoped_name() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":"relative"});
    write_json(&f.catalog, &catalog)?;
    let (out, result) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["coverage"]["state"], "invalid");
    assert_eq!(result["data"]["incomplete"], true);
    let (out, result) = f.run(&["app", "discover"])?;
    assert_code(&out, 3);
    assert!(
        result["data"]
            .as_array()
            .ok_or("discover rows")?
            .iter()
            .any(|row| row["manager"] == "npm" && row["incomplete"] == true)
    );

    let scope = "s".repeat(211);
    let name = format!("@{scope}/p");
    let prefix = f.home.join("valid");
    npm_manifest(&prefix, &name, "1.2.3")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":name,"prefix":prefix});
    write_json(&f.catalog, &catalog)?;
    let (out, result) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["packages"][0]["name"], name);
    assert_eq!(result["data"]["packages"][0]["status"], "present");
    fs::write(
        prefix
            .join("lib/node_modules")
            .join(&name)
            .join("package.json"),
        serde_json::to_vec(&json!([name, "1.2.3"]))?,
    )?;
    let (out, result) = f.run(&["app", "doctor", "fixture:demo"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"][0]["observation"]["status"], "unknown");
    let (out, result) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["packages"][0]["status"], "unknown");
    Ok(())
}

#[test]
fn npm_inventory_does_not_drop_an_independent_source_when_a_receipt_is_stale() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("prefix");
    npm_manifest(&prefix, "demo", "1.0.0")?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"npm","package":"demo","prefix":prefix});
    write_json(&f.catalog, &catalog)?;
    let (out, _) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    let receipt = fs::read(f.file("state/receipts.json"))?;
    fs::rename(&prefix, f.home.join("retired-prefix"))?;
    npm_manifest(&prefix, "demo", "2.1.0")?;
    f.script(
        "npm",
        r#"[ "$1" = prefix ] || exit 98
printf '%s' "$MACOPS_TEST_HOME/prefix""#,
    )?;
    let (out, result) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(
        result["data"]["namespaces"],
        json!([{"prefix":prefix,"sources":["catalog","default"],"count":1}])
    );
    assert_eq!(result["data"]["coverage"]["state"], "invalid");
    assert_eq!(
        result["data"]["packages"][0]["releases"][0]["version"],
        "2.1.0"
    );
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipt);
    Ok(())
}

#[test]
fn npm_inventory_budget_orders_canonical_roots_before_filling_slots() -> TestResult {
    let f = Fixture::new()?;
    let mut packages = vec![];
    let alias = f.home.join("a-alias");
    for index in 0..33 {
        let prefix = f.home.join(format!("root-{index:02}"));
        fs::create_dir(&prefix)?;
        let declared = if index == 32 {
            symlink(&prefix, &alias)?;
            &alias
        } else {
            &prefix
        };
        packages.push(json!({"id":format!("demo:{index}"),"name":"demo","kind":"cli","official_url":"https://example.org","provider":{"type":"npm","package":"demo","prefix":declared}}));
    }
    write_json(&f.catalog, &json!({"schema_version":1,"packages":packages}))?;
    let (out, result) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 3);
    let roots = result["data"]["namespaces"]
        .as_array()
        .ok_or("namespaces")?;
    assert_eq!(roots.len(), 32);
    for (index, root) in roots.iter().enumerate() {
        assert_eq!(
            root["prefix"],
            json!(f.home.join(format!("root-{index:02}")))
        );
    }
    assert!(
        result["data"]["coverage"]["issues"]
            .as_array()
            .ok_or("issues")?
            .iter()
            .any(|issue| issue == "npm_namespace_limit")
    );
    Ok(())
}

#[test]
fn capability_pack_adoption_preserves_profile_isolation_lease_immunity_and_recovery() -> TestResult
{
    let f = Fixture::new()?;
    for tool in ["npx", "claude", "claudectl", "mo", "mise"] {
        f.script(
            tool,
            r#"printf 'called' > "$MACOPS_TEST_HOME/cap-poison"
exit 97"#,
        )?;
    }
    let canonical = f.home.join(".agents/skills/tdd");
    fs::create_dir_all(&canonical)?;
    fs::write(
        canonical.join("SKILL.md"),
        "---\nname: tdd\n---\nSYNTHETIC_SECRET_SKILL_BODY\n",
    )?;
    write_json(
        &f.home.join(".agents/.skill-lock.json"),
        &json!({
            "version": 3,
            "skills": {
                "tdd": {
                    "source": "mattpocock/skills",
                    "sourceType": "github",
                    "sourceUrl": "https://github.com/mattpocock/skills",
                    "skillPath": "skills/tdd",
                    "ref": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                    "skillFolderHash": "tree-v1"
                }
            }
        }),
    )?;
    for prof in ["mi", "glm"] {
        let root = f.home.join(format!(".claude-{prof}"));
        fs::create_dir_all(root.join("skills"))?;
        symlink(&canonical, root.join("skills/tdd"))?;
        let plug_cache = root.join("plugins/cache/official/code-review/1.0.0");
        fs::create_dir_all(&plug_cache)?;
        fs::write(
            plug_cache.join("plugin.json"),
            r#"{"name":"code-review","version":"1.0.0"}"#,
        )?;
        write_json(
            &root.join("settings.json"),
            &json!({"enabledPlugins":{"code-review@official":true},"env":{"ANTHROPIC_AUTH_TOKEN":"SYNTHETIC_SECRET_AUTH"}}),
        )?;
        write_json(
            &root.join("plugins/installed_plugins.json"),
            &json!({"version":2,"plugins":{"code-review@official":[{"scope":"user","installPath":plug_cache,"version":"1.0.0"}]}}),
        )?;
    }

    let (out, ambig) = f.run(&["app", "adopt", "plugin:code-review@official", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(ambig["error"]["code"], "ambiguous");

    let recipe = fixed_native_recipe(&f, "tdd")?;
    let recipe = recipe.to_str().ok_or("recipe path")?;
    let (out, preview) = f.run(&[
        "skill",
        "adopt",
        "skill:tdd",
        "--recipe",
        recipe,
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert!(!preview.to_string().contains("SYNTHETIC_SECRET"));

    let (out, adopted) = f.run(&["skill", "adopt", "skill:tdd", "--recipe", recipe, "--yes"])?;
    assert_code(&out, 0);
    let skill_id = adopted["data"]["journal"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("skill id")?
        .to_owned();
    let op_id = adopted["data"]["journal"]["id"]
        .as_str()
        .ok_or("op id")?
        .to_owned();
    let receipt_bytes = fs::read(f.file("state/receipts.json"))?;

    let (out, err) = f.run(&["app", "install", &skill_id, "--yes"])?;
    assert_code(&out, 1);
    assert_eq!(err["error"]["code"], "operation_unsupported");

    fs::write(canonical.join(".in_use"), "lease")?;
    let (out, doc_clean) = f.run(&["app", "doctor", &skill_id])?;
    assert_code(&out, 0);
    assert_eq!(doc_clean["data"][0]["baseline"]["comparison"], "same");
    fs::remove_file(canonical.join(".in_use"))?;

    assert_capability_lease_conflicts(&f, &canonical, &skill_id)?;
    fs::write(canonical.join("SKILL.md"), "---\nname: tdd\n---\nedited\n")?;
    let (out, doc_drift) = f.run(&["app", "doctor", &skill_id])?;
    assert_code(&out, 3);
    assert_eq!(doc_drift["data"][0]["baseline"]["comparison"], "different");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipt_bytes);

    let (out, rec) = f.run(&["recover", &op_id])?;
    assert_code(&out, 0);
    assert_eq!(rec["data"]["journal"]["phase"], "succeeded");
    assert!(!f.home.join("cap-poison").exists());
    Ok(())
}

fn observed_npm_binding_fixture(
    f: &Fixture,
    prefix: &std::path::Path,
) -> Result<String, Box<dyn std::error::Error>> {
    write_json(&f.catalog, &json!({"schema_version":1,"packages":[]}))?;
    let (out, data) = f.run(&["app", "discover", "npm"])?;
    assert_code(&out, 0);
    let target = data["data"]["packages"]
        .as_array()
        .ok_or("packages")?
        .iter()
        .find(|row| row["native_id"] == "@scope/demo")
        .ok_or("observed installation")?;
    let id = target["id"].as_str().ok_or("identity")?.to_owned();
    assert_code(&f.run(&["app", "adopt", &id, "--yes"])?.0, 0);
    write_authorized_npm_catalog(f, prefix, true, true)?;
    let mut explicit = f.read_json("catalog.json")?;
    explicit["packages"][0]["id"] = json!(id);
    write_json(&f.catalog, &explicit)?;
    Ok(id)
}

#[test]
fn observed_npm_instance_can_receive_an_explicit_recipe_on_the_same_identity() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-current");
    let other_prefix = f.home.join("npm-untouched");
    npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
    npm_manifest(&other_prefix, "@scope/demo", "3.0.0")?;
    let preserved = other_prefix.join("lib/node_modules/@scope/demo/package.json");
    let original = fs::read(&preserved)?;
    fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
    f.script(
        "npm",
        r#"case "$1" in
prefix) /bin/cat "$MACOPS_TEST_HOME/npm-prefix" ;;
install|uninstall)
  printf '%s\n' "$*" >> "$MACOPS_TEST_HOME/native-writes"
  if [ "$1" = install ]; then
    printf '{"name":"@scope/demo","version":"2.0.0"}' > "$4/lib/node_modules/@scope/demo/package.json"
  else
    /bin/rm "$4/lib/node_modules/@scope/demo/package.json"
    /bin/rmdir "$4/lib/node_modules/@scope/demo"
  fi ;;
*) exit 96 ;;
esac"#,
    )?;
    let id = observed_npm_binding_fixture(&f, &prefix)?;
    let id = id.as_str();
    let mut catalog = f.read_json("catalog.json")?;
    let mut independent = catalog["packages"][0].clone();
    independent["id"] = json!("npm:independent");
    independent["provider"]["prefix"] = json!(other_prefix);
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(independent);
    write_json(&f.catalog, &catalog)?;
    let initial = f.read_json("state/receipts.json")?;
    assert_eq!(
        initial["installations"][id]["package"]["provider"]["type"],
        "npm-observed"
    );
    let (out, unbound) = f.run(&["app", "upgrade", id, "--version", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(unbound["error"]["code"], "manager_scope_changed");
    assert!(!f.home.join("native-writes").exists());

    let (ordinary, rejected) = f.run(&["app", "adopt", id, "--yes"])?;
    assert_code(&ordinary, 3);
    assert_eq!(rejected["error"]["code"], "manager_scope_changed");
    let original_receipts = fs::read(f.file("state/receipts.json"))?;
    let (out, preview) = f.run(&["--dry-run", "app", "adopt", id, "--bind-recipe"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["mode"], "dry-run");
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert_eq!(fs::read(f.file("state/receipts.json"))?, original_receipts);
    assert_code(
        &f.run(&["app", "adopt", id, "--bind-recipe", "--yes"])?.0,
        0,
    );
    let enrolled = f.read_json("state/receipts.json")?;
    assert_eq!(
        enrolled["installations"]
            .as_object()
            .ok_or("installations")?
            .len(),
        1
    );
    assert_eq!(
        enrolled["installations"][id]["package"]["provider"]["type"],
        "npm"
    );
    assert!(!f.home.join("native-writes").exists());

    let (out, upgraded) = f.run(&["app", "upgrade", id, "--version", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(upgraded["data"]["plan"]["package"]["id"], id);
    let (_, observed) = f.run(&["app", "discover", "npm"])?;
    let row = observed["data"]["packages"]
        .as_array()
        .ok_or("packages")?
        .iter()
        .find(|row| row["id"] == id)
        .ok_or("stable id")?;
    assert_eq!(row["releases"][0]["version"], "2.0.0");
    assert_code(&f.run(&["app", "uninstall", id, "--yes"])?.0, 0);
    assert_eq!(fs::read(preserved)?, original);
    let log = fs::read_to_string(f.home.join("native-writes"))?;
    assert_eq!(log.lines().count(), 2);
    assert!(
        log.lines()
            .all(|line| line.contains(&format!("--prefix {}", prefix.display())))
    );
    Ok(())
}

#[test]
fn npm_binding_recovers_only_exact_old_or_new_receipts() -> TestResult {
    for (case, phase) in [
        ("uncommitted", Some("failed")),
        ("committed", Some("succeeded")),
        ("missing", None),
        ("unrelated", None),
        ("wrong-query", None),
        ("wrong-observation", None),
        ("wrong-scope", None),
        ("wrong-operation", None),
        ("invalid", None),
    ] {
        let f = Fixture::new()?;
        let prefix = f.home.join("npm-bound");
        npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
        fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
        f.script(
            "npm",
            r#"[ "$1" = prefix ] && /bin/cat "$MACOPS_TEST_HOME/npm-prefix""#,
        )?;
        let package = observed_npm_binding_fixture(&f, &prefix)?;
        let receipt = f.file("state/receipts.json");
        let original = fs::read(&receipt)?;
        fs::remove_file(f.home.join("bin/npm"))?;
        let (out, result) = f.run(&["app", "adopt", &package, "--bind-recipe", "--yes"])?;
        assert_code(&out, 0);
        let mut journal = result["data"].clone();
        let id = journal["id"].as_str().ok_or("operation id")?.to_owned();
        journal["phase"] = json!("prepared");
        journal["message"] = json!("");
        let active = f.file("state/active.json");
        let history = f.file(&format!("state/operations/{id}.json"));
        write_json(&active, &journal)?;
        write_json(&history, &journal)?;
        match case {
            "uncommitted" => fs::write(&receipt, original)?,
            "committed" => (),
            "missing" => fs::remove_file(&receipt)?,
            "unrelated" | "wrong-query" | "wrong-observation" | "wrong-scope"
            | "wrong-operation" => {
                let mut changed = f.read_json("state/receipts.json")?;
                let entry = &mut changed["installations"][&package];
                match case {
                    "unrelated" => {
                        entry["package"]["official_url"] = json!("https://example.org/replaced");
                    }
                    "wrong-query" => {
                        entry["query"]["namespace"]["anchor"]["identity"]["inode"] = json!(1);
                    }
                    "wrong-observation" => entry["observation"]["version"] = json!("2.0.0"),
                    "wrong-scope" => entry["scope"]["home"] = json!(f.home.join("other")),
                    "wrong-operation" => entry["operation_id"] = json!("OP-other"),
                    _ => return Err("unknown receipt corruption".into()),
                }
                write_json(&receipt, &changed)?;
            }
            "invalid" => fs::write(&receipt, b"{")?,
            _ => return Err("unknown fixture".into()),
        }
        let before = fs::read(&receipt).ok();
        let active_before = fs::read(&active)?;
        let history_before = fs::read(&history)?;
        assert_code(&f.run(&["--dry-run", "recover", &id])?.0, 0);
        assert_eq!(fs::read(&active)?, active_before);
        assert_eq!(fs::read(&history)?, history_before);
        for flag in ["--mark-failed", "--resolve-failed"] {
            let (out, rejected) = f.run(&["recover", &id, flag, "--yes"])?;
            assert_code(&out, 3);
            assert_eq!(rejected["error"]["code"], "mark_failed_unsupported");
            assert_eq!(fs::read(&active)?, active_before);
            assert_eq!(fs::read(&history)?, history_before);
            assert_eq!(fs::read(&receipt).ok(), before);
        }
        let (out, recovered) = f.run(&["recover", &id])?;
        if let Some(phase) = phase {
            assert_code(&out, if phase == "failed" { 3 } else { 0 });
            assert_eq!(recovered["data"]["phase"], phase, "{case}");
            let (out, second) = f.run(&["recover", &id])?;
            assert_code(&out, if phase == "failed" { 3 } else { 0 });
            assert_eq!(second["data"], recovered["data"]);
        } else {
            assert_code(&out, 3);
            assert_eq!(
                recovered["error"]["code"], "npm_binding_outcome_unknown",
                "{case}"
            );
            assert_eq!(fs::read(&active)?, active_before);
            assert_eq!(fs::read(&history)?, history_before);
        }
        assert_eq!(fs::read(&receipt).ok(), before, "{case}");
    }
    Ok(())
}

#[test]
fn npm_binding_rejects_a_missing_recipe_changed_scope_and_installation_drift() -> TestResult {
    for (case, code) in [
        ("missing-catalog", "npm_binding_recipe_required"),
        ("no-authority", "npm_binding_recipe_required"),
        ("wrong-id", "npm_binding_recipe_required"),
        ("duplicate-recipe", "owner_conflict"),
        ("aliased-recipe", "owner_conflict"),
        ("case-aliased-recipe", "owner_conflict"),
        ("missing-receipt", "npm_binding_observation_required"),
        ("unfrozen-receipt", "manager_scope_changed"),
        ("changed-root", "manager_scope_changed"),
        ("changed-name", "manager_scope_changed"),
        ("drift", "installation_drift"),
    ] {
        let f = Fixture::new()?;
        let prefix = f.home.join("npm-bound");
        npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
        if case == "case-aliased-recipe" && !f.home.join("NPM-BOUND").exists() {
            continue;
        }
        fs::write(f.home.join("npm-prefix"), prefix.display().to_string())?;
        f.script(
            "npm",
            r#"[ "$1" = prefix ] && /bin/cat "$MACOPS_TEST_HOME/npm-prefix""#,
        )?;
        let id = observed_npm_binding_fixture(&f, &prefix)?;
        let receipt = f.file("state/receipts.json");
        let mut catalog = f.read_json("catalog.json")?;
        match case {
            "missing-catalog" => catalog["packages"] = json!([]),
            "no-authority" => {
                catalog["packages"][0]["provider"]
                    .as_object_mut()
                    .ok_or("provider")?
                    .remove("authority");
            }
            "wrong-id" => catalog["packages"][0]["id"] = json!("npm:other"),
            "duplicate-recipe" | "aliased-recipe" | "case-aliased-recipe" => {
                let mut second = catalog["packages"][0].clone();
                second["id"] = json!("npm:second");
                if case == "aliased-recipe" {
                    let alias = f.home.join("npm-alias");
                    symlink(&prefix, &alias)?;
                    second["provider"]["prefix"] = json!(alias);
                } else if case == "case-aliased-recipe" {
                    second["provider"]["prefix"] = json!(f.home.join("NPM-BOUND"));
                }
                catalog["packages"]
                    .as_array_mut()
                    .ok_or("packages")?
                    .push(second);
            }
            "missing-receipt" => fs::remove_file(&receipt)?,
            "unfrozen-receipt" => {
                let mut receipts = f.read_json("state/receipts.json")?;
                receipts["installations"][&id]
                    .as_object_mut()
                    .ok_or("receipt")?
                    .remove("query");
                write_json(&receipt, &receipts)?;
            }
            "changed-root" => {
                fs::rename(&prefix, f.home.join("relocated"))?;
                npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
            }
            "changed-name" => {
                catalog["packages"][0]["provider"]["package"] = json!("@scope/other");
            }
            "drift" => npm_manifest(&prefix, "@scope/demo", "2.0.0")?,
            _ => return Err("unknown fixture".into()),
        }
        write_json(&f.catalog, &catalog)?;
        let old_receipt = fs::read(&receipt).ok();
        let old_active = fs::read(f.file("state/active.json"))?;
        fs::remove_file(f.home.join("bin/npm"))?;
        let (out, data) = f.run(&["app", "adopt", &id, "--bind-recipe", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(data["error"]["code"], code, "{case}");
        assert_eq!(fs::read(&receipt).ok(), old_receipt, "{case}");
        assert_eq!(fs::read(f.file("state/active.json"))?, old_active, "{case}");
    }
    Ok(())
}

#[test]
#[allow(clippy::too_many_lines)]
fn npm_write_authority_enforces_remove_registry_and_env_sanitization() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-auth-root");
    npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
    npm_manifest(&prefix, "sibling-keep", "2.0.0")?;
    fs::write(
        f.home.join(".npmrc"),
        "//registry.npmjs.org/:_authToken=SYNTHETIC_NPM_SECRET\n",
    )?;
    f.script(
        "npm",
        r#"if [ -n "${NODE_OPTIONS:-}" ] || [ -n "${npm_config_registry:-}" ] || [ -n "${NPM_Config_Registry:-}" ]; then
  printf 'env-leaked' > "$MACOPS_TEST_HOME/npm-env-leak"
  exit 91
fi
printf '%s\n' "$*" >> "$MACOPS_TEST_HOME/npm-calls.log"
case "$1" in
  view) printf '"1.2.0"\n' ;;
  install)
    mkdir -p "$4/lib/node_modules/@scope/demo"
    printf '{"name":"@scope/demo","version":"1.2.0"}\n' > "$4/lib/node_modules/@scope/demo/package.json" ;;
  uninstall) rm -rf "$4/lib/node_modules/@scope/demo" ;;
  *) exit 92 ;;
esac"#,
    )?;

    write_authorized_npm_catalog(&f, &prefix, false, false)?;
    assert_code(&f.run(&["app", "adopt", "npm:scoped-demo", "--yes"])?.0, 0);
    assert_eq!(
        f.run(&["app", "uninstall", "npm:scoped-demo", "--yes"])?.1["error"]["code"],
        "remove_authority_required"
    );
    assert_eq!(
        f.run(&[
            "app",
            "upgrade",
            "npm:scoped-demo",
            "--version",
            "1.2.0",
            "--yes"
        ])?
        .1["error"]["code"],
        "registry_authority_required"
    );

    write_authorized_npm_catalog(&f, &prefix, true, true)?;
    let (out, drift) = f.run(&[
        "app",
        "upgrade",
        "npm:scoped-demo",
        "--version",
        "1.2.0",
        "--yes",
    ])?;
    assert_code(&out, 3);
    assert_eq!(drift["error"]["code"], "installation_drift");
    assert!(
        drift["error"]["message"]
            .as_str()
            .ok_or("msg")?
            .contains("写权限")
    );

    let (out, adopted) = f.run(&["app", "adopt", "npm:scoped-demo", "--yes"])?;
    assert_code(&out, 0);
    assert!(!adopted.to_string().contains("SYNTHETIC_NPM_SECRET"));
    assert_eq!(
        f.run(&["app", "refresh", "npm:scoped-demo"])?.1["data"]["results"][0]["version"],
        "1.2.0"
    );

    let raw = f
        .command()
        .env("NODE_OPTIONS", "--require=/evil.js")
        .env("NPM_Config_Registry", "https://evil.invalid")
        .args([
            "app",
            "upgrade",
            "npm:scoped-demo",
            "--version",
            "1.2.0",
            "--yes",
        ])
        .output()?;
    assert_code(&raw, 0);
    assert!(!f.home.join("npm-env-leak").exists());
    let calls = fs::read_to_string(f.home.join("npm-calls.log"))?;
    assert!(calls.contains("--@scope:registry=https://npm.pkg.example.test/org"));
    assert!(calls.contains("--replace-registry-host=never") && calls.contains("--ignore-scripts"));

    let calls_before_link = fs::read_to_string(f.home.join("npm-calls.log"))?;
    let real_nm = prefix.join("lib/node_modules");
    let moved_nm = prefix.join("lib/node_modules_real");
    fs::rename(&real_nm, &moved_nm)?;
    symlink(&moved_nm, &real_nm)?;
    let (out, link_err) = f.run(&["app", "uninstall", "npm:scoped-demo", "--yes"])?;
    assert_code(&out, 3);
    assert!(matches!(
        link_err["error"]["code"].as_str(),
        Some("unsupported_npm_placement" | "npm_namespace_link_unsafe")
    ));
    assert_eq!(
        fs::read_to_string(f.home.join("npm-calls.log"))?,
        calls_before_link
    );
    fs::remove_file(&real_nm)?;
    fs::rename(&moved_nm, &real_nm)?;

    let (out, removed) = f.run(&["app", "uninstall", "npm:scoped-demo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(removed["data"]["phase"], "succeeded");
    assert!(
        prefix
            .join("lib/node_modules/sibling-keep/package.json")
            .is_file()
    );
    Ok(())
}

#[test]
fn native_mise_reconcile_honors_chezmoi_roots_redacts_secrets_and_blocks_overrides() -> TestResult {
    let f = Fixture::new()?;
    for tool in ["chezmoi", "mise", "mo"] {
        f.script(
            tool,
            r#"printf 'called' > "$MACOPS_TEST_HOME/reconcile-poison"
exit 97"#,
        )?;
    }
    let custom_src = f.home.join("custom-cz-src");
    let custom_dst = f.home.join("custom-cz-dst");
    fs::create_dir_all(custom_src.join("dot_config/mise"))?;
    fs::create_dir_all(custom_dst.join(".config/mise"))?;
    let src_toml = custom_src.join("dot_config/mise/private_config.toml");
    let live_toml = custom_dst.join(".config/mise/config.toml");

    fs::write(
        &src_toml,
        "[tools]\n\"aqua:openai/codex\" = \"0.10.0\"\n[env]\nTOKEN = \"SYNTHETIC_ENV_SECRET_OLD\"\n",
    )?;
    fs::write(
        &live_toml,
        "[tools]\n\"aqua:openai/codex\" = \"0.11.0\"\n[env]\nTOKEN = \"SYNTHETIC_ENV_SECRET_NEW\"\n",
    )?;

    let src_arg = custom_src.to_str().ok_or("s")?;
    let dst_arg = custom_dst.to_str().ok_or("d")?;
    let (out, env_drift) = f.run(&[
        "--chezmoi-source",
        src_arg,
        "--chezmoi-destination",
        dst_arg,
        "config",
        "reconcile",
        "codex",
        "--yes",
    ])?;
    assert_code(&out, 3);
    assert_eq!(env_drift["data"]["reason"], "unrelated_keys_drifted");
    assert!(!env_drift.to_string().contains("SYNTHETIC_ENV_SECRET"));

    fs::write(
        &src_toml,
        "[tools]\n\"aqua:openai/codex\" = \"0.10.0\"\n[env]\nTOKEN = \"SYNTHETIC_ENV_SECRET_NEW\"\n",
    )?;
    let (out, mimo_err) = f.run(&["config", "reconcile", "mimo", "--yes"])?;
    assert_code(&out, 1);
    assert_eq!(mimo_err["error"]["code"], "vendor_tool_no_mise_pin");

    let env_blocked = f
        .command()
        .env("MISE_ENV", "staging")
        .args(["agent", "upgrade"])
        .output()?;
    assert_code(&env_blocked, 3);

    let (out, reconciled) = f.run(&[
        "--chezmoi-source",
        src_arg,
        "--chezmoi-destination",
        dst_arg,
        "config",
        "reconcile",
        "codex",
        "--yes",
    ])?;
    assert_code(&out, 0);
    assert_eq!(reconciled["data"]["reconcile_state"], "reconciled");
    let (out, clean) = f.run(&[
        "--chezmoi-source",
        src_arg,
        "--chezmoi-destination",
        dst_arg,
        "agent",
        "upgrade",
    ])?;
    assert_code(&out, 0);
    assert_eq!(clean["data"]["reconcile_state"], "in_sync");
    assert!(!f.home.join("reconcile-poison").exists());
    Ok(())
}

#[test]
fn config_reconcile_preview_and_execution_agree_on_comment_drift_without_writes() -> TestResult {
    let f = Fixture::new()?;
    let source = f
        .home
        .join(".local/share/chezmoi/dot_config/mise/private_config.toml");
    let live = f.home.join(".config/mise/config.toml");
    fs::create_dir_all(source.parent().ok_or("source parent")?)?;
    fs::create_dir_all(live.parent().ok_or("live parent")?)?;
    let before_source = "# source\n[tools]\n\"aqua:openai/codex\" = \"1.0.0\"\n";
    let before_live = "# local\n[tools]\n\"aqua:openai/codex\" = \"2.0.0\"\n";
    fs::write(&source, before_source)?;
    fs::write(&live, before_live)?;
    for flag in ["--dry-run", "--yes"] {
        let (out, value) = f.run(&["config", "reconcile", "codex", flag])?;
        assert_code(&out, 3);
        assert_eq!(value["data"]["reconcile_state"], "drift");
        assert_eq!(value["data"]["reason"], "formatting_or_comment_drift");
        assert_eq!(fs::read_to_string(&source)?, before_source);
        assert_eq!(fs::read_to_string(&live)?, before_live);
        assert!(!f.file("state/active.json").exists());
    }
    Ok(())
}

#[test]
fn invalid_typed_json_never_echoes_configuration_values_in_diagnostics() -> TestResult {
    let f = Fixture::new()?;
    write_json(
        &f.catalog,
        &json!({"schema_version":"SYNTHETIC_SECRET_VALUE_0014", "packages":[]}),
    )?;
    let (out, value) = f.run(&["app", "resources"])?;
    assert_code(&out, 1);
    assert_eq!(value["error"]["code"], "invalid_json");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("SYNTHETIC_SECRET_VALUE_0014"));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("SYNTHETIC_SECRET_VALUE_0014"));
    Ok(())
}

#[test]
fn native_skills_readonly_and_no_pending_recovery_never_execute_installers_or_write_state()
-> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo", "other"])?;
    for tool in ["npx", "skills", "claude", "mise", "mo"] {
        f.script(
            tool,
            r#"printf 'called' > "$MACOPS_TEST_HOME/installer-poison"; exit 97"#,
        )?;
    }
    for args in [
        vec!["skill"],
        vec!["skill", "doctor", "demo"],
        vec!["skill", "doctor", "demo"],
        vec!["skill", "policy", "check"],
        vec!["skill", "policy", "recover"],
        vec!["skill", "policy", "recover", "--change-id", "CASE-1"],
    ] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, 0);
        assert_eq!(value["data"]["backend"], "rust");
        assert_eq!(value["data"]["persistentWrites"], false);
        assert!(!f.home.join("installer-poison").exists());
        assert!(!f.file("state").exists());
        assert!(!f.file("cache").exists());
    }
    let pending = f.home.join(".agents/invocation-policy.pending.json");
    fs::write(&pending, "{\"schema_version\":1}")?;
    let (out, value) = f.run(&["skill", "policy", "recover"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "capability_recovery_required");
    assert_eq!(fs::read_to_string(pending)?, "{\"schema_version\":1}");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn pending_capability_transactions_block_health_queries_without_mutating_pending_or_invoking_backends()
-> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    let path = f.home.join(".agents/invocation-policy.pending.json");
    let before = b"{\"schema_version\":1,\"phase\":\"running\"}";
    fs::write(&path, before)?;
    for args in [
        vec!["skill"],
        vec!["skill", "doctor", "demo"],
        vec!["skill", "doctor", "demo"],
        vec!["skill", "policy", "check"],
        vec!["skill", "resources"],
    ] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "capability_recovery_required");
        assert_eq!(fs::read(&path)?, before);
        assert!(!f.file("state").exists());
    }
    Ok(())
}

#[test]
fn missing_policy_targets_and_duplicate_native_lock_keys_are_attention_not_healthy() -> TestResult {
    let f = Fixture::new()?;
    let rules = f
        .root
        .path()
        .join("settings/manifests/agent-invocation-policy.json");
    write_json(
        &rules,
        &json!({"schema_version":1,"standalone_manual_only":["absent"]}),
    )?;
    let (out, value) = f.run(&["skill", "policy", "check"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["status"], "drift");
    assert_eq!(value["data"]["missing"], json!(["absent"]));
    assert!(!f.file("state").exists());
    native_skill_fixture(&f, &["demo"])?;
    let record = r#"{"source":"owner/repo","sourceType":"github","skillFolderHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#;
    fs::write(
        f.home.join(".agents/.skill-lock.json"),
        format!("{{\"version\":3,\"skills\":{{\"demo\":{record},\"demo\":{record}}}}}"),
    )?;
    let (out, value) = f.run(&["skill", "doctor"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["incomplete"], true);
    assert!(
        value["data"]["coverage"]
            .as_array()
            .ok_or("coverage")?
            .iter()
            .any(|c| c["state"] == "invalid")
    );
    Ok(())
}

#[test]
fn native_capability_queries_reject_array_shaped_lock_roots_and_records_with_or_without_channel_config()
-> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    for channels in [false, true] {
        if channels {
            write_json(
                &f.root
                    .path()
                    .join("settings/manifests/agent-source-channels.json"),
                &json!({"schema_version":1,"source_ids":{}}),
            )?;
        }
        for invalid in [
            "[3,{}]",
            r#"{"version":3,"skills":{"demo":["owner/repo","github",null,null,null,null]}}"#,
        ] {
            fs::write(f.home.join(".agents/.skill-lock.json"), invalid)?;
            for args in [vec!["skill", "doctor"], vec!["skill", "resources"]] {
                let (out, value) = f.run(&args)?;
                assert_code(&out, 3);
                assert_eq!(value["data"]["incomplete"], true);
                assert!(!f.file("state").exists());
            }
        }
    }
    Ok(())
}

fn seed_legacy_policy_receipt(f: &Fixture) -> Result<String, Box<dyn std::error::Error>> {
    let recipe = fixed_native_recipe(f, "demo")?;
    let recipe = recipe.to_str().ok_or("recipe path")?;
    let (out, adopted) = f.run(&["--yes", "skill", "adopt", "skill:demo", "--recipe", recipe])?;
    assert_code(&out, 0);
    let skill_id = adopted["data"]["journal"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("skill id")?
        .to_owned();
    // Keep the common receipt as the fixture's older installation record.
    fs::remove_file(f.home.join(".config/macops/native-skills-recipes.json"))?;
    fs::remove_file(
        f.file("state/native-skills/receipts")
            .join(format!("{}.json", skill_id.replace(':', "-"))),
    )?;
    Ok(skill_id)
}

#[test]
fn native_policy_sync_commits_and_retires_overlays_without_legacy_execution_or_receipt_drift()
-> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    let rules = f.file("settings/manifests/agent-invocation-policy.json");
    write_json(
        &rules,
        &json!({"schema_version":1,"standalone_manual_only":["demo"]}),
    )?;
    let skill = f.home.join(".agents/skills/demo/SKILL.md");
    let original = fs::read(&skill)?;
    for tool in ["mo", "npx", "skills", "claude", "mise"] {
        f.script(
            tool,
            r#"printf 'called' > "$MACOPS_TEST_HOME/installer-poison"; exit 97"#,
        )?;
    }
    let (out, value) = f.run(&["--dry-run", "skill", "policy", "sync", "--change-id", "DRY"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["persistentWrites"], false);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    let (out, value) = f.run(&["skill", "policy", "sync"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "confirmation_required");
    assert!(!f.file("state").exists());
    let skill_id = seed_legacy_policy_receipt(&f)?;
    let receipt_path = f.file("state/receipts.json");
    let receipt_before = fs::read(&receipt_path)?;
    let receipts: serde_json::Value = serde_json::from_slice(&receipt_before)?;
    let receipt = &receipts["installations"][&skill_id];
    assert_eq!(
        receipt["package"]["provider"]["type"],
        "capability-observed"
    );
    assert_eq!(
        receipt["package"]["provider"]["payload"],
        json!(skill.parent().ok_or("payload")?)
    );
    let codex_policy = skill.parent().ok_or("payload")?.join("agents/openai.yaml");
    assert!(!codex_policy.exists());
    let (out, value) = f.run(&["--yes", "skill", "policy", "sync", "--change-id", "POLICY"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["status"], "succeeded");
    assert_ne!(fs::read(&skill)?, original);
    assert!(fs::read_to_string(&skill)?.contains("disable-model-invocation: true"));
    assert!(fs::read_to_string(&codex_policy)?.contains("allow_implicit_invocation: false"));
    let applied_bytes = fs::read(&receipt_path)?;
    let applied_book: serde_json::Value = serde_json::from_slice(&applied_bytes)?;
    let applied = &applied_book["installations"][&skill_id];
    assert_eq!(applied["operation_id"], value["data"]["operation_id"]);
    assert_eq!(applied["observation"]["pack"]["file_count"], 2);
    for key in ["revision", "record_digest", "payload_identity"] {
        assert_eq!(
            applied["observation"]["pack"][key],
            receipt["observation"]["pack"][key]
        );
    }
    let (out, value) = f.run(&["app", "doctor", &skill_id])?;
    assert_code(&out, 0);
    assert_eq!(value["data"][0]["baseline"]["comparison"], "same");
    let (out, value) = f.run(&["skill", "policy", "recover"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["persistentWrites"], false);
    assert_eq!(fs::read(&receipt_path)?, applied_bytes);
    write_json(
        &rules,
        &json!({"schema_version":1,"standalone_manual_only":[]}),
    )?;
    let (out, value) = f.run(&["--yes", "skill", "policy", "sync", "--change-id", "RETIRE"])?;
    assert_code(&out, 0);
    assert_eq!(fs::read(&skill)?, original);
    assert!(!codex_policy.exists());
    let retired_bytes = fs::read(&receipt_path)?;
    let retired_book: serde_json::Value = serde_json::from_slice(&retired_bytes)?;
    let retired = &retired_book["installations"][&skill_id];
    assert_eq!(retired["operation_id"], value["data"]["operation_id"]);
    assert_eq!(retired["observation"], receipt["observation"]);
    for current in [applied, retired] {
        for key in ["scope", "query", "package"] {
            assert_eq!(current[key], receipt[key]);
        }
    }
    let (out, value) = f.run(&["app", "doctor", &skill_id])?;
    assert_code(&out, 0);
    assert_eq!(value["data"][0]["baseline"]["comparison"], "same");
    assert_eq!(fs::read(&receipt_path)?, retired_bytes);
    assert!(!f.home.join("installer-poison").exists());
    assert!(!f.home.join("cap-poison").exists());
    assert!(!f.file("cache").exists());
    Ok(())
}

#[test]
fn brew_positional_records_are_reported_invalid_without_creating_management_state() -> TestResult {
    let f = Fixture::new()?;
    fs::write(
        f.home.join("brew-prefix"),
        f.home.join("native/brew").display().to_string(),
    )?;
    f.script("brew", r#"case "$1" in --prefix) /bin/cat "$MACOPS_TEST_HOME/brew-prefix" ;; info) /bin/cat "$MACOPS_TEST_HOME/brew-data" ;; *) exit 8 ;; esac"#)?;
    brew_native::register_discovery_fixture(&f, &f.home.join("native/brew"))?;
    for positional in [
        json!({"formulae":[["demo",null,[{"version":"1.0.0"}],null]],"casks":[]}),
        json!({"formulae":[],"casks":[["app",null,"2.0.0"]]}),
        json!({"formulae":[{"name":"demo","installed":[["1.0.0"]]}],"casks":[]}),
    ] {
        write_json(&f.home.join("brew-data"), &positional)?;
        let (out, value) = f.run(&["app", "resources"])?;
        assert_code(&out, 3);
        let report = value["data"]["coverage"]
            .as_array()
            .and_then(|v| v.iter().find(|row| row["source"] == "brew"))
            .ok_or("brew coverage")?;
        assert_eq!(report["state"], "invalid");
        assert!(
            report["issues"]
                .as_array()
                .is_some_and(|v| v.contains(&json!("invalid_brew_inventory")))
        );
        assert!(
            !value["data"]["resources"]
                .as_array()
                .ok_or("resources")?
                .iter()
                .any(|r| r["manager"] == "brew" || r["manager"] == "brew-cask")
        );
        assert!(!f.file("state/write.lock").exists());
        assert_eq!(fs::read_dir(f.file("state"))?.count(), 1);
        assert!(!f.file("cache").exists());
    }
    Ok(())
}

#[test]
fn native_profile_sync_is_scoped_preserves_models_and_reports_unfinished_configuration_policies()
-> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha", "beta"])?;
    let profile = f.home.join(".claude-mi");
    fs::create_dir(&profile)?;
    let settings = profile.join("settings.json");
    fs::write(
        &settings,
        b"{\"model\":\"custom-model\",\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"PRIVATE_SYNC_SECRET\"}}",
    )?;
    let original = fs::read(&settings)?;
    fs::create_dir_all(f.home.join(".claude/skills/alpha"))?;
    fs::create_dir_all(f.home.join(".cursor/skills/alpha"))?;
    let root = f.file("settings/manifests/skill-profiles.json");
    write_json(
        &root,
        &json!({"claude":{"mi":{"links":["alpha"],"pstack_allow":["tdd"]}}}),
    )?;
    for tool in ["mo", "claude", "mise", "npx", "skills"] {
        f.script(
            tool,
            r#"printf called > "$MACOPS_TEST_HOME/poison-sync"; exit 97"#,
        )?;
    }
    let (out, unknown) = f.run(&["--dry-run", "skill", "sync", "--all"])?;
    assert_code(&out, 3);
    assert_eq!(unknown["error"]["code"], "unverified_profile_configuration");
    assert!(!profile.join("skills").exists());
    let (out, plan) = f.run(&["--dry-run", "skill", "sync", "--only", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(plan["data"]["backend"], "rust");
    assert_eq!(plan["data"]["incomplete"], true);
    assert!(!f.file("state").exists());
    let (out, synced) = f.run(&[
        "--yes",
        "skill",
        "sync",
        "--only",
        "alpha",
        "--change-id",
        "LINKS-PUBLIC",
    ])?;
    assert_code(&out, 3);
    assert_eq!(synced["data"]["status"], "succeeded");
    assert_eq!(
        fs::read_link(profile.join("skills/alpha"))?,
        std::path::Path::new("../../.agents/skills/alpha")
    );
    assert!(!profile.join("skills/beta").exists());
    assert_eq!(fs::read(&settings)?, original);
    assert!(!synced.to_string().contains("PRIVATE_SYNC_SECRET"));
    assert!(f.home.join(".claude/skills/alpha").is_dir());
    assert!(f.home.join(".cursor/skills/alpha").is_dir());
    assert!(!f.home.join("poison-sync").exists());
    assert!(!f.file("cache").exists());
    let (out, noop) = f.run(&["--yes", "skill", "sync", "--only", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(noop["data"]["persistentWrites"], false);
    Ok(())
}

#[test]
fn different_native_sources_cannot_silently_share_a_source_selector() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha", "beta"])?;
    fs::create_dir(f.home.join(".claude-mi"))?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let mut book: serde_json::Value = serde_json::from_slice(&fs::read(&lock)?)?;
    book["skills"]["alpha"]["source"] = json!("owner/alpha");
    book["skills"]["beta"]["source"] = json!("owner/beta");
    write_json(&lock, &book)?;
    write_json(
        &f.file("settings/manifests/agent-source-channels.json"),
        &json!({"schema_version":1,"source_ids":{"owner/alpha":"shared","owner/beta":"shared"}}),
    )?;
    let (out, value) = f.run(&["--yes", "skill", "sync", "--only", "shared"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "ambiguous_link_source_alias");
    assert!(!f.home.join(".claude-mi/skills").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn configuration_profile_selector_accepts_explicit_static_roots_without_requiring_canonical_skills()
-> TestResult {
    let f = Fixture::new()?;
    let profile = f.home.join(".claude-mi");
    fs::create_dir(&profile)?;
    let live = profile.join("settings.json");
    let source = f.home.join("explicit-source");
    let source_file = source.join("dot_claude-mi/settings.json");
    fs::create_dir_all(source_file.parent().ok_or("parent")?)?;
    let live_before = br#"{"model":"live-\u006dodel","env":{"KEY":"PRIVATE_PROFILE_SECRET"}}"#;
    fs::write(&live, live_before)?;
    fs::write(
        &source_file,
        br#"{"model":"source-model","enabledPlugins":{"review@official":true}}"#,
    )?;
    write_json(
        &f.file("settings/manifests/skill-profiles.json"),
        &json!({"claude":{"mi":{"plugins_off":["review@official"]}}}),
    )?;
    let source_arg = source.to_str().ok_or("utf8")?;
    let home_arg = f.home.to_str().ok_or("utf8")?;
    let scope = [
        "--chezmoi-source",
        source_arg,
        "--chezmoi-destination",
        home_arg,
    ];
    let args: Vec<_> = scope
        .into_iter()
        .chain(["--dry-run", "skill", "sync", "--only", "profile:mi"])
        .collect();
    let (out, dry) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(dry["data"]["planned_changes"], 2);
    assert_eq!(fs::read(&live)?, live_before);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    let args: Vec<_> = scope
        .into_iter()
        .chain(["--yes", "skill", "sync", "--only", "profile:mi"])
        .collect();
    let (out, applied) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(applied["data"]["status"], "succeeded");
    assert!(!applied.to_string().contains("PRIVATE_PROFILE_SECRET"));
    for path in [&live, &source_file] {
        let config: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(config["enabledPlugins"]["review@official"], false);
    }
    let (out, noop) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(noop["data"]["status"], "same");
    assert!(
        !noop["data"]["persistentWrites"]
            .as_bool()
            .ok_or("boolean")?
    );
    assert!(!f.home.join(".agents/skills").exists());
    Ok(())
}

#[test]
fn declared_github_source_selector_cannot_claim_a_different_native_repository() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha"])?;
    fs::create_dir(f.home.join(".claude-mi"))?;
    write_json(
        &f.file("settings/manifests/managed-agent-assets.json"),
        &json!({"schema_version":1,"assets":{"alpha":{"kind":"github-skill","repository":"expected/declared-repo","revision":"b".repeat(40),"path":"skills/alpha"}}}),
    )?;
    let (out, value) = f.run(&["--yes", "skill", "sync", "--only", "source:alpha"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "declared_link_source_mismatch");
    assert!(!f.home.join(".claude-mi/skills").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn fixed_native_recipe_adoption_preserves_native_owned_payload_and_raw_documents() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["demo"])?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let lock_before = fs::read(&lock)?;
    let payload = f.home.join(".agents/skills/demo");
    fs::create_dir(payload.join("empty"))?;
    let before = fs::read(payload.join("SKILL.md"))?;
    for tool in ["npx", "mo", "skills", "git"] {
        f.script(
            tool,
            r#"printf 'called' > "$MACOPS_TEST_HOME/source-poison"; exit 97"#,
        )?;
    }
    let recipe = fixed_native_recipe(&f, "demo")?;
    let recipe = recipe.to_str().ok_or("recipe path")?;
    let (out, value) = f.run(&[
        "--dry-run",
        "skill",
        "adopt",
        "skill:demo",
        "--recipe",
        recipe,
    ])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["recipe"]["scope"], "global");
    assert!(!f.file("state").exists());
    assert!(!f.home.join(".agents/backups").exists());
    let (out, value) = f.run(&["--yes", "skill", "adopt", "skill:demo", "--recipe", recipe])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["journal"]["phase"], "succeeded");
    let member = value["data"]["journal"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("member")?;
    let receipt = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipt["installations"][member]["package"]["provider"]["type"],
        "capability-observed"
    );
    assert!(payload.join("empty").is_dir());
    assert!(!f.home.join(".agents/managed-assets.json").exists());
    assert!(!f.home.join(".agents/backups").exists());
    assert_eq!(fs::read(&lock)?, lock_before);
    assert_eq!(fs::read(payload.join("SKILL.md"))?, before);
    assert!(
        !f.home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    assert!(!f.home.join("source-poison").exists());
    Ok(())
}
#[test]
fn malformed_native_recipe_adoption_and_chezmoi_override_cannot_reach_legacy_route() -> TestResult {
    for malformed in [false, true] {
        let f = Fixture::new()?;
        native_skill_fixture(&f, &["demo"])?;
        if malformed {
            let lock = f.home.join(".agents/.skill-lock.json");
            let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&lock)?)?;
            value["skills"]["demo"]
                .as_object_mut()
                .ok_or("row")?
                .remove("skillPath");
            write_json(&lock, &value)?;
        }
        let recipe = fixed_native_recipe(&f, "demo")?;
        let recipe = recipe.to_str().ok_or("recipe path")?;
        let args = if malformed {
            vec!["--yes", "skill", "adopt", "skill:demo", "--recipe", recipe]
        } else {
            vec![
                "--yes",
                "--chezmoi-source",
                "/private/tmp/macops-unused-source",
                "skill",
                "adopt",
                "skill:demo",
                "--recipe",
                recipe,
            ]
        };
        let (out, value) = f.run(&args)?;
        assert_code(&out, if malformed { 3 } else { 1 });
        assert_eq!(
            value["error"]["code"],
            if malformed {
                "native_skills_evidence_changed"
            } else {
                "config_scope_unsupported"
            }
        );
        assert!(!f.file("state").exists());
        assert!(!f.home.join(".agents/backups").exists());
    }
    Ok(())
}

fn assert_capability_lease_conflicts(
    f: &Fixture,
    canonical: &std::path::Path,
    skill_id: &str,
) -> TestResult {
    symlink("/bin/sh", canonical.join(".in_use"))?;
    let (out, doc_link_lease) = f.run(&["app", "doctor", skill_id])?;
    assert_code(&out, 3);
    assert_eq!(
        doc_link_lease["data"][0]["baseline"]["comparison"],
        "unknown"
    );
    fs::remove_file(canonical.join(".in_use"))?;

    fs::create_dir_all(canonical.join(".in_use"))?;
    fs::write(canonical.join(".in_use/run.sh"), "echo bad")?;
    let (out, doc_dir_lease) = f.run(&["app", "doctor", skill_id])?;
    assert_code(&out, 3);
    assert_eq!(
        doc_dir_lease["data"][0]["baseline"]["comparison"],
        "unknown"
    );
    fs::remove_dir_all(canonical.join(".in_use"))?;

    Ok(())
}
#[path = "acceptance/uv_readonly.rs"]
mod uv_readonly;

fn write_authorized_npm_catalog(
    f: &Fixture,
    prefix: &std::path::Path,
    allow_remove: bool,
    with_reg: bool,
) -> TestResult {
    let prov = if with_reg {
        json!({
            "type": "npm",
            "package": "@scope/demo",
            "prefix": prefix,
            "authority": {
                "allow_remove": allow_remove,
                "registry": {
                    "url": "https://registry.npmjs.org",
                    "scope_url": "https://npm.pkg.example.test/org",
                    "allow_scripts": false
                }
            }
        })
    } else {
        json!({
            "type": "npm",
            "package": "@scope/demo",
            "prefix": prefix,
            "authority": { "allow_remove": allow_remove }
        })
    };
    write_json(
        &f.catalog,
        &json!({
            "schema_version": 1,
            "packages": [{
                "id": "npm:scoped-demo",
                "name": "scoped-demo",
                "kind": "cli",
                "official_url": "https://docs.npmjs.com/cli/v11",
                "provider": prov,
                "paths": [],
                "exact_version_required": false
            }]
        }),
    )
}
#[path = "cli/native_recipes.rs"]
mod native_recipes;

#[test]
fn recommend_reads_explicit_evidence_without_native_calls_or_state_writes() -> TestResult {
    let f = Fixture::new()?;
    let evidence = f.file("sources.json");
    write_json(
        &evidence,
        &json!({
            "schema_version":1,
            "sources":[{
                "url":"https://www.typescriptlang.org/download/",
                "subject":"TypeScript",
                "excerpt":"npm install typescript --save-dev\nnpm install -g typescript\nnpx tsc"
            }]
        }),
    )?;
    fs::write(&f.catalog, b"not a catalog")?;
    let (out, result) = f.run(&[
        "recommend",
        "typescript",
        "--evidence",
        evidence.to_str().ok_or("evidence path")?,
    ])?;
    assert_code(&out, 0);
    let result = &result["data"];
    assert_eq!(result["evidenceStatus"], "provided-by-caller");
    assert_eq!(result["canExecute"], false);
    let findings = result["analysis"]["findings"]
        .as_array()
        .ok_or("findings")?;
    for (channel, scope) in [("npm", "global"), ("npm", "project"), ("npx", "invocation")] {
        assert!(findings.iter().any(
            |row| row["candidate"]["channel"] == channel && row["candidate"]["scope"] == scope
        ));
    }
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn explicit_legacy_model_account_is_validated_before_a_preview() -> TestResult {
    let f = Fixture::new()?;
    let (out, invalid) = f.run(&[
        "recommend",
        "jq",
        "--dry-run",
        "--model",
        "--keychain-account",
        "invalid/fixture",
    ])?;
    assert_code(&out, 1);
    assert_eq!(invalid["error"]["code"], "recommend_model_account_invalid");
    let (out, valid) = f.run(&[
        "recommend",
        "jq",
        "--dry-run",
        "--model",
        "--keychain-account",
        "synthetic",
    ])?;
    assert_code(&out, 0);
    assert_eq!(valid["data"]["model"]["status"], "dry-run");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn recommend_previews_public_and_remote_evidence_without_a_model_or_network_request() -> TestResult
{
    let f = Fixture::new()?;
    let (out, brewed) = f.run(&["--dry-run", "recommend", "jq", "--model"])?;
    assert_code(&out, 0);
    assert_eq!(brewed["data"]["evidenceStatus"], "embedded-public-snapshot");
    assert_eq!(brewed["data"]["model"]["status"], "dry-run");
    assert_eq!(
        brewed["data"]["analysis"]["findings"][0]["candidate"]["name"],
        "jq"
    );
    let (out, git) = f.run(&[
        "--dry-run",
        "recommend",
        "Project",
        "--github",
        "example/project",
    ])?;
    assert_code(&out, 0);
    assert_eq!(git["data"]["evidenceStatus"], "dry-run");
    let (out, local) = f.run(&["recommend", "fixture:demo", "--model"])?;
    assert_code(&out, 0);
    assert_eq!(local["data"]["model"]["status"], "no-public-evidence");
    assert_eq!(local["data"]["known"][0]["id"], "fixture:demo");
    assert!(!f.file("state").exists() && !f.file("cache").exists() && !f.file("calls").exists());
    Ok(())
}

#[test]
fn manifest_export_and_plan_compare_baselines_without_writing_or_embedded_commands() -> TestResult {
    let f = Fixture::new()?;
    fs::write(f.file("version"), "1.0.0")?;
    let (out, _) = f.run(&["app", "adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    let receipts = fs::read(f.file("state/receipts.json"))?;
    let before = fs::read(f.file("state/active.json"))?;
    let (out, export) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let mut file = export["data"]["manifest"].clone();
    let row = &file["entries"][0];
    assert_eq!(row["id"], "fixture:demo");
    assert_eq!(row["version"], "1.0.0");
    assert!(row.get("provider").is_none() && row.get("command").is_none());
    assert!(
        !file
            .to_string()
            .contains(f.home.to_str().ok_or("fixture home")?)
    );
    let path = f.file("export.json");
    let link = path.to_str().ok_or("path")?;
    write_json(&path, &file)?;
    let (out, exact) = f.run(&["manifest", "plan", link])?;
    assert_code(&out, 0);
    assert_eq!(exact["data"]["entries"][0]["status"], "aligned");
    file["entries"][0]["version"] = json!("2.0.0");
    write_json(&path, &file)?;
    let (out, mismatch) = f.run(&["manifest", "plan", link])?;
    assert_code(&out, 0);
    assert_eq!(
        mismatch["data"]["entries"][0]["status"],
        "version-difference"
    );
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
    assert_eq!(fs::read(f.file("state/active.json"))?, before);
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn manifest_replay_uses_the_current_bound_npm_recipe_for_only_the_selected_id() -> TestResult {
    let f = Fixture::new()?;
    let chosen = f.home.join("npm-chosen");
    let independent = f.home.join("npm-independent");
    npm_manifest(&independent, "@scope/demo", "9.0.0")?;
    let sentinel = independent.join("lib/node_modules/@scope/demo/package.json");
    let preserved = fs::read(&sentinel)?;
    write_authorized_npm_catalog(&f, &chosen, false, true)?;
    f.script(
        "npm",
        r#"[ "$1" = install ] || exit 91
native_prefix="$4"
version=""
for argument; do
  case "$argument" in @scope/demo@*) version="${argument##*@}";; esac
done
[ -n "$version" ] || exit 92
/bin/mkdir -p "$native_prefix/lib/node_modules/@scope/demo"
printf '{"name":"@scope/demo","version":"%s"}' "$version" > "$native_prefix/lib/node_modules/@scope/demo/package.json"
printf '%s %s\n' "$native_prefix" "$version" >> "$MACOPS_TEST_HOME/replay-calls""#,
    )?;
    let id = "npm:scoped-demo";
    let file = f.file("manifest.json");
    let path = file.to_str().ok_or("manifest path")?;
    let (out, export) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let mut input = export["data"]["manifest"].clone();
    input["entries"][0]["version"] = json!("1.0.0");
    write_json(&file, &input)?;
    let (out, preview) = f.run(&["--dry-run", "manifest", "replay", path, id])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["plan"]["action"], "install");
    assert!(!f.file("state").exists());

    let (out, installed) = f.run(&["manifest", "replay", path, id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(installed["data"]["phase"], "succeeded");
    assert_eq!(installed["data"]["plan"]["target"], "1.0.0");
    let calls = f.home.join("replay-calls");
    let initial = fs::read(&calls)?;
    let (out, unchanged) = f.run(&["manifest", "replay", path, id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(unchanged["data"]["status"], "unchanged");
    assert_eq!(unchanged["data"]["persistentWrites"], false);
    assert_eq!(fs::read(&calls)?, initial);

    let mut pending = installed["data"].clone();
    pending["phase"] = json!("unknown");
    write_json(&f.file("state/active.json"), &pending)?;
    let (out, retried) = f.run(&["manifest", "replay", path, id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(retried["data"]["phase"], "succeeded");
    assert_eq!(retried["data"]["id"], installed["data"]["id"]);
    assert_eq!(fs::read(&calls)?, initial);

    let (out, export) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let mut input = export["data"]["manifest"].clone();
    input["entries"][0]["version"] = json!("2.0.0");
    write_json(&file, &input)?;
    let (out, upgraded) = f.run(&["manifest", "replay", path, id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(upgraded["data"]["plan"]["action"], "upgrade");
    assert_eq!(upgraded["data"]["plan"]["target"], "2.0.0");
    let calls = fs::read_to_string(&calls)?;
    assert_eq!(calls.lines().count(), 2);
    assert!(
        calls
            .lines()
            .all(|line| line.starts_with(&*chosen.to_string_lossy()))
    );
    assert_eq!(fs::read(&sentinel)?, preserved);
    Ok(())
}

#[test]
fn manifest_replay_refuses_pending_scope_and_withdrawn_recipe_even_for_a_known_version()
-> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("manifest-npm");
    npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
    write_authorized_npm_catalog(&f, &prefix, false, true)?;
    f.script(
        "npm",
        r#"printf 'called' > "$MACOPS_TEST_HOME/never-called"; exit 14"#,
    )?;
    let id = "npm:scoped-demo";
    let (out, adopted) = f.run(&["app", "adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let (_, export) = f.run(&["manifest", "export"])?;
    let mut intent = export["data"]["manifest"].clone();
    let record = f.file("manifest.json");
    let path = record.to_str().ok_or("path")?;
    let scope = intent["entries"][0]["scope_sha256"].clone();
    let args = ["manifest", "replay", path, id, "--yes"];
    for invalid in [json!(null), json!("a".repeat(64))] {
        intent["entries"][0]["scope_sha256"] = invalid;
        write_json(&record, &intent)?;
        let (out, data) = f.run(&args)?;
        assert_code(&out, 3);
        assert_eq!(data["error"]["code"], "manifest_scope_changed");
    }

    intent["entries"][0]["scope_sha256"] = scope;
    write_json(&record, &intent)?;
    let active = f.file("state/active.json");
    let exact = fs::read(&active)?;
    let mut pending = adopted["data"].clone();
    pending["phase"] = json!("unknown");
    write_json(&active, &pending)?;
    let (out, data) = f.run(&args)?;
    assert_code(&out, 3);
    assert_eq!(data["error"]["code"], "operation_recovery_required");
    pending["phase"] = json!("prepared");
    write_json(&active, &pending)?;
    let (out, data) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["status"], "unchanged");
    assert_eq!(data["data"]["persistentWrites"], true);
    fs::write(&active, &exact)?;

    intent["entries"][0]["version"] = json!("2.0.0");
    write_json(&record, &intent)?;
    let command = f.command();
    let mut tester = std::process::Command::new("python3");
    tester.args(["-B", "-c", include_str!("cli/manifest_confirm.py")]);
    tester
        .arg(&f.catalog)
        .arg(command.get_program())
        .args(command.get_args());
    tester.args(["manifest", "replay", path, id]);
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            tester.env(key, value);
        }
    }
    assert_code(&tester.output()?, 0);
    assert_eq!(fs::read(active)?, exact);
    let (out, data) = f.run(&args)?;
    assert_code(&out, 3);
    assert_eq!(data["error"]["code"], "manifest_recipe_required");
    assert!(!f.home.join("never-called").exists());
    Ok(())
}

#[test]
fn explicit_recommendation_evidence_keeps_a_strict_local_document_shape() -> TestResult {
    let f = Fixture::new()?;
    let evidence = f.file("recommend-evidence.json");
    write_json(
        &evidence,
        &json!({"schema_version":1,"sources":[[
            "https://example.org/docs","project","brew install project",
        ]]}),
    )?;
    let file = evidence.to_str().ok_or("evidence path")?;
    let (out, result) = f.run(&["recommend", "project", "--evidence", file, "--model"])?;
    assert_code(&out, 1);
    assert!(result.get("error").is_some());

    write_json(
        &evidence,
        &json!({"schema_version":1,"sources":[{
            "url":"https://example.org/docs","subject":"project","excerpt":"brew install project",
        }]}),
    )?;
    let (out, result) = f.run(&["recommend", "project", "--evidence", file, "--model"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["model"]["status"], "local-evidence-only");

    let (out, result) = f.run(&["recommend", "jq", "--model"])?;
    assert_code(&out, 0);
    assert_eq!(result["data"]["model"]["status"], "keychain-unavailable");
    assert_eq!(
        result["data"]["analysis"]["findings"][0]["candidate"]["name"],
        "jq"
    );
    assert!(!f.file("state").exists() && !f.file("cache").exists());
    Ok(())
}

#[test]
fn manifest_plan_requires_exact_npm_versions_and_reports_read_only_sources() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("scope");
    write_authorized_npm_catalog(&f, &prefix, false, true)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(json!({
            "id":"brew:example","kind":"cli","name":"example","official_url":"https://example.org",
            "provider":{"type":"brew","package":"example"}
        }));
    write_json(&f.catalog, &catalog)?;
    let (_, exported) = f.run(&["manifest", "export"])?;
    let mut manifest = exported["data"]["manifest"].clone();
    let entries = manifest["entries"].as_array_mut().ok_or("entries")?;
    let position = entries
        .iter()
        .position(|item| item["id"] == "npm:scoped-demo")
        .ok_or("npm row")?;
    let file = f.file("import.json");
    let path = file.to_str().ok_or("path")?;
    f.script(
        "npm",
        r#"printf 'called' > "$MACOPS_TEST_HOME/native-called"; exit 19"#,
    )?;
    for version in ["1", "1.2", "1.x", "1.2.3+build.01"] {
        manifest["entries"][position]["version"] = json!(version);
        write_json(&file, &manifest)?;
        let (out, data) = f.run(&["manifest", "replay", path, "npm:scoped-demo", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(data["error"]["code"], "manifest_version_required");
    }
    manifest["entries"][position]["version"] = json!("1.2.3-beta.2");
    write_json(&file, &manifest)?;
    let (out, data) = f.run(&["manifest", "plan", path])?;
    assert_code(&out, 0);
    let rows = data["data"]["entries"].as_array().ok_or("rows")?;
    assert!(
        rows.iter()
            .any(|row| row["id"] == "brew:example" && row["replay"] == "read-only")
    );
    assert!(
        rows.iter()
            .any(|row| row["id"] == "npm:scoped-demo" && row["replay"] == "verify-native-state")
    );
    assert!(!f.home.join("native-called").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn manifest_maximum_roster_can_be_exported_and_read_back() -> TestResult {
    let f = Fixture::new()?;
    let labels = "x".repeat(508);
    let packages = (0..4096)
        .map(|i| {
            json!({
                "id":format!("{i:04}{labels}"),"kind":"cli","name":"fixture",
                "official_url":"https://example.org","provider":{"type":"macops"}
            })
        })
        .collect::<Vec<_>>();
    write_json(&f.catalog, &json!({"schema_version":1,"packages":packages}))?;
    let (out, export) = f.run(&["manifest", "export"])?;
    assert!(out.status.success());
    let raw = serde_json::to_vec_pretty(&export["data"]["manifest"])?;
    assert!(raw.len() > 2 * 1024 * 1024);
    let file = f.file("roundtrip.json");
    fs::write(&file, raw)?;
    let (out, plan) = f.run(&["manifest", "plan", file.to_str().ok_or("path")?])?;
    assert!(out.status.success());
    let rows = plan["data"]["entries"].as_array().ok_or("rows")?;
    assert_eq!(rows.len(), 4096);
    assert!(
        rows.iter()
            .all(|item| item["status"] == "aligned" && item["replay"] == "read-only")
    );
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn manifest_export_includes_native_registered_skills_under_the_same_identity() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["fixture-manifest-skill"])?;
    let (out, skills) = f.run(&["skill", "--details"])?;
    assert_code(&out, 0);
    let record = skills["data"]["resources"]
        .as_array()
        .ok_or("skills")?
        .iter()
        .find(|record| record["native_id"] == "fixture-manifest-skill")
        .ok_or("native skill identifier")?;
    let id = record["id"].as_str().ok_or("identifier")?;
    let (out, export) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let manifest = &export["data"]["manifest"];
    assert!(
        manifest["entries"]
            .as_array()
            .ok_or("entries")?
            .iter()
            .any(|row| row["id"] == id && row["channel"] == "capability")
    );
    assert!(!manifest.to_string().contains("SKILL.md"));
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn manifest_preview_and_replay_both_reject_a_linked_active_record() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-prefix");
    npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
    write_authorized_npm_catalog(&f, &prefix, false, true)?;
    let id = "npm:scoped-demo";
    assert_code(&f.run(&["app", "adopt", id, "--yes"])?.0, 0);
    let (out, exported) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let doc = f.file("manifest.json");
    write_json(&doc, &exported["data"]["manifest"])?;
    let active = f.file("state/active.json");
    let saved = f.file("state/original.json");
    fs::rename(&active, &saved)?;
    symlink(&saved, &active)?;
    for dry in [false, true] {
        let mut args = Vec::new();
        if dry {
            args.push("--dry-run");
        }
        args.extend([
            "manifest",
            "replay",
            doc.to_str().ok_or("path")?,
            id,
            "--yes",
        ]);
        let (out, data) = f.run(&args)?;
        assert_code(&out, 1);
        assert!(data.get("error").is_some());
    }
    assert_eq!(fs::read_link(active)?, saved);
    Ok(())
}

#[test]
fn recommendation_matches_a_public_install_channel_to_a_registered_native_recipe() -> TestResult {
    let f = Fixture::new()?;
    write_authorized_npm_catalog(&f, &f.home.join("declared-scope"), true, true)?;
    let mut catalog = f.read_json("catalog.json")?;
    let npm = &mut catalog["packages"][0];
    npm["id"] = json!("native:typescript-scope");
    npm["name"] = json!("registered-instance");
    npm["provider"]["package"] = json!("typescript");
    npm["provider"]["authority"]["registry"]
        .as_object_mut()
        .ok_or("registry")?
        .remove("scope_url");
    let mut independent = npm.clone();
    independent["id"] = json!("native:registry-other");
    independent["name"] = json!("independent-registry-instance");
    independent["provider"]["prefix"] = json!(f.home.join("other-prefix"));
    independent["provider"]["authority"]["registry"]["url"] = json!("https://registry.example.org");
    catalog["packages"]
        .as_array_mut()
        .ok_or("packages")?
        .push(independent);
    write_json(&f.catalog, &catalog)?;
    let (out, found) = f.run(&["recommend", "typescript"])?;
    assert_code(&out, 0);
    assert_eq!(found["data"]["known"].as_array().ok_or("known")?.len(), 1);
    let result = &found["data"]["known"][0];
    assert_eq!(result["id"], "native:typescript-scope");
    assert_eq!(result["matchesPublicChannel"], true);
    assert_eq!(result["preview"]["entry"], "install");
    assert_eq!(result["preview"]["dryRun"], true);
    assert!(!f.file("state").exists());
    Ok(())
}

fn recommended_npm_fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-prefix");
    write_authorized_npm_catalog(&f, &prefix, true, true)?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["exact_version_required"] = json!(true);
    write_json(&f.catalog, &catalog)?;
    f.script(
        "npm",
        r#"case "$1" in
prefix) printf '%s' "$MACOPS_TEST_HOME/npm-prefix" ;;
install)
 spec=
 for arg; do spec=$arg; done
 [ "$4" = "$MACOPS_TEST_HOME/npm-prefix" ] && [ "$spec" = '@scope/demo@1.2.3' ]
 printf '%s\n' "$spec" >> "$MACOPS_TEST_HOME/installed"
 [ ! -e "$MACOPS_TEST_HOME/unverified-install" ] || exit 0
 /bin/mkdir -p "$4/lib/node_modules/@scope/demo"
 printf '{"name":"@scope/demo","version":"1.2.3"}' > "$4/lib/node_modules/@scope/demo/package.json" ;;
*) exit 8 ;;
esac"#,
    )?;
    Ok(f)
}

#[test]
fn recommend_confirms_one_frozen_npm_install_and_registers_it() -> TestResult {
    let f = recommended_npm_fixture()?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "scoped-demo"],
        "recommend-picker-cancel",
        &f.home,
        "100",
        "0",
    )?;
    assert!(!f.file("state").exists());
    assert!(!f.home.join("installed").exists());
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "scoped-demo"],
        "recommend-cancel",
        &f.home,
        "100",
        "3",
    )?;
    assert!(!f.file("state").exists());
    assert!(!f.home.join("installed").exists());
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "scoped-demo"],
        "recommend-install",
        &f.home,
        "100",
        "0",
    )?;
    assert_eq!(
        fs::read_to_string(f.home.join("installed"))?,
        "@scope/demo@1.2.3\n"
    );
    assert!(f.read_json("state/receipts.json")?["installations"]["npm:scoped-demo"].is_object());
    Ok(())
}

#[test]
fn recommendation_reports_how_to_recover_an_unverified_original_install() -> TestResult {
    let f = recommended_npm_fixture()?;
    fs::write(
        f.home.join("unverified-install"),
        b"verify result instead of retrying",
    )?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "scoped-demo"],
        "recommend-install-unknown",
        &f.home,
        "100",
        "3",
    )?;
    let active = f.read_json("state/active.json")?;
    assert_eq!(active["phase"], "unknown");
    assert_eq!(
        fs::read_to_string(f.home.join("installed"))?,
        "@scope/demo@1.2.3\n"
    );
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn manifest_plan_uses_the_selected_scope_even_when_the_old_receipt_matches() -> TestResult {
    let f = Fixture::new()?;
    let prefix = f.home.join("npm-scope");
    npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
    write_authorized_npm_catalog(&f, &prefix, false, true)?;
    f.script(
        "npm",
        r#"printf 'called' > "$MACOPS_TEST_HOME/native-called"; exit 19"#,
    )?;
    let invoke = |args: &[&str], changed: bool| -> Result<_, Box<dyn std::error::Error>> {
        let mut command = f.command();
        command.env_clear().env("HOME", &f.home).env(
            "PATH",
            format!("{}:/usr/bin:/bin", f.home.join("bin").display()),
        );
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        if changed {
            command.env("MISE_DATA_DIR", f.home.join("another-mise-data"));
        }
        let out = command.args(args).output()?;
        let data = serde_json::from_slice::<serde_json::Value>(&out.stdout)?;
        Ok((out, data))
    };
    let id = "npm:scoped-demo";
    assert_code(&invoke(&["app", "adopt", id, "--yes"], false)?.0, 0);
    let (_, exported) = invoke(&["manifest", "export"], false)?;
    let file = f.file("record.json");
    write_json(&file, &exported["data"]["manifest"])?;
    let path = file.to_str().ok_or("path")?;
    let (out, observed) = invoke(&["manifest", "plan", path], true)?;
    assert_code(&out, 0);
    assert_eq!(observed["data"]["entries"][0]["status"], "scope-changed");
    assert_eq!(observed["data"]["entries"][0]["replay"], "inconsistent");
    let (out, rejected) = invoke(&["--dry-run", "manifest", "replay", path, id], true)?;
    assert_code(&out, 3);
    assert_eq!(rejected["error"]["code"], "manifest_scope_changed");
    assert!(!f.home.join("native-called").exists());
    Ok(())
}

#[test]
fn manifest_preserves_missing_legacy_scope_without_treating_it_as_current_authority() -> TestResult
{
    let f = Fixture::new()?;
    let prefix = f.home.join("npm");
    npm_manifest(&prefix, "@scope/demo", "1.0.0")?;
    write_authorized_npm_catalog(&f, &prefix, false, true)?;
    let id = "npm:scoped-demo";
    assert_code(&f.run(&["app", "adopt", id, "--yes"])?.0, 0);
    let mut receipts = f.read_json("state/receipts.json")?;
    receipts["installations"][id]
        .as_object_mut()
        .ok_or("receipt")?
        .remove("scope");
    let path = f.file("state/receipts.json");
    write_json(&path, &receipts)?;
    let before = fs::read(&path)?;
    let (out, export) = f.run(&["manifest", "export"])?;
    assert_code(&out, 0);
    let file = export["data"]["manifest"].clone();
    assert!(file["entries"][0]["scope_sha256"].is_null());
    assert_eq!(file["entries"][0]["version"], "1.0.0");
    let doc = f.file("legacy.json");
    write_json(&doc, &file)?;
    let src = doc.to_str().ok_or("document")?;
    let (out, plan) = f.run(&["manifest", "plan", src])?;
    assert_code(&out, 0);
    assert_eq!(
        plan["data"]["entries"][0]["status"],
        "needs-native-observation"
    );
    assert_eq!(plan["data"]["entries"][0]["replay"], "needs-intent");
    let (out, replay) = f.run(&["manifest", "replay", src, id, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(replay["error"]["code"], "manifest_scope_changed");
    assert_eq!(fs::read(path)?, before);
    Ok(())
}

#[test]
fn operation_history_browses_newest_pending_record_without_mutating_state() -> TestResult {
    let f = Fixture::new()?;
    chezmoi_native::run_browser_pty(&f, &["history"], "history-empty", &f.home, "78", "0")?;
    assert!(!f.file("state").exists());
    let (out, result) = f.run(&["app", "install", "demo", "--version", "1.0.0", "--yes"])?;
    assert_code(&out, 0);
    let mut pending = result["data"].clone();
    assert_eq!(pending["phase"], "succeeded");
    pending["id"] = json!("OP-fixture-recent");
    pending["phase"] = json!("unknown");
    pending["started_at"] = json!(pending["started_at"].as_u64().ok_or("started")? + 1);
    pending["message"] = json!("SYNTHETIC_PRIVATE_COMMAND_OUTPUT");
    let path = f.file("state/operations/OP-fixture-recent.json");
    write_json(&path, &pending)?;
    let record_bytes = fs::read(&path)?;
    let active = fs::read(f.file("state/active.json"))?;
    let receipts = fs::read(f.file("state/receipts.json"))?;
    chezmoi_native::run_browser_pty(&f, &["history"], "history-direct", &f.home, "78", "0")?;
    chezmoi_native::run_browser_pty(&f, &["app"], "history-unified", &f.home, "96", "0")?;
    let (out, json) = f.run(&["history", "fixture:demo"])?;
    assert_code(&out, 0);
    let rows = json["data"].as_array().ok_or("journal response")?;
    assert_eq!(rows.len(), 2);
    assert!(rows.contains(&pending));
    assert_eq!(fs::read(path)?, record_bytes);
    assert_eq!(fs::read(f.file("state/active.json"))?, active);
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
    Ok(())
}

#[test]
fn operation_history_remains_readable_when_the_active_record_is_damaged() -> TestResult {
    let f = Fixture::new()?;
    let (out, result) = f.run(&["app", "install", "demo", "--version", "1.0.0", "--yes"])?;
    assert_code(&out, 0);
    let mut pending = result["data"].clone();
    pending["id"] = json!("OP-fixture-unreadable-active");
    pending["phase"] = json!("unknown");
    pending["started_at"] = json!(pending["started_at"].as_u64().ok_or("started")? + 1);
    write_json(
        &f.file("state/operations/OP-fixture-unreadable-active.json"),
        &pending,
    )?;
    let active = f.file("state/active.json");
    fs::write(&active, b"{")?;
    let receipts = fs::read(f.file("state/receipts.json"))?;
    let calls = fs::read(f.file("calls"))?;
    let (out, response) = f.run(&["history"])?;
    assert_code(&out, 0);
    assert!(
        response["data"]
            .as_array()
            .ok_or("history rows")?
            .contains(&pending)
    );
    let (out, response) = f.run(&["recover"])?;
    assert_code(&out, 1);
    assert_eq!(response["error"]["code"], "invalid_json");
    chezmoi_native::run_browser_pty(
        &f,
        &["history"],
        "history-active-unreadable",
        &active,
        "110",
        "3",
    )?;
    assert_eq!(fs::read(active)?, b"{");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
    assert_eq!(fs::read(f.file("calls"))?, calls);
    Ok(())
}

#[test]
fn codex_launch_json_and_unknown_profiles_never_dispatch() -> TestResult {
    let f = Fixture::new()?;
    let (result, value) = f.run(&["agent", "launch", "codex:default"])?;
    assert_code(&result, 1);
    assert_eq!(value["error"]["code"], "launch_json_requires_preview");
    let (result, value) = f.run(&["agent", "launch", "codex:default", "--dry-run"])?;
    assert_code(&result, 3);
    assert_eq!(value["error"]["code"], "codex_session_profile_unavailable");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn history_recovery_requires_confirmation_of_the_same_original_record() -> TestResult {
    let f = Fixture::new()?;
    let (out, installed) = f.run(&["app", "install", "demo", "--version", "1.0.0", "--yes"])?;
    assert_code(&out, 0);
    let mut pending = installed["data"].clone();
    pending["id"] = json!("OP-fixture-recovery");
    pending["phase"] = json!("prepared");
    pending["process_group"] = serde_json::Value::Null;
    pending["started_at"] = json!(pending["started_at"].as_u64().ok_or("started")? + 1);
    let active = f.file("state/active.json");
    let history = f.file("state/operations/OP-fixture-recovery.json");
    write_json(&active, &pending)?;
    write_json(&history, &pending)?;
    let frozen_record = fs::read(&history)?;
    let before = fs::read(&active)?;
    let calls = fs::read(f.file("calls"))?;
    let receipts = fs::read(f.file("state/receipts.json"))?;
    chezmoi_native::run_browser_pty(
        &f,
        &["history"],
        "history-recover-cancel",
        &active,
        "96",
        "0",
    )?;
    assert_eq!(fs::read(&active)?, before);
    assert_eq!(fs::read(&history)?, frozen_record);
    chezmoi_native::run_browser_pty(
        &f,
        &["history"],
        "history-recover-stale",
        &active,
        "96",
        "0",
    )?;
    assert_eq!(fs::read(&history)?, frozen_record);
    let changed = f.read_json("state/active.json")?;
    assert_eq!(changed["phase"], "prepared");
    assert_eq!(
        changed["message"],
        "operation progress changed during confirmation"
    );
    chezmoi_native::run_browser_pty(
        &f,
        &["history"],
        "history-recover-migration",
        &active,
        "96",
        "0",
    )?;
    assert_eq!(f.read_json("state/active.json")?, changed);
    assert_eq!(fs::read(&history)?, frozen_record);
    assert_eq!(fs::read(f.file("calls"))?, calls);
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
    let migration = f.file("state/migration-active.json");
    assert_eq!(fs::read_to_string(&migration)?, "interrupted migration");
    fs::remove_file(migration)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["history"],
        "history-recover-confirm",
        &active,
        "96",
        "0",
    )?;
    let checked = f.read_json("state/operations/OP-fixture-recovery.json")?;
    assert_eq!(checked["phase"], "failed");
    assert_eq!(checked["id"], "OP-fixture-recovery");
    assert_eq!(checked["plan_sha256"], pending["plan_sha256"]);
    assert_eq!(fs::read(f.file("calls"))?, calls);
    assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
    Ok(())
}

#[test]
fn skills_inventory_confirms_exact_maintenance_recipe_and_rejects_changed_book() -> TestResult {
    let f = Fixture::new()?;
    let name = "fixture-skill";
    native_skill_fixture(&f, &[name])?;
    let recipe = fixed_native_recipe(&f, name)?;
    let recipe = recipe.to_str().ok_or("recipe")?;
    let (out, value) = f.run(&[
        "skill",
        "adopt",
        "skill:fixture-skill",
        "--recipe",
        recipe,
        "--yes",
    ])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["journal"]["phase"], "succeeded");
    let before = fs::read(f.file("state/active.json"))?;
    let receipts = fs::read(f.file("state/receipts.json"))?;
    let book = f.home.join(".config/macops/native-skills-recipes.json");
    let book_before = fs::read(&book)?;
    for scenario in ["skill-maintenance-cancel", "skill-maintenance-stale"] {
        chezmoi_native::run_browser_pty(&f, &["skill"], scenario, &book, "104", "0")?;
        assert_eq!(fs::read(f.file("state/receipts.json"))?, receipts);
        assert_eq!(fs::read(f.file("state/active.json"))?, before);
        assert!(
            f.home
                .join(".agents/skills/fixture-skill/SKILL.md")
                .is_file()
        );
        assert!(!f.home.join("cap-poison").exists());
        if scenario == "skill-maintenance-cancel" {
            assert_eq!(fs::read(&book)?, book_before);
        } else {
            assert_ne!(fs::read(&book)?, book_before);
        }
    }
    Ok(())
}

#[test]
fn skills_inventory_removes_the_original_installation_with_one_native_call() -> TestResult {
    let f = Fixture::new()?;
    let name = "fixture-skill";
    native_skill_fixture(&f, &[name])?;
    let recipe = fixed_native_recipe(&f, name)?;
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(&recipe)?)?;
    let runner = raw["runner"]["node"].as_str().ok_or("runner")?;
    fs::write(
        runner,
        r#"#!/bin/sh
set -eu
if [ "$1" = --version ]; then printf 'v26.13.1\n'; exit 0; fi
if [ "$2" = --version ]; then printf '1.9.0\n'; exit 0; fi
[ "$#" = 8 ]
[ "$2 $3 $4 $5 $6 $7 $8" = 'remove --global --yes --agent codex --skill fixture-skill' ]
printf 'remove\n' >> "$HOME/removal-calls"
/bin/rm "$HOME/.agents/skills/fixture-skill/SKILL.md"
/bin/rmdir "$HOME/.agents/skills/fixture-skill"
printf '{"version":3,"skills":{}}\n' > "$HOME/.agents/.skill-lock.json"
"#,
    )?;
    let (out, adopted) = f.run(&[
        "skill",
        "adopt",
        "skill:fixture-skill",
        "--recipe",
        recipe.to_str().ok_or("recipe")?,
        "--yes",
    ])?;
    assert_code(&out, 0);
    assert_eq!(adopted["data"]["journal"]["phase"], "succeeded");
    chezmoi_native::run_browser_pty(
        &f,
        &["skill"],
        "skill-maintenance-removed",
        &f.home,
        "104",
        "0",
    )?;
    assert_eq!(
        fs::read_to_string(f.home.join("removal-calls"))?,
        "remove\n"
    );
    assert!(!f.home.join(".agents/skills/fixture-skill").exists());
    assert_eq!(
        f.read_json("state/receipts.json")?["installations"],
        json!({})
    );
    let active = f.read_json("state/active.json")?;
    assert_eq!(active["phase"], "succeeded");
    assert_eq!(active["plan"]["action"], "uninstall");
    Ok(())
}

#[test]
fn recommend_skill_chooses_fixed_registered_scope_and_refuses_changed_recipe() -> TestResult {
    for scenario in ["recommend-skill-cancel", "recommend-skill-stale"] {
        let f = Fixture::new()?;
        let name = "fixture-skill";
        native_skill_fixture(&f, &[name])?;
        let recipe_file = fixed_native_recipe(&f, name)?;
        let mut recipe: serde_json::Value = serde_json::from_slice(&fs::read(recipe_file)?)?;
        recipe["manualOnly"] = json!(true);
        recipe["runner"]["sha256"] = json!("c".repeat(64));
        fs::remove_dir_all(f.home.join(".agents/skills").join(name))?;
        write_json(
            &f.home.join(".agents/.skill-lock.json"),
            &json!({"version":3,"skills":{}}),
        )?;
        let book = f.home.join(".config/macops/native-skills-recipes.json");
        fs::create_dir_all(book.parent().ok_or("recipe parent")?)?;
        write_json(&book, &json!({"schemaVersion":1,"recipes":[recipe]}))?;
        chezmoi_native::run_browser_pty(&f, &["recommend", name], scenario, &book, "102", "3")?;
        assert!(!f.home.join("cap-poison").exists());
        assert!(!f.file("state/active.json").exists());
        assert!(!f.file("state/receipts.json").exists());
        assert!(!f.home.join(".agents/skills").join(name).exists());
        if scenario == "recommend-skill-cancel" {
            assert!(!f.file("state").exists());
        }
    }
    Ok(())
}

#[test]
fn empty_claude_resume_preview_does_not_create_state() -> TestResult {
    let f = Fixture::new()?;
    let (result, value) = f.run(&["agent", "resume", "OP-fixture", "--dry-run"])?;
    assert_ne!(result.status.code(), Some(0));
    assert!(value["data"].is_null());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn generic_mise_distinguishes_checked_alternate_layers_from_unselected_override() -> TestResult {
    for (dir, name, owned) in [
        ("mise", "private_config.local.toml", true),
        ("mise", "private_config.fixture.toml", true),
        ("mise", "private_Config.TOML", true),
        ("miſe", "private_config.toml", true),
        ("mise", "private_config.override.toml", false),
    ] {
        let f = Fixture::new()?;
        let mut catalog = f.read_json("catalog.json")?;
        catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
        write_json(&f.catalog, &catalog)?;
        let root = f.home.join("alternate-chezmoi");
        let declared = root.join("managed/dot_config").join(dir).join(name);
        fs::create_dir_all(declared.parent().ok_or("managed path")?)?;
        fs::write(root.join(".chezmoiroot"), b"managed\n")?;
        fs::write(&declared, b"[tools]\ndemo='1.0.0'\n")?;
        let cfg = f.home.join(".config/chezmoi/chezmoi.toml");
        fs::create_dir_all(cfg.parent().ok_or("configuration root")?)?;
        fs::write(
            &cfg,
            format!("sourceDir = {:?}\n", root.display().to_string()),
        )?;
        let version = f.home.join("native-version");
        fs::write(&version, b"1.0.0")?;
        f.script(
            "chezmoi",
            r#"printf unexpected > "$MACOPS_TEST_HOME/owner-query"; exit 97"#,
        )?;
        f.script(
            "mise",
            r#"case "$1" in
ls) printf '[{"version":"%s","active":true}]' "$(/bin/cat "$HOME/native-version")";;
use) [ "$2 $3 $4" = '--global --pin demo@2.0.0' ]; printf 2.0.0 > "$HOME/native-version";;
*) exit 98;;
esac"#,
        )?;
        assert_code(&f.run(&["app", "adopt", "demo", "--yes"])?.0, 0);
        let (out, result) = f.run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
        if owned {
            assert_code(&out, 3);
            assert_eq!(
                result["error"]["code"],
                "configuration_owner_requires_review"
            );
            assert_eq!(fs::read(&version)?, b"1.0.0");
        } else {
            assert_code(&out, 0);
            assert_eq!(result["data"]["phase"], "succeeded");
            assert_eq!(fs::read(&version)?, b"2.0.0");
        }
        assert!(!f.home.join("owner-query").exists());
        assert_eq!(fs::read(declared)?, b"[tools]\ndemo='1.0.0'\n");
    }
    Ok(())
}

#[test]
fn recommended_registered_mise_tool_is_confirmed_and_uses_native_global_activation() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    f.script(
        "mise",
        r#"case "$1" in
ls) if [ -f "$HOME/selected-version" ]; then
    printf '[{"version":"%s","active":true}]' "$(/bin/cat "$HOME/selected-version")";
    else printf '[]'; fi;;
use) [ "$2 $3 $4" = '--global --pin demo@1.2.3' ]
     printf 'use\n' >> "$HOME/uses"
     /bin/mkdir -p "$MISE_DATA_DIR" "$MISE_CONFIG_DIR"
     printf 1.2.3 > "$HOME/selected-version";;
*) exit 79;;
esac"#,
    )?;
    chezmoi_native::run_browser_pty(
        &f,
        &["recommend", "demo"],
        "recommend-mise",
        &f.home,
        "104",
        "0",
    )?;
    assert_eq!(fs::read(f.home.join("selected-version"))?, b"1.2.3");
    let receipt = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipt["installations"]["fixture:demo"]["observation"]["version"],
        "1.2.3"
    );
    let namespace = &receipt["installations"]["fixture:demo"]["query"]["namespace"];
    assert!(namespace["data_identity"].is_object(), "{namespace}");
    assert!(namespace["config_identity"].is_object(), "{namespace}");
    let active = f.file("state/active.json");
    let mut pending = f.read_json("state/active.json")?;
    pending["phase"] = json!("unknown");
    for (scenario, code) in [
        ("recommend-cancel-version", "0"),
        ("recommend-retry-version", "0"),
        ("recommend-other-version", "3"),
    ] {
        write_json(&active, &pending)?;
        let before = fs::read(&active)?;
        chezmoi_native::run_browser_pty(
            &f,
            &["recommend", "demo"],
            scenario,
            &active,
            "104",
            code,
        )?;
        if scenario == "recommend-cancel-version" {
            assert_eq!(fs::read(&active)?, before);
        } else {
            let settled = f.read_json("state/active.json")?;
            assert_eq!(settled["phase"], "succeeded");
            assert_eq!(settled["id"], pending["id"]);
        }
        assert_eq!(fs::read_to_string(f.home.join("uses"))?, "use\n");
    }
    Ok(())
}

#[test]
fn direct_apps_adopts_one_registered_mise_row_and_confirms_the_exact_upgrade() -> TestResult {
    use sha2::Digest as _;

    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    let version = f.home.join("active-version");
    fs::write(&version, b"1.2.3")?;
    f.script(
        "mise",
        r#"case "$1" in
ls) current=$(/bin/cat "$HOME/active-version")
    [ "$2 $3" = '--installed --json' ] || exit 84
    if [ "$#" = 3 ]; then printf '{"demo":[{"version":"%s","active":true}]}' "$current"
    else [ "$4" = demo ]; printf '[{"version":"%s","active":true}]' "$current"; fi;;
latest) [ "$2" = demo ]; printf '2.0.0\n';;
use) [ "$2 $3 $4" = '--global --pin demo@2.0.0' ] || exit 93
     printf 2.0.0 > "$HOME/active-version";;
*) exit 94;;
esac"#,
    )?;
    let (out, data) = f.run(&["app", "resources"])?;
    assert_code(&out, 0);
    let resources = data["data"]["resources"]
        .as_array()
        .ok_or("software resources")?;
    let observed = resources
        .iter()
        .filter(|row| row["native_id"] == "demo")
        .collect::<Vec<_>>();
    assert_eq!(observed.len(), 1, "{data}");
    assert_eq!(observed[0]["recipe_ref"], "fixture:demo");
    assert_eq!(observed[0]["releases"][0]["version"], "1.2.3");
    let lookup = serde_json::to_vec(&("mise", "demo", f.home.display().to_string()))?;
    let earlier_id = format!("application:{:x}", sha2::Sha256::digest(lookup));
    assert!(
        observed[0]["aliases"]
            .as_array()
            .ok_or("software aliases")?
            .contains(&json!(earlier_id))
    );
    for (scenario, expected) in [
        ("mise-adopt", b"1.2.3"),
        ("mise-upgrade-cancel", b"1.2.3"),
        ("mise-upgrade-conflict", b"1.2.3"),
        ("mise-upgrade", b"2.0.0"),
        ("mise-retire", b"2.0.0"),
    ] {
        chezmoi_native::run_browser_pty(
            &f,
            &["app"],
            scenario,
            std::path::Path::new("demo"),
            "110",
            if scenario == "mise-upgrade-conflict" {
                "3"
            } else {
                "0"
            },
        )?;
        if scenario == "mise-upgrade-conflict" {
            write_json(&f.catalog, &catalog)?;
        }
        assert_eq!(fs::read(&version)?, expected);
        assert_eq!(
            f.read_json("state/receipts.json")?["installations"]["fixture:demo"]["observation"]["version"],
            std::str::from_utf8(expected)?
        );
    }
    Ok(())
}

#[test]
fn registered_mise_receipt_binds_the_actual_data_directory_across_commands() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    let a = f.home.join("scope-a");
    let b = f.home.join("scope-b");
    let alias = f.home.join("mise-data");
    for path in [&a, &b] {
        fs::create_dir(path)?;
        fs::write(path.join("version"), b"1.2.3")?;
    }
    symlink(&a, &alias)?;
    f.script(
        "mise",
        r#"case "$1" in
ls) printf '[{"version":"%s","active":true}]' "$(/bin/cat "$MISE_DATA_DIR/version")";;
use) [ "$2 $3 $4" = '--global --pin demo@2.0.0' ] || exit 93
     printf 2.0.0 > "$MISE_DATA_DIR/version";;
*) exit 94;;
esac"#,
    )?;
    let run = |arguments: &[&str]| {
        f.command()
            .env("HOME", &f.home)
            .env("MISE_DATA_DIR", &alias)
            .env_remove("XDG_DATA_DIRS")
            .env_remove("XDG_CONFIG_DIRS")
            .args(arguments)
            .output()
    };
    assert_code(&run(&["app", "adopt", "demo", "--yes"])?, 0);
    let adopted = fs::read(f.file("state/receipts.json"))?;
    fs::remove_file(&alias)?;
    symlink(&b, &alias)?;
    let out = run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 3);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["error"]["code"], "manager_identity_changed");
    assert_eq!(fs::read(a.join("version"))?, b"1.2.3");
    assert_eq!(fs::read(b.join("version"))?, b"1.2.3");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, adopted);
    fs::remove_file(&alias)?;
    symlink(&a, &alias)?;
    let original = f.home.join("original-data");
    fs::rename(&a, &original)?;
    fs::rename(&b, &a)?;
    let replaced = run(&["app", "upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&replaced, 3);
    assert_eq!(fs::read(a.join("version"))?, b"1.2.3");
    assert_eq!(fs::read(original.join("version"))?, b"1.2.3");
    assert_eq!(fs::read(f.file("state/receipts.json"))?, adopted);
    fs::rename(&a, &b)?;
    fs::rename(&original, &a)?;
    assert_code(&run(&["app", "upgrade", "demo", "--version", "2.0.0"])?, 0);
    assert_eq!(fs::read(a.join("version"))?, b"2.0.0");
    assert_eq!(fs::read(b.join("version"))?, b"1.2.3");
    Ok(())
}

#[test]
fn mise_first_install_recovery_keeps_the_first_observed_directory_identity() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    let replacement = f.home.join("replacement-data");
    fs::create_dir(&replacement)?;
    fs::write(replacement.join("version"), b"1.2.3")?;
    fs::write(f.home.join("swap-on-verification"), b"")?;
    f.script(
        "mise",
        r#"case "$1" in
ls) if [ -f "$MISE_DATA_DIR/version" ]; then
       version=$(/bin/cat "$MISE_DATA_DIR/version")
       if [ -f "$HOME/swap-on-verification" ]; then
           /bin/mv "$MISE_DATA_DIR" "$HOME/original-data"
           /bin/mv "$HOME/replacement-data" "$MISE_DATA_DIR"
           /bin/rm "$HOME/swap-on-verification"
       fi
       printf '[{"version":"%s","active":true}]' "$version"
    else printf '[]'; fi;;
use) [ "$2 $3 $4" = '--global --pin demo@1.2.3' ] || exit 93
     /bin/mkdir -p "$MISE_DATA_DIR"
     printf 1.2.3 > "$MISE_DATA_DIR/version"
     printf writer >> "$HOME/native-writers";;
*) exit 94;;
esac"#,
    )?;
    let (out, initial) = f.run(&["app", "install", "demo", "--version", "1.2.3"])?;
    assert_code(&out, 3);
    assert_eq!(initial["data"]["phase"], "unknown");
    let id = initial["data"]["id"]
        .as_str()
        .ok_or("original operation identity")?;
    let (out, foreign) = f.run(&["recover", id, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(foreign["data"]["phase"], "unknown");
    assert_eq!(foreign["data"]["id"], id);
    assert!(!f.file("state/receipts.json").exists());
    let root = f.home.join(".local/share/mise");
    fs::rename(&root, &replacement)?;
    fs::rename(f.home.join("original-data"), &root)?;
    let completion = f.file(&format!("state/{id}-mise-completion.json"));
    let bytes = fs::read(&completion)?;
    fs::remove_file(&completion)?;
    let (out, missing) = f.run(&["recover", id, "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(missing["data"]["phase"], "unknown");
    assert!(!f.file("state/receipts.json").exists());
    fs::write(&completion, bytes)?;
    let (out, repaired) = f.run(&["recover", id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(repaired["data"]["phase"], "succeeded");
    assert_eq!(repaired["data"]["id"], id);
    assert_eq!(fs::read(f.home.join("native-writers"))?, b"writer");
    assert_eq!(fs::read(replacement.join("version"))?, b"1.2.3");
    Ok(())
}

fn inactive_mise_fixture() -> Result<(Fixture, PathBuf), Box<dyn std::error::Error>> {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"] = json!({"type":"mise","package":"demo"});
    write_json(&f.catalog, &catalog)?;
    let payload = f.home.join(".local/share/mise/installs/demo/1.2.3");
    fs::create_dir_all(&payload)?;
    fs::write(payload.join("tool"), b"fixture payload")?;
    f.script(
        "mise",
        r#"scope="${MISE_INSTALLS_DIR:-$HOME/.local/share/mise/installs}"
payload="$scope/demo/1.2.3"
case "$1" in
ls)
    if [ -f "$HOME/fixture-response" ]; then
        /bin/cat "$HOME/fixture-response"
    elif [ -f "$payload/tool" ]; then
        row=$(printf '{"version":"1.2.3","active":false,"install_path":"%s"}' "$payload")
        if [ "$#" = 3 ]; then printf '{"demo":[%s]}' "$row"
        else printf '[%s]' "$row"; fi
    else
        if [ "$#" = 3 ]; then printf '{}'; else printf '[]'; fi
    fi;;
where) [ "$#" = 2 ] && [ "$2" = 'demo@1.2.3' ]; printf '%s\n' "$payload";;
uninstall) [ "$#" = 2 ] && [ "$2" = 'demo@1.2.3' ]
    /bin/rm "$payload/tool"
    /bin/rmdir "$payload"
    printf writer >> "$HOME/retirement-writers";;
*) exit 94;;
esac"#,
    )?;
    assert_code(&f.run(&["app", "adopt", "demo", "--yes"])?.0, 0);
    Ok((f, payload))
}

#[test]
fn mise_retirement_rejects_unproved_release_evidence_without_a_native_writer() -> TestResult {
    let (f, payload) = inactive_mise_fixture()?;
    let proof = f.home.join("fixture-response");
    let receipt = fs::read(f.file("state/receipts.json"))?;
    let version = |active: bool, path: &std::path::Path| json!({"version":"1.2.3","active":active,"install_path":path});
    for rows in [
        json!([version(true, &payload)]),
        json!([{"version":"1.2.3","install_path":&payload}]),
        json!([version(false, &payload), version(false, &payload)]),
        json!([version(false, &f.home.join("outside/demo/1.2.3"))]),
        json!({"failed":"unexpected native format"}),
    ] {
        write_json(&proof, &rows)?;
        let (out, data) = f.run(&["app", "uninstall", "demo", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(data["error"]["code"], "consumer_review_required");
        assert!(payload.join("tool").exists());
        assert_eq!(fs::read(f.file("state/receipts.json"))?, receipt);
        assert!(!f.home.join("retirement-writers").exists());
    }
    let other_backend = f.home.join(".local/share/mise/installs/other-demo/1.2.3");
    fs::create_dir_all(&other_backend)?;
    write_json(&proof, &json!([version(false, &other_backend)]))?;
    let (out, mismatch) = f.run(&["app", "uninstall", "demo", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(mismatch["error"]["code"], "mise_release_changed");
    assert!(other_backend.exists() && payload.exists());
    assert!(!f.home.join("retirement-writers").exists());
    Ok(())
}

#[test]
fn mise_retirement_recovery_requires_the_original_absence_proof_and_preserves_new_receipts()
-> TestResult {
    let (f, payload) = inactive_mise_fixture()?;
    let known = f.read_json("state/receipts.json")?;
    let (out, removed) = f.run(&["app", "uninstall", "demo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(removed["data"]["phase"], "succeeded");
    assert!(!payload.exists());
    let id = removed["data"]["id"]
        .as_str()
        .ok_or("retirement operation")?;
    let proof = f.file(&format!("state/{id}-mise-release-absence.json"));
    let bytes = fs::read(&proof)?;
    let receipt_path = f.file("state/receipts.json");
    let committed = fs::read(&receipt_path)?;
    let mut interrupted = removed["data"].clone();
    interrupted["phase"] = json!("unknown");
    write_json(&f.file("state/active.json"), &interrupted)?;
    fs::remove_file(&proof)?;
    assert_code(&f.run(&["recover", id])?.0, 3);
    assert_eq!(fs::read(&receipt_path)?, committed);

    fs::write(&proof, bytes)?;
    let mut rebound = known;
    rebound["installations"]["fixture:demo"]["operation_id"] = json!("OP-new-owner");
    write_json(&receipt_path, &rebound)?;
    let foreign = fs::read(&receipt_path)?;
    assert_code(&f.run(&["recover", id])?.0, 3);
    assert_eq!(fs::read(&receipt_path)?, foreign);
    fs::write(&receipt_path, &committed)?;
    let (result, verified) = f.run(&["recover", id])?;
    assert_code(&result, 0);
    assert_eq!(verified["data"]["phase"], "succeeded");
    assert_code(&f.run(&["recover", id])?.0, 0);
    assert_eq!(fs::read(receipt_path)?, committed);
    assert_eq!(fs::read(f.home.join("retirement-writers"))?, b"writer");
    Ok(())
}

#[test]
fn direct_apps_confirms_inactive_mise_retirement_and_refuses_replaced_payload() -> TestResult {
    for scenario in [
        "mise-retirement-cancel",
        "mise-retirement-stale",
        "mise-retirement-confirm",
    ] {
        let (f, payload) = inactive_mise_fixture()?;
        let original = fs::read(f.file("state/receipts.json"))?;
        let config = f.home.join(".config/mise/config.toml");
        fs::create_dir_all(config.parent().ok_or("mise configuration root")?)?;
        fs::write(&config, b"[env]\nFIXTURE = 'retained'\n")?;
        assert_code(&f.run(&["app", "adopt", "demo", "--yes"])?.0, 0);
        assert_ne!(fs::read(f.file("state/receipts.json"))?, original);
        let bound = fs::read(f.file("state/receipts.json"))?;
        chezmoi_native::run_browser_pty(
            &f,
            &["app"],
            scenario,
            std::path::Path::new("demo"),
            "150",
            "0",
        )?;
        let completed = scenario == "mise-retirement-confirm";
        assert_eq!(payload.exists(), !completed);
        assert_eq!(f.home.join("retirement-writers").exists(), completed);
        assert_eq!(fs::read(config)?, b"[env]\nFIXTURE = 'retained'\n");
        if completed {
            assert!(f.read_json("state/receipts.json")?["installations"]["fixture:demo"].is_null());
        } else {
            assert_eq!(fs::read(f.file("state/receipts.json"))?, bound);
        }
    }
    Ok(())
}

#[test]
fn mise_installs_override_does_not_invalidate_an_existing_other_provider_receipt() -> TestResult {
    let f = Fixture::new()?;
    let scope = f.home.join("alternate-mise-installs");
    fs::create_dir_all(&scope)?;
    let command = || {
        let mut command = f.command();
        command
            .env("HOME", &f.home)
            .env_remove("MISE_INSTALLS_DIR")
            .env_remove("XDG_DATA_DIRS")
            .env_remove("XDG_CONFIG_DIRS");
        command
    };
    let before = command()
        .args(["app", "install", "demo", "--version", "1.2.3"])
        .output()?;
    assert_code(&before, 0);
    let report = command()
        .env("MISE_INSTALLS_DIR", scope)
        .args(["app", "upgrade", "demo", "--version", "2.0.0"])
        .output()?;
    assert_code(&report, 0);
    assert_eq!(fs::read(f.file("version"))?, b"2.0.0");
    Ok(())
}

#[test]
fn cursor_project_plugin_observation_preserves_declared_scope_and_native_authority() -> TestResult {
    let f = Fixture::new()?;
    let project = f.home.join("workspace");
    let cursor = project.join(".cursor");
    fs::create_dir_all(&cursor)?;
    let settings = cursor.join("settings.json");
    let commit = "a".repeat(40);
    let inaccessible = format!("https://{}@github.com/example/private", "fixture-private");
    write_json(
        &settings,
        &json!({"plugins":{
            "public-plugin":{"enabled":true},
            "team-market/member":{"enabled":false,"gitUrl":"https://github.com/example/tools.git","gitRef":&commit},
            "private-plugin":{"gitUrl":inaccessible,"gitRef":"fixture-private-ref"}
        }}),
    )?;
    let project = project.to_str().ok_or("fixture project")?;
    let commands = ["skill", "--project", project];
    let (output, snapshot) = f.run(&commands)?;
    assert_code(&output, 0);
    let items = snapshot["data"]["resources"]
        .as_array()
        .ok_or("capability resources")?;
    let get = |id: &str| items.iter().find(|item| item["native_id"] == id);
    let selected = get("team-market/member").ok_or("configured Cursor plugin")?;
    assert_eq!(selected["management"], "declared");
    assert_eq!(selected["status"], "declared");
    assert_eq!(selected["origin"]["record"], json!(settings));
    assert_eq!(
        selected["origin"]["label"],
        "https://github.com/example/tools"
    );
    assert_eq!(selected["revision"]["declared"], commit);
    assert_eq!(selected["configured_enabled"], false);
    assert!(selected["loaded"].is_null());
    assert_eq!(selected["contents"], json!([]));
    assert_eq!(selected["deployments"], json!([]));
    assert_eq!(
        get("public-plugin").ok_or("public entry")?["configured_enabled"],
        true
    );
    let private = get("private-plugin").ok_or("private source entry")?;
    assert!(private["origin"]["label"].is_null());
    assert!(private["revision"]["declared"].is_null());
    assert!(private["configured_enabled"].is_null());
    assert!(!String::from_utf8(output.stdout)?.contains("fixture-private"));
    let id = selected["id"].as_str().ok_or("resource identity")?;
    let (output, detail) = f.run(&["skill", "--project", project, id])?;
    assert_code(&output, 0);
    assert_eq!(detail["data"]["resource"]["id"], id);
    let (output, _) = f.run(&["app", "adopt", id, "--yes"])?;
    assert_code(&output, 3);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn cursor_project_plugin_coverage_reports_malformed_and_unbound_settings() -> TestResult {
    let f = Fixture::new()?;
    let project = f.home.join("workspace");
    let cursor = project.join(".cursor");
    fs::create_dir_all(&cursor)?;
    let settings = cursor.join("settings.json");
    let project = project.to_str().ok_or("fixture project")?;
    let commands = ["skill", "--project", project];
    let status = || -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let (_, snapshot) = f.run(&commands)?;
        let observed = snapshot["data"]["coverage"]
            .as_array()
            .ok_or("coverage")?
            .iter()
            .find(|row| row["source"] == "cursor-project-plugin-settings")
            .ok_or("Cursor coverage")?;
        Ok(observed.clone())
    };
    assert_eq!(status()?["state"], "absent");
    write_json(&settings, &json!({}))?;
    assert_eq!(status()?["state"], "complete");
    write_json(&settings, &json!({"plugins":22}))?;
    assert_eq!(status()?["issues"][0], "cursor_project_plugins_invalid");
    write_json(
        &settings,
        &json!({"plugins":{"bad/key/more":{},"good":{"enabled":1}}}),
    )?;
    let invalid = status()?;
    assert_eq!(invalid["state"], "invalid");
    assert_eq!(invalid["count"], 1);
    assert_eq!(
        invalid["issues"],
        json!([
            "cursor_project_plugin_enabled_invalid",
            "cursor_project_plugin_entry_invalid"
        ])
    );
    let excessive = (0..1025)
        .map(|n| (format!("plugin-{n}"), json!({})))
        .collect::<serde_json::Map<_, _>>();
    write_json(&settings, &json!({"plugins":excessive}))?;
    assert_eq!(status()?["state"], "truncated");
    fs::remove_file(&settings)?;
    std::os::unix::fs::symlink(&f.catalog, &settings)?;
    assert_eq!(status()?["issues"][0], "cursor_project_settings_unreadable");
    Ok(())
}

#[test]
fn app_details_do_not_offer_mutation_for_a_discovered_cargo_installation() -> TestResult {
    let f = Fixture::new()?;
    f.script(
        "cargo",
        r#"case "$*" in
"install --list "*) printf 'detail-fixture v1.0.0:\n    detail-fixture\n' ;;
*) printf attempted > "$MACOPS_TEST_HOME/unsafe-mutation"; exit 8 ;;
esac"#,
    )?;
    chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-app-readonly",
        std::path::Path::new("detail-fixture"),
        "160",
        "0",
    )?;
    assert!(!f.home.join("unsafe-mutation").exists());
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn app_details_execute_the_same_declared_upgrade_after_confirmation() -> TestResult {
    let f = Fixture::new()?;
    fs::write(f.file("version"), "1.0.0")?;
    let (out, _) = f.run(&["app", "adopt", "--id", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-declared-upgrade",
        std::path::Path::new("demo"),
        "160",
        "0",
    )?;
    assert_eq!(fs::read_to_string(f.file("version"))?, "2.0.0");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "upgrade\n");
    Ok(())
}

#[test]
fn app_details_cancel_then_upgrade_to_the_explicit_version() -> TestResult {
    let f = Fixture::new()?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["provider"]["available"] = json!(null);
    write_json(&f.catalog, &catalog)?;
    fs::write(f.file("version"), "1.0.0")?;
    assert_code(
        &f.run(&["app", "adopt", "--id", "fixture:demo", "--yes"])?.0,
        0,
    );
    chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-version-upgrade",
        std::path::Path::new("demo"),
        "160",
        "0",
    )?;
    assert_eq!(fs::read_to_string(f.file("version"))?, "2.4.0");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "upgrade\n");
    Ok(())
}

#[test]
fn app_details_install_a_declared_package_through_its_provider() -> TestResult {
    let f = Fixture::new()?;
    chezmoi_native::run_browser_pty(
        &f,
        &["app"],
        "detail-declared-install",
        std::path::Path::new("demo"),
        "160",
        "0",
    )?;
    assert_eq!(fs::read_to_string(f.file("version"))?, "2.0.0");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    Ok(())
}

#[test]
fn skill_details_uninstall_only_the_selected_discovered_member() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha", "beta"])?;
    public_skill_runner(&f)?;
    let other = f.home.join(".agents/skills/beta/SKILL.md");
    let before = fs::read(&other)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["skill"],
        "detail-discovered-skill-uninstall",
        std::path::Path::new("alpha"),
        "160",
        "0",
    )?;
    assert!(!f.home.join(".agents/skills/alpha/SKILL.md").exists());
    assert_eq!(fs::read(other)?, before);
    let calls = fs::read_to_string(f.home.join("native-calls.jsonl"))?;
    assert_eq!(calls.lines().count(), 1);
    assert!(
        calls.contains("\"remove\"") && calls.contains("\"alpha\"") && !calls.contains("\"beta\"")
    );
    Ok(())
}

#[test]
fn skill_details_can_register_an_existing_shared_member_without_reinstalling() -> TestResult {
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha", "beta"])?;
    public_skill_runner(&f)?;
    let lock = f.home.join(".agents/.skill-lock.json");
    let payload = f.home.join(".agents/skills/beta/SKILL.md");
    let original_lock = fs::read(&lock)?;
    let original_payload = fs::read(&payload)?;
    chezmoi_native::run_browser_pty(
        &f,
        &["skill"],
        "detail-discovered-skill-adopt",
        std::path::Path::new("beta"),
        "160",
        "0",
    )?;
    assert_eq!(fs::read(lock)?, original_lock);
    assert_eq!(fs::read(payload)?, original_payload);
    assert!(!f.home.join("native-calls.jsonl").exists());
    let receipts = f.read_json("state/receipts.json")?;
    let managed = receipts["installations"].as_object().ok_or("receipts")?;
    assert_eq!(managed.len(), 1);
    assert_eq!(
        managed.values().next().ok_or("receipt")?["package"]["name"],
        "beta"
    );
    Ok(())
}

#[test]
fn agent_details_forget_only_the_selected_registration() -> TestResult {
    let f = Fixture::new()?;
    let first = f.home.join("config-one");
    let second = f.home.join("config-two");
    for (name, config) in [("first-wrapper", &first), ("second-wrapper", &second)] {
        fs::create_dir(config)?;
        fs::write(config.join("settings.json"), b"{}\n")?;
        f.script(name, "exit 0")?;
        let (registered, _) = f.run(&[
            "agent",
            "add",
            name,
            "--config",
            config.to_str().ok_or("config")?,
            "--type",
            "claude",
            "--yes",
        ])?;
        assert_code(&registered, 0);
    }
    let before = f.read_json("state/agent-profiles.json")?;
    let target = std::path::Path::new("first-wrapper");
    chezmoi_native::run_browser_pty(
        &f,
        &["agent"],
        "detail-agent-forget-cancel",
        target,
        "160",
        "0",
    )?;
    assert_eq!(f.read_json("state/agent-profiles.json")?, before);
    chezmoi_native::run_browser_pty(
        &f,
        &["agent"],
        "detail-agent-forget-stale",
        target,
        "160",
        "0",
    )?;
    assert_eq!(f.read_json("state/agent-profiles.json")?, before);
    fs::remove_dir(&first)?;
    fs::rename(f.home.join("config-one-before"), &first)?;
    chezmoi_native::run_browser_pty(&f, &["agent"], "detail-agent-forget", target, "160", "0")?;
    let after = f.read_json("state/agent-profiles.json")?;
    assert!(after.get(first.to_str().ok_or("first")?).is_none());
    assert_eq!(
        after.get(second.to_str().ok_or("second")?),
        before.get(second.to_str().ok_or("second")?)
    );
    assert_eq!(fs::read(first.join("settings.json"))?, b"{}\n");
    assert!(f.home.join("bin/first-wrapper").is_file());
    Ok(())
}
