#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use serde_json::json;
use std::fs;

fn test_context(home: &Path) -> Result<Context> {
    let mut warnings = Vec::new();
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(|| Error::new("utf8", "home"))?,
        "list",
    ])
    .map_err(|e| Error::new("parse", e.to_string()))?;
    Context::from_cli(&cli, &mut warnings)
}

#[test]
fn anchored_skill_and_plugin_observations_ignore_leases_and_reject_unsafe_payloads() -> TestResult {
    use std::os::unix::fs::symlink;
    let temp = tempfile::Builder::new()
        .prefix("macops-cap-unit-")
        .tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let skill_dir = home.join(".agents/skills/demo/scripts");
    fs::create_dir_all(&skill_dir)?;
    fs::write(
        home.join(".agents/skills/demo/SKILL.md"),
        "---\nname: demo\n---\nSYNTHETIC_SECRET_SKILL\n",
    )?;
    fs::write(skill_dir.join("run.sh"), "#!/bin/sh\necho ok\n")?;
    fs::write(
        home.join(".agents/.skill-lock.json"),
        serde_json::to_vec(&json!({
            "version": 3,
            "skills": {
                "demo": {
                    "source": "owner/repo",
                    "sourceType": "github",
                    "ref": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "skillFolderHash": "tree-1"
                }
            }
        }))?,
    )?;
    let ctx = test_context(&home)?;
    let pkg = resolve_package(&ctx, "skill:demo", false, false)?.ok_or("skill resolved")?;
    assert!(resolve_package(&ctx, "skill:demo", true, false).is_err());
    assert!(resolve_package(&ctx, "other:key", false, false)?.is_none());
    let query = capture(&pkg)?;
    let obs1 = observe_default(&pkg.id, &query)?;
    assert!(!serde_json::to_string(&obs1)?.contains("SYNTHETIC_SECRET"));

    fs::write(home.join(".agents/skills/demo/.in_use"), "pid=123")?;
    fs::write(home.join(".agents/skills/demo/.DS_Store"), "finder")?;
    let obs_lease = observe_default(&pkg.id, &query)?;
    assert_eq!(obs1, obs_lease);
    fs::remove_file(home.join(".agents/skills/demo/.in_use"))?;

    symlink("/bin/sh", home.join(".agents/skills/demo/.in_use"))?;
    assert_eq!(
        observe_default(&pkg.id, &query)
            .err()
            .ok_or("symlink lease")?
            .code,
        "unsupported_capability_payload"
    );
    fs::remove_file(home.join(".agents/skills/demo/.in_use"))?;

    fs::create_dir_all(home.join(".agents/skills/demo/.in_use"))?;
    fs::write(home.join(".agents/skills/demo/.in_use/run.sh"), "echo bad")?;
    assert_eq!(
        observe_default(&pkg.id, &query)
            .err()
            .ok_or("dir lease")?
            .code,
        "unsupported_capability_payload"
    );
    fs::remove_dir_all(home.join(".agents/skills/demo/.in_use"))?;

    fs::write(skill_dir.join(".in_use"), "nested")?;
    assert_eq!(
        observe_default(&pkg.id, &query)
            .err()
            .ok_or("nested lease")?
            .code,
        "unsupported_capability_payload"
    );
    fs::remove_file(skill_dir.join(".in_use"))?;

    symlink("/bin/sh", home.join(".agents/skills/demo/link"))?;
    assert_eq!(
        observe_default(&pkg.id, &query).err().ok_or("link")?.code,
        "unsupported_capability_payload"
    );
    fs::remove_file(home.join(".agents/skills/demo/link"))?;

    fs::remove_file(home.join(".agents/skills/demo/SKILL.md"))?;
    assert_eq!(
        observe_default(&pkg.id, &query)
            .err()
            .ok_or("missing entrypoint")?
            .code,
        "skill_entrypoint_missing"
    );

    fs::remove_dir_all(home.join(".agents/skills/demo"))?;
    assert!(matches!(
        observe_default(&pkg.id, &query)?,
        Observation::CapabilityPack { pack: None, .. }
    ));

    let untracked = home.join(".agents/skills/local-only");
    fs::create_dir_all(&untracked)?;
    fs::write(untracked.join("SKILL.md"), "# local\n")?;
    assert_eq!(
        resolve_package(&ctx, "skill:local-only", false, false)
            .err()
            .ok_or("untracked")?
            .code,
        "unsupported_capability_source"
    );
    Ok(())
}

#[test]
fn multi_profile_claude_plugins_require_disambiguation_and_detect_settings_drift() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-plug-unit-")
        .tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    for prof in [".claude-mi", ".claude-glm"] {
        let root = home.join(prof);
        let cache = root.join("plugins/cache/official/review/1.0.0");
        fs::create_dir_all(&cache)?;
        fs::create_dir_all(root.join("plugins"))?;
        fs::write(
            cache.join("plugin.json"),
            r#"{"name":"review","version":"1.0.0"}"#,
        )?;
        fs::write(
            root.join("settings.json"),
            r#"{"enabledPlugins":{"review@official":true},"env":{"ANTHROPIC_AUTH_TOKEN":"SYNTHETIC_SECRET"}}"#,
        )?;
        fs::write(
            root.join("plugins/installed_plugins.json"),
            serde_json::to_vec(&json!({
                "version": 2,
                "plugins": {
                    "review@official": [{
                        "scope": "user",
                        "installPath": cache,
                        "version": "1.0.0"
                    }]
                }
            }))?,
        )?;
    }
    let ctx = test_context(&home)?;
    assert_eq!(
        resolve_package(&ctx, "plugin:review@official", false, false)
            .err()
            .ok_or("ambiguous across profiles")?
            .code,
        "ambiguous"
    );
    let profiles = crate::resource_agents::profile_roots(&ctx)?;
    let (caps, _) = crate::resource_capabilities::collect(&ctx, &profiles, None)?;
    let mi_cap = caps
        .iter()
        .find(|r| {
            matches!(&r.data, Data::Capability { origin, .. }
                if origin.record.starts_with(home.join(".claude-mi")))
        })
        .ok_or("mi plugin")?;
    let pkg = resolve_package(&ctx, &mi_cap.id, false, false)?.ok_or("resolved mi plugin")?;
    assert_eq!(refresh_package(&ctx, &pkg)?.id, pkg.id);
    let plan = crate::operations::plan(&ctx, &pkg, crate::domain::Action::Adopt, None, false)?;
    crate::operations::execute(&ctx, plan, false, true)?;

    fs::write(
        home.join(".claude-mi/settings.json"),
        r#"{"enabledPlugins":{"review@official":false}}"#,
    )?;
    let (caps_after, _) = crate::resource_capabilities::collect(&ctx, &profiles, None)?;
    let mi_after = caps_after
        .iter()
        .find(|r| r.id == pkg.id)
        .ok_or("mi after")?;
    if let Data::Capability {
        baseline: Some(base),
        ..
    } = &mi_after.data
    {
        assert_eq!(base.comparison, crate::resource::Comparison::Different);
    } else {
        return Err("missing baseline on adopted plugin".into());
    }
    assert_eq!(canonical_identity(&pkg)?, pkg.id);
    let q = capture(&pkg)?;
    assert_eq!(query_identity(&pkg, &q)?, pkg.id);
    assert!(canonical_identity(&crate::test_support::package()).is_err());
    assert!(capture(&crate::test_support::package()).is_err());
    assert_eq!(
        observe(&pkg.id, &q, &mut 0).err().ok_or("budget")?.code,
        "capability_budget_limit"
    );
    Ok(())
}

#[test]
fn deleted_and_rebound_adopted_plugins_stay_visible_with_truthful_diagnostics() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-plug-del-")
        .tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let root = home.join(".claude-mi");
    let cache = root.join("plugins/cache/official/review/1.0.0");
    fs::create_dir_all(&cache)?;
    fs::write(
        cache.join("plugin.json"),
        r#"{"name":"review","version":"1.0.0"}"#,
    )?;
    fs::write(
        root.join("plugins/installed_plugins.json"),
        serde_json::to_vec(&json!({
            "version": 2,
            "plugins": {"review@official": [{"scope": "user", "installPath": cache, "version": "1.0.0"}]}
        }))?,
    )?;
    let ctx = test_context(&home)?;
    let pkg = resolve_package(&ctx, "plugin:review@official", false, false)?.ok_or("plugin")?;
    let plan = crate::operations::plan(&ctx, &pkg, crate::domain::Action::Adopt, None, false)?;
    crate::operations::execute(&ctx, plan, false, true)?;
    let profiles = crate::resource_agents::profile_roots(&ctx)?;

    fs::remove_dir_all(root.join("plugins/cache"))?;
    fs::remove_file(root.join("plugins/installed_plugins.json"))?;
    let (caps_deleted, _) = crate::resource_capabilities::collect(&ctx, &profiles, None)?;
    let retained = caps_deleted
        .iter()
        .find(|r| r.id == pkg.id)
        .ok_or("retained")?;
    assert_eq!(retained.status, State::Missing);
    assert_eq!(
        resolve_package(&ctx, &pkg.id, false, false)?
            .ok_or("by receipt")?
            .id,
        pkg.id
    );
    assert_eq!(
        refresh_package(&ctx, &pkg).err().ok_or("stale")?.code,
        "plan_stale"
    );

    fs::remove_dir_all(root.join("plugins"))?;
    fs::create_dir_all(root.join("plugins"))?;
    let (_, cov_rebound) = crate::resource_capabilities::collect(&ctx, &profiles, None)?;
    assert!(cov_rebound.iter().any(|c| {
        c.issues
            .iter()
            .any(|i| i == "adopted_capability_unobserved")
    }));
    Ok(())
}
#[test]
fn empty_payload_files_consume_no_bytes_even_when_the_remaining_byte_budget_is_zero() -> TestResult
{
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(home.join(".agents/skills/demo"))?;
    fs::write(home.join(".agents/skills/demo/SKILL.md"), b"")?;
    fs::write(home.join(".agents/.skill-lock.json"),br#"{"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github","skillFolderHash":"tree"}}}"#)?;
    let ctx = test_context(&home)?;
    let package = resolve_package(&ctx, "skill:demo", false, false)?.ok_or("package")?;
    let query = capture(&package)?;
    let mut bytes = 0;
    let snapshot = snapshot_payload(CapabilityKind::SkillsCli, &query.payload, &mut bytes)?
        .ok_or("snapshot")?;
    assert_eq!(snapshot.entries.len(), 1);
    assert_eq!(bytes, 0);
    fs::write(home.join(".agents/skills/demo/SKILL.md"), b"x")?;
    assert!(snapshot_payload(CapabilityKind::SkillsCli, &query.payload, &mut bytes).is_err());
    Ok(())
}
