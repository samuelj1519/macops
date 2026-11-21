#![cfg(test)]
use super::*;
use crate::{
    domain::{Action, Observation},
    test_support::TestResult,
};
use std::{fs, path::PathBuf};
fn request(profile: &str) -> crate::cli::SkillSelection {
    crate::cli::SkillSelection {
        name: None,
        only: vec![format!("profile:{profile}")],
        all: false,
        revision: None,
        change_id: None,
    }
}
fn fixture()
-> std::result::Result<(tempfile::TempDir, Context, PathBuf), Box<dyn std::error::Error>> {
    use clap::Parser;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "skills",
        "sync",
        "--only",
        "profile:mi",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let rule = ctx.settings.join("manifests/skill-profiles.json");
    fs::create_dir_all(rule.parent().ok_or("parent")?)?;
    fs::write(
        &rule,
        br#"{"claude":{"mi":{"plugins_off":["review@official"]}}}"#,
    )?;
    fs::create_dir(ctx.home.join(".claude-mi"))?;
    Ok((temp, ctx, rule))
}
#[test]
fn profile_only_sync_is_atomic_idempotent_and_restores_missing_parent_without_touching_model()
-> TestResult {
    let (_temp, ctx, rule) = fixture()?;
    let target = ctx.home.join(".claude-mi/settings.json");
    let before = br#"{ "model" : "private-\u006dodel", "env" : {"KEY":"SYNTHETIC_PRIVATE"} }"#;
    fs::write(&target, before)?;
    let dry = sync(&ctx, &request("mi"), true, true)?;
    assert_eq!(dry["planned_changes"], 1);
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    assert_eq!(fs::read(&target)?, before);
    let result = sync(&ctx, &request("mi"), false, true)?;
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["incomplete"], false);
    assert_eq!(
        crate::policy::json::parse(&fs::read(&target)?)?["enabledPlugins"]["review@official"],
        false
    );
    assert!(String::from_utf8(fs::read(&target)?)?.contains(r#""model" : "private-\u006dodel""#));
    assert_eq!(sync(&ctx, &request("mi"), false, true)?["status"], "same");
    fs::write(rule, b"{}")?;
    assert_eq!(
        sync(&ctx, &request("mi"), false, true)?["status"],
        "succeeded"
    );
    assert_eq!(
        crate::policy::json::parse(&fs::read(&target)?)?,
        crate::policy::json::parse(before)?
    );
    assert!(
        !ctx.home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    assert!(!ctx.home.join(".agents/skills").exists());
    Ok(())
}
#[test]
fn adopted_plugin_receipts_project_settings_before_mutation_and_match_the_final_native_observation()
-> TestResult {
    let (_temp, ctx, _rule) = fixture()?;
    let root = ctx.home.join(".claude-mi");
    let payload = root.join("plugins/cache/official/review/1.0.0");
    fs::create_dir_all(&payload)?;
    fs::write(
        payload.join("plugin.json"),
        br#"{"name":"review","version":"1.0.0"}"#,
    )?;
    fs::write(
        root.join("settings.json"),
        br#"{"enabledPlugins":{"review@official":true},"model":"private"}"#,
    )?;
    fs::write(
        root.join("plugins/installed_plugins.json"),
        serde_json::to_vec(
            &json!({"version":2,"plugins":{"review@official":[{"scope":"user","installPath":payload,"version":"1.0.0"}]}}),
        )?,
    )?;
    let package = crate::capability_local::resolve_package(&ctx, "plugin:review@official", false)?
        .ok_or("package")?;
    let plan = crate::operations::plan(&ctx, &package, Action::Adopt, None, false)?;
    crate::operations::execute(&ctx, plan, false, true)?;
    let old = fs::read(ctx.state.join("receipts.json"))?;
    let result = sync(&ctx, &request("mi"), false, true)?;
    assert_eq!(result["status"], "succeeded");
    let raw = fs::read(ctx.state.join("receipts.json"))?;
    assert_ne!(raw, old);
    let book: crate::domain::Receipts = serde_json::from_slice(&raw)?;
    let receipt = &book.installations[&package.id];
    let Some(crate::domain::QuerySpec::LocalCapability(query)) = &receipt.query else {
        return Err("query".into());
    };
    assert_eq!(
        crate::capability_local::observe_default(&package.id, query)?,
        receipt.observation
    );
    let Observation::CapabilityPack {
        pack: Some(stamp), ..
    } = &receipt.observation
    else {
        return Err("stamp".into());
    };
    assert_eq!(stamp.configured_enabled, Some(false));
    assert_eq!(stamp.file_count, 1);
    Ok(())
}

#[test]
fn live_and_static_source_restore_their_independent_originals_and_keep_unowned_edits() -> TestResult
{
    let (_temp, mut ctx, rule) = fixture()?;
    let source = ctx.home.join("source");
    let source_file = source.join("dot_claude-mi/settings.json");
    fs::create_dir_all(source_file.parent().ok_or("parent")?)?;
    ctx.chezmoi.source = Some(source);
    ctx.chezmoi.destination = Some(ctx.home.clone());
    let live = ctx.home.join(".claude-mi/settings.json");
    fs::write(&live, br#"{"model":"live-model"}"#)?;
    fs::write(
        &source_file,
        br#"{"model":"source-model","enabledPlugins":{"review@official":true}}"#,
    )?;
    let dry = sync(&ctx, &request("mi"), true, true)?;
    assert_eq!(dry["planned_changes"], 2);
    assert!(!ctx.state.exists());
    assert_eq!(
        sync(&ctx, &request("mi"), false, true)?["status"],
        "succeeded"
    );
    let mut live_value = crate::policy::json::parse(&fs::read(&live)?)?;
    live_value["model"] = json!("user-new-model");
    fs::write(&live, serde_json::to_vec(&live_value)?)?;
    fs::write(rule, b"{}")?;
    assert_eq!(
        sync(&ctx, &request("mi"), false, true)?["status"],
        "succeeded"
    );
    assert_eq!(
        crate::policy::json::parse(&fs::read(&live)?)?,
        json!({"model":"user-new-model"})
    );
    assert_eq!(
        crate::policy::json::parse(&fs::read(&source_file)?)?,
        json!({"model":"source-model","enabledPlugins":{"review@official":true}})
    );
    Ok(())
}

#[test]
fn codex_config_only_sync_retires_created_tables_and_preserves_raw_model_and_eof() -> TestResult {
    let (_temp, ctx, rule) = fixture()?;
    fs::write(&rule, br#"{"codex":{"disable_agents_skills":["example"]}}"#)?;
    let skill = ctx.home.join(".agents/skills/example/SKILL.md");
    fs::create_dir_all(skill.parent().ok_or("parent")?)?;
    fs::write(&skill, b"---\nname: example\n---\nexample\n")?;
    let config = ctx.home.join(".codex/config.toml");
    fs::create_dir(config.parent().ok_or("parent")?)?;
    let before =
        b"model = 'private-model'\n[model_providers.private]\nbase_url = 'https://example.invalid'";
    fs::write(&config, before)?;
    assert_eq!(
        sync(&ctx, &request("codex"), true, true)?["planned_changes"],
        1
    );
    assert!(!ctx.state.exists());
    assert_eq!(
        sync(&ctx, &request("codex"), false, true)?["status"],
        "succeeded"
    );
    let current = fs::read(&config)?;
    assert!(String::from_utf8(current.clone())?.contains("base_url = 'https://example.invalid'"));
    let observed = crate::policy::codex_fields::observe(&current)?;
    assert_eq!(
        observed.entries[skill.to_str().ok_or("utf8")?].enabled,
        crate::policy::profile_fields::Presence::Present(false)
    );
    assert_eq!(
        sync(&ctx, &request("codex"), false, true)?["status"],
        "same"
    );
    fs::write(rule, b"{}")?;
    assert_eq!(
        sync(&ctx, &request("codex"), false, true)?["status"],
        "succeeded"
    );
    assert_eq!(fs::read(&config)?, before);
    Ok(())
}

#[test]
fn reviewer_retired_unselected_owner_is_still_reported_pending() -> TestResult {
    let (_temp, ctx, rule) = fixture()?;
    let target = ctx.home.join(".claude-mi/settings.json");
    fs::write(&target, b"{}")?;
    assert_eq!(
        sync(&ctx, &request("mi"), false, true)?["status"],
        "succeeded"
    );
    fs::write(rule, b"{}")?;
    let result = sync(&ctx, &request("zen"), true, true)?;
    eprintln!("TOMBSTONE_RESULT={result}");
    assert_eq!(
        result["incomplete"], true,
        "unselected old owner still needs retirement"
    );
    let empty: &[serde_json::Value] = &[];
    assert_ne!(
        result["pending_config_policies"]
            .as_array()
            .ok_or("array")?
            .as_slice(),
        empty
    );
    assert_eq!(
        crate::policy::json::parse(&fs::read(target)?)?["enabledPlugins"]["review@official"],
        false
    );
    Ok(())
}

#[test]
fn reviewer_absent_config_lookup_cannot_change_after_profile_assessment() -> TestResult {
    let (_temp, ctx, _rule) = fixture()?;
    fs::write(ctx.home.join(".claude-mi/settings.json"), b"{}")?;
    let governance = crate::governance::capture(&ctx)?;
    let selected =
        crate::policy::profile_select::split(crate::policy::delivery::Selection::Explicit(vec![
            "profile:mi".into(),
        ]))?;
    let assessment = crate::profile_inventory::assess(&ctx, &governance, &selected)?;
    assert_eq!(assessment.patches.len(), 1);
    let source = ctx.home.join("elsewhere-source");
    fs::create_dir_all(source.join("dot_claude-mi"))?;
    fs::write(source.join("dot_claude-mi/settings.json"), b"{}")?;
    let config = ctx.home.join(".config/chezmoi/chezmoi.json");
    fs::create_dir_all(config.parent().ok_or("parent")?)?;
    fs::write(
        &config,
        serde_json::to_vec(&json!({"sourceDir":source,"destDir":ctx.home}))?,
    )?;
    let now = crate::config_inventory::discover(&ctx);
    assert!(
        now.candidates
            .iter()
            .any(|c| c.target == ctx.home.join(".claude-mi/settings.json"))
    );
    let verified = assessment.verify();
    eprintln!("CONFIG_LOOKUP_VERIFY={verified:?}");
    assert!(
        verified.is_err(),
        "unmanaged positive proof was invalidated by new higher-priority config"
    );
    Ok(())
}

#[test]
fn reviewer_deny_retirement_does_not_return_unreadable_after() -> TestResult {
    use crate::policy::{delivery::Profile, deny_fields as d, profile_fields as p};
    use std::collections::BTreeMap;
    let owner = p::Owner {
        profile: p::ConfigProfile::Claude(Profile::Mi),
        role: p::Role::Live,
        intent: p::Intent::Pstack,
    };
    let id = owner.id();
    let original = vec![
        "Bash(anchor)".into(),
        "Skill(pstack:old-a)".into(),
        "Skill(pstack:old-b)".into(),
    ];
    let desired = vec!["Skill(pstack:new)".to_owned()];
    let first = d::merge(
        &original,
        None,
        &BTreeMap::from([(
            id.clone(),
            Some(d::Claim {
                owner,
                intent_sha256: "a".repeat(64),
                desired: desired.clone(),
            }),
        )]),
    )?;
    let mut current: Vec<String> = (0..9998).map(|n| format!("Bash(new{n})")).collect();
    current.push("Bash(anchor)".into());
    current.extend(desired);
    assert_eq!(current.len(), 10000);
    let result = d::merge(
        &current,
        first.record.as_ref(),
        &BTreeMap::from([(id, None)]),
    );
    eprintln!(
        "RETIRE_OUTPUT_LEN={:?}",
        result.as_ref().map(|p| p.after.len())
    );
    if let Ok(proposal) = result {
        assert!(
            proposal.after.len() <= 10000,
            "retirement publishes array rejected by next merge"
        );
    }
    Ok(())
}

#[test]
fn reviewer_new_chezmoi_lookup_blocks_frozen_profile_transaction_before_live_write() -> TestResult {
    let (_temp, ctx, _rule) = fixture()?;
    let target = ctx.home.join(".claude-mi/settings.json");
    fs::write(&target, b"{}")?;
    let governance = crate::governance::capture(&ctx)?;
    let selected =
        crate::policy::profile_select::split(crate::policy::delivery::Selection::Explicit(vec![
            "profile:mi".into(),
        ]))?;
    let assessment = Assessment {
        links: None,
        configuration: crate::profile_inventory::assess(&ctx, &governance, &selected)?,
        profiles_selected: true,
    };
    let store = Store::lock(&ctx)?;
    let operation = crate::store::operation_id();
    let root = backup(&governance, "LOOKUP-CHANGED")?;
    let blobs = files::Blobs::create(&root)?;
    let frozen = manifest(
        &ctx,
        &governance,
        &assessment,
        &operation,
        "LOOKUP-CHANGED",
        &blobs,
    )?;
    let raw = bytes(&frozen)?;
    let digest = files::hash(&raw);
    Store::atomic_bytes(root.file(), "capability-plan.json", &raw, true)?;
    let mut journal = journal(
        &ctx,
        &frozen,
        &root.path.join("capability-plan.json"),
        &digest,
    )?;
    store.save(&journal)?;
    let mut pending = empty_pending(&frozen, &digest);
    save_pending(&root, &frozen, &pending)?;
    let source = ctx.home.join("elsewhere-source");
    fs::create_dir_all(source.join("dot_claude-mi"))?;
    let source_target = source.join("dot_claude-mi/settings.json");
    fs::write(&source_target, b"{}")?;
    let config = ctx.home.join(".config/chezmoi/chezmoi.json");
    fs::create_dir_all(config.parent().ok_or("parent")?)?;
    fs::write(
        config,
        serde_json::to_vec(&json!({"sourceDir":source,"destDir":ctx.home}))?,
    )?;
    let result = execute(&store, &root, &blobs, &frozen, &mut pending, &mut journal);
    eprintln!(
        "LOOKUP_CHANGED_EXECUTE={result:?}; LIVE={:?}; SOURCE={:?}; PENDING={}",
        fs::read(&target)?,
        fs::read(&source_target)?,
        frozen.pending.exists()
    );
    assert!(
        result.is_err(),
        "changed actual source mapping must block frozen unmanaged write"
    );
    assert_eq!(fs::read(target)?, b"{}");
    assert_eq!(fs::read(source_target)?, b"{}");
    Ok(())
}

#[test]
fn reviewer_codex_existing_empty_config_key_comments_survive_table_creation() -> TestResult {
    use crate::policy::{codex_fields as c, profile_fields::Presence};
    let raw=b"model='private-model'\n[skills]\n# user's empty list note\nconfig=[]\n[other]\ntoken='private'\n";
    let result = c::patch(
        raw,
        &[c::Change {
            path: "/demo/SKILL.md".into(),
            enabled: Presence::Present(false),
            remove_created_table: false,
        }],
    );
    eprintln!(
        "CODEX_EMPTY_CONFIG_RESULT={:?}",
        result.as_ref().map(|b| String::from_utf8_lossy(b))
    );
    if let Ok(after) = result {
        assert!(
            String::from_utf8(after)?.contains("# user's empty list note"),
            "config parent key decor is not an owned bool"
        );
    }
    Ok(())
}

#[test]
fn reviewer_deny_nested_records_require_object_shapes() -> TestResult {
    use crate::policy::{claude_fields as c, delivery::Profile, invocation, profile_fields as p};
    let owner = p::Owner {
        profile: p::ConfigProfile::Claude(Profile::Mi),
        role: p::Role::Live,
        intent: p::Intent::Pstack,
    };
    let id = owner.id();
    let target = PathBuf::from("/synthetic-home/.claude-mi/settings.json");
    let plan = c::plan(
        &target,
        br#"{"permissions":{"deny":["Bash(anchor)","Skill(pstack:old)"]}}"#,
        None,
        &[c::Request {
            owner,
            intent_sha256: "a".repeat(64),
            desired: Some(c::Desired::Pstack(vec!["Skill(pstack:new)".into()])),
        }],
    )?;
    let file = serde_json::to_value(plan.record.ok_or("record")?)?;
    let mut rejected = vec![];
    for kind in ["slot", "anchor", "claim", "claim-owner"] {
        let mut row = file.clone();
        let pstack = &mut row["fields"]["pstack"];
        match kind {
            "slot" => {
                let slot = pstack["original"][0].clone();
                pstack["original"][0] = json!([slot["entry"], slot["previous"], slot["next"]]);
            }
            "anchor" => {
                let a = pstack["original"][0]["previous"].clone();
                pstack["original"][0]["previous"] =
                    json!([a["sha256"], a["occurrence"], a["count"]]);
            }
            "claim" => {
                let c = pstack["claims"][&id].clone();
                pstack["claims"][&id] = json!([c["owner"], c["intent_sha256"], c["desired"]]);
            }
            "claim-owner" => {
                let o = pstack["claims"][&id]["owner"].clone();
                pstack["claims"][&id]["owner"] = json!([o["profile"], o["role"], o["intent"]]);
            }
            _ => return Err("kind".into()),
        }
        let book = json!({"schema_version":2,"policy_hash":null,"files":{},"profiles":{target.to_str().ok_or("utf8")?:row},"profile_policy_hash":null});
        let result = invocation::record(Path::new("/synthetic-home"), &serde_json::to_vec(&book)?);
        eprintln!(
            "DENY_POSITIONAL_{kind}={:?}",
            result.as_ref().map(|_| "accepted")
        );
        rejected.push((kind, result.is_err()));
    }
    assert!(
        rejected.iter().all(|(_, rejected)| *rejected),
        "positional DTOs accepted: {rejected:?}"
    );
    Ok(())
}

#[test]
fn reviewer_higher_priority_default_source_lookup_cannot_change_after_assessment() -> TestResult {
    let (_temp, mut ctx, _rule) = fixture()?;
    fs::write(ctx.home.join(".claude-mi/settings.json"), b"{}")?;
    ctx.isolated = false;
    let fallback = ctx.home.join("fallback-data");
    let source = fallback.join("chezmoi");
    fs::create_dir_all(source.join("dot_claude-mi"))?;
    fs::write(source.join("dot_claude-mi/settings.json"), b"{}")?;
    ctx.chezmoi.env.insert("XDG_DATA_DIRS".into(), fallback);
    ctx.chezmoi.env.insert(
        "XDG_CONFIG_DIRS".into(),
        ctx.home.join("absent-config-dirs"),
    );
    let governance = crate::governance::capture(&ctx)?;
    let selected =
        crate::policy::profile_select::split(crate::policy::delivery::Selection::Explicit(vec![
            "profile:mi".into(),
        ]))?;
    let assessment = crate::profile_inventory::assess(&ctx, &governance, &selected)?;
    assert_eq!(assessment.patches.len(), 2);
    let primary = ctx.home.join(".local/share/chezmoi");
    fs::create_dir_all(primary.join("dot_claude-mi"))?;
    fs::write(primary.join("dot_claude-mi/settings.json"), b"{}")?;
    let now = crate::config_inventory::discover(&ctx);
    assert_eq!(now.roots.ok_or("roots")?.configured_source, primary);
    let verified = assessment.verify();
    eprintln!("DEFAULT_SOURCE_LOOKUP_VERIFY={verified:?}");
    assert!(
        verified.is_err(),
        "default_source priority lookup is part of source write authority"
    );
    Ok(())
}

#[test]
fn reviewer_v4_live_source_recovery_preserves_before_after_and_unknown_boundaries() -> TestResult {
    use crate::capability_tx::{candidate_for, prepare, recover_journal};
    use crate::store::Phase;
    for step in 0..=4 {
        let (_temp, mut ctx, _rule) = fixture()?;
        let target = ctx.home.join(".claude-mi/settings.json");
        let live_before = br#"{"model":"private-live"}"#;
        fs::write(&target, live_before)?;
        let source = ctx.home.join("source");
        let source_target = source.join("dot_claude-mi/settings.json");
        fs::create_dir_all(source_target.parent().ok_or("parent")?)?;
        let source_before =
            br#"{"model":"private-source","enabledPlugins":{"review@official":true}}"#;
        fs::write(&source_target, source_before)?;
        ctx.chezmoi.source = Some(source);
        ctx.chezmoi.destination = Some(ctx.home.clone());
        let governance = crate::governance::capture(&ctx)?;
        let selected = crate::policy::profile_select::split(
            crate::policy::delivery::Selection::Explicit(vec!["profile:mi".into()]),
        )?;
        let assessment = Assessment {
            links: None,
            configuration: crate::profile_inventory::assess(&ctx, &governance, &selected)?,
            profiles_selected: true,
        };
        let store = Store::lock(&ctx)?;
        let operation = crate::store::operation_id();
        let change = format!("REVIEW-V4-{step}");
        let root = backup(&governance, &change)?;
        let blobs = files::Blobs::create(&root)?;
        let frozen = manifest(&ctx, &governance, &assessment, &operation, &change, &blobs)?;
        assert_eq!(frozen.schema_version, 4);
        assert_eq!(frozen.files.len(), 2);
        let raw = bytes(&frozen)?;
        let digest = files::hash(&raw);
        Store::atomic_bytes(root.file(), "capability-plan.json", &raw, true)?;
        let mut journal = journal(
            &ctx,
            &frozen,
            &root.path.join("capability-plan.json"),
            &digest,
        )?;
        journal.phase = Phase::Running;
        store.save(&journal)?;
        let mut pending = empty_pending(&frozen, &digest);
        save_pending(&root, &frozen, &pending)?;
        prepare(&root, &frozen, &blobs, &mut pending)?;
        for (index, (file, candidate)) in frozen.files.iter().zip(&pending.files).enumerate() {
            if step > index {
                files::publish(
                    &file.namespace,
                    &file.before,
                    &candidate_for(candidate, &pending)?,
                )?;
            }
        }
        if step >= 3 {
            for ledger in &frozen.ledgers {
                if let Some(candidate) = pending.ledgers.iter().find(|c| c.target == ledger.path) {
                    files::publish(
                        &ledger.namespace,
                        &ledger.before,
                        &candidate_for(candidate, &pending)?,
                    )?;
                }
            }
        }
        if step == 4 {
            fs::write(&target, b"external-full-file-edit")?;
        }
        let result = recover_journal(&ctx, &store, &mut journal, false, true);
        if step == 4 {
            assert!(result.is_err());
            assert!(frozen.pending.exists());
            assert_eq!(fs::read(&target)?, b"external-full-file-edit");
        } else {
            result?;
            assert!(!frozen.pending.exists());
            if step < 3 {
                assert_eq!(journal.phase, Phase::Failed);
                assert_eq!(fs::read(&target)?, live_before);
                assert_eq!(fs::read(&source_target)?, source_before);
            } else {
                assert_eq!(journal.phase, Phase::Succeeded);
                assert_eq!(
                    crate::policy::json::parse(&fs::read(&target)?)?["enabledPlugins"]["review@official"],
                    false
                );
                assert_eq!(
                    crate::policy::json::parse(&fs::read(&source_target)?)?["enabledPlugins"]["review@official"],
                    false
                );
            }
        }
    }
    Ok(())
}

#[test]
fn reviewer_bool_original_presence_requires_object_shape() -> TestResult {
    use crate::policy::{invocation, profile_fields::Presence};
    let (_temp, ctx, _rule) = fixture()?;
    let target = ctx.home.join(".claude-mi/settings.json");
    fs::write(&target, br#"{"enabledPlugins":{"review@official":true}}"#)?;
    assert_eq!(
        sync(&ctx, &request("mi"), false, true)?["status"],
        "succeeded"
    );
    let raw = fs::read(ctx.home.join(".agents/invocation-overlays.json"))?;
    let baseline = invocation::record(&ctx.home, &raw)?;
    let crate::policy::profile_registry::Fields::Claude { plugins, .. } =
        &baseline.profiles.get(&target).ok_or("profile")?.fields
    else {
        return Err("claude profile".into());
    };
    assert_eq!(plugins["review@official"].original, Presence::Present(true));
    let valid: Value = serde_json::from_slice(&raw)?;
    let key = target.to_str().ok_or("utf8")?;
    assert_eq!(valid["schema_version"], 2);
    assert_eq!(
        valid["profiles"][key]["fields"]["plugins"]["review@official"]["original"],
        json!({"state":"present","value":true})
    );
    let mut accepted = vec![];
    for positional in [
        json!(["present", true]),
        json!(["missing"]),
        json!(["missing", null]),
    ] {
        let mut changed = valid.clone();
        changed["profiles"][key]["fields"]["plugins"]["review@official"]["original"] =
            positional.clone();
        let decoded = invocation::record(&ctx.home, &serde_json::to_vec(&changed)?);
        eprintln!("BOOL_ORIGINAL_POSITIONAL={positional} RESULT={decoded:?}");
        if decoded.is_ok() {
            accepted.push(positional);
        }
    }
    assert!(
        accepted.is_empty(),
        "BoolField.original accepted positional arrays: {accepted:?}"
    );
    Ok(())
}

#[test]
fn malformed_original_values_block_sync_without_rewriting_the_book_or_config() -> TestResult {
    let (_temp, ctx, _rule) = fixture()?;
    let target = ctx.home.join(".claude-mi/settings.json");
    fs::write(
        &target,
        br#"{"model":"private-model","enabledPlugins":{"review@official":true}}"#,
    )?;
    assert_eq!(
        sync(&ctx, &request("mi"), false, true)?["status"],
        "succeeded"
    );
    let book = ctx.home.join(".agents/invocation-overlays.json");
    let valid: Value = serde_json::from_slice(&fs::read(&book)?)?;
    let config = fs::read(&target)?;
    let operations = fs::read_dir(ctx.state.join("operations"))?.count();
    let backups = fs::read_dir(ctx.home.join(".agents/backups"))?.count();
    let key = target.to_str().ok_or("utf8")?;
    for positional in [json!(["present", true]), json!(["missing", null])] {
        let mut corrupt = valid.clone();
        corrupt["profiles"][key]["fields"]["plugins"]["review@official"]["original"] = positional;
        let raw = serde_json::to_vec(&corrupt)?;
        fs::write(&book, &raw)?;
        assert!(sync(&ctx, &request("mi"), false, true).is_err());
        assert_eq!(fs::read(&book)?, raw);
        assert_eq!(fs::read(&target)?, config);
        assert_eq!(
            fs::read_dir(ctx.state.join("operations"))?.count(),
            operations
        );
        assert_eq!(
            fs::read_dir(ctx.home.join(".agents/backups"))?.count(),
            backups
        );
        assert!(
            !ctx.home
                .join(".agents/invocation-policy.pending.json")
                .exists()
        );
    }
    Ok(())
}
