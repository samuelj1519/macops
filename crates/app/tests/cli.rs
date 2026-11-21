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
use std::{fs, os::unix::fs::symlink};

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
fn bare_skills_json_reuses_only_skill_rows_and_identifies_agent_deployments() -> TestResult {
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
    let (_, legacy) = f.run(&["skills", "list", "--details"])?;
    assert!(
        legacy["data"]["resources"]
            .as_array()
            .ok_or("legacy resources")?
            .iter()
            .any(|row| row["delivery_kind"] == "mcp")
    );
    let (original, value) = f.run(&["skills"])?;
    assert_code(&original, 0);
    let rows = value["data"]["items"].as_array().ok_or("items")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "fixture-skill");
    assert_eq!(rows[0]["agents"], json!(["codex:default"]));
    assert_eq!(rows[0]["loaded"], serde_json::Value::Null);
    chezmoi_native::run_browser_pty(&f, &["skills"], "skill", &f.home, "72", "0")?;
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
        (".codex", "codex-import"),
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
    for root in [".claude-glm", ".cursor"] {
        fs::create_dir(f.home.join(root))?;
    }
    let (out, v) = f.run(&["skills"])?;
    assert_code(&out, 0);
    let rows = v["data"]["items"].as_array().ok_or("items")?;
    let row = |name: &str| {
        rows.iter()
            .find(|row| row["name"] == name)
            .ok_or("missing skill")
    };
    let shared = row("fixture-skill")?;
    assert_eq!(
        shared["agents"],
        json!(["claude:mi", "codex:default", "cursor:default"])
    );
    let details = shared["details"].as_array().ok_or("details")?;
    for (label, path) in [
        ("载荷 在磁盘上", ".agents/skills/fixture-skill"),
        (
            "可发现 claude:mi · 档案目录",
            ".claude-mi/skills/fixture-skill",
        ),
        (
            "可发现 cursor:default · 共享目录",
            ".agents/skills/fixture-skill",
        ),
    ] {
        assert!(
            details.contains(&json!(format!("{label} · {}", f.home.join(path).display()))),
            "{details:?}"
        );
    }
    assert!(shared["loaded"].is_null());
    assert_eq!(
        row("claude-import")?["agents"],
        json!(["claude:default", "cursor:default"])
    );
    assert_eq!(row("codex-import")?["agents"], json!(["cursor:default"]));
    chezmoi_native::run_browser_pty(&f, &["skills"], "skill-visibility", &f.home, "95", "0")?;
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
            &["list"],
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
    let (out, v) = f.run(&["skills"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["coverage"], "partial");
    assert_eq!(v["data"]["items"][0]["name"], "fixture-skill");
    assert_eq!(
        v["data"]["items"][0]["agents"],
        json!(["codex:default", "cursor:default"])
    );
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
    for (command, kind) in [("apps", "applications"), ("agents", "agents")] {
        let (_, legacy) = f.run(&["list", "--kind", kind])?;
        let (_, selected) = f.run(&[command])?;
        let ids = |data: &serde_json::Value, field: &str| -> TestResult {
            assert!(data["data"][field].as_array().is_some());
            Ok(())
        };
        ids(&legacy, "resources")?;
        ids(&selected, "items")?;
        let keys = |rows: &[serde_json::Value]| {
            rows.iter()
                .filter_map(|row| row["id"].as_str().map(str::to_owned))
                .collect::<std::collections::BTreeSet<_>>()
        };
        let legacy = legacy["data"]["resources"].as_array().ok_or("legacy")?;
        let selected = selected["data"]["items"].as_array().ok_or("items")?;
        assert_eq!(keys(legacy), keys(selected));
        if kind == "agents" {
            assert_eq!(selected.len(), 2);
            assert!(selected.iter().any(|row| {
                row["summary"] == "claude · mi"
                    && row["details"]
                        .as_array()
                        .is_some_and(|lines| lines.contains(&json!("模型 fixture-mi-model")))
            }));
            assert!(selected.iter().any(|row| {
                row["summary"] == "claude · glm"
                    && row["details"]
                        .as_array()
                        .is_some_and(|lines| lines.contains(&json!("模型 fixture-glm-model")))
            }));
        }
    }
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
    chezmoi_native::run_browser_pty(&f, &["skills"], "skills-discovered", &f.home, "72", "0")?;
    let (out, v) = f.run(&[
        "skills",
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
    let (out, json) = f.run(&["list"])?;
    assert_code(&out, 0);
    assert!(json["data"]["resources"].is_array(), "{json}");
    chezmoi_native::run_browser_pty(&f, &["list"], "unified-list", &f.home, "100", "0")?;
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
    let (_, registry) = f.run(&["list", "--kind", "applications"])?;
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
        &["list", "--chezmoi-source", source],
        "inventory-reject-scope",
        name,
        "110",
        "0",
    )?;
    assert!(!f.file("state").exists());
    chezmoi_native::run_browser_pty(&f, &["list"], "inventory-adopt-cancel", name, "110", "0")?;
    assert!(!f.file("state").exists());
    chezmoi_native::run_browser_pty(&f, &["list"], "inventory-adopt", name, "110", "0")?;
    assert!(f.read_json("state/receipts.json")?["installations"]["npm:scoped-demo"].is_object());
    assert_eq!(fs::read(&path)?, original);
    chezmoi_native::run_browser_pty(&f, &["list"], "inventory-upgrade-cancel", name, "110", "0")?;
    assert_eq!(fs::read(&path)?, original);
    chezmoi_native::run_browser_pty(&f, &["list"], "inventory-remove", name, "110", "0")?;
    assert!(!path.exists());
    assert!(!f.home.join("native-called").exists());
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
fn setup_reports_local_mode_and_cancels_without_creating_state() -> TestResult {
    let f = Fixture::new()?;
    let (out, data) = f.run(&["setup"])?;
    assert_code(&out, 0);
    assert_eq!(data["data"]["mode"], "local", "{data}");
    let fixture = f.command();
    let noninteractive = std::process::Command::new(fixture.get_program())
        .args(fixture.get_args().filter(|arg| *arg != "--json"))
        .arg("setup")
        .output()?;
    assert_code(&noninteractive, 3);
    assert!(String::from_utf8(noninteractive.stderr)?.contains("model_setup_terminal_required"));
    chezmoi_native::run_browser_pty(&f, &["setup"], "setup-cancel", &f.home, "100", "0")?;
    chezmoi_native::run_browser_pty(&f, &["setup"], "setup-hide-key", &f.home, "100", "0")?;
    assert!(!f.file("state").exists());
    assert!(!f.home.join(".config/macops/recommend-model.json").exists());
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
        "install",
        "demo",
        "--version",
        "1.0.0",
    ])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "unknown");
    assert_eq!(fs::read_to_string(f.file("calls"))?, "install\n");
    let (out, v) = f.run(&["update", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "recovery_required");
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
fn killed_parent_leaves_live_group_blocking_other_writes() -> TestResult {
    use std::process::Stdio;
    let f = Fixture::new()?;
    slow_installer(&f)?;
    let mut parent = f
        .command()
        .args(["install", "demo", "--version", "1.0.0"])
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
    let (out, v) = f.run(&["update", "demo"])?;
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
    let (out, _) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, _) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
                let (out, value) = f.run(&[verb, "fixture:demo", "--yes"])?;
                assert_code(&out, 3);
                assert_eq!(value["error"]["code"], "brew_context_binding_required");
            }
            assert_eq!(fs::read_to_string(&native_version)?, "1.0.0");
            assert!(!f.file("state/receipts.json").exists());
            continue;
        }
        if manager == "uv" {
            let (out, value) = f.run(&["adopt", "fixture:demo", "--yes"])?;
            assert_code(&out, 3);
            assert!(value.to_string().contains("legacy_uv_query_unbound"));
            let (out, _) = f.run(&["upgrade", "fixture:demo", "--version", "2.0.0"])?;
            assert_code(&out, 3);
            assert_eq!(fs::read_to_string(&native_version)?, "1.0.0");
            assert!(!f.file("state/receipts.json").exists());
            continue;
        }
        let (out, _) = f.run(&["adopt", "fixture:demo", "--yes"])?;
        assert_code(&out, 0);
        let args = if manager == "brew" {
            vec!["upgrade", "fixture:demo"]
        } else {
            vec!["upgrade", "fixture:demo", "--version", "2.0.0"]
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
    let (out, _) = f.run(&["install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?.to_owned();
    journal["phase"] = json!("prepared");
    journal["process_group"] = serde_json::Value::Null;
    write_json(&f.file("state/active.json"), &journal)?;
    fs::remove_file(f.file(&format!("state/operations/{id}.json")))?;
    fs::remove_file(f.file("version"))?;
    fs::remove_file(f.file("state/receipts.json"))?;
    let (out, v) = f.run(&["recover", &id])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "failed");
    assert!(f.file(&format!("state/operations/{id}.json")).exists());
    let (out, _) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, _) = f.run(&["adopt", "demo", "--yes"])?;
    assert_code(&out, 0);
    let mut journal = f.read_json("state/active.json")?;
    let id = journal["id"].as_str().ok_or("id")?.to_owned();
    journal["phase"] = json!("prepared");
    write_json(&f.file("state/active.json"), &journal)?;
    fs::remove_file(f.file(&format!("state/operations/{id}.json")))?;
    let (out, v) = f.run(&["recover", &id])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    let (out, _) = f.run(&["upgrade", "demo", "--version", "2.0.0"])?;
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
    let (out, _) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
            vec!["install", "demo"]
        } else {
            vec!["install", "demo", "--version", "1.0.0"]
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
    let (out, v) = f.run(&["doctor", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["observation"]["status"], "missing");
    fs::create_dir_all(f.home.join("npm/lib/node_modules/demo"))?;
    let (out, v) = f.run(&["doctor", "demo"])?;
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
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(v["data"]["phase"], "unknown");
    assert!(!f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn app_info_unknown_matches_doctor_exit_contract() -> TestResult {
    let f = Fixture::new()?;
    f.script("demo", "printf 'invalid-json'")?;
    let (out, v) = f.run(&["app", "info", "demo"])?;
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
        vec!["skills", "status", "demo"],
        vec!["skills", "list", "--only", "demo"],
        vec!["skills", "check", "--only", "demo"],
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
        .args(["install", "demo", "--version", "1.0.0"])
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
    let (out, v) = f.run(&["ai", "list"])?;
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
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, _) = f.run(&["upgrade", "fixture:demo", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    let (out, _) = f.run(&["uninstall", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_to_string(f.home.join("installed"))?,
        "demo@1.0.0\ndemo@2.0.0\n"
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
        .args(["install", "demo", "--version", "1.0.0"])
        .output()?;
    assert_code(&out, 0);
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("UV_TOOL_DIR", f.home.join("scope-b"))
        .args(["upgrade", "demo", "--version", "2.0.0"])
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
    let (out, _) = f.run(&["install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    let mut first = f.read_json("state/active.json")?;
    let id = first["id"].as_str().ok_or("id")?.to_owned();
    let (out, _) = f.run(&["upgrade", "demo", "--version", "2.0.0"])?;
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
    let (out, v) = f.run(&["doctor", "manager:cargo"])?;
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
    let (out, v) = f.run(&["doctor", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"][0]["observation"]["version"], "1.2.3");
    f.script(
        "mise",
        r#"printf '[{"version":"1.2.3","active":true},{"version":"2.0.0","active":true}]'"#,
    )?;
    let (out, v) = f.run(&["doctor", "demo"])?;
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
    for command in ["doctor", "update", "install", "upgrade", "uninstall"] {
        let (out, value) = f.run(&[command, "demo", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "brew_context_binding_required");
    }
    let (out, preview) = f.run(&["upgrade", "demo", "--dry-run"])?;
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
    let (out, v) = f.run(&["doctor", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["observation"]["status"], "missing");
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, v) = f.run(&["update", "demo"])?;
    assert_code(&out, 0);
    assert!(v["data"]["index"].is_string());
    assert_eq!(fs::read(f.file("cache/updates.json"))?, bytes);
    let (out, v) = f.run(&["cleanup", "macops:cache", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["review"], json!(["updates.json"]));
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, _) = f.run(&["adopt", "demo", "--yes"])?;
    assert_code(&out, 0);
    let (out, v) = f.run(&["upgrade", "demo", "--version", "2.0.0"])?;
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
        let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, value) = f.run(&["skills", "outdated", "--only", "demo"])?;
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
    let (out, first) = f.run(&["update", "demo"])?;
    assert_code(&out, 0);
    let index = f.current_index()?;
    let bytes = fs::read(&index)?;
    let (out, second) = f.run(&["update", "demo"])?;
    assert_code(&out, 0);
    assert_eq!(first["data"]["index"], second["data"]["index"]);
    assert_eq!(second["data"]["created"], false);
    assert_eq!(second["data"]["owned"], true);
    fs::remove_file(f.file("state/version-indexes.json"))?;
    let (out, borrowed) = f.run(&["update", "demo"])?;
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
        .args(["update", "demo"])
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
    let (out, _) = f.run(&["update", "demo"])?;
    assert_code(&out, 0);
    let index = f.current_index()?;
    fs::write(f.file("cache/indexes/.macops-scope.json"), "{}")?;
    for args in [
        vec!["outdated", "demo"],
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
    let (out, _) = f.run(&["update", "demo"])?;
    assert_code(&out, 0);
    let path = f.current_index()?;
    fs::remove_file(&path)?;
    fs::remove_file(f.file("state/version-indexes.json"))?;
    metadata_barrier(&f, "2.0.0")?;
    let child = f
        .command()
        .args(["update", "demo"])
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
        .args(["update", "demo"])
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
    let (out, _) = f.run(&["update", "demo"])?;
    assert_code(&out, 0);
    let before = fs::read(f.file("state/version-indexes.json"))?;
    metadata_barrier(&f, "3.0.0")?;
    let child = f
        .command()
        .args(["update", "demo"])
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
    let (out, v) = f.run(&["update", "demo"])?;
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
        let (out, _) = f.run(&["update", name])?;
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
    let (out, _) = f.run(&["update", "demo"])?;
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
    let (out, _) = f.run(&["update", "demo"])?;
    assert_code(&out, 3);
    assert_eq!(before, fs::read(f.file("state/version-indexes.json"))?);
    Ok(())
}

#[test]
fn cleanup_does_not_recreate_the_retired_ownership_file() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["update", "demo"])?;
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
    let (out, v) = f.run(&["app", "info", "com.demo"])?;
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
        .args(["update", "demo"])
        .output()?;
    assert_code(&out, 0);
    let out = f
        .command()
        .env("HOME", &f.home)
        .env("UV_TOOL_DIR", f.home.join("scope-b"))
        .args(["install", "demo", "--dry-run"])
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
    let (out, plan) = f.run(&["install", "demo", "--version", "1.0.0", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(plan["data"]["persistentWrites"], false);
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());
    let (out, installed) = f.run(&["install", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(installed["data"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.file("version"))?, "1.0.0");
    let receipt = f.read_json("state/receipts.json")?;
    assert_eq!(
        receipt["installations"]["fixture:demo"]["observation"]["version"],
        "1.0.0"
    );
    let (out, _) = f.run(&["upgrade", "demo", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(fs::read_to_string(f.file("version"))?, "2.0.0");
    let (out, _) = f.run(&["uninstall", "demo", "--yes"])?;
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
    let (out, v) = f.run(&["adopt", "demo", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "not_installed");
    fs::write(f.file("version"), "0.5.0")?;
    let (out, v) = f.run(&["upgrade", "demo", "--version", "1.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "adoption_required");
    let (out, _) = f.run(&["adopt", "demo", "--yes"])?;
    assert_code(&out, 0);
    assert!(!f.file("calls").exists());
    fs::write(f.file("version"), "external")?;
    let (out, v) = f.run(&["uninstall", "demo", "--yes"])?;
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
    let (out, v) = f.run(&["list"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "invalid_manifest");
    fs::write(&manifest, "path\tbundle_id\tversion\towner\tsource\n")?;
    let (out, _) = f.run(&["app", "list"])?;
    assert_code(&out, 0);
    fs::write(
        &manifest,
        "path\tbundle_id\tversion\towner\tsource\nonly-one-column\n",
    )?;
    let (out, _) = f.run(&["list"])?;
    assert_code(&out, 1);
    Ok(())
}

#[test]
fn conda_wrong_field_type_is_not_a_healthy_environment() -> TestResult {
    let f = Fixture::new()?;
    f.script("conda", "printf '%s' '{\"envs\":\"abc\"}'")?;
    let (out, v) = f.run(&["discover", "conda"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "invalid_json");
    f.script("conda", "printf '%s' '{\"envs\":[\"/synthetic/env\"]}'")?;
    let (out, v) = f.run(&["toolchain", "info", "conda"])?;
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
    let (out, _) = f.run(&["service", "list"])?;
    assert_code(&out, 1);
    f.script(
        "brew",
        "printf '%s' '[{\"name\":\"postgresql@16\",\"running\":false}]'",
    )?;
    let (out, v) = f.run(&["service", "list"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["status"], "stopped");
    let (out, _) = f.run(&["service", "check"])?;
    assert_code(&out, 3);
    f.script(
        "brew",
        "printf '%s' '[{\"name\":\"unrelated\",\"running\":true}]'",
    )?;
    let (out, v) = f.run(&["service", "list"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "service_identity_mismatch");
    Ok(())
}

#[test]
fn update_outdated_and_owned_cleanup_leave_unknown_files() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["update", "demo", "--dry-run"])?;
    assert_code(&out, 0);
    assert!(!f.file("cache").exists());
    assert!(!f.file("state").exists());
    let (out, _) = f.run(&["outdated", "demo"])?;
    assert_code(&out, 3);
    let (out, _) = f.run(&["update", "demo"])?;
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
    let (out, _) = f.run(&["update", "demo"])?;
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
        vec!["install", "demo", "--apply"],
        vec!["upgrade"],
        vec!["credential", "get"],
        vec!["install", "demo", "--dry"],
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
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let out = f
        .command()
        .arg("--catalog")
        .arg(path)
        .arg("list")
        .output()?;
    assert_code(&out, 2);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_macops"))
        .args(["--json", "--catalog"])
        .arg(f.file("excluded"))
        .arg("list")
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
        vec!["install", "demo", "--version", "2"],
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
    let (out, v) = f.run(&["credential", "add", "mi", "fixture"])?;
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
        .args(["credential", "check", "mi"])
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
        let (out, v) = f.run(&["credential", "check", "mi"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"][0]["status"], "unknown");
        assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    }
    f.script("security", "/bin/sleep 2")?;
    let (out, v) = f.run(&["--timeout-secs", "1", "credential", "check", "mi"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["status"], "unknown");
    let out = f
        .command()
        .env("PATH", f.home.join("empty-bin"))
        .args(["credential", "check", "mi"])
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
    let (out, v) = f.run(&["credential", "list"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"].as_array().ok_or("profiles")?.len(), 4);
    let (out, v) = f.run(&["credential", "check"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"][0]["reason"], "settings_missing");
    for profile in ["glm", "kimi", "mi", "zen"] {
        let (out, v) = f.run(&["credential", "activate", profile, "safe_alias-1"])?;
        assert_code(&out, 0);
        assert_eq!(
            v["data"]["command"]["args"],
            json!(["credential", "activate", profile, "safe_alias-1"])
        );
        assert_eq!(v["data"]["executed"], false);
    }
    for args in [
        vec!["credential", "check", "unknown"],
        vec!["credential", "add", "unknown", "fixture"],
        vec!["credential", "add", "mi", "a;bad"],
        vec!["credential", "add", "mi", "fixture", "secret-value"],
        vec!["credential", "get"],
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
        let (out, v) = f.run(&["credential", "check", "mi"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"][0]["reason"], "invalid_profile_settings");
    }
    fs::remove_file(&settings)?;
    symlink(f.catalog.clone(), &settings)?;
    let (out, v) = f.run(&["credential", "check", "mi"])?;
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
    let (out, v) = f.run(&["service", "start", "postgresql", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(
        v["data"]["plan"]["command"]["args"],
        json!(["services", "start", "postgresql@16"])
    );
    assert!(!f.file("state").exists());
    assert!(!f.home.join("service-calls").exists());
    let (out, v) = f.run(&["service", "start", "postgresql"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "confirmation_required");
    assert!(!f.file("state").exists());
    let (out, v) = f.run(&["service", "start", "postgresql", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert_eq!(v["data"]["plan"]["action"], "start");
    assert!(!f.file("state/receipts.json").exists());
    let before = fs::read(f.file("state/active.json"))?;
    let (out, v) = f.run(&["service", "start", "postgresql"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["mode"], "no-op");
    assert_eq!(before, fs::read(f.file("state/active.json"))?);
    let (out, v) = f.run(&["service", "stop", "postgresql", "--yes"])?;
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
                    let (out, v) = f.run(&["service", verb, "postgresql", "--dry-run"])?;
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
    let (out, v) = f.run(&["service", "start", "postgresql", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["phase"], "unknown");
    assert_eq!(fs::read_to_string(f.home.join("service-calls"))?, "start\n");
    let (out, v) = f.run(&["service", "start", "postgresql", "--yes"])?;
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
        let (out, v) = f.run(&["service", "start", "postgresql", "--yes"])?;
        assert_code(&out, 3);
        assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
        assert!(!String::from_utf8(out.stderr)?.contains("SYNTHETIC_SECRET"));
        assert!(!f.file("state").exists());
        assert!(!f.home.join("service-calls").exists());
    }
    let mut valid = service_row(true, true, true, Some(&service_user()?));
    valid[0]["command"] = json!("SYNTHETIC_SECRET_COMMAND");
    write_json(&f.home.join("service-state"), &valid)?;
    let (out, v) = f.run(&["service", "start", "postgresql", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["mode"], "no-op");
    assert!(!v.to_string().contains("SYNTHETIC_SECRET"));
    f.script("brew", "printf 'SYNTHETIC_SECRET_STDERR' >&2; exit 2")?;
    let (out, v) = f.run(&["service", "list"])?;
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
        .args([
            "--timeout-secs",
            "1",
            "service",
            "start",
            "postgresql",
            "--yes",
        ])
        .output()?;
    assert_code(&out, 3);
    let journal = f.read_json("state/active.json")?;
    assert_eq!(journal["phase"], "unknown");
    let id = journal["id"].as_str().ok_or("id")?;
    let (out, v) = f.run(&["service", "stop", "postgresql", "--yes"])?;
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
    let (out, v) = f.run(&["install", "demo", "--version", "1.0.0"])?;
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
    let (out, v) = f.run(&["list", "--kind", "agents"])?;
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
    let (out, v) = f.run(&["list", "--kind", "capabilities"])?;
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
    let (out, v) = f.run(&["list", "--kind", "capabilities"])?;
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
    let (out, v) = f.run(&["info", "skill:demo"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "ambiguous");
    write_json(
        &f.home.join(".agents/.skill-lock.json"),
        &json!({"version":1,"skills":{}}),
    )?;
    let (out, v) = f.run(&["list", "--kind", "capabilities"])?;
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
    let release1 = f.home.join("releases/1.0.0");
    let release2 = f.home.join("releases/2.0.0");
    fs::create_dir_all(&release1)?;
    fs::create_dir_all(&release2)?;
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
        fs::write(
            f.home.join(format!(".local/bin/claude-{profile}")),
            format!(
                "#!/bin/sh\nset -eu\nexec {} run {profile} \"$@\"\n",
                helper.display()
            ),
        )?;
    }
    let p1 = f.file("project-one");
    let p2 = f.file("project-two");
    fs::create_dir(&p1)?;
    fs::create_dir(&p2)?;
    for (project, version) in [(&p1, "1.0.0"), (&p2, "2.0.0")] {
        let (out, v) = f.run(&[
            "list",
            "--kind",
            "agents",
            "--project",
            project.to_str().ok_or("project")?,
        ])?;
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
    }
    let contexts = fs::read_to_string(f.home.join("query-context"))?;
    for line in contexts.lines() {
        let values: Vec<_> = line.split('|').collect();
        assert_eq!(values[0], values[1]);
        assert!(values[2].contains(".claude-"));
    }
    assert!(!f.file("invoked-helper").exists());
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
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
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
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
    let (out, v) = f.run(&["info", "dup"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "ambiguous");
    for action in ["adopt", "upgrade", "uninstall"] {
        let (out, v) = f.run(&[action, "dup", "--yes"])?;
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
    let (out, v) = f.run(&[
        "list",
        "--kind",
        "capabilities",
        "--project",
        project.to_str().ok_or("project")?,
    ])?;
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
    let (out, v) = f.run(&["list"])?;
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
    let (out, v) = f.run(&["list", "--kind", "not-supported"])?;
    assert_code(&out, 1);
    assert_eq!(v["error"]["code"], "invalid_kind");
    let (out, _) = f.run(&["list", "--project", "/Users/example/job/forbidden"])?;
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
    for kind in ["agents", "capabilities", "configs"] {
        let (out, v) = f.run(&["list", "--kind", kind])?;
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
    let (out, v) = f.run(&["list", "--kind", "capabilities"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["incomplete"], true);
    let (out, _) = f.run(&["list", "--kind", "applications"])?;
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
    let (out, v) = f.run(&["list", "--kind", "capabilities"])?;
    assert_code(&out, 0);
    let items = v["data"]["resources"].as_array().ok_or("resources")?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "missing");
    assert_eq!(items[0]["contents"][0]["status"], "missing");
    fs::create_dir(skill.join("SKILL.md"))?;
    let (out, v) = f.run(&["list", "--kind", "capabilities"])?;
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
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
    assert_code(&out, 0);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "npm")
        .ok_or("npm resource")?;
    let id = resource["id"].as_str().ok_or("id")?.to_owned();
    let (out, v) = f.run(&["adopt", &id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    let receipt = f.read_json("state/receipts.json")?;
    assert!(receipt["installations"][&id].is_object());
    assert_eq!(
        receipt["installations"][&id]["observation"]["status"],
        "native-package"
    );
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
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
    let (out, v) = f.run(&["upgrade", &id, "--version", "2.0.0"])?;
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
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
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
    let (out, _) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&out, 0);
    let receipts = f.read_json("state/receipts.json")?;
    assert!(receipts["installations"]["fixture:demo"].is_object());
    assert!(receipts["installations"][id].is_null());
    assert!(!f.home.join("writes").exists());
    let (out, v) = f.run(&["upgrade", id, "--version", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["plan"]["package"]["id"], "fixture:demo");
    assert_eq!(fs::read_to_string(prefix.join("version"))?, "2.0.0");
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
    assert_code(&out, 0);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["id"] == id)
        .ok_or("stable id")?;
    assert_eq!(resource["management"], "adopted");
    assert_eq!(resource["releases"][0]["version"], "2.0.0");
    let (out, _) = f.run(&["uninstall", id, "--yes"])?;
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
    let (out, _) = f.run(&["adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    catalog["packages"] = json!([]);
    write_json(&f.catalog, &catalog)?;
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
    assert_code(&out, 0);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "npm")
        .ok_or("npm")?;
    let id = resource["id"].as_str().ok_or("id")?;
    let (out, _) = f.run(&["adopt", id, "--yes"])?;
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
        let (out, v) = f.run(&["adopt", key, "--yes"])?;
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
    let (out, v) = f.run(&["adopt", "native-demo", "--yes"])?;
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
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
    assert_code(&out, 3);
    let resource = v["data"]["resources"]
        .as_array()
        .ok_or("resources")?
        .iter()
        .find(|r| r["manager"] == "npm")
        .ok_or("npm")?;
    assert_eq!(resource["status"], "unknown");
    let (out, v) = f.run(&["adopt", resource["id"].as_str().ok_or("id")?, "--yes"])?;
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
    let (out, v) = f.run(&["adopt", "native-demo", "--yes"])?;
    assert_code(&out, 3);
    assert!(v["error"]["code"].is_string());
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
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
        let (out, v) = f.run(&[action, "fixture:demo", "--yes"])?;
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
    let (out, _) = f.run(&["adopt", "native-demo", "--yes"])?;
    assert_code(&out, 0);
    let receipt = fs::read(f.file("state/receipts.json"))?;
    let (out, v) = f.run(&["info", "native-demo"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["resource"]["baseline"]["comparison"], "same");
    npm_manifest(&prefix, "native-demo", "2.0.0")?;
    let (out, v) = f.run(&["info", "native-demo"])?;
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
    let (out, v) = f.run(&["list", "--kind", "applications"])?;
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
    let (out, v) = f.run(&["doctor", id])?;
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
    for args in [vec!["config", "list"], vec!["list", "--kind", "configs"]] {
        let (out, value) = f.run(&args)?;
        assert_code(&out, if args[0] == "config" { 3 } else { 0 });
        if args[0] == "config" {
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
        let (out, result) = f.run(&["install", "fixture:demo", "--version", "1.0.0"])?;
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
        vec!["discover", "npm"],
        vec!["discover"],
        vec!["toolchain", "info", "npm"],
        vec!["toolchain", "list", "npm"],
        vec!["list", "--kind", "applications"],
        vec!["doctor", "fixture:demo"],
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
    let (out, value) = f.run(&["adopt", "@scope/other", "--yes"])?;
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
    let (out, _) = f.run(&["adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    let baseline = fs::read(f.file("state/receipts.json"))?;
    catalog["packages"] = json!([]);
    write_json(&f.catalog, &catalog)?;
    fs::remove_file(&alias)?;
    symlink(&replacement, &alias)?;
    let (out, result) = f.run(&["discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(
        result["data"]["namespaces"],
        json!([{"prefix":original,"sources":["receipt"],"count":1}])
    );
    assert_eq!(
        result["data"]["packages"][0]["releases"][0]["version"],
        "1.0.0"
    );
    let (out, result) = f.run(&["doctor", "fixture:demo"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"][0]["observation"]["status"], "unknown");
    let (out, result) = f.run(&["adopt", "fixture:demo", "--yes"])?;
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
    let (out, result) = f.run(&["discover", "npm"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["coverage"]["state"], "invalid");
    assert_eq!(result["data"]["incomplete"], true);
    let (out, result) = f.run(&["discover"])?;
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
    let (out, result) = f.run(&["discover", "npm"])?;
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
    let (out, result) = f.run(&["doctor", "fixture:demo"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"][0]["observation"]["status"], "unknown");
    let (out, result) = f.run(&["discover", "npm"])?;
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
    let (out, _) = f.run(&["adopt", "fixture:demo", "--yes"])?;
    assert_code(&out, 0);
    let receipt = fs::read(f.file("state/receipts.json"))?;
    fs::rename(&prefix, f.home.join("retired-prefix"))?;
    npm_manifest(&prefix, "demo", "2.1.0")?;
    f.script(
        "npm",
        r#"[ "$1" = prefix ] || exit 98
printf '%s' "$MACOPS_TEST_HOME/prefix""#,
    )?;
    let (out, result) = f.run(&["discover", "npm"])?;
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
    let (out, result) = f.run(&["discover", "npm"])?;
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

    let (out, ambig) = f.run(&["adopt", "plugin:code-review@official", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(ambig["error"]["code"], "ambiguous");

    let recipe = fixed_native_recipe(&f, "tdd")?;
    let recipe = recipe.to_str().ok_or("recipe path")?;
    let (out, preview) = f.run(&[
        "skills",
        "adopt",
        "skill:tdd",
        "--recipe",
        recipe,
        "--dry-run",
    ])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert!(!preview.to_string().contains("SYNTHETIC_SECRET"));

    let (out, adopted) = f.run(&["skills", "adopt", "skill:tdd", "--recipe", recipe, "--yes"])?;
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

    let (out, err) = f.run(&["install", &skill_id, "--yes"])?;
    assert_code(&out, 1);
    assert_eq!(err["error"]["code"], "operation_unsupported");

    fs::write(canonical.join(".in_use"), "lease")?;
    let (out, doc_clean) = f.run(&["doctor", &skill_id])?;
    assert_code(&out, 0);
    assert_eq!(doc_clean["data"][0]["baseline"]["comparison"], "same");
    fs::remove_file(canonical.join(".in_use"))?;

    assert_capability_lease_conflicts(&f, &canonical, &skill_id)?;
    fs::write(canonical.join("SKILL.md"), "---\nname: tdd\n---\nedited\n")?;
    let (out, doc_drift) = f.run(&["doctor", &skill_id])?;
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
    let (out, data) = f.run(&["discover", "npm"])?;
    assert_code(&out, 0);
    let target = data["data"]["packages"]
        .as_array()
        .ok_or("packages")?
        .iter()
        .find(|row| row["native_id"] == "@scope/demo")
        .ok_or("observed installation")?;
    let id = target["id"].as_str().ok_or("identity")?.to_owned();
    assert_code(&f.run(&["adopt", &id, "--yes"])?.0, 0);
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
    let (out, unbound) = f.run(&["upgrade", id, "--version", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(unbound["error"]["code"], "manager_scope_changed");
    assert!(!f.home.join("native-writes").exists());

    let (ordinary, rejected) = f.run(&["adopt", id, "--yes"])?;
    assert_code(&ordinary, 3);
    assert_eq!(rejected["error"]["code"], "manager_scope_changed");
    let original_receipts = fs::read(f.file("state/receipts.json"))?;
    let (out, preview) = f.run(&["--dry-run", "adopt", id, "--bind-recipe"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["mode"], "dry-run");
    assert_eq!(preview["data"]["persistentWrites"], false);
    assert_eq!(fs::read(f.file("state/receipts.json"))?, original_receipts);
    assert_code(&f.run(&["adopt", id, "--bind-recipe", "--yes"])?.0, 0);
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

    let (out, upgraded) = f.run(&["upgrade", id, "--version", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(upgraded["data"]["plan"]["package"]["id"], id);
    let (_, observed) = f.run(&["discover", "npm"])?;
    let row = observed["data"]["packages"]
        .as_array()
        .ok_or("packages")?
        .iter()
        .find(|row| row["id"] == id)
        .ok_or("stable id")?;
    assert_eq!(row["releases"][0]["version"], "2.0.0");
    assert_code(&f.run(&["uninstall", id, "--yes"])?.0, 0);
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
        let (out, result) = f.run(&["adopt", &package, "--bind-recipe", "--yes"])?;
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
        let (out, recovered) = f.run(&["recover", &id, "--resolve-failed", "--yes"])?;
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
        let (out, data) = f.run(&["adopt", &id, "--bind-recipe", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(data["error"]["code"], code, "{case}");
        assert_eq!(fs::read(&receipt).ok(), old_receipt, "{case}");
        assert_eq!(fs::read(f.file("state/active.json"))?, old_active, "{case}");
    }
    Ok(())
}

#[test]
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
    assert_code(&f.run(&["adopt", "npm:scoped-demo", "--yes"])?.0, 0);
    assert_eq!(
        f.run(&["uninstall", "npm:scoped-demo", "--yes"])?.1["error"]["code"],
        "remove_authority_required"
    );
    assert_eq!(
        f.run(&["upgrade", "npm:scoped-demo", "--version", "1.2.0", "--yes"])?
            .1["error"]["code"],
        "registry_authority_required"
    );

    write_authorized_npm_catalog(&f, &prefix, true, true)?;
    let (out, drift) = f.run(&["upgrade", "npm:scoped-demo", "--version", "1.2.0", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(drift["error"]["code"], "installation_drift");
    assert!(
        drift["error"]["message"]
            .as_str()
            .ok_or("msg")?
            .contains("写权限")
    );

    let (out, adopted) = f.run(&["adopt", "npm:scoped-demo", "--yes"])?;
    assert_code(&out, 0);
    assert!(!adopted.to_string().contains("SYNTHETIC_NPM_SECRET"));
    assert_eq!(
        f.run(&["update", "npm:scoped-demo"])?.1["data"]["results"][0]["version"],
        "1.2.0"
    );

    let raw = f
        .command()
        .env("NODE_OPTIONS", "--require=/evil.js")
        .env("NPM_Config_Registry", "https://evil.invalid")
        .args(["upgrade", "npm:scoped-demo", "--version", "1.2.0", "--yes"])
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
    let (out, link_err) = f.run(&["uninstall", "npm:scoped-demo", "--yes"])?;
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

    let (out, removed) = f.run(&["uninstall", "npm:scoped-demo", "--yes"])?;
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
        .args(["ai", "reconcile"])
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
        "ai",
        "reconcile",
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
    let (out, value) = f.run(&["list"])?;
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
        vec!["skills", "list"],
        vec!["skills", "check", "demo"],
        vec!["skills", "status", "demo"],
        vec!["skills", "policy", "check"],
        vec!["skills", "policy", "recover"],
        vec!["skills", "policy", "recover", "--change-id", "CASE-1"],
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
    let (out, value) = f.run(&["skills", "policy", "recover"])?;
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
        vec!["skills", "list"],
        vec!["skills", "status", "demo"],
        vec!["skills", "check", "demo"],
        vec!["skills", "policy", "check"],
        vec!["list", "--kind", "capabilities"],
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
    let (out, value) = f.run(&["skills", "policy", "check"])?;
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
    let (out, value) = f.run(&["skills", "check"])?;
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
            for args in [
                vec!["skills", "check"],
                vec!["list", "--kind", "capabilities"],
            ] {
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
    let (out, adopted) = f.run(&["--yes", "skills", "adopt", "skill:demo", "--recipe", recipe])?;
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
    let (out, value) = f.run(&[
        "--dry-run",
        "skills",
        "policy",
        "sync",
        "--change-id",
        "DRY",
    ])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["persistentWrites"], false);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    let (out, value) = f.run(&["skills", "policy", "sync"])?;
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
    let (out, value) = f.run(&["--yes", "skills", "policy", "sync", "--change-id", "POLICY"])?;
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
    let (out, value) = f.run(&["doctor", &skill_id])?;
    assert_code(&out, 0);
    assert_eq!(value["data"][0]["baseline"]["comparison"], "same");
    let (out, value) = f.run(&["skills", "policy", "recover"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["persistentWrites"], false);
    assert_eq!(fs::read(&receipt_path)?, applied_bytes);
    write_json(
        &rules,
        &json!({"schema_version":1,"standalone_manual_only":[]}),
    )?;
    let (out, value) = f.run(&["--yes", "skills", "policy", "sync", "--change-id", "RETIRE"])?;
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
    let (out, value) = f.run(&["doctor", &skill_id])?;
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
        let (out, value) = f.run(&["list", "--kind", "applications"])?;
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
    let (out, unknown) = f.run(&["--dry-run", "skills", "sync", "--all"])?;
    assert_code(&out, 3);
    assert_eq!(unknown["error"]["code"], "unverified_profile_configuration");
    assert!(!profile.join("skills").exists());
    let (out, plan) = f.run(&["--dry-run", "skills", "sync", "--only", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(plan["data"]["backend"], "rust");
    assert_eq!(plan["data"]["incomplete"], true);
    assert!(!f.file("state").exists());
    let (out, synced) = f.run(&[
        "--yes",
        "skills",
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
    let (out, noop) = f.run(&["--yes", "skills", "sync", "--only", "alpha"])?;
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
    let (out, value) = f.run(&["--yes", "skills", "sync", "--only", "shared"])?;
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
        .chain(["--dry-run", "skills", "sync", "--only", "profile:mi"])
        .collect();
    let (out, dry) = f.run(&args)?;
    assert_code(&out, 0);
    assert_eq!(dry["data"]["planned_changes"], 2);
    assert_eq!(fs::read(&live)?, live_before);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    let args: Vec<_> = scope
        .into_iter()
        .chain(["--yes", "skills", "sync", "--only", "profile:mi"])
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
    let (out, value) = f.run(&["--yes", "skills", "sync", "--only", "source:alpha"])?;
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
        "skills",
        "adopt",
        "skill:demo",
        "--recipe",
        recipe,
    ])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["recipe"]["scope"], "global");
    assert!(!f.file("state").exists());
    assert!(!f.home.join(".agents/backups").exists());
    let (out, value) = f.run(&["--yes", "skills", "adopt", "skill:demo", "--recipe", recipe])?;
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
            vec!["--yes", "skills", "adopt", "skill:demo", "--recipe", recipe]
        } else {
            vec![
                "--yes",
                "--chezmoi-source",
                "/private/tmp/macops-unused-source",
                "skills",
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
    let (out, doc_link_lease) = f.run(&["doctor", skill_id])?;
    assert_code(&out, 3);
    assert_eq!(
        doc_link_lease["data"][0]["baseline"]["comparison"],
        "unknown"
    );
    fs::remove_file(canonical.join(".in_use"))?;

    fs::create_dir_all(canonical.join(".in_use"))?;
    fs::write(canonical.join(".in_use/run.sh"), "echo bad")?;
    let (out, doc_dir_lease) = f.run(&["doctor", skill_id])?;
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
    let (out, _) = f.run(&["adopt", "fixture:demo", "--yes"])?;
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
    let (out, adopted) = f.run(&["adopt", id, "--yes"])?;
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
    let (out, skills) = f.run(&["skills", "list", "--details"])?;
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
    assert_code(&f.run(&["adopt", id, "--yes"])?.0, 0);
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

#[test]
fn recommend_confirms_one_frozen_npm_install_and_registers_it() -> TestResult {
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
 /bin/mkdir -p "$4/lib/node_modules/@scope/demo"
 printf '{"name":"@scope/demo","version":"1.2.3"}' > "$4/lib/node_modules/@scope/demo/package.json"
 printf '%s\n' "$spec" >> "$MACOPS_TEST_HOME/installed" ;;
*) exit 8 ;;
esac"#,
    )?;
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
    assert_code(&invoke(&["adopt", id, "--yes"], false)?.0, 0);
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
    assert_code(&f.run(&["adopt", id, "--yes"])?.0, 0);
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
