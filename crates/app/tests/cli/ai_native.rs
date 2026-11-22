use super::common::{Fixture, TestResult, assert_code, write_json};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn spec(tool: &str) -> &str {
    match tool {
        "codex" => "aqua:openai/codex",
        "claude" => "aqua:anthropics/claude-code",
        "cursor" => "http:cursor-agent",
        "kimi" => "npm:@moonshot-ai/kimi-code",
        "dsh" => "npm:@deepseek-ai/dsh",
        _ => "npm:@colbymchenry/codegraph",
    }
}
fn config(tool: &str, version: &str) -> String {
    format!(
        "# preserve\n[tools]\n\"{}\" = \"{version}\"\nnode = \"24.0.0\"\n",
        spec(tool)
    )
}
fn setup(tool: &str, live_version: &str) -> Result<Fixture, Box<dyn std::error::Error>> {
    let f = Fixture::new()?;
    fs::create_dir_all(f.home.join("payload"))?;
    let source = f
        .home
        .join(".local/share/chezmoi/dot_config/mise/private_config.toml");
    fs::create_dir_all(source.parent().ok_or("source")?)?;
    let live = f.home.join(".config/mise/config.toml");
    fs::create_dir_all(live.parent().ok_or("live")?)?;
    fs::write(source, config(tool, "1.0.0"))?;
    fs::write(live, config(tool, live_version))?;
    for profile in [
        ".claude",
        ".claude-mi",
        ".claude-kimi",
        ".claude-glm",
        ".claude-zen",
    ] {
        let dir = f.home.join(profile);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("settings.json"),
            format!(
                "{{ \"model\":\"custom-{profile}\",\"env\":{{\"API_KEY\":\"SYNTHETIC_CLAUDE_SECRET\"}} }}\n"
            ),
        )?;
        fs::write(
            dir.join("models.json"),
            format!("{{\"model\":\"distinct-{profile}\"}}\n"),
        )?;
    }
    let mise = f.script("mise", "exit 9")?;
    fs::write(
        &mise,
        r#"#!/usr/bin/python3
import sys,os,json,pathlib
h=pathlib.Path(os.environ['HOME']); a=sys.argv[1:]
if a[0] in ['exec','where'] and (os.environ.get('MISE_AUTO_INSTALL')!='0' or os.environ.get('MISE_EXEC_AUTO_INSTALL')!='0' or (a[0]=='exec' and '--no-deps' not in a)):
 (h/'implicit-install').write_text('attempted');sys.exit(9)
if a[0]=='where' and a[1].endswith('@2.0.0') and (h/'missing-tool').exists(): sys.exit(9)
if (h/'poison-cache').exists() and os.environ.get('CODEGRAPH_INSTALL_DIR')!='/dev/null/macops-codegraph-disabled':
 (h/'.codegraph/bundles/poison').write_text('TOUCHED'); sys.exit(9)
with (h/'mise-calls').open('a') as out: out.write(json.dumps([a,os.environ.get('CODEGRAPH_NO_DOWNLOAD'),os.environ.get('CODEGRAPH_NO_UPDATE_CHECK')])+'\n')
live=h/'.config/mise/config.toml'
if a[0]=='latest': print('2.0.0')
elif a[0]=='use':
 assert a[1]=='--path'
 live=pathlib.Path(a[2]); text=live.read_text(); spec,version=a[-1].rsplit('@',1)
 lines=text.splitlines(True)
 for i,line in enumerate(lines):
  if line.startswith('"'+spec+'"'): lines[i]='"'+spec+'" = "'+version+'"\n'
 live.write_text(''.join(lines))
 if (h/'change-other').exists(): live.write_text(live.read_text().replace('24.0.0','25.0.0'))
 if (h/'change-source').exists():
  source=h/'.local/share/chezmoi/dot_config/mise/private_config.toml'; source.write_text(source.read_text()+'# external\n')
 if (h/'installer-fail').exists(): sys.exit(9)
elif a[0]=='where':
 marker=h/'replace-upgrade-recipe'
 if marker.exists():
  catalog=h.parent/'catalog.json'; catalog.write_bytes(marker.read_bytes()); marker.unlink()
 print(h/'payload'/a[1].rsplit('@',1)[-1])
elif a[0]=='which':
 if a[2].endswith('@2.0.0') and (h/'outside-binary').exists(): print(h/'bin/host')
 else: print(h/'payload'/a[2].rsplit('@',1)[-1]/'bin'/a[3])
elif a[0]=='reshim':
 if (h/'reshim-fail').exists(): sys.exit(9)
else: sys.exit(8)
"#,
    )?;
    fs::set_permissions(&mise, fs::Permissions::from_mode(0o755))?;
    synthetic_runtimes(&f, live_version)?;
    Ok(f)
}
fn synthetic_runtimes(f: &Fixture, live_version: &str) -> TestResult {
    for version in ["1.0.0", "2.0.0", "9.0.0", live_version] {
        for binary in [
            "codex",
            "claude",
            "cursor-agent",
            "kimi",
            "dsh",
            "codegraph",
        ] {
            let program = f
                .home
                .join("payload")
                .join(version)
                .join("bin")
                .join(binary);
            file(&program)?;
            let script = r#"#!/usr/bin/python3
import os,pathlib,sys,json
h=pathlib.Path(os.environ['HOME']); version='VERSION'
with (h/'probe-calls').open('a') as out: out.write(json.dumps(sys.argv[1:])+'\n')
if (h/'poison-cache').exists() and os.environ.get('CODEGRAPH_INSTALL_DIR')!='/dev/null/macops-codegraph-disabled':
 (h/'.codegraph/bundles/poison').write_text('TOUCHED');sys.exit(9)
if version=='2.0.0':
 if (h/'replace-state').exists():
  state=h.parent/'state';state.rename(h.parent/'held-state');state.mkdir();sys.exit(9)
 live=h/'.config/mise/config.toml'
 if (h/'change-before-reconcile').exists(): live.write_text(live.read_text()+'# foreign before reconcile\n')
 if (h/'change-during-gate').exists(): live.write_text(live.read_text()+'# foreign during gate\n');sys.exit(9)
 if (h/'change-profile').exists(): (h/'.claude-mi/settings.json').write_text('{"model":"foreign"}')
 if (h/'version-fail').exists(): sys.exit(9)
print(version)
"#;
            fs::write(program, script.replace("VERSION", version))?;
        }
    }
    let node = f.home.join("payload/node/bin/node");
    file(&node)?;
    fs::write(
        node,
        r"#!/usr/bin/python3
import os,pathlib,sys,json
h=pathlib.Path(os.environ['HOME'])
(h/'node-calls').write_text(json.dumps(sys.argv[1:]))
if (h/'sharp-fail').exists():sys.exit(9)
if sys.argv[-1]=='--version':print('2.0.0')
",
    )?;
    Ok(())
}
fn calls(f: &Fixture) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(f.home.join("mise-calls"))?;
    Ok(text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()?)
}
fn pin(f: &Fixture, source: bool) -> Result<String, Box<dyn std::error::Error>> {
    Ok(fs::read_to_string(if source {
        f.home
            .join(".local/share/chezmoi/dot_config/mise/private_config.toml")
    } else {
        f.home.join(".config/mise/config.toml")
    })?)
}
fn payload(f: &Fixture, tool: &str) -> TestResult {
    let root = f.home.join("payload/2.0.0/lib/node_modules");
    if tool == "codegraph" {
        for leaf in [
            "@colbymchenry/codegraph/npm-shim.js",
            "@colbymchenry/codegraph-darwin-arm64/node",
            "@colbymchenry/codegraph-darwin-arm64/bin/codegraph",
        ] {
            file(&root.join(leaf))?;
        }
    } else {
        for leaf in [
            "@deepseek-ai/dsh/package.json",
            "@deepseek-ai/dsh/node_modules/sharp/package.json",
            "@deepseek-ai/dsh/node_modules/@img/sharp-darwin-arm64/lib/sharp-darwin-arm64.node",
            "@deepseek-ai/dsh/node_modules/@img/sharp-libvips-darwin-arm64/lib/libvips-cpp.8.dylib",
        ] {
            file(&root.join(leaf))?;
        }
    }
    Ok(())
}
fn file(path: &Path) -> TestResult {
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    fs::write(path, "fixture\n")?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}
fn interrupt_terminal(f: &Fixture, value: &Value) -> TestResult {
    let mut journal = value["data"]["journal"].clone();
    journal["phase"] = json!("unknown");
    write_json(&f.file("state/active.json"), &journal)?;
    write_json(
        &f.file("state/operations")
            .join(format!("{}.json", journal["id"].as_str().ok_or("id")?)),
        &journal,
    )?;
    Ok(())
}

fn claude_profile_bytes(
    f: &Fixture,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, Box<dyn std::error::Error>> {
    let mut bytes = BTreeMap::new();
    for profile in [
        ".claude",
        ".claude-mi",
        ".claude-kimi",
        ".claude-glm",
        ".claude-zen",
    ] {
        for leaf in ["settings.json", "models.json"] {
            let path = f.home.join(profile).join(leaf);
            bytes.insert(path.clone(), fs::read(path)?);
        }
    }
    Ok(bytes)
}

#[test]
fn ai_native_claude_recovery_leaves_user_model_edits_intact_when_resolving_failure() -> TestResult {
    let f = setup("claude", "1.0.0")?;
    fs::write(f.home.join("installer-fail"), b"fixture marker")?;
    let (out, started) = f.run(&["ai", "upgrade", "claude", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(started["data"]["journal"]["phase"], "unknown");
    let id = started["data"]["journal"]["id"]
        .as_str()
        .ok_or("operation ID")?;
    let original_profiles = claude_profile_bytes(&f)?;
    let model = f.home.join(".claude-zen/models.json");
    let edited =
        b"{ \"model\": \"custom-model-edited-by-user\", \"provider\": {\"extra\":true} }\n";
    fs::write(&model, edited)?;
    let mut expected_profiles = original_profiles;
    expected_profiles.insert(model, edited.to_vec());
    let initial_calls = calls(&f)?;
    assert_eq!(
        initial_calls
            .iter()
            .filter(|call| call[0][0] == "use")
            .count(),
        1
    );
    let plan = f.file(&format!("state/ai-{id}.json"));
    let plan_before = fs::read(&plan)?;
    assert_eq!(pin(&f, false)?, config("claude", "2.0.0"));
    assert_eq!(pin(&f, true)?, config("claude", "1.0.0"));

    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(recovered["data"]["journal"]["phase"], "unknown");
    assert_eq!(recovered["data"]["journal"]["id"], id);
    assert!(
        recovered["data"]["journal"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("claude_profile_changed"))
    );
    assert_eq!(claude_profile_bytes(&f)?, expected_profiles);

    let (out, failed) = f.run(&["recover", id, "--resolve-failed", "--yes"])?;
    assert_code(&out, 3);
    assert_eq!(failed["data"]["journal"]["phase"], "failed");
    assert_eq!(failed["data"]["journal"]["id"], id);
    assert_eq!(pin(&f, false)?, config("claude", "2.0.0"));
    assert_eq!(pin(&f, true)?, config("claude", "1.0.0"));
    assert_eq!(claude_profile_bytes(&f)?, expected_profiles);
    assert_eq!(calls(&f)?, initial_calls);
    assert_eq!(fs::read(plan)?, plan_before);
    assert_eq!(fs::read_dir(f.file("state/operations"))?.count(), 1);
    assert!(!failed.to_string().contains("SYNTHETIC_CLAUDE_SECRET"));
    Ok(())
}

#[test]
fn ai_native_claude_reshim_recovery_preserves_the_exact_prior_live_pin_and_profiles() -> TestResult
{
    let f = setup("claude", "7.0.0")?;
    let profiles = claude_profile_bytes(&f)?;
    let shims_failed = f.home.join("reshim-fail");
    fs::write(&shims_failed, b"fixture marker")?;
    fs::write(f.home.join("version-fail"), b"fixture marker")?;
    let (out, started) = f.run(&["ai", "upgrade", "claude", "2.0.0", "--force-live"])?;
    assert_code(&out, 3);
    assert_eq!(started["data"]["journal"]["phase"], "unknown");
    let id = started["data"]["journal"]["id"]
        .as_str()
        .ok_or("operation ID")?;
    let step: Value =
        serde_json::from_slice(&fs::read(f.file(&format!("state/ai-{id}-step.json")))?)?;
    assert_eq!(step, "reshim");
    assert_eq!(pin(&f, false)?, config("claude", "7.0.0"));
    assert_eq!(pin(&f, true)?, config("claude", "1.0.0"));
    let initial_calls = calls(&f)?;
    assert_eq!(
        initial_calls
            .iter()
            .filter(|call| call[0][0] == "use")
            .count(),
        1
    );
    let before_probe = fs::read(f.home.join("probe-calls"))?;
    fs::remove_file(shims_failed)?;

    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(recovered["data"]["journal"]["phase"], "failed");
    assert_eq!(recovered["data"]["journal"]["id"], id);
    assert_eq!(pin(&f, false)?, config("claude", "7.0.0"));
    assert_eq!(pin(&f, true)?, config("claude", "1.0.0"));
    assert_eq!(claude_profile_bytes(&f)?, profiles);
    let after = calls(&f)?;
    assert!(after.starts_with(&initial_calls));
    let latest = &after[initial_calls.len()..];
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0][0], json!(["reshim"]));
    assert_eq!(fs::read(f.home.join("probe-calls"))?, before_probe);
    assert_eq!(fs::read_dir(f.file("state/operations"))?.count(), 1);
    assert!(!f.home.join("implicit-install").exists());
    Ok(())
}

#[test]
fn ai_native_codex_reconciles_under_original_operation_and_recovers_source_commit_window()
-> TestResult {
    let f = setup("codex", "1.0.0")?;
    let (out, dry) = f.run(&["ai", "upgrade", "codex", "2.0.0", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(dry["data"]["backend"], "rust");
    assert!(!f.file("state").exists());
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    let (out, value) = f.run(&["app", "upgrade", "codex", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["journal"]["phase"], "succeeded");
    let id = value["data"]["journal"]["id"].as_str().ok_or("id")?;
    assert_eq!(pin(&f, false)?, config("codex", "2.0.0"));
    assert_eq!(pin(&f, true)?, pin(&f, false)?);
    assert_eq!(fs::read_dir(f.file("state/operations"))?.count(), 1);
    assert_eq!(
        fs::read(f.file("state/backups").join(id).join("before.bin"))?,
        config("codex", "1.0.0").as_bytes()
    );
    interrupt_terminal(&f, &value)?;
    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 0);
    assert_eq!(recovered["data"]["journal"]["phase"], "succeeded");
    assert_eq!(calls(&f)?.iter().filter(|v| v[0][0] == "use").count(), 1);
    Ok(())
}

#[test]
fn unified_ai_upgrade_freezes_the_legacy_plan_and_uses_the_same_recovery() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    let (old_out, old) = f.run(&["ai", "upgrade", "codex", "2.0.0", "--dry-run"])?;
    let (new_out, new) = f.run(&["upgrade", "ai:codex", "--version", "2.0.0", "--dry-run"])?;
    assert_code(&old_out, 0);
    assert_code(&new_out, 0);
    assert_eq!(new, old);
    assert!(!f.file("state").exists());
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    let (out, completed) = f.run(&["upgrade", "ai:codex", "--version", "2.0.0"])?;
    assert_code(&out, 0);
    let id = completed["data"]["journal"]["id"].as_str().ok_or("id")?;
    assert_eq!(completed["data"]["journal"]["phase"], "succeeded");
    assert_eq!(pin(&f, false)?, pin(&f, true)?);
    assert_eq!(pin(&f, true)?, config("codex", "2.0.0"));
    interrupt_terminal(&f, &completed)?;
    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 0);
    assert_eq!(recovered["data"]["journal"]["id"], id);
    assert_eq!(recovered["data"]["journal"]["phase"], "succeeded");
    assert_eq!(calls(&f)?.iter().filter(|v| v[0][0] == "use").count(), 1);
    Ok(())
}

#[test]
fn unified_ai_flags_do_not_switch_a_declared_recipe_to_another_adapter() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["id"] = json!("ai:codex");
    write_json(&f.catalog, &catalog)?;
    let (out, refusal) = f.run(&["upgrade", "ai:codex", "--version", "2.0.0", "--force-live"])?;
    assert_code(&out, 1);
    assert_eq!(refusal["error"]["code"], "force_live_target_unsupported");
    let (out, refusal) = f.run(&[
        "--chezmoi-source",
        f.home.to_str().ok_or("home")?,
        "upgrade",
        "ai:codex",
        "--version",
        "2.0.0",
    ])?;
    assert_code(&out, 1);
    assert_eq!(refusal["error"]["code"], "config_scope_unsupported");
    assert!(!f.file("state").exists());
    assert!(!f.file("calls").exists());

    assert_code(&f.run(&["install", "ai:codex", "--version", "1.0.0"])?.0, 0);
    assert_code(&f.run(&["upgrade", "ai:codex", "--version", "2.0.0"])?.0, 0);
    assert_eq!(fs::read(f.file("version"))?, b"2.0.0");
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    Ok(())
}

#[test]
fn unified_ai_upgrade_refuses_recipe_replacement_during_native_plan_probe() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    let mut catalog = f.read_json("catalog.json")?;
    catalog["packages"][0]["id"] = json!("ai:codex");
    write_json(&f.home.join("replace-upgrade-recipe"), &catalog)?;
    let (out, refused) = f.run(&["upgrade", "ai:codex", "--version", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(refused["error"]["code"], "plan_stale");
    assert_eq!(f.read_json("catalog.json")?, catalog);
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    assert_eq!(pin(&f, true)?, config("codex", "1.0.0"));
    assert!(!calls(&f)?.iter().any(|call| call[0][0] == "use"));
    assert!(!f.file("state/active.json").exists());
    Ok(())
}

#[test]
fn config_reconcile_lists_drift_without_the_legacy_ai_entry() -> TestResult {
    let f = setup("codex", "2.0.0")?;
    let (legacy, old) = f.run(&["ai", "reconcile"])?;
    let (current, new) = f.run(&["config", "reconcile"])?;
    assert_eq!(legacy.status.code(), current.status.code());
    assert_eq!(new, old);
    assert!(!f.file("state").exists());
    Ok(())
}

#[test]
fn ai_native_all_config_drift_blocks_before_mise_and_force_live_keeps_source() -> TestResult {
    for drift in [
        config("codex", "9.0.0"),
        config("codex", "1.0.0").replace("24.0.0", "25.0.0"),
        format!("{}# local\n", config("codex", "1.0.0")),
    ] {
        let f = setup("codex", "1.0.0")?;
        fs::write(f.home.join(".config/mise/config.toml"), &drift)?;
        let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0"])?;
        assert_code(&out, 3);
        assert_eq!(v["error"]["code"], "mise_config_drift");
        assert!(!f.home.join("mise-calls").exists());
        assert!(!f.file("state").exists());
        let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0", "--force-live"])?;
        assert_code(&out, 0);
        assert_eq!(v["data"]["journal"]["phase"], "succeeded");
        assert_eq!(pin(&f, true)?, config("codex", "1.0.0"));
    }
    Ok(())
}

#[test]
fn ai_native_payload_failure_returns_previous_live_pin_and_recovers_rollback_window() -> TestResult
{
    for tool in ["dsh", "codegraph"] {
        let f = setup(tool, "7.0.0")?;
        let (out, v) = f.run(&["ai", "upgrade", tool, "2.0.0", "--force-live"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["journal"]["phase"], "failed", "{v}");
        assert_eq!(pin(&f, false)?, config(tool, "7.0.0"));
        assert_eq!(pin(&f, true)?, config(tool, "1.0.0"));
        interrupt_terminal(&f, &v)?;
        let id = v["data"]["journal"]["id"].as_str().ok_or("id")?;
        write_json(
            &f.file(&format!("state/ai-{id}-step.json")),
            &json!("rollback"),
        )?;
        let (out, recovered) = f.run(&["recover", id])?;
        assert_code(&out, 3);
        assert_eq!(recovered["data"]["journal"]["phase"], "failed");
        assert_eq!(calls(&f)?.iter().filter(|v| v[0][0] == "use").count(), 1);
        assert!(calls(&f)?.iter().all(|v| v[1] == "1" && v[2] == "1"));
    }
    Ok(())
}

#[test]
fn ai_native_complete_payloads_and_claude_profiles_are_preserved() -> TestResult {
    for tool in ["claude", "cursor", "kimi", "dsh", "codegraph"] {
        let f = setup(tool, "1.0.0")?;
        if matches!(tool, "dsh" | "codegraph") {
            payload(&f, tool)?;
        }
        let mut before = Vec::new();
        for p in [
            ".claude",
            ".claude-mi",
            ".claude-kimi",
            ".claude-glm",
            ".claude-zen",
        ] {
            for leaf in ["settings.json", "models.json"] {
                let path = f.home.join(p).join(leaf);
                before.push((path.clone(), fs::read(path)?));
            }
        }
        let (out, v) = f.run(&["ai", "upgrade", tool, "2.0.0"])?;
        assert_code(&out, 0);
        assert_eq!(v["data"]["journal"]["phase"], "succeeded", "{v}");
        for (path, bytes) in before {
            assert_eq!(fs::read(path)?, bytes);
        }
        assert!(!v.to_string().contains("SYNTHETIC_CLAUDE_SECRET"));
        assert!(calls(&f)?.iter().all(|v| v[1] == "1" && v[2] == "1"));
        if tool == "dsh" {
            assert!(fs::read_to_string(f.home.join("node-calls"))?.contains("require("));
        }
    }
    Ok(())
}

#[test]
fn ai_native_foreign_config_changes_and_failed_reshim_stay_unknown() -> TestResult {
    for marker in ["change-other", "change-source", "reshim-fail"] {
        let f = setup("dsh", "1.0.0")?;
        fs::write(f.home.join(marker), "")?;
        let (out, v) = f.run(&["ai", "upgrade", "dsh", "2.0.0"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["journal"]["phase"], "unknown", "{marker}: {v}");
        let (out, _) = f.run(&["ai", "upgrade", "dsh", "2.0.0", "--force-live"])?;
        assert_code(&out, 3);
        if marker != "change-source" {
            assert_eq!(pin(&f, true)?, config("dsh", "1.0.0"));
        }
        assert_eq!(calls(&f)?.iter().filter(|v| v[0][0] == "use").count(), 1);
    }
    Ok(())
}

#[test]
fn ai_native_vendor_mimo_uses_operation_and_list_never_invokes_legacy_or_keychain() -> TestResult {
    let f = Fixture::new()?;
    f.script(
        "mimo",
        r#"case "$1" in
-v) if [ -f "$HOME/mimo-version" ]; then /bin/cat "$HOME/mimo-version"; else printf '1.0.0\n'; fi ;;
upgrade) printf '%s\n' "$@" > "$HOME/mimo-calls";printf '2.0.0\n' > "$HOME/mimo-version" ;;
esac"#,
    )?;
    for tool in [
        "codex",
        "claude",
        "cursor-agent",
        "kimi",
        "dsh",
        "codegraph",
    ] {
        f.script(tool, "printf '1.0.0\\n'")?;
    }
    fs::create_dir_all(f.home.join(".local/bin"))?;
    let bad = f.script("old-mo", "printf bad > \"$HOME/legacy-called\"; exit 9")?;
    fs::copy(&bad, f.home.join(".local/bin/mo"))?;
    f.script("security", "printf bad > \"$HOME/keychain-called\"; exit 9")?;
    let (out, list) = f.run(&["ai", "list"])?;
    assert_code(&out, 0);
    assert_eq!(list["data"]["backend"], "rust");
    assert_eq!(list["data"]["tools"].as_array().ok_or("tools")?.len(), 7);
    assert!(!f.file("state").exists());
    assert!(!f.home.join("legacy-called").exists());
    assert!(!f.home.join("keychain-called").exists());
    let (out, v) = f.run(&["ai", "upgrade", "mimo", "2.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["journal"]["phase"], "succeeded");
    assert_eq!(
        fs::read_to_string(f.home.join("mimo-calls"))?,
        "upgrade\n2.0.0\n--method\ncurl\n"
    );
    Ok(())
}

#[test]
fn ai_native_each_required_payload_component_and_real_probe_are_indispensable() -> TestResult {
    for (tool, leaf) in [
        ("dsh", "@deepseek-ai/dsh/node_modules/sharp/package.json"),
        (
            "dsh",
            "@deepseek-ai/dsh/node_modules/@img/sharp-darwin-arm64/lib/sharp-darwin-arm64.node",
        ),
        (
            "dsh",
            "@deepseek-ai/dsh/node_modules/@img/sharp-libvips-darwin-arm64/lib/libvips-cpp.8.dylib",
        ),
        ("codegraph", "@colbymchenry/codegraph/npm-shim.js"),
        ("codegraph", "@colbymchenry/codegraph-darwin-arm64/node"),
        (
            "codegraph",
            "@colbymchenry/codegraph-darwin-arm64/bin/codegraph",
        ),
    ] {
        let f = setup(tool, "1.0.0")?;
        payload(&f, tool)?;
        fs::remove_file(f.home.join("payload/2.0.0/lib/node_modules").join(leaf))?;
        let (out, v) = f.run(&["ai", "upgrade", tool, "2.0.0"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["journal"]["phase"], "failed", "{leaf}: {v}");
        assert_eq!(pin(&f, false)?, config(tool, "1.0.0"));
        assert_eq!(pin(&f, true)?, config(tool, "1.0.0"));
    }
    for (tool, marker) in [
        ("dsh", "sharp-fail"),
        ("dsh", "version-fail"),
        ("codegraph", "version-fail"),
    ] {
        let f = setup(tool, "1.0.0")?;
        payload(&f, tool)?;
        fs::write(f.home.join(marker), "")?;
        let (out, v) = f.run(&["ai", "upgrade", tool, "2.0.0"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["journal"]["phase"], "failed", "{marker}: {v}");
        assert_eq!(pin(&f, false)?, config(tool, "1.0.0"));
    }
    Ok(())
}

#[test]
fn ai_native_rollback_cas_rejects_gate_time_concurrent_edits_and_profile_drift() -> TestResult {
    for marker in ["change-during-gate", "change-profile"] {
        let f = setup("claude", "1.0.0")?;
        fs::write(f.home.join(marker), "")?;
        let (out, v) = f.run(&["ai", "upgrade", "claude", "2.0.0"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["journal"]["phase"], "unknown");
        assert_eq!(pin(&f, true)?, config("claude", "1.0.0"));
        if marker == "change-during-gate" {
            assert!(pin(&f, false)?.contains("2.0.0"));
            assert!(pin(&f, false)?.contains("foreign during gate"));
            assert!(!calls(&f)?.iter().any(|v| v[0][0] == "reshim"));
        } else {
            assert_eq!(pin(&f, false)?, config("claude", "1.0.0"));
            assert_eq!(
                fs::read_to_string(f.home.join(".claude-mi/settings.json"))?,
                "{\"model\":\"foreign\"}"
            );
        }
    }
    Ok(())
}

#[test]
fn ai_native_exact_required_tools_never_resolve_latest_and_other_tools_resolve_it() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    let (out, v) = f.run(&["ai", "upgrade", "codex"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["journal"]["plan"]["target"], "2.0.0");
    assert_eq!(calls(&f)?.iter().filter(|v| v[0][0] == "latest").count(), 1);
    for args in [
        vec!["ai", "upgrade", "dsh"],
        vec!["app", "upgrade", "codegraph"],
        vec!["ai", "upgrade", "codegraph", "2.0.0-rc.1"],
        vec!["ai", "upgrade", "dsh", "1.0"],
        vec!["ai", "upgrade", "other", "1.0.0"],
    ] {
        let f = setup("dsh", "1.0.0")?;
        let (out, _) = f.run(&args)?;
        assert_code(&out, if args.contains(&"other") { 2 } else { 1 });
        assert!(!f.home.join("mise-calls").exists());
        assert!(!f.file("state").exists());
    }
    Ok(())
}

#[test]
fn ai_native_vendor_unknown_result_queries_before_recovery_without_reinstalling() -> TestResult {
    let f = Fixture::new()?;
    f.script("mimo", r#"case "$1" in
-v) if [ -f "$HOME/mimo-version" ]; then /bin/cat "$HOME/mimo-version"; else printf '1.0.0\n'; fi ;;
upgrade) printf 'upgrade\n' >> "$HOME/mimo-calls";printf '2.0.0\n' > "$HOME/mimo-version";/bin/sleep 30 ;;
esac"#)?;
    let (out, v) = f.run(&["--timeout-secs", "1", "ai", "upgrade", "mimo", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["journal"]["phase"], "unknown");
    let id = v["data"]["journal"]["id"].as_str().ok_or("id")?;
    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 0);
    assert_eq!(recovered["data"]["journal"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(f.home.join("mimo-calls"))?, "upgrade\n");
    Ok(())
}

#[test]
fn ai_native_source_write_failure_keeps_new_verified_live_and_never_advances_source() -> TestResult
{
    let f = setup("codex", "1.0.0")?;
    fs::write(f.home.join("change-before-reconcile"), "")?;
    let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["journal"]["phase"], "unknown");
    assert_eq!(pin(&f, true)?, config("codex", "1.0.0"));
    assert!(pin(&f, false)?.contains("foreign before reconcile"));
    assert!(pin(&f, false)?.contains("2.0.0"));
    assert!(!calls(&f)?.iter().any(|v| v[0][0] == "reshim"));
    Ok(())
}

#[test]
fn ai_native_namespace_exchange_during_gate_cannot_publish_rollback_or_new_state() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    fs::write(f.home.join("replace-state"), "")?;
    let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["error"]["code"], "state_namespace_changed");
    assert_eq!(pin(&f, false)?, config("codex", "2.0.0"));
    assert_eq!(pin(&f, true)?, config("codex", "1.0.0"));
    assert_eq!(fs::read_dir(f.file("state"))?.count(), 0);
    assert!(f.file("held-state/active.json").exists());
    assert!(!calls(&f)?.iter().any(|v| v[0][0] == "reshim"));
    Ok(())
}

#[test]
fn ai_native_codegraph_existing_poisoned_cache_is_never_used_or_cleaned() -> TestResult {
    let f = setup("codegraph", "1.0.0")?;
    payload(&f, "codegraph")?;
    fs::create_dir_all(f.home.join(".codegraph/bundles"))?;
    fs::write(
        f.home.join(".codegraph/bundles/poison"),
        "KEEP EXISTING CACHE",
    )?;
    fs::write(f.home.join("poison-cache"), "")?;
    f.script("codegraph", r#"if [ "${CODEGRAPH_INSTALL_DIR:-}" != /dev/null/macops-codegraph-disabled ]; then printf TOUCHED > "$HOME/.codegraph/bundles/poison"; exit 9; fi
printf '1.0.0\n'"#)?;
    let out = f
        .command()
        .env("CODEGRAPH_INSTALL_DIR", f.home.join(".codegraph"))
        .args(["ai", "list"])
        .output()?;
    assert_eq!(
        fs::read_to_string(f.home.join(".codegraph/bundles/poison"))?,
        "KEEP EXISTING CACHE"
    );
    assert_code(&out, 0);
    let out = f
        .command()
        .env("CODEGRAPH_INSTALL_DIR", f.home.join(".codegraph"))
        .args(["ai", "upgrade", "codegraph", "2.0.0"])
        .output()?;
    assert_code(&out, 0);
    assert_eq!(
        fs::read_to_string(f.home.join(".codegraph/bundles/poison"))?,
        "KEEP EXISTING CACHE"
    );
    assert_eq!(fs::read_dir(f.home.join(".codegraph/bundles"))?.count(), 1);
    assert!(calls(&f)?.iter().any(|v| v[0][0] == "use"));
    assert!(calls(&f)?.iter().any(|v| v[0][0] == "where"));
    assert!(calls(&f)?.iter().any(|v| v[0][0] == "which"));
    Ok(())
}

#[test]
fn ai_native_frozen_explicit_destination_is_the_only_config_passed_to_mise() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    let destination = f.home.join("alternate");
    let target = destination.join(".config/mise/config.toml");
    fs::create_dir_all(target.parent().ok_or("target")?)?;
    fs::write(&target, config("codex", "1.0.0"))?;
    let out = f
        .command()
        .arg("--chezmoi-source")
        .arg(f.home.join(".local/share/chezmoi"))
        .arg("--chezmoi-destination")
        .arg(&destination)
        .args(["ai", "upgrade", "codex", "2.0.0"])
        .output()?;
    assert_code(&out, 0);
    let value: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(value["data"]["journal"]["phase"], "succeeded");
    assert_eq!(fs::read_to_string(&target)?, config("codex", "2.0.0"));
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    assert_eq!(pin(&f, true)?, config("codex", "2.0.0"));
    let call = calls(&f)?
        .into_iter()
        .find(|v| v[0][0] == "use")
        .ok_or("use")?;
    assert_eq!(call[0][1], "--path");
    assert_eq!(call[0][2], target.to_str().ok_or("utf8")?);
    Ok(())
}

#[test]
fn ai_native_missing_installed_version_cannot_implicitly_run_another_installer() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    fs::write(f.home.join("missing-tool"), "")?;
    let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["journal"]["phase"], "failed");
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    assert!(!f.home.join("implicit-install").exists());
    assert_eq!(calls(&f)?.iter().filter(|v| v[0][0] == "use").count(), 1);
    assert!(
        !calls(&f)?
            .iter()
            .any(|v| v[0][0] == "which" && v[0][2].as_str().is_some_and(|s| s.ends_with("@2.0.0")))
    );
    Ok(())
}

#[test]
fn ai_native_js_and_shell_probes_use_frozen_installed_node_without_host_fallback() -> TestResult {
    for tool in ["codegraph", "cursor"] {
        let f = setup(tool, "1.0.0")?;
        if tool == "codegraph" {
            payload(&f, tool)?;
        }
        f.script("node", "printf BAD > \"$HOME/host-node-called\";exit 9")?;
        let binary = f.home.join("payload/2.0.0/bin").join(if tool == "cursor" {
            "cursor-agent"
        } else {
            tool
        });
        fs::write(
            &binary,
            if tool == "cursor" {
                "#!/bin/sh\nnode --version\n"
            } else {
                "#!/usr/bin/env node\n// synthetic entry\n"
            },
        )?;
        let (out, v) = f.run(&["ai", "upgrade", tool, "2.0.0"])?;
        assert_code(&out, 0);
        assert_eq!(v["data"]["journal"]["phase"], "succeeded");
        assert!(!f.home.join("host-node-called").exists());
        assert!(fs::read_to_string(f.home.join("node-calls"))?.contains("--version"));
        assert!(!calls(&f)?.iter().any(|v| v[0][0] == "exec"));
    }
    Ok(())
}

#[test]
fn ai_native_mise_which_cannot_select_an_executable_outside_the_installed_version() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    f.script(
        "host",
        "printf BAD > \"$HOME/host-called\";printf '2.0.0\\n'",
    )?;
    fs::write(f.home.join("outside-binary"), "")?;
    let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(v["data"]["journal"]["phase"], "failed");
    assert!(!f.home.join("host-called").exists());
    assert_eq!(pin(&f, false)?, config("codex", "1.0.0"));
    assert_eq!(pin(&f, true)?, config("codex", "1.0.0"));
    Ok(())
}

#[test]
fn ai_native_recovery_marker_does_not_capture_an_unrelated_user_recipe_with_same_prefix()
-> TestResult {
    let f = Fixture::new()?;
    let mut catalog: Value = serde_json::from_slice(&fs::read(&f.catalog)?)?;
    catalog["packages"][0]["id"] = json!("ai-native:codex");
    write_json(&f.catalog, &catalog)?;
    let (out, v) = f.run(&["install", "ai-native:codex", "--version", "1.0.0"])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    let id = v["data"]["id"].as_str().ok_or("id")?;
    let (out, v) = f.run(&["recover", id])?;
    assert_code(&out, 0);
    assert_eq!(v["data"]["phase"], "succeeded");
    assert!(!f.file(&format!("state/ai-{id}.json")).exists());
    Ok(())
}

fn positional(value: &Value, fields: &[&str]) -> Value {
    Value::Array(fields.iter().map(|field| value[*field].clone()).collect())
}

#[test]
fn ai_native_private_record_objects_cannot_be_reinterpreted_as_positional_arrays() -> TestResult {
    let f = setup("codex", "1.0.0")?;
    let (out, v) = f.run(&["ai", "upgrade", "codex", "2.0.0"])?;
    assert_code(&out, 0);
    let id = v["data"]["journal"]["id"].as_str().ok_or("id")?;
    let record_path = f.file(&format!("state/ai-{id}.json"));
    let original: Value = serde_json::from_slice(&fs::read(&record_path)?)?;
    let before_active = fs::read(f.file("state/active.json"))?;
    let before_calls = calls(&f)?;
    let mut cases = Vec::new();
    cases.push(positional(
        &original,
        &[
            "isolated",
            "tool",
            "version",
            "force_live",
            "mise",
            "profiles",
        ],
    ));
    let mut case = original.clone();
    case["mise"] = positional(
        &original["mise"],
        &[
            "live",
            "source",
            "prev_live_pin",
            "expected_live",
            "expected_source",
            "root_digest",
            "roots",
        ],
    );
    cases.push(case);
    let mut case = original.clone();
    case["mise"]["live"] = positional(
        &original["mise"]["live"],
        &["anchor", "lineage", "identity", "mode", "text"],
    );
    cases.push(case);
    let mut case = original.clone();
    case["mise"]["roots"] = positional(
        &original["mise"]["roots"],
        &["source", "destination", "config", "env"],
    );
    cases.push(case);
    let mut case = original.clone();
    case["profiles"][0] = positional(&original["profiles"][0], &["path", "digest"]);
    cases.push(case);
    let mut case = original.clone();
    case["mise"]["live"]["anchor"] = positional(
        &original["mise"]["live"]["anchor"],
        &["requested", "canonical_parent", "parent_identity", "name"],
    );
    cases.push(case);
    for case in cases {
        write_json(&record_path, &case)?;
        let (out, rejected) = f.run(&["recover", id])?;
        assert_code(&out, 1);
        assert_eq!(rejected["error"]["code"], "invalid_json");
        assert_eq!(fs::read(f.file("state/active.json"))?, before_active);
        assert_eq!(pin(&f, false)?, config("codex", "2.0.0"));
        assert_eq!(pin(&f, true)?, config("codex", "2.0.0"));
        assert_eq!(calls(&f)?, before_calls);
    }
    write_json(&record_path, &original)?;
    let (out, _) = f.run(&["recover", id])?;
    assert_code(&out, 0);
    Ok(())
}

#[test]
fn ai_native_partial_or_mismatched_previous_pin_is_rejected_before_upgrade() -> TestResult {
    let f = setup("codex", "1.0")?;
    fs::write(
        f.home.join("payload/1.0/bin/codex"),
        "#!/bin/sh\nprintf '1.0.0\\n'\n",
    )?;
    let (out, rejected) = f.run(&["ai", "upgrade", "codex", "2.0.0", "--force-live"])?;
    assert_code(&out, 3);
    assert_eq!(rejected["error"]["code"], "ai_live_pin_not_exact");
    assert!(!f.file("state").exists());
    assert!(!calls(&f)?.iter().any(|call| call[0][0] == "use"));
    assert_eq!(pin(&f, false)?, config("codex", "1.0"));
    assert_eq!(pin(&f, true)?, config("codex", "1.0.0"));
    Ok(())
}

#[test]
fn ai_r2_force_live_success_requires_post_gate_full_config_and_identity_cas() -> TestResult {
    for mutation in ["target", "other", "live-inode", "source", "source-inode"] {
        let f = setup("codegraph", "1.0.0")?;
        payload(&f, "codegraph")?;
        let program = f.home.join("payload/2.0.0/bin/codegraph");
        let edit = match mutation {
            "target" => {
                "p=h/'.config/mise/config.toml';p.write_text(p.read_text().replace('2.0.0','9.0.0'))"
            }
            "other" => {
                "p=h/'.config/mise/config.toml';p.write_text(p.read_text().replace('24.0.0','25.0.0'))"
            }
            "live-inode" => {
                "p=h/'.config/mise/config.toml';q=p.with_name('replacement');q.write_text(p.read_text());q.replace(p)"
            }
            "source-inode" => {
                "p=h/'.local/share/chezmoi/dot_config/mise/private_config.toml';q=p.with_name('replacement');q.write_text(p.read_text());q.replace(p)"
            }
            _ => {
                "p=h/'.local/share/chezmoi/dot_config/mise/private_config.toml';p.write_text(p.read_text()+'# external source\\n')"
            }
        };
        let script = fs::read_to_string(&program)?;
        fs::write(
            &program,
            script.replace(
                "print(version)",
                &format!("if version=='2.0.0':\n {edit}\nprint(version)"),
            ),
        )?;
        let (out, v) = f.run(&["ai", "upgrade", "codegraph", "2.0.0", "--force-live"])?;
        assert_code(&out, 3);
        assert_eq!(v["data"]["journal"]["phase"], "unknown", "{mutation}: {v}");
        assert_eq!(
            pin(&f, false)?,
            if mutation == "target" {
                config("codegraph", "9.0.0")
            } else if mutation == "other" {
                config("codegraph", "2.0.0").replace("24.0.0", "25.0.0")
            } else {
                config("codegraph", "2.0.0")
            }
        );
        assert_eq!(
            pin(&f, true)?,
            if mutation == "source" {
                format!("{}# external source\n", config("codegraph", "1.0.0"))
            } else {
                config("codegraph", "1.0.0")
            }
        );
        assert_eq!(
            calls(&f)?.iter().filter(|call| call[0][0] == "use").count(),
            1
        );
        assert!(!calls(&f)?.iter().any(|call| call[0][0] == "reshim"));
    }
    Ok(())
}

fn publicly_pending_codegraph() -> Result<(Fixture, String), Box<dyn std::error::Error>> {
    let f = setup("codegraph", "1.0.0")?;
    payload(&f, "codegraph")?;
    fs::write(f.home.join("change-before-reconcile"), "")?;
    let (out, initial) = f.run(&["ai", "upgrade", "codegraph", "2.0.0"])?;
    assert_code(&out, 3);
    assert_eq!(initial["data"]["journal"]["phase"], "unknown");
    let id = initial["data"]["journal"]["id"]
        .as_str()
        .ok_or("id")?
        .to_owned();
    let step: Value =
        serde_json::from_slice(&fs::read(f.file(&format!("state/ai-{id}-step.json")))?)?;
    assert_eq!(step, "reconcile");
    assert_eq!(pin(&f, true)?, config("codegraph", "1.0.0"));
    fs::write(
        f.home.join(".config/mise/config.toml"),
        pin(&f, false)?.replace("# foreign before reconcile\n", ""),
    )?;
    fs::remove_file(f.home.join("change-before-reconcile"))?;
    Ok((f, id))
}

#[test]
fn ai_r2_public_reconcile_recovery_rechecks_complete_payload_before_source_publication()
-> TestResult {
    for damage in [
        "missing-install",
        "missing-bundle",
        "missing-binary",
        "wrong-version",
        "healthy",
    ] {
        let (f, id) = publicly_pending_codegraph()?;
        let root = f.home.join("payload/2.0.0");
        match damage {
            "missing-install" => fs::remove_dir_all(&root)?,
            "missing-bundle" => fs::remove_file(
                root.join("lib/node_modules/@colbymchenry/codegraph-darwin-arm64/bin/codegraph"),
            )?,
            "missing-binary" => fs::remove_file(root.join("bin/codegraph"))?,
            "wrong-version" => {
                let path = root.join("bin/codegraph");
                fs::write(
                    &path,
                    fs::read_to_string(&path)?.replace("version='2.0.0'", "version='3.0.0'"),
                )?;
            }
            _ => (),
        }
        let before_calls = calls(&f)?.len();
        let (out, recovered) = f.run(&["recover", &id])?;
        assert_code(&out, if damage == "healthy" { 0 } else { 3 });
        assert_eq!(
            recovered["data"]["journal"]["phase"],
            if damage == "healthy" {
                "succeeded"
            } else {
                "unknown"
            },
            "{damage}: {recovered}"
        );
        assert_eq!(
            pin(&f, true)?,
            config(
                "codegraph",
                if damage == "healthy" {
                    "2.0.0"
                } else {
                    "1.0.0"
                }
            )
        );
        assert_eq!(pin(&f, false)?, config("codegraph", "2.0.0"));
        let log = calls(&f)?;
        assert!(log.len() > before_calls);
        assert_eq!(log.iter().filter(|call| call[0][0] == "use").count(), 1);
        assert!(!log.iter().any(|call| call[0][0] == "exec"));
        assert!(!f.home.join("implicit-install").exists());
    }
    Ok(())
}

#[test]
fn ai_r2_source_publication_crash_recovery_cannot_accept_a_missing_target_payload() -> TestResult {
    let f = setup("codegraph", "1.0.0")?;
    payload(&f, "codegraph")?;
    let (out, initial) = f.run(&["ai", "upgrade", "codegraph", "2.0.0"])?;
    assert_code(&out, 0);
    interrupt_terminal(&f, &initial)?;
    let id = initial["data"]["journal"]["id"].as_str().ok_or("id")?;
    fs::remove_dir_all(f.home.join("payload/2.0.0"))?;
    let before_calls = calls(&f)?.len();
    let (out, recovered) = f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(recovered["data"]["journal"]["phase"], "unknown");
    assert_eq!(pin(&f, true)?, config("codegraph", "2.0.0"));
    assert!(calls(&f)?.len() > before_calls);
    assert_eq!(
        calls(&f)?.iter().filter(|call| call[0][0] == "use").count(),
        1
    );
    assert!(!f.home.join("implicit-install").exists());
    Ok(())
}

fn probe_edit(f: &Fixture, edit: &str) -> TestResult {
    let entry = f.home.join("payload/2.0.0/bin/codegraph");
    let script = fs::read_to_string(&entry)?;
    fs::write(
        entry,
        script.replace(
            "print(version)",
            &format!("if version=='2.0.0':\n {edit}\nprint(version)"),
        ),
    )?;
    Ok(())
}

#[test]
fn ai_r3_recovery_requires_payload_identity_and_content_to_survive_version_probe() -> TestResult {
    for edit in [
        "import shutil;shutil.rmtree(h/'payload/2.0.0')",
        "(h/'payload/2.0.0/lib/node_modules/@colbymchenry/codegraph-darwin-arm64/bin/codegraph').unlink()",
        "(h/'payload/2.0.0/bin/codegraph').unlink()",
        "p=h/'payload/2.0.0/lib/node_modules/@colbymchenry/codegraph-darwin-arm64/bin/codegraph';p.write_text(p.read_text()+'changed')",
        "p=h/'payload/2.0.0/bin/codegraph';p.write_text(p.read_text()+'# changed\\n')",
        "p=h/'payload/2.0.0/bin/codegraph';q=p.with_name('replacement');q.write_bytes(p.read_bytes());q.chmod(p.stat().st_mode);q.replace(p)",
        "import shutil;p=h/'payload/2.0.0';q=h/'held-install';p.rename(q);shutil.copytree(q,p)",
        "import shutil;p=h/'payload/2.0.0/lib/node_modules/@colbymchenry/codegraph-darwin-arm64';q=h/'held-bundle';p.rename(q);shutil.copytree(q,p)",
    ] {
        let (f, id) = publicly_pending_codegraph()?;
        probe_edit(&f, edit)?;
        let before = calls(&f)?.len();
        let (out, result) = f.run(&["recover", &id])?;
        assert_code(&out, 3);
        assert_eq!(
            result["data"]["journal"]["phase"], "unknown",
            "{edit}: {result}"
        );
        assert_eq!(pin(&f, true)?, config("codegraph", "1.0.0"));
        assert_eq!(pin(&f, false)?, config("codegraph", "2.0.0"));
        let log = calls(&f)?;
        assert!(log.len() > before);
        assert_eq!(log.iter().filter(|call| call[0][0] == "use").count(), 1);
        assert!(
            !log.iter()
                .any(|call| call[0][0] == "exec" || call[0][0] == "install")
        );
        assert!(!f.home.join("implicit-install").exists());
    }
    Ok(())
}

#[test]
fn ai_r3_force_live_does_not_accept_payload_removed_before_version_returns() -> TestResult {
    let f = setup("codegraph", "1.0.0")?;
    payload(&f, "codegraph")?;
    probe_edit(
        &f,
        "(h/'payload/2.0.0/lib/node_modules/@colbymchenry/codegraph-darwin-arm64/bin/codegraph').unlink()",
    )?;
    let (out, result) = f.run(&["ai", "upgrade", "codegraph", "2.0.0", "--force-live"])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["journal"]["phase"], "unknown");
    assert_eq!(pin(&f, true)?, config("codegraph", "1.0.0"));
    assert!(!calls(&f)?.iter().any(|call| call[0][0] == "reshim"));
    Ok(())
}

#[test]
fn ai_r3_js_probe_requires_its_frozen_interpreter_to_survive_unchanged() -> TestResult {
    let (f, id) = publicly_pending_codegraph()?;
    fs::write(
        f.home.join("payload/2.0.0/bin/codegraph"),
        "#!/usr/bin/env node\n// synthetic entry\n",
    )?;
    let node = f.home.join("payload/node/bin/node");
    let script = fs::read_to_string(&node)?;
    fs::write(&node, script.replace("if sys.argv[-1]=='--version':print('2.0.0')", "if sys.argv[-1]=='--version':\n p=pathlib.Path(sys.argv[0]);p.write_text(p.read_text()+'# changed\\n');print('2.0.0')"))?;
    let (out, result) = f.run(&["recover", &id])?;
    assert_code(&out, 3);
    assert_eq!(result["data"]["journal"]["phase"], "unknown");
    assert_eq!(pin(&f, true)?, config("codegraph", "1.0.0"));
    assert!(!f.home.join("host-node-called").exists());
    Ok(())
}

#[test]
fn ai_r3_healthy_payload_and_wrong_version_retain_public_recovery_behavior() -> TestResult {
    for healthy in [true, false] {
        let (f, id) = publicly_pending_codegraph()?;
        if !healthy {
            let path = f.home.join("payload/2.0.0/bin/codegraph");
            fs::write(
                &path,
                fs::read_to_string(&path)?.replace("version='2.0.0'", "version='3.0.0'"),
            )?;
        }
        let (out, result) = f.run(&["recover", &id])?;
        assert_code(&out, if healthy { 0 } else { 3 });
        assert_eq!(
            result["data"]["journal"]["phase"],
            if healthy { "succeeded" } else { "unknown" }
        );
        assert_eq!(
            pin(&f, true)?,
            config("codegraph", if healthy { "2.0.0" } else { "1.0.0" })
        );
        assert_eq!(
            calls(&f)?.iter().filter(|call| call[0][0] == "use").count(),
            1
        );
    }
    Ok(())
}
