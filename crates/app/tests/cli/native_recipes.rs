use super::*;
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

#[path = "native_recipe_batch.rs"]
mod batch_tests;
#[path = "native_recipe_history.rs"]
mod history_tests;

struct NativeFixture {
    f: Fixture,
    recipe: PathBuf,
}
impl NativeFixture {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let f = Fixture::new()?;
        let entry = f.file("runner/skills/bin/cli.mjs");
        fs::create_dir_all(entry.parent().ok_or("entry")?)?;
        fs::write(&entry, include_bytes!("../fixtures/native-skills/cli.py"))?;
        fs::write(
            f.file("runner/skills/package.json"),
            br#"{"name":"skills","version":"1.9.0"}"#,
        )?;
        let node = f.script(
            "frozen-node",
            r#"if [ "$1" = --version ];then printf 'v26.13.1\n';exit 0;fi
exec /usr/bin/python3 "$@""#,
        )?;
        let recipe = f.file("alpha-recipe.json");
        fs::write(
            &recipe,
            serde_json::to_vec(
                &json!({"schemaVersion":1,"id":"alpha","owner":"skills-cli","cliVersion":"1.9.0",
            "source":{"repository":"owner/repo","commit":"a".repeat(40),"skillDirectory":"skills/alpha","memberName":"alpha","trackingRef":null},
            "installName":"alpha","scope":"global","clientScope":["codex"],"copy":true,"manualOnly":true,"licenseFiles":[],"derived":null,
            "runner":{"node":node,"entry":entry}}),
            )?,
        )?;
        Ok(Self { f, recipe })
    }
    fn install(&self) -> Result<(std::process::Output, Value), Box<dyn std::error::Error>> {
        self.f.run(&[
            "skill",
            "install",
            "--recipe",
            self.recipe.to_str().ok_or("recipe")?,
        ])
    }
    fn calls(&self) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
        let path = self.f.home.join("native-calls.jsonl");
        if !path.exists() {
            return Ok(vec![]);
        }
        fs::read_to_string(path)?
            .lines()
            .map(|line| serde_json::from_str(line).map_err(Into::into))
            .collect()
    }
    fn mode(&self, mode: &str) -> TestResult {
        fs::write(self.f.home.join("native-mode"), mode)?;
        Ok(())
    }
}
#[test]
fn legacy_policy_preview_leaves_the_fixed_recipe_metadata_under_its_installation_owner()
-> TestResult {
    let n = NativeFixture::new()?;
    let mut recipe: Value = serde_json::from_slice(&fs::read(&n.recipe)?)?;
    recipe["manualOnly"] = json!(false);
    fs::write(&n.recipe, serde_json::to_vec(&recipe)?)?;
    let (out, value) = n.install()?;
    assert_code(&out, 0);
    let skill_id = value["data"]["journal"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("fixed skill ID")?;
    let receipt_path = n.f.file("state/receipts.json");
    let receipt_before = fs::read(&receipt_path)?;
    let receipts: Value = serde_json::from_slice(&receipt_before)?;
    assert_eq!(
        receipts["installations"][skill_id]["package"]["name"],
        "alpha"
    );
    let (out, baseline) = n.f.run(&["app", "doctor", skill_id])?;
    assert_code(&out, 0);
    assert_eq!(baseline["data"][0]["baseline"]["comparison"], "same");
    let skill = n.f.home.join(".agents/skills/alpha/SKILL.md");
    let codex_policy = n.f.home.join(".agents/skills/alpha/agents/openai.yaml");
    let installed = fs::read(&skill)?;
    assert!(!String::from_utf8_lossy(&installed).contains("disable-model-invocation: true"));
    assert!(!codex_policy.exists());
    write_json(
        &n.f.file("settings/manifests/agent-invocation-policy.json"),
        &json!({"schema_version":1,"standalone_manual_only":["alpha"]}),
    )?;
    let (out, preview) = n.f.run(&["--dry-run", "skill", "policy", "sync"])?;
    assert_code(&out, 0);
    assert_eq!(preview["data"]["plan"]["planned_changes"], 0, "{preview}");
    assert_eq!(fs::read(&skill)?, installed);
    assert!(!codex_policy.exists());
    assert_eq!(fs::read(&receipt_path)?, receipt_before);

    let other = n.f.home.join(".agents/skills/beta/SKILL.md");
    fs::create_dir_all(other.parent().ok_or("other skill")?)?;
    fs::write(&other, b"---\nname: beta\n---\nlegacy body\n")?;
    write_json(
        &n.f.file("settings/manifests/agent-invocation-policy.json"),
        &json!({"schema_version":1,"standalone_manual_only":["alpha","beta"]}),
    )?;
    let (out, synced) = n.f.run(&["--yes", "skill", "policy", "sync"])?;
    assert_code(&out, 0);
    assert_eq!(synced["data"]["changed"], 2);
    let operation = synced["data"]["operation_id"]
        .as_str()
        .ok_or("policy operation")?;
    let journal =
        n.f.read_json(&format!("state/operations/{operation}.json"))?;
    let manifest = journal["plan"]["package"]["provider"]["manifest"]
        .as_str()
        .ok_or("policy manifest")?;
    let proof: Value = serde_json::from_slice(&fs::read(manifest)?)?;
    assert_eq!(proof["schema_version"], 3);
    assert_eq!(
        proof["inputs"]
            .as_array()
            .ok_or("policy inputs")?
            .iter()
            .filter(|row| {
                row["path"] == json!(n.f.home.join(".config/macops/native-skills-recipes.json"))
            })
            .count(),
        1
    );
    assert_eq!(fs::read(&skill)?, installed);
    assert!(!codex_policy.exists());
    assert!(fs::read_to_string(other)?.contains("disable-model-invocation: true"));
    assert_eq!(fs::read(&receipt_path)?, receipt_before);
    let (out, baseline) = n.f.run(&["app", "doctor", skill_id])?;
    assert_code(&out, 0);
    assert_eq!(baseline["data"][0]["baseline"]["comparison"], "same");
    assert_eq!(fs::read(receipt_path)?, receipt_before);
    assert_eq!(n.calls()?.len(), 1);
    Ok(())
}

#[test]
fn unconfigured_native_skills_keep_readonly_aliases_without_claiming_update_evidence() -> TestResult
{
    let f = Fixture::new()?;
    native_skill_fixture(&f, &["alpha", "beta"])?;
    let lock_path = f.home.join(".agents/.skill-lock.json");
    let original = fs::read(&lock_path)?;
    let (out, all) = f.run(&["skill", "outdated"])?;
    assert_code(&out, 3);
    let rows = all["data"]["results"]
        .as_array()
        .ok_or("installed skills")?;
    assert_eq!(rows.len(), 2);
    let alpha = rows
        .iter()
        .find(|row| row["native_id"] == "alpha")
        .ok_or("alpha")?;
    let id = alpha["id"].as_str().ok_or("alpha ID")?;
    for key in ["alpha", id, "skill:alpha"] {
        let (out, selected) = f.run(&["skill", "outdated", "--only", key])?;
        assert_code(&out, 3);
        assert_eq!(selected["data"]["results"], json!([alpha]));
        assert_eq!(selected["data"]["results"][0]["reason"], "recipe_required");
        assert_eq!(selected["data"]["networkInvoked"], false);
    }
    let (out, source) = f.run(&["skill", "outdated", "source:owner/repo"])?;
    assert_code(&out, 3);
    assert_eq!(source["data"]["results"], all["data"]["results"]);
    assert!(!f.file("state").exists());
    assert!(!f.file("cache").exists());
    assert_eq!(fs::read(lock_path)?, original);
    Ok(())
}

#[test]
fn configured_native_source_does_not_hide_an_unconfigured_source() -> TestResult {
    let n = NativeFixture::new()?;
    let (out, _) = n.install()?;
    assert_code(&out, 0);
    let lock = n.f.home.join(".agents/.skill-lock.json");
    let mut installed: Value = serde_json::from_slice(&fs::read(&lock)?)?;
    let mut unconfigured = installed["skills"]["alpha"].clone();
    unconfigured["source"] = json!("other/repo");
    unconfigured["sourceUrl"] = json!("https://github.com/other/repo");
    unconfigured["skillPath"] = json!("skills/beta");
    installed["skills"]["beta"] = unconfigured;
    write_json(&lock, &installed)?;
    let beta = n.f.home.join(".agents/skills/beta");
    fs::create_dir_all(&beta)?;
    fs::write(beta.join("SKILL.md"), "---\nname: beta\n---\nfixture\n")?;
    let receipt = n.f.file("state/receipts.json");
    let receipts = fs::read(&receipt)?;
    let raw_lock = fs::read(&lock)?;

    let (out, original) =
        n.f.run(&["skill", "upgrade", "source:owner/repo", "--dry-run"])?;
    assert_code(&out, 0);
    assert_eq!(
        original["data"]["results"][0]["recipe"]["installName"],
        "alpha"
    );
    let (out, other) =
        n.f.run(&["skill", "upgrade", "source:other/repo", "--dry-run"])?;
    assert_code(&out, 3);
    assert_eq!(other["error"]["code"], "skills_cli_unavailable");
    assert_eq!(n.calls()?.len(), 1);
    assert_eq!(fs::read(receipt)?, receipts);
    assert_eq!(fs::read(lock)?, raw_lock);
    Ok(())
}

#[test]
fn native_recipe_default_write_exact_reinstall_and_remove_keep_one_owner_and_fixed_manual_metadata()
-> TestResult {
    let n = NativeFixture::new()?;
    let (out, value) = n.install()?;
    assert_code(&out, 0);
    assert_eq!(value["data"]["deliveryStage"], "verified");
    assert_eq!(n.calls()?.len(), 1);
    let root = n.f.home.join(".agents/skills/alpha");
    assert!(fs::read_to_string(root.join("SKILL.md"))?.contains("disable-model-invocation: true"));
    assert!(
        fs::read_to_string(root.join("agents/openai.yaml"))?
            .contains("allow_implicit_invocation: false")
    );
    let id = value["data"]["journal"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("id")?;
    let raw = fs::read(n.f.home.join(".agents/.skill-lock.json"))?;
    let native: Value = serde_json::from_slice(&raw)?;
    assert_eq!(native["skills"]["alpha"]["skillFolderHash"], "b".repeat(40));
    assert_eq!(native["skills"]["alpha"]["ref"], "a".repeat(40));
    let (out, info) = n.f.run(&["skill", id])?;
    assert_code(&out, 0);
    assert_eq!(info["data"]["delivery"]["stage"], "verified");
    assert_eq!(fs::read(n.f.home.join(".agents/.skill-lock.json"))?, raw);
    let (out, upgraded) = n.f.run(&["skill", "upgrade", id])?;
    assert_code(&out, 0);
    assert_eq!(
        upgraded["data"]["results"][0]["journal"]["plan"]["package"]["id"],
        id
    );
    assert_eq!(n.calls()?.len(), 2);
    assert_eq!(
        n.f.read_json("state/receipts.json")?["installations"]
            .as_object()
            .ok_or("owners")?
            .len(),
        1
    );
    let (out, removed) = n.f.run(&["--yes", "skill", "remove", id])?;
    assert_code(&out, 0);
    assert_eq!(removed["data"]["deliveryStage"], "verified");
    assert!(!root.exists());
    assert_eq!(n.calls()?.len(), 3);
    assert!(
        n.f.read_json("state/receipts.json")?["installations"]
            .as_object()
            .ok_or("owners")?
            .is_empty()
    );
    Ok(())
}
#[test]
fn native_partial_result_and_recovery_do_not_rerun_or_customize_native_payload() -> TestResult {
    let n = NativeFixture::new()?;
    n.mode("partial")?;
    let (out, value) = n.install()?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["deliveryStage"], "native-unknown");
    let id = value["data"]["journal"]["id"].as_str().ok_or("operation")?;
    assert_eq!(n.calls()?.len(), 1);
    let skill = n.f.home.join(".agents/skills/alpha/SKILL.md");
    assert!(!fs::read_to_string(skill)?.contains("disable-model-invocation"));
    let (out, recovered) = n.f.run(&["recover", id])?;
    assert_code(&out, 3);
    assert_eq!(recovered["data"]["deliveryStage"], "native-unknown");
    assert_eq!(n.calls()?.len(), 1);
    let (out, info) = n.f.run(&["skill", "alpha"])?;
    assert_code(&out, 0);
    assert_eq!(info["data"]["delivery"]["stage"], "native-unknown");
    assert_eq!(info["data"]["recipe"]["id"], "alpha");
    assert!(!n.f.file("state/receipts.json").exists());
    Ok(())
}
#[test]
fn native_unselected_client_different_skill_change_cannot_be_verified() -> TestResult {
    let n = NativeFixture::new()?;
    let foreign = n.f.home.join(".claude-mi/skills/unselected");
    fs::create_dir_all(&foreign)?;
    fs::write(foreign.join("SKILL.md"), b"original")?;
    n.mode("corrupt-client")?;
    let (out, value) = n.install()?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["deliveryStage"], "native-unknown");
    assert_eq!(n.calls()?.len(), 1);
    assert!(!n.f.file("state/receipts.json").exists());
    Ok(())
}
#[test]
fn native_invalid_registry_and_foreign_target_refuse_before_mutator() -> TestResult {
    for version in [2, 4] {
        let n = NativeFixture::new()?;
        fs::create_dir_all(n.f.home.join(".agents"))?;
        fs::write(
            n.f.home.join(".agents/.skill-lock.json"),
            serde_json::to_vec(&json!({"version":version,"skills":{}}))?,
        )?;
        let (out, _) = n.install()?;
        assert_code(&out, 3);
        assert_eq!(n.calls()?.len(), 0);
    }
    let n = NativeFixture::new()?;
    let root = n.f.home.join(".agents/skills/alpha");
    fs::create_dir_all(&root)?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755))?;
    fs::write(root.join("SKILL.md"), b"foreign")?;
    let (out, value) = n.install()?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "adoption_required");
    assert_eq!(n.calls()?.len(), 0);
    assert_eq!(fs::read(root.join("SKILL.md"))?, b"foreign");
    Ok(())
}

#[test]
fn native_unselected_registry_row_change_cannot_be_verified_or_repaired_by_macops() -> TestResult {
    let n = NativeFixture::new()?;
    let other = n.f.home.join(".agents/skills/other");
    fs::create_dir_all(&other)?;
    fs::write(other.join("SKILL.md"), b"foreign root")?;
    let row = json!({"source":"other/repo","sourceType":"github","sourceUrl":"https://github.com/other/repo.git","commit":"c".repeat(40),"ref":"c".repeat(40),"skillPath":"skills/other/SKILL.md","skillFolderHash":"d".repeat(40),"installedAt":"2026-11-20T00:00:00.000Z","updatedAt":"2026-11-20T00:00:00.000Z"});
    fs::write(
        n.f.home.join(".agents/.skill-lock.json"),
        serde_json::to_vec(&json!({"version":3,"skills":{"other":row}}))?,
    )?;
    n.mode("corrupt-row")?;
    let (out, value) = n.install()?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["deliveryStage"], "native-unknown");
    assert_eq!(n.calls()?.len(), 1);
    let after: Value =
        serde_json::from_slice(&fs::read(n.f.home.join(".agents/.skill-lock.json"))?)?;
    assert_eq!(after["skills"]["other"]["source"], "foreign/change");
    assert_eq!(fs::read(other.join("SKILL.md"))?, b"foreign root");
    Ok(())
}
#[test]
fn selected_native_mutator_receives_only_fixed_global_copy_codex_member_arguments() -> TestResult {
    let n = NativeFixture::new()?;
    let (out, _) = n.install()?;
    assert_code(&out, 0);
    let calls = n.calls()?;
    let args = calls[0]["args"].as_array().ok_or("args")?;
    assert_eq!(args[0], "add");
    for flag in ["--global", "--yes", "--copy", "--json"] {
        assert_eq!(args.iter().filter(|v| v.as_str() == Some(flag)).count(), 1);
    }
    let agent = args.iter().position(|v| v == "--agent").ok_or("agent")?;
    assert_eq!(args[agent + 1], "codex");
    let member = args.iter().position(|v| v == "--skill").ok_or("member")?;
    assert_eq!(args[member + 1], "alpha");
    assert!(
        !args
            .iter()
            .any(|v| matches!(v.as_str(), Some("--all" | "--link" | "--symlink")))
    );
    Ok(())
}

#[test]
fn explicit_native_maintenance_never_parses_unrelated_plugin_settings_or_cache() -> TestResult {
    let n = NativeFixture::new()?;
    let profile = n.f.home.join(".claude-mi");
    fs::create_dir_all(profile.join("plugins"))?;
    fs::write(
        profile.join("settings.json"),
        b"invalid JSON SYNTHETIC_UNRELATED_SECRET",
    )?;
    fs::write(
        profile.join("plugins/installed_plugins.json"),
        b"invalid JSON",
    )?;
    let (out, _) = n.install()?;
    assert_code(&out, 0);
    let (out, _) = n.f.run(&["skill", "upgrade", "alpha"])?;
    assert_code(&out, 0);
    assert_eq!(n.calls()?.len(), 2);
    let (out, _) = n.f.run(&["skill", "upgrade", "--all"])?;
    assert_code(&out, 3);
    assert_eq!(n.calls()?.len(), 2);
    assert_eq!(
        fs::read(profile.join("settings.json"))?,
        b"invalid JSON SYNTHETIC_UNRELATED_SECRET"
    );
    Ok(())
}

#[test]
fn native_success_with_unsupported_metadata_remains_customization_pending_and_info_uses_original_operation()
-> TestResult {
    let n = NativeFixture::new()?;
    n.mode("bad-yaml")?;
    let (out, value) = n.install()?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["deliveryStage"], "customization-pending");
    let operation = value["data"]["journal"]["id"].as_str().ok_or("operation")?;
    let (out, info) = n.f.run(&["skill", "alpha"])?;
    assert_code(&out, 0);
    assert_eq!(info["data"]["delivery"]["stage"], "customization-pending");
    assert_eq!(info["data"]["delivery"]["operationId"], operation);
    let (out, recovered) = n.f.run(&["recover", operation])?;
    assert_code(&out, 3);
    assert_eq!(recovered["data"]["deliveryStage"], "customization-pending");
    assert_eq!(n.calls()?.len(), 1);
    assert!(!n.f.file("state/receipts.json").exists());
    Ok(())
}

#[test]
fn compiled_outdated_uses_the_fixed_recipe_locator_without_starting_native_or_refreshing_cache()
-> TestResult {
    let n = NativeFixture::new()?;
    let (out, _) = n.install()?;
    assert_code(&out, 0);
    let calls = n.calls()?;
    let native = fs::read(n.f.home.join(".agents/.skill-lock.json"))?;
    let (out, value) = n.f.run(&["skill", "outdated", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["resources"][0]["status"], "unknown");
    assert_eq!(
        value["data"]["resources"][0]["reason"],
        "index_missing_or_stale"
    );
    assert_eq!(n.calls()?, calls);
    assert_eq!(fs::read(n.f.home.join(".agents/.skill-lock.json"))?, native);
    assert!(!n.f.file("cache/native-skills-checks.json").exists());
    Ok(())
}

#[test]
fn central_remove_refuses_an_unregistered_client_reference_without_starting_native() -> TestResult {
    let n = NativeFixture::new()?;
    let (out, _) = n.install()?;
    assert_code(&out, 0);
    let canonical = n.f.home.join(".agents/skills/alpha");
    let parent = n.f.home.join(".claude-unregistered/skills");
    fs::create_dir_all(&parent)?;
    std::os::unix::fs::symlink(&canonical, parent.join("alpha"))?;
    let raw = fs::read(n.f.home.join(".agents/.skill-lock.json"))?;
    let (out, value) = n.f.run(&["--yes", "skill", "remove", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "native_skills_shared_installation");
    assert_eq!(n.calls()?.len(), 1);
    assert!(canonical.is_dir());
    assert_eq!(fs::read(n.f.home.join(".agents/.skill-lock.json"))?, raw);
    Ok(())
}
#[test]
fn fixed_runner_drift_never_installs_a_new_runner_or_starts_the_old_native_command() -> TestResult {
    let n = NativeFixture::new()?;
    let (out, _) = n.install()?;
    assert_code(&out, 0);
    let entry = n.f.file("runner/skills/bin/cli.mjs");
    let mut bytes = fs::read(&entry)?;
    bytes.extend_from_slice(b"\n# changed runner\n");
    fs::write(&entry, &bytes)?;
    let (out, value) = n.f.run(&["skill", "upgrade", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(value["error"]["code"], "native_skills_runner_changed");
    assert_eq!(n.calls()?.len(), 1);
    assert_eq!(fs::read(&entry)?, bytes);
    Ok(())
}

#[test]
fn compiled_schema3_malformed_commit_or_skill_path_refuses_before_native_and_preserves_raw_lock()
-> TestResult {
    for (field, bad) in [("commit", "not-a-commit"), ("skillPath", "../escape")] {
        let n = NativeFixture::new()?;
        fs::create_dir_all(n.f.home.join(".agents"))?;
        let mut row = json!({"source":"other/repo","sourceType":"github","sourceUrl":"https://github.com/other/repo.git","commit":"c".repeat(40),"ref":"c".repeat(40),"skillPath":"skills/other/SKILL.md","skillFolderHash":"d".repeat(40),"installedAt":"2026-11-20T00:00:00.000Z","updatedAt":"2026-11-20T00:00:00.000Z"});
        row[field] = json!(bad);
        let raw = serde_json::to_vec(&json!({"version":3,"skills":{"other":row}}))?;
        let lock = n.f.home.join(".agents/.skill-lock.json");
        fs::write(&lock, &raw)?;
        let (out, _) = n.install()?;
        assert_code(&out, 3);
        assert_eq!(n.calls()?.len(), 0);
        assert_eq!(fs::read(&lock)?, raw);
        assert!(!n.f.file("state").exists());
    }
    Ok(())
}
