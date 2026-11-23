use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn migration_fixture(f: &Fixture) -> TestResult {
    let agents = f.home.join(".agents");
    fs::create_dir_all(agents.join("cache"))?;
    fs::write(
        agents.join("managed-assets.json"),
        "{\"vendor\":{\"version\":\"1\",\"unknown\":true}}\n",
    )?;
    write_json(
        &agents.join("invocation-overlays.json"),
        &json!({"schema_version":1,"files":{}}),
    )?;
    write_json(
        &agents.join("cache/managed-updates.json"),
        &json!({"version":1,"updates":{}}),
    )?;
    let manifests = f.root.path().join("settings/manifests");
    write_json(
        &manifests.join("skill-profiles.json"),
        &json!({"claude":{}}),
    )?;
    write_json(
        &manifests.join("managed-agent-assets.json"),
        &json!({"schema_version":1,"assets":{}}),
    )?;
    write_json(
        &manifests.join("agent-invocation-policy.json"),
        &json!({"schema_version":1,"standalone_manual_only":[]}),
    )?;
    write_json(
        &manifests.join("agent-source-channels.json"),
        &json!({"schema_version":1,"source_ids":{}}),
    )?;
    fs::write(
        manifests.join("mac-apps.tsv"),
        "path\tbundle_id\tversion\towner\tsource\n",
    )?;
    fs::write(
        f.root.path().join("settings/dirty-new.txt"),
        "uncommitted original\n",
    )?;
    symlink(
        "../.local/settings/manifests/skill-profiles.json",
        agents.join("skill-profiles.json"),
    )?;
    fs::remove_file(agents.join("skill-profiles.json"))?;
    symlink(
        manifests.join("skill-profiles.json"),
        agents.join("skill-profiles.json"),
    )?;
    Ok(())
}

fn bytes(path: &Path) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    Ok(fs::read(path)?)
}

#[test]
fn migration_preflight_run_verify_preserve_legacy_bytes_and_new_business_state() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let old = f.home.join(".agents/managed-assets.json");
    let original = bytes(&old)?;
    let (out, v) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["ready"], true);
    assert!(!f.root.path().join("state").exists());
    assert!(!f.home.join(".config/macops").exists());
    let (out, v) = f.run(&["migrate", "run", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["read_only"], true);
    assert!(!f.root.path().join("state").exists());
    let (out, v) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["authority"], "migrated");
    assert_eq!(bytes(&old)?, original);
    let target = f.root.path().join("state/managed-assets.json");
    assert_eq!(bytes(&target)?, original);
    assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o7777, 0o600);
    for path in [
        f.root.path().join("state"),
        f.home.join(".config/macops"),
        f.home.join(".local/share/macops"),
    ] {
        assert_eq!(fs::metadata(path)?.permissions().mode() & 0o7777, 0o700);
    }
    write_json(
        &target,
        &json!({"vendor":{"version":"2","new_business":true}}),
    )?;
    for command in [
        ["migrate", "verify"],
        ["migrate", "verify"],
        ["migrate", "run"],
        ["migrate", "preflight"],
    ] {
        let (out, v) = f.run(&command)?;
        assert_code(&out, 0);
        assert_eq!(v["data"]["mode"], "verify");
    }
    assert_eq!(
        f.read_json("state/managed-assets.json")?["vendor"]["version"],
        "2"
    );
    fs::write(&old, "{\"vendor\":{\"version\":\"late divergence\"}}\n")?;
    let (out, v) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_source_diverged");
    assert_eq!(
        f.read_json("state/managed-assets.json")?["vendor"]["version"],
        "2"
    );
    Ok(())
}

#[test]
fn migration_unknown_governance_target_conflicts_and_special_inputs_block_without_writes()
-> TestResult {
    for kind in [
        "unknown", "manifest", "pending", "conflict", "symlink", "fifo", "git-lock",
    ] {
        let f = Fixture::new()?;
        migration_fixture(&f)?;
        match kind {
            "unknown" => fs::write(f.home.join(".agents/unknown-state.json"), "{}")?,
            "manifest" => fs::write(f.root.path().join("settings/manifests/unknown.json"), "{}")?,
            "pending" => fs::write(f.home.join(".agents/invocation-policy.pending.json"), "{}")?,
            "conflict" => {
                fs::create_dir_all(f.root.path().join("state"))?;
                fs::set_permissions(
                    f.root.path().join("state"),
                    fs::Permissions::from_mode(0o700),
                )?;
                fs::write(f.root.path().join("state/managed-assets.json"), "foreign")?;
            }
            "symlink" => {
                fs::remove_file(f.home.join(".agents/managed-assets.json"))?;
                symlink("/not-opened", f.home.join(".agents/managed-assets.json"))?;
            }
            "fifo" => {
                assert!(
                    std::process::Command::new("/usr/bin/mkfifo")
                        .arg(f.root.path().join("settings/pipe"))
                        .status()?
                        .success()
                );
            }
            _ => {
                fs::create_dir_all(f.root.path().join("settings/.git"))?;
                fs::write(f.root.path().join("settings/.git/index.lock"), "writer")?;
            }
        }
        let (out, v) = f.run(&["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["ready"], false, "{kind}: {v}");
        let (out, _) = f.run(&["migrate", "run", "--yes"])?;
        assert_code(&out, 3);
        assert!(!f.root.path().join("state/migration-ledger.json").exists());
        if kind == "conflict" {
            assert_eq!(
                fs::read_to_string(f.root.path().join("state/managed-assets.json"))?,
                "foreign"
            );
        }
    }
    Ok(())
}

#[test]
fn migration_cache_is_volatile_but_durable_schema_and_source_proofs_remain_required() -> TestResult
{
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    fs::remove_file(f.home.join(".agents/cache/managed-updates.json"))?;
    fs::remove_file(f.root.path().join("cache/managed-updates.json"))?;
    let (out, v) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["cache_miss"], true);
    let mut args: Vec<_> = f
        .command()
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
    let index = args
        .iter()
        .position(|x| x == "--cache-dir")
        .ok_or("cache arg")?;
    args[index + 1] = f.file("rebuilt-cache").into_os_string();
    let out = common::empty_environment_command()
        .env("HOME", &f.home)
        .args(args)
        .args(["migrate", "verify"])
        .output()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["cache_miss"], true);
    fs::write(f.root.path().join("state/invocation-overlays.json"), "[]")?;
    let (out, _) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 1);
    Ok(())
}

#[test]
fn migration_verify_validates_new_business_datasets_even_when_originally_missing() -> TestResult {
    let f = Fixture::new()?;
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    fs::write(f.root.path().join("state/invocation-overlays.json"), b"[]")?;
    let (out, _) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 1);
    Ok(())
}

#[test]
fn migration_pending_blocks_every_normal_entry_and_preserves_foreign_marker() -> TestResult {
    for (name, kind) in [
        ("migration.pending.json", "file"),
        ("migration.pending.json", "symlink"),
        ("migration.pending.json", "directory"),
        ("migration-active.json", "file"),
        ("migration-active.json", "symlink"),
        ("migration-active.json", "directory"),
    ] {
        let f = Fixture::new()?;
        let state = f.root.path().join("state");
        fs::create_dir_all(&state)?;
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))?;
        let marker = state.join(name);
        match kind {
            "file" => fs::write(&marker, "foreign damaged marker")?,
            "symlink" => symlink("/never-opened", &marker)?,
            _ => fs::create_dir(&marker)?,
        }
        for command in [
            vec!["app", "resources"],
            vec!["app", "doctor"],
            vec!["app", "install", "demo", "--version", "1.0.0", "--yes"],
            vec!["recover", "OP-123-456"],
            vec!["migrate", "preflight"],
        ] {
            let (out, v) = f.run(&command)?;
            assert_code(&out, 3);
            assert_eq!(v["error"]["code"], "migration_recovery_required");
        }
        assert!(fs::symlink_metadata(&marker).is_ok());
        assert!(!f.file("calls").exists());
    }
    Ok(())
}

#[test]
fn migration_pending_blocks_browser_before_display_in_all_output_modes() -> TestResult {
    let f = Fixture::new()?;
    fs::create_dir(f.file("state"))?;
    let marker = f.file("state/migration-active.json");
    let bytes = b"interrupted migration";
    fs::write(&marker, bytes)?;
    for noun in ["app", "config", "agent", "skill", "history"] {
        let (json, data) = f.run(&[noun])?;
        assert_code(&json, 3);
        assert_eq!(
            data["error"]["code"], "migration_recovery_required",
            "{noun}"
        );
        let message = data["error"]["message"].as_str().ok_or("error message")?;
        assert!(message.contains("无法识别迁移编号"));
        assert!(!message.contains("migrate recover"));
        let command = f.command();
        let plain = std::process::Command::new(command.get_program())
            .args(command.get_args().filter(|arg| *arg != "--json"))
            .envs(
                command
                    .get_envs()
                    .filter_map(|(name, value)| value.map(|value| (name, value))),
            )
            .arg(noun)
            .output()?;
        assert_code(&plain, 3);
        assert!(plain.stdout.is_empty(), "displayed {noun} during migration");
        assert!(String::from_utf8(plain.stderr)?.contains("migration_recovery_required"));
        chezmoi_native::run_browser_pty(&f, &[noun], "migration-blocked", &marker, "96", "3")?;
    }
    assert_eq!(fs::read(marker)?, bytes);
    assert!(!f.file("state/operations").exists());
    assert!(!f.file("state/write.lock").exists());
    Ok(())
}

#[test]
fn migration_retains_all_claude_models_versions_and_codex_config_bytes() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let mut protected = Vec::new();
    for (index, root) in [
        ".claude",
        ".claude-mi",
        ".claude-kimi",
        ".claude-glm",
        ".claude-zen",
    ]
    .into_iter()
    .enumerate()
    {
        let path = f.home.join(root).join("settings.json");
        fs::create_dir_all(path.parent().ok_or("parent")?)?;
        let value =
            format!("{{\"env\":{{\"ANTHROPIC_MODEL\":\"custom-{index}\"}},\"unknown\":true}}\n");
        fs::write(&path, &value)?;
        protected.push((path, value.into_bytes()));
    }
    for (relative, value) in [
        (
            ".codex/config.toml",
            "# keep comment\nmodel = 'custom-codex'\n",
        ),
        (
            ".local/share/mise/installs/claude-code/1/bin/claude",
            "first binary sentinel\n",
        ),
        (
            ".local/share/mise/installs/claude-code/2/bin/claude",
            "second binary sentinel\n",
        ),
    ] {
        let path = f.home.join(relative);
        fs::create_dir_all(path.parent().ok_or("parent")?)?;
        fs::write(&path, value)?;
        protected.push((path, value.as_bytes().to_vec()));
    }
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let (out, _) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    for (path, before) in protected {
        assert_eq!(fs::read(path)?, before);
    }
    assert!(!f.file("calls").exists());
    Ok(())
}

#[test]
fn migration_indexes_dependency_lock_and_dirty_link_bytes_without_executing_or_following()
-> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let settings = f.root.path().join("settings");
    fs::write(settings.join("Cargo.lock"), "a normal dependency manifest")?;
    fs::create_dir_all(settings.join("fixtures"))?;
    fs::write(settings.join("fixtures/active.json"), "a test fixture")?;
    symlink(
        "/not-traversed/credential-canary",
        settings.join("dirty-link"),
    )?;
    fs::write(settings.join("installer.sh"), "exit 97\n")?;
    let (out, v) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["ready"], true);
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_link(settings.join("dirty-link"))?,
        Path::new("/not-traversed/credential-canary")
    );
    let (out, _) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    fs::write(
        settings.join("dirty-new.txt"),
        "late archived source divergence",
    )?;
    let (out, v) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_source_diverged");
    Ok(())
}

#[test]
fn migration_r6_retains_native_state_without_observing_its_contents() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let native = f.home.join(".local/state/gh");
    fs::create_dir_all(&native)?;
    let payload = native.join("opaque");
    fs::write(&payload, "before")?;
    symlink("/no/fixture/target", native.join("opaque-link"))?;

    let (out, preflight) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    assert!(
        preflight["data"]["retained"]
            .as_array()
            .ok_or("retained")?
            .iter()
            .any(|entry| { entry["path"] == json!(native) && entry["owner"] == "native_state" })
    );
    assert!(!f.file("state").exists());

    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let ledger = f.read_json("state/migration-ledger.json")?;
    assert_eq!(ledger["schema_version"], 2);
    assert_eq!(fs::read_to_string(&payload)?, "before");
    fs::write(&payload, "after")?;
    fs::write(native.join("added"), "native change")?;
    for _ in 0..2 {
        let (out, result) = f.run(&["migrate", "verify"])?;
        assert_code(&out, 0);
        assert_eq!(result["data"]["authority"], "migrated");
    }
    assert_eq!(fs::read_to_string(&payload)?, "after");
    fs::create_dir(f.home.join(".agents/cache/upstream"))?;
    fs::write(
        f.home.join(".agents/cache/upstream/unknown"),
        "unknown checkout sibling",
    )?;
    let (out, result) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 3);
    assert_eq!(result["error"]["code"], "migration_preflight_blocked");
    Ok(())
}

fn migration_with_root(
    f: &Fixture,
    flag: &str,
    root: &Path,
    command: &[&str],
) -> Result<(std::process::Output, serde_json::Value), Box<dyn std::error::Error>> {
    let mut args: Vec<_> = f
        .command()
        .get_args()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
    if let Some(index) = args.iter().position(|arg| arg == flag) {
        args[index + 1] = root.as_os_str().into();
    } else {
        args.extend([flag.into(), root.as_os_str().into()]);
    }
    let output = common::empty_environment_command()
        .env("HOME", &f.home)
        .args(args)
        .args(command)
        .output()?;
    let value = serde_json::from_slice(&output.stdout)?;
    Ok((output, value))
}

#[test]
fn migration_r6_checks_lexical_and_anchored_aliases_before_reading_the_selected_state_ledger()
-> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let lexical = f.home.join(".local/state");
    let physical = f.file("physical-native");
    fs::create_dir_all(physical.join("gh"))?;
    fs::create_dir(lexical.parent().ok_or("parent")?)?;
    symlink(&physical, &lexical)?;
    let native = physical.join("gh");
    fs::write(
        native.join("migration-ledger.json"),
        "never interpreted as a ledger",
    )?;
    for flag in ["--config-dir", "--data-dir", "--state-dir", "--cache-dir"] {
        for root in [
            native.clone(),
            native.join("missing/deep"),
            lexical.join("gh"),
            lexical.join("gh/missing"),
        ] {
            let (out, v) = migration_with_root(&f, flag, &root, &["migrate", "run", "--yes"])?;
            assert_code(&out, 3);
            assert_eq!(
                v["error"]["code"], "migration_input_changed",
                "{flag} {root:?}"
            );
            assert!(!native.join("missing").exists());
            assert!(!native.join("write.lock").exists());
            assert!(!f.file("state").exists());
        }
    }
    assert_eq!(
        fs::read_to_string(native.join("migration-ledger.json"))?,
        "never interpreted as a ledger"
    );
    Ok(())
}

#[test]
fn migration_r6_retains_the_exact_audit_paths_and_the_native_link_text() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let agents = f.home.join(".agents");
    let state = f.home.join(".local/state");
    let lock = agents.join("locks");
    let branch = agents.join("bin");
    fs::create_dir(&branch)?;
    fs::create_dir(&lock)?;
    fs::create_dir(agents.join(".git"))?;
    fs::create_dir_all(&state)?;
    fs::write(
        agents.join(".git/index.lock"),
        "synthetic Native Git marker",
    )?;
    for path in [
        agents.join(".gitignore"),
        agents.join("README.zh-CN.md"),
        agents.join("SKILL-LEARNING-GUIDE.zh-CN.md"),
        agents.join("SKILL-USAGE-GUIDE.zh-CN.md"),
        agents.join(".DS_Store"),
        state.join(".DS_Store"),
        branch.join("agentflow-sync-skills"),
    ] {
        fs::write(path, "untouched")?;
    }
    let link_text = Path::new("fixture-dangling-native-owner");
    symlink(link_text, state.join("herdr"))?;
    let (out, preview) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    let retained = preview["data"]["retained"].as_array().ok_or("retained")?;
    for (owner, count) in [
        ("agent_repository_metadata", 5),
        ("system_metadata", 2),
        ("independent_agent_entry", 1),
        ("empty_legacy_lock_container", 1),
        ("native_state", 1),
    ] {
        assert_eq!(
            retained
                .iter()
                .filter(|item| item["owner"] == owner)
                .count(),
            count,
            "{owner}"
        );
    }
    assert!(
        retained
            .iter()
            .any(|item| item["path"] == json!(state.join("herdr"))
                && item["entry"] == json!({"kind":"symlink","text":link_text}))
    );

    assert_eq!(fs::read_link(state.join("herdr"))?, link_text);
    assert_eq!(
        fs::read_to_string(agents.join(".git/index.lock"))?,
        "synthetic Native Git marker"
    );
    Ok(())
}

#[test]
fn migration_keeps_history_roots_outside_settings_archive_scope() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let upstream = f.home.join(".agents/cache/upstream");
    let hosted = upstream.join("cursor-plugins");
    let child = hosted.join("history");
    fs::create_dir_all(&child)?;
    fs::write(child.join("item"), "preserved history")?;
    let alias = f.file("settings-alias");
    symlink(&hosted, &alias)?;

    for settings in [&upstream, &hosted, &child, &alias] {
        let (out, status) =
            migration_with_root(&f, "--settings", settings, &["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert_eq!(status["error"]["code"], "migration_input_changed");
        assert!(!f.file("state").exists());
        assert_eq!(bytes(&child.join("item"))?, b"preserved history");
    }
    Ok(())
}

#[test]
fn migration_preserves_only_known_legacy_exports_and_checkout_in_place() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let agents = f.home.join(".agents");
    let checkout = agents.join("cache/upstream/cursor-plugins");
    fs::create_dir_all(&checkout)?;
    fs::write(checkout.join("local-history"), "unchanged checkout")?;
    let entries = [
        ".agents/out/M29-20261020-001-REPORT.txt",
        ".agents/out/grok-bot-skills-SKILLSYNC-20261025-01.tar.gz",
        ".agents/bin/tests/test_skillctl.py",
        ".agents/bin/tests/__pycache__/test_skillctl.cpython-314.pyc",
        ".agents/bin/__pycache__/skillctlcpython-314.pyc",
    ];
    for relative in entries {
        let path = f.home.join(relative);
        fs::create_dir_all(path.parent().ok_or("entry parent")?)?;
        fs::write(path, "unchanged historical file")?;
    }

    let (out, status) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    let retained = status["data"]["retained"].as_array().ok_or("retained")?;
    let owner = |path: &Path, kind| {
        retained.iter().any(|row| {
            row["path"] == json!(path)
                && row["owner"] == "legacy_governance_history"
                && row["entry"]["kind"] == kind
        })
    };
    assert!(owner(&checkout, "directory"));
    for relative in entries {
        assert!(owner(&f.home.join(relative), "file"), "{relative}");
    }

    let wrong_path = agents.join("out/SKILLCTL-20261020-check.txt");
    fs::create_dir(&wrong_path)?;
    let (out, status) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(status["error"]["code"], "migration_preflight_blocked");
    assert!(!f.file("state").exists());
    fs::remove_dir(wrong_path)?;

    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let (out, _) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    assert_eq!(
        bytes(&checkout.join("local-history"))?,
        b"unchanged checkout"
    );
    for relative in entries {
        assert_eq!(bytes(&f.home.join(relative))?, b"unchanged historical file");
    }
    Ok(())
}

#[test]
fn migration_rechecks_legacy_writer_and_owner_sources_after_successful_preflight() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let mask = b"#!/bin/sh\nexit 75\n";
    let slots = [
        (".local/bin/mo", 0o700),
        (".local/bin/macops", 0o700),
        (".agents/bin/skillctl", 0o700),
        (
            ".local/share/chezmoi/home/dot_local/bin/executable_mo",
            0o600,
        ),
        (
            ".local/share/chezmoi/home/dot_local/bin/executable_macops",
            0o600,
        ),
        (
            ".local/share/chezmoi/home/dot_agents/bin/executable_skillctl",
            0o600,
        ),
    ];
    for (relative, mode) in slots {
        let path = f.home.join(relative);
        fs::create_dir_all(path.parent().ok_or("slot parent")?)?;
        fs::write(&path, mask)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
    }

    for (relative, _) in [slots[0], slots[4]] {
        let (out, status) = f.run(&["migrate", "preflight"])?;
        assert_code(&out, 0);
        assert_eq!(status["data"]["ready"], true);
        let restored = f.home.join(relative);
        fs::write(&restored, b"#!/bin/sh\nexit 0\n")?;
        let (out, status) = f.run(&["migrate", "run", "--yes"])?;
        assert_code(&out, 3);
        assert_eq!(status["error"]["code"], "migration_preflight_blocked");
        assert!(!f.file("state").exists());
        let (out, status) = f.run(&["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert_eq!(status["data"]["blocked"][0]["path"], json!(restored));
        assert_eq!(
            status["data"]["blocked"][0]["reason"],
            "migration_old_writer_active"
        );
        fs::write(restored, mask)?;
    }

    let (relative, mode) = slots[3];
    let source = f.home.join(relative);
    fs::remove_file(&source)?;
    let (out, status) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    assert_eq!(status["data"]["blocked"][0]["path"], json!(source));
    fs::write(&source, mask)?;
    fs::set_permissions(&source, fs::Permissions::from_mode(mode))?;

    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    for (relative, _) in slots {
        assert_eq!(bytes(&f.home.join(relative))?, mask);
        fs::write(f.home.join(relative), b"new verified launcher fixture\n")?;
    }
    let (out, status) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    assert_eq!(status["data"]["authority"], "migrated");
    Ok(())
}

#[test]
fn migration_r6_blocks_unsettled_locks_old_entries_unknown_paths_and_wrong_types() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let agents = f.home.join(".agents");
    let state = f.home.join(".local/state");
    let lock = agents.join("locks");
    let branch = agents.join("bin");
    fs::create_dir(&lock)?;
    fs::create_dir(&branch)?;
    fs::create_dir_all(&state)?;
    let skillctl = branch.join("skillctl");
    fs::write(&skillctl, "unretired writer")?;
    let unknown = branch.join("agentflow-sync-skills-other");
    fs::write(&unknown, "unknown sibling")?;
    let marker = agents.join("invocation-policy.pending.json");
    fs::write(&marker, "pending")?;
    let (out, preview) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    let blocked = preview["data"]["blocked"].as_array().ok_or("blocked")?;
    for path in [&skillctl, &unknown, &marker] {
        assert!(
            blocked.iter().any(|item| item["path"] == json!(path)),
            "{path:?}"
        );
    }
    assert!(
        !preview["data"]["retained"]
            .as_array()
            .ok_or("retained")?
            .iter()
            .any(|item| item["path"] == json!(lock))
    );
    for path in [&skillctl, &unknown, &marker] {
        fs::remove_file(path)?;
    }
    fs::write(lock.join("unknown.lock"), "unsettled")?;
    fs::write(state.join("mise"), "wrong native shape")?;
    symlink(f.file("state"), state.join("pnpm"))?;
    fs::create_dir(agents.join("cache/upstream"))?;
    fs::create_dir(agents.join("out"))?;
    fs::write(agents.join("cache/upstream/unknown"), "unbound source")?;
    fs::write(agents.join("out/grok-bot-skills-unbound.tar.gz"), "unknown")?;
    let backups = agents.join("backups");
    fs::create_dir(&backups)?;
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o755))?;
    let (out, preview) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    let blocked = preview["data"]["blocked"].as_array().ok_or("blocked")?;
    for path in [
        lock,
        state.join("mise"),
        state.join("pnpm"),
        agents.join("cache/upstream/unknown"),
        agents.join("out/grok-bot-skills-unbound.tar.gz"),
        backups.clone(),
    ] {
        assert!(
            blocked.iter().any(|item| item["path"] == json!(path)),
            "{path:?}: {preview}"
        );
    }
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 3);
    assert!(!f.file("state").exists());
    assert_eq!(fs::metadata(backups)?.permissions().mode() & 0o7777, 0o755);
    Ok(())
}

#[test]
fn migration_r6_reads_v1_and_rejects_forged_v2_without_falling_back() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let old = f.home.join(".agents/managed-assets.json");
    let original = br#"{ "odd_vendor": [false, {"more":null}], "legacy": 17 }"#;
    fs::write(&old, original)?;
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(fs::read(f.file("state/managed-assets.json"))?, original);
    let path = f.file("state/migration-ledger.json");
    let mut ledger = f.read_json("state/migration-ledger.json")?;
    ledger["schema_version"] = json!(1);
    write_json(&path, &ledger)?;
    let (out, verified) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    assert_eq!(verified["data"]["authority"], "migrated");

    for (version, owner, retained) in [
        (2, "native_state", ".local/state/macos-rebuild"),
        (2, "native_state", ".local/state/unknown"),
        (2, "unknown_owner", ".local/state/gh"),
        (1, "native_state", ".local/state/gh"),
        (3, "native_state", ".local/state/gh"),
    ] {
        ledger["schema_version"] = json!(version);
        ledger["retained"] = json!([{
            "path":f.home.join(retained), "owner":owner, "entry":{"kind":"directory"}
        }]);
        write_json(&path, &ledger)?;
        let before = fs::read(&path)?;
        let (out, response) = f.run(&["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert_eq!(response["error"]["code"], "invalid_migration_ledger");
        let (out, response) = f.run(&["skill"])?;
        assert_code(&out, 3);
        assert_eq!(response["error"]["code"], "invalid_migration_ledger");
        assert_eq!(fs::read(&path)?, before);
        assert_eq!(fs::read(&old)?, original);
        assert_eq!(fs::read(f.file("state/managed-assets.json"))?, original);
    }
    Ok(())
}

#[test]
fn migration_r6_managed_assets_checks_only_the_existing_macops_namespace() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let old = f.home.join(".agents/managed-assets.json");
    for damaged in [
        json!(null),
        json!({"schema_version":2,"roots":{}}),
        json!({"schema_version":1,"roots":{"member":{"bad":"shape"}}}),
    ] {
        write_json(
            &old,
            &json!({"unknown_vendor":[null,false,4],"_macops_sources":damaged}),
        )?;
        let (out, response) = f.run(&["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert!(
            response["data"]["blocked"]
                .as_array()
                .ok_or("blocked")?
                .iter()
                .any(|block| block["path"] == json!(old))
        );
        let (out, _) = f.run(&["migrate", "run", "--yes"])?;
        assert_code(&out, 3);
        assert!(!f.file("state").exists());
    }
    let original = br#"{ "unknown_vendor":[null,false,4], "_macops_sources":{"schema_version":1,"roots":{}} }"#;
    fs::write(&old, original)?;
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let target = f.file("state/managed-assets.json");
    assert_eq!(fs::read(&target)?, original);
    assert_eq!(fs::read(&old)?, original);
    write_json(
        &target,
        &json!({"unknown_vendor":[null,false,4],"_macops_sources":{"schema_version":0,"roots":{}}}),
    )?;
    let (out, response) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 3);
    assert_eq!(response["error"]["code"], "unverified_source_baseline");
    Ok(())
}

#[test]
fn migration_r6_recover_reports_absent_unknown_and_committed_ids() -> TestResult {
    let f = Fixture::new()?;
    let (out, v) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_not_committed");
    let (out, v) = f.run(&["migrate", "recover", "not-an-id", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_input_changed");
    let (out, v) = f.run(&["migrate", "recover", "OP-123-456", "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["result"], "not-needed");
    assert!(!f.file("state").exists());

    let (out, v) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let id = v["data"]["migration_id"].as_str().ok_or("id")?;
    let (out, v) = f.run(&["migrate", "recover", "OP-123-456", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_not_found");
    let (out, v) = f.run(&["migrate", "recover", id, "--yes"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["mode"], "verify");
    Ok(())
}

#[test]
fn migration_r6_old_root_alias_and_existing_operation_take_precedence_over_retention() -> TestResult
{
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let migrated_state = f.file("alt-old-state");
    fs::create_dir(&migrated_state)?;
    let alias = f.home.join(".local/state/macos-rebuild");
    fs::create_dir_all(alias.parent().ok_or("parent")?)?;
    symlink(&migrated_state, &alias)?;
    let nested = migrated_state.join("missing");
    let (out, v) = migration_with_root(&f, "--data-dir", &nested, &["migrate", "preflight"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_input_changed");
    assert!(!nested.exists());
    assert!(!f.file("state").exists());
    fs::remove_file(&alias)?;

    let locks = f.home.join(".agents/locks");
    fs::create_dir(&locks)?;
    fs::create_dir(f.file("state"))?;
    fs::set_permissions(f.file("state"), fs::Permissions::from_mode(0o700))?;
    fs::write(f.file("state/active.json"), "damaged control record")?;
    let (out, v) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    assert!(
        v["data"]["blocked"]
            .as_array()
            .ok_or("blocked")?
            .iter()
            .any(|item| item["path"] == json!(f.file("state/active.json"))
                && item["reason"] == "migration_existing_operation_pending")
    );
    assert!(
        !v["data"]["retained"]
            .as_array()
            .ok_or("retained")?
            .iter()
            .any(|item| item["path"] == json!(locks))
    );
    assert_eq!(
        fs::read_to_string(f.file("state/active.json"))?,
        "damaged control record"
    );
    assert!(!f.file("state/write.lock").exists());
    Ok(())
}

#[test]
fn migration_r6_archive_limits_distinguish_legacy_data_from_opaque_native_state() -> TestResult {
    use std::os::unix::fs::DirBuilderExt;

    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let native_roots = [
        f.home.join(".local/state/claude"),
        f.home.join(".local/state/billion-context"),
    ];
    let nested = std::iter::repeat_n("nested", 50).collect::<std::path::PathBuf>();
    for native in &native_roots {
        fs::create_dir_all(native.join(&nested))?;
    }
    let (out, v) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    let retained = v["data"]["retained"].as_array().ok_or("retained")?;
    for native in &native_roots {
        assert!(retained.iter().any(|item| item["path"] == json!(native)
            && item["owner"] == "native_state"
            && item["entry"] == json!({"kind":"directory"})));
    }

    let backups = f.home.join(".agents/backups");
    fs::DirBuilder::new().mode(0o700).create(&backups)?;
    fs::create_dir_all(backups.join(nested))?;
    let wrong_manifest = f.file("settings/manifests/agent-media.json");
    fs::create_dir(&wrong_manifest)?;
    let (out, v) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    let blocked = v["data"]["blocked"].as_array().ok_or("blocked")?;
    for (path, reason) in [
        (backups, "migration_archive_limit"),
        (f.file("settings"), "migration_unknown_manifest"),
    ] {
        assert!(
            blocked
                .iter()
                .any(|item| item["path"] == json!(path) && item["reason"] == reason)
        );
    }
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 3);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn migration_preserves_archived_bun_dependency_locks_and_blocks_legacy_writer_locks() -> TestResult
{
    use std::os::unix::fs::{DirBuilderExt, symlink};

    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let backups = f.home.join(".agents/backups");
    fs::DirBuilder::new().mode(0o700).create(&backups)?;
    let stable = "SKILLS-example/managed-assets/example/objects/0/payload";
    let payload = backups.join(stable);
    fs::create_dir_all(&payload)?;
    let config = payload.join("package.json");
    let lock = payload.join("bun.lock");
    let lock_bytes = b"{\n  \"lockfileVersion\": 1,\n  \"workspaces\": {},\n}\n";
    let require_blocked = || -> TestResult {
        let (out, report) = f.run(&["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert!(
            report["data"]["blocked"]
                .as_array()
                .ok_or("blocked")?
                .iter()
                .any(|item| item["path"] == json!(backups)
                    && item["reason"] == "migration_old_writer_pending")
        );
        assert!(!f.file("state/migration-ledger.json").exists());
        Ok(())
    };
    for relative in [
        "",
        "unknown/managed-assets/example/objects/0/payload",
        "SKILLS-example/foreign/example/objects/0/payload",
        "SKILLS-example/managed-assets/example/foreign/0/payload",
        "SKILLS-example/managed-assets/example/objects/unknown/payload",
    ] {
        let invalid = backups.join(relative);
        fs::create_dir_all(&invalid)?;
        fs::write(invalid.join("package.json"), b"{}")?;
        fs::write(invalid.join("bun.lock"), lock_bytes)?;
        require_blocked()?;
        fs::remove_file(invalid.join("bun.lock"))?;
    }
    fs::write(&lock, lock_bytes)?;
    require_blocked()?;
    fs::create_dir(&config)?;
    require_blocked()?;
    fs::remove_dir(&config)?;
    fs::write(&config, b"{}")?;
    for invalid in [
        b"pending writer".as_slice(),
        b"{\"other\": 1}",
        b"{\"lockfileVersion\":null}",
        b"{\"lockfileVersion\":1",
        b"{\"lockfileVersion\":10,}",
        b"{\"lockfileVersion\":\"1\",}",
    ] {
        fs::write(&lock, invalid)?;
        require_blocked()?;
    }
    fs::remove_file(&lock)?;
    symlink("package.json", &lock)?;
    require_blocked()?;
    fs::remove_file(&lock)?;
    fs::write(&lock, lock_bytes)?;
    let writer = payload.join("operation.lock");
    fs::write(&writer, b"pending writer")?;
    require_blocked()?;
    fs::remove_file(&writer)?;
    let rebuild = f.home.join(".local/state/macos-rebuild");
    fs::create_dir_all(rebuild.parent().ok_or("legacy state directory")?)?;
    fs::DirBuilder::new().mode(0o700).create(&rebuild)?;
    let outside_backups = rebuild.join(stable);
    fs::create_dir_all(&outside_backups)?;
    fs::write(outside_backups.join("package.json"), b"{}")?;
    fs::write(outside_backups.join("bun.lock"), lock_bytes)?;
    let (out, report) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    assert!(
        report["data"]["blocked"]
            .as_array()
            .ok_or("blocked")?
            .iter()
            .any(|item| item["path"] == json!(rebuild)
                && item["reason"] == "migration_old_writer_pending")
    );
    fs::remove_file(outside_backups.join("bun.lock"))?;

    let (out, report) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    assert_eq!(report["data"]["ready"], true);
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let (out, _) = f.run(&["migrate", "verify"])?;
    assert_code(&out, 0);
    assert_eq!(fs::read(lock)?, lock_bytes);
    Ok(())
}

#[test]
fn migration_r6_rejects_an_unowned_symlink_new_root_and_requires_confirmation_for_writes()
-> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let alternate = f.file("alternate-config");
    let symlink_root = f.file("alias-config");
    fs::create_dir(&alternate)?;
    symlink(&alternate, &symlink_root)?;
    let (out, v) = migration_with_root(
        &f,
        "--config-dir",
        &symlink_root,
        &["migrate", "run", "--yes"],
    )?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "migration_input_changed");
    assert_eq!(fs::read_dir(&alternate)?.count(), 0);

    let (out, v) = f.run(&["migrate", "run"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "confirmation_required");
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn migration_r6_refuses_to_read_a_native_state_tree_as_old_governance() -> TestResult {
    let f = Fixture::new()?;
    let native = f.home.join(".local/state/gh");
    let private = native.join("nested");
    fs::create_dir_all(&private)?;
    fs::write(private.join("opaque-fixture"), "synthetic owner bytes")?;

    for old_settings in [&native, &private] {
        let (out, value) =
            migration_with_root(&f, "--settings", old_settings, &["migrate", "preflight"])?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "migration_input_changed");
        assert!(value.get("data").is_none_or(serde_json::Value::is_null));
        assert!(!f.file("state").exists());
    }

    let settings_alias = f.file("legacy-settings-alias");
    symlink(&private, &settings_alias)?;
    let (out, value) =
        migration_with_root(&f, "--settings", &settings_alias, &["migrate", "preflight"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "migration_input_changed");
    assert!(value.get("data").is_none_or(serde_json::Value::is_null));

    symlink(&native, f.home.join(".agents"))?;
    fs::create_dir(f.file("state"))?;
    let invalid_ledger = f.file("state/migration-ledger.json");
    fs::write(&invalid_ledger, "unread guard canary")?;
    let (out, value) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "migration_input_changed");
    assert_eq!(fs::read_to_string(invalid_ledger)?, "unread guard canary");
    assert_eq!(
        fs::read_to_string(private.join("opaque-fixture"))?,
        "synthetic owner bytes"
    );
    assert!(!native.join("write.lock").exists());
    Ok(())
}

#[test]
fn migration_r6_blocks_missing_case_aliases_before_any_directory_creation() -> TestResult {
    let f = Fixture::new()?;
    let native_parent = f.home.join(".local/state");
    fs::create_dir_all(&native_parent)?;
    let folded = native_parent.join("GH");
    let physical_parent = Path::new("/System/Volumes/Data").join(native_parent.strip_prefix("/")?);
    let mut candidates = vec![folded.clone(), folded.join("separate")];
    if physical_parent.is_dir() {
        candidates.push(physical_parent.join("GH/separate"));
    }
    for candidate in candidates {
        for flag in ["--config-dir", "--data-dir", "--state-dir", "--cache-dir"] {
            for command in [
                vec!["migrate", "preflight"],
                vec!["migrate", "run", "--yes"],
            ] {
                let (out, value) = migration_with_root(&f, flag, &candidate, &command)?;
                assert_code(&out, 3);
                assert_eq!(value["error"]["code"], "migration_input_changed");
                assert!(fs::read_dir(&native_parent)?.next().is_none());
                assert!(!f.file("state").exists());
            }
        }
    }
    Ok(())
}

#[test]
fn migration_r6_blocks_native_symlinks_with_physical_or_unprovable_control_aliases() -> TestResult {
    let f = Fixture::new()?;
    let agents = f.home.join(".agents");
    fs::create_dir(&agents)?;
    let native = f.home.join(".local/state/gh");
    fs::create_dir_all(native.parent().ok_or("parent")?)?;
    symlink(".agents", f.home.join("peer-control"))?;
    symlink("missing", f.home.join("peer-dangling"))?;
    symlink("peer-loop", f.home.join("peer-loop"))?;
    let mut links = vec![
        f.home.join(".AGENTS"),
        f.home.join("peer-control"),
        f.home.join("peer-dangling/child"),
        f.home.join("peer-loop"),
        f.home.join("peer-control/absent"),
    ];
    let firmlink = Path::new("/System/Volumes/Data").join(agents.strip_prefix("/")?);
    if firmlink.is_dir() {
        links.push(firmlink);
    }
    for text in links {
        symlink(&text, &native)?;
        for command in [
            &["migrate", "preflight"][..],
            &["migrate", "run", "--yes"][..],
        ] {
            let (out, value) = f.run(command)?;
            assert_code(&out, 3);
            if command == ["migrate", "preflight"] {
                assert!(
                    value["data"]["blocked"]
                        .as_array()
                        .ok_or("blocked preflight")?
                        .iter()
                        .any(|entry| entry["path"] == json!(native)
                            && entry["reason"] == "migration_retained_entry_invalid")
                );
            } else {
                assert_eq!(value["error"]["code"], "migration_preflight_blocked");
            }
            assert!(!f.file("state").exists());
        }
        assert_eq!(fs::read_link(&native)?, text);
        fs::remove_file(&native)?;
    }

    symlink("safe-absent", &native)?;
    let (out, value) = f.run(&["migrate", "preflight"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["ready"], true);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn migration_r6_rechecks_frozen_link_evidence_even_if_the_native_symlink_is_gone() -> TestResult {
    let f = Fixture::new()?;
    migration_fixture(&f)?;
    let native = f.home.join(".local/state/gh");
    fs::create_dir_all(native.parent().ok_or("parent")?)?;
    let external = f.file("unrelated");
    fs::create_dir(&external)?;
    let peer = f.home.join("peer");
    symlink(&external, &peer)?;
    symlink("../../peer", &native)?;
    let (out, _) = f.run(&["migrate", "run", "--yes"])?;
    assert_code(&out, 0);
    let ledger = f.file("state/migration-ledger.json");
    let before = fs::read(&ledger)?;
    fs::remove_file(&native)?;
    fs::remove_file(&peer)?;
    symlink(".agents", &peer)?;
    for command in [vec!["migrate", "verify"], vec!["skill"]] {
        let (out, value) = f.run(&command)?;
        assert_code(&out, 3);
        assert_eq!(value["error"]["code"], "invalid_migration_ledger");
        assert_eq!(fs::read(&ledger)?, before);
    }
    Ok(())
}
