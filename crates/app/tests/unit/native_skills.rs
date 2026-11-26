#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{fs, os::unix::fs::PermissionsExt};

#[path = "native_skills_guard.rs"]
mod guard_tests;
#[path = "native_skills_refresh.rs"]
mod refresh_tests;

fn fixture() -> std::result::Result<(tempfile::TempDir, Context, Recipe), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let node = temp.path().join("node");
    fs::write(&node,b"#!/bin/sh\nif [ \"$1\" = --version ];then printf 'v26.13.1\\n';exit 0;fi\nif [ \"$2\" = check ];then printf checked > \"$HOME/check-called\";exit 0;fi\nexit 97\n")?;
    fs::set_permissions(&node, fs::Permissions::from_mode(0o700))?;
    let entry = temp.path().join("runner/skills/bin/cli.mjs");
    fs::create_dir_all(entry.parent().ok_or("bin")?)?;
    fs::write(&entry, b"// Synthetic query fixture\n")?;
    fs::write(
        temp.path().join("runner/skills/package.json"),
        br#"{"name":"skills","version":"1.9.0"}"#,
    )?;
    let mut recipe: Recipe = policy::decode(&serde_json::to_vec(
        &json!({"schemaVersion":1,"id":"alpha","owner":"skills-cli","cliVersion":"1.9.0",
        "source":{"repository":"owner/repo","commit":"a".repeat(40),"skillDirectory":"skills/alpha","memberName":"alpha","trackingRef":null},
        "installName":"alpha","scope":"global","clientScope":["codex"],"copy":true,"manualOnly":false,"licenseFiles":[],"derived":null,
        "runner":{"node":node,"entry":entry}}),
    )?)?;
    recipe.runner.sha256 = Some(disk::runner_stamp(&recipe)?);
    let payload = disk::payload(&ctx, "alpha")?;
    fs::create_dir_all(&payload)?;
    fs::write(payload.join("SKILL.md"), b"---\nname: alpha\n---\nbody\n")?;
    fs::write(
        disk::lock_path(&ctx),
        serde_json::to_vec(
            &json!({"version":3,"skills":{"alpha":{"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo.git","commit":"a".repeat(40),"ref":"a".repeat(40),"skillPath":"skills/alpha/SKILL.md","skillFolderHash":"b".repeat(40),"installMode":"copy","installedAt":"2026-11-20T00:00:00.000Z","updatedAt":"2026-11-20T00:00:00.000Z"}}}),
        )?,
    )?;
    Ok((temp, ctx, recipe))
}
fn adopt(ctx: &Context, recipe: &Recipe) -> Result<Value> {
    execute(
        ctx,
        recipe.clone(),
        Action::Adopt,
        false,
        true,
        InputFence::default(),
    )
}

#[test]
fn repository_menu_routes_existing_rows_and_leaves_uninstalled_members_out_of_upgrades()
-> TestResult {
    let (_temp, ctx, recipe) = fixture()?;
    let book = policy::merge(policy::book(None)?, &recipe)?;
    let mut removed = recipe.clone();
    removed.id = "removed".into();
    removed.install_name = "removed".into();
    removed.source.skill_directory = "skills/removed".into();
    removed.source.member_name = "removed".into();
    let book = policy::merge(book, &removed)?;
    scoped_files::private(&ctx.config)?;
    fs::write(book_path(&ctx), disk::encode(&book)?)?;
    let public = "owner/repo@v1.12.0";
    assert!(matches!(
        repository_action(&ctx, "other/repo@v1.12.0")?,
        RepositoryAction::Install
    ));
    assert!(matches!(
        repository_action(&ctx, public)?,
        RepositoryAction::Adopt
    ));
    assert!(!ctx.state.exists());
    adopt(&ctx, &recipe)?;
    assert!(matches!(
        repository_action(&ctx, public)?,
        RepositoryAction::Upgrade
    ));
    let installed = journey::installed_selection(&ctx, "owner/repo")?.ok_or("source")?;
    assert_eq!(
        installed
            .managed
            .iter()
            .map(|r| r.id.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
    assert_eq!(recommended_recipes(&ctx, "owner/repo")?[0].id, "removed");
    let lock_before = fs::read(disk::lock_path(&ctx))?;
    let skill = disk::payload(&ctx, "alpha")?.join("SKILL.md");
    let skill_before = fs::read(&skill)?;
    let pinned = format!("owner/repo@{}", "c".repeat(40));
    let error = journey::upgrade_repository_using(&ctx, &pinned, |prompt| {
        assert!(prompt.contains(&pinned));
        assert!(prompt.contains("技能　alpha"));
        assert!(!prompt.contains("removed"));
        Err(Error::attention("cancelled", "fixture cancel"))
    })
    .err()
    .ok_or("confirmation was required")?;
    assert_eq!(error.code, "cancelled");
    assert_eq!(fs::read(disk::lock_path(&ctx))?, lock_before);
    assert_eq!(fs::read(&skill)?, skill_before);
    Ok(())
}
#[test]
fn adopted_recipe_views_share_one_owner_and_stale_common_receipt_cannot_claim_verified_delivery()
-> TestResult {
    let (_temp, ctx, recipe) = fixture()?;
    let result = adopt(&ctx, &recipe)?;
    assert_eq!(result["deliveryStage"], "adopted-baseline");
    let id = result["journal"]["plan"]["package"]["id"]
        .as_str()
        .ok_or("id")?;
    assert_eq!(select(&ctx, "alpha")?, recipe);
    let info = native_info(&ctx, id)?.ok_or("info")?;
    assert_eq!(info["delivery"]["stage"], "adopted-baseline");
    assert_eq!(
        catalog_summary(&ctx)?["sources"][0]["recipe"]["id"],
        "alpha"
    );
    let summary = recipe_summary(&recipe)?;
    assert!(summary.get("runner").is_none());
    let mut common: Receipts = policy::decode(&fs::read(ctx.state.join("receipts.json"))?)?;
    common
        .installations
        .get_mut(id)
        .ok_or("receipt")?
        .operation_id = "another-operation".into();
    fs::write(ctx.state.join("receipts.json"), disk::encode(&common)?)?;
    assert_eq!(
        native_info(&ctx, id)?.ok_or("info")?["delivery"]["stage"],
        "unknown"
    );
    let file = ctx.config.join("test-input-recipe.json");
    fs::write(&file, disk::encode(&recipe)?)?;
    let (_, fence) = read_recipe(&file)?;
    fence.verify()?;
    fs::rename(&file, ctx.config.join("old-input"))?;
    fs::write(&file, disk::encode(&recipe)?)?;
    assert!(fence.verify().is_err());
    assert!(native_info(&ctx, "plugin:absent@official")?.is_none());
    Ok(())
}
#[test]
fn native_check_and_typed_candidate_cache_are_readonly_for_native_registry_and_scope_bound()
-> TestResult {
    let (_temp, ctx, recipe) = fixture()?;
    adopt(&ctx, &recipe)?;
    let before = fs::read(disk::lock_path(&ctx))?;
    let store = Store::lock(&ctx)?;
    let snapshot = disk::snapshot(&ctx, &recipe)?;
    let checked = native_check(&ctx, &store, &recipe, &snapshot)?;
    assert_eq!(checked.code, Some(0));
    assert!(!checked.uncertain);
    assert_eq!(fs::read(disk::lock_path(&ctx))?, before);
    assert_eq!(fs::read(ctx.home.join("check-called"))?, b"checked");
    let id = policy::installation_id(&disk::payload(&ctx, "alpha")?)?;
    let now = crate::operations::store::now();
    let scope = cache_scope(&ctx);
    let candidate = crate::source::rules::source_index::Candidate {
        member: id.clone(),
        repository: "owner/repo".into(),
        locator_sha256: policy::locator(&recipe, &id)?,
        requested_ref: None,
        commit: "c".repeat(40),
        skill_tree: "d".repeat(40),
        payload_sha256: "e".repeat(64),
        checked_at: now,
    };
    let cache = CheckCache {
        schema_version: 1,
        index: crate::source::rules::source_index::Index::empty(scope.clone())?.merge(
            &scope,
            vec![candidate],
            now,
        )?,
        baselines: BTreeMap::from([(id.clone(), "f".repeat(64))]),
        native_checks: BTreeMap::from([(id.clone(), checked)]),
    };
    let path = ctx.cache.join("native-skills-checks.json");
    fs::write(&path, disk::encode(&cache)?)?;
    assert!(
        cached_row(&ctx, &recipe, &check_cache(&ctx)?, &snapshot.rows)?["outdated"]
            .as_bool()
            .ok_or("outdated")?
    );
    let read = outdated(&ctx, Some("alpha"), &[])?;
    assert_eq!(read["resources"].as_array().ok_or("rows")?.len(), 1);
    let mut changed = recipe.clone();
    changed.manual_only = true;
    assert_eq!(
        cached_row(&ctx, &changed, &cache, &snapshot.rows)?["status"],
        "unknown"
    );
    let mut invalid = cache.clone();
    invalid.schema_version = 2;
    fs::write(&path, disk::encode(&invalid)?)?;
    assert!(check_cache(&ctx).is_err());
    let legacy = json!({"schema_version":1,"sources_sha256":"a".repeat(64),"checked_at":10,"checked_at_by_asset":{},"revisions":{}});
    fs::write(&path, disk::encode(&legacy)?)?;
    assert_eq!(
        check_cache(&ctx).err().ok_or("legacy")?.code,
        "native_skills_legacy_index"
    );
    Ok(())
}
#[test]
fn source_groups_and_recipe_changes_cannot_silently_rebind_the_installed_contract() -> TestResult {
    let (_temp, ctx, recipe) = fixture()?;
    let adopted = adopt(&ctx, &recipe)?;
    let authority = crate::operations::governance::capture(&ctx)?;
    let channels = authority
        .paths
        .dataset(crate::policy::governance::Dataset::SourceChannels);
    fs::create_dir_all(channels.parent().ok_or("parent")?)?;
    fs::write(
        &channels,
        br#"{"schema_version":1,"source_ids":{"owner/repo":"bundle"}}"#,
    )?;
    let request = crate::cli::SkillSelection {
        name: Some("bundle".into()),
        only: vec![],
        all: false,
        revision: None,
        change_id: None,
        agents: crate::cli::SkillAgents::default(),
    };
    assert_eq!(selected_recipes(&ctx, &request)?.len(), 1);
    let all = crate::cli::SkillSelection {
        name: None,
        all: true,
        ..request
    };
    assert_eq!(selected_recipes(&ctx, &all)?.len(), 1);
    let overlap = crate::cli::SkillSelection {
        name: None,
        all: false,
        only: vec!["bundle".into(), "alpha".into()],
        revision: None,
        change_id: None,
        agents: crate::cli::SkillAgents::default(),
    };
    assert!(selected_recipes(&ctx, &overlap).is_err());
    let mut book = load_book(&ctx)?.1;
    book.recipes[0].manual_only = true;
    fs::write(book_path(&ctx), disk::encode(&book)?)?;
    assert!(
        execute(
            &ctx,
            book.recipes[0].clone(),
            Action::Upgrade,
            false,
            true,
            InputFence::default()
        )
        .is_err()
    );
    assert!(!ctx.home.join("check-called").exists());
    let operation = adopted["journal"]["id"].as_str().ok_or("id")?;
    let mut journal: Journal = crate::platform::fs::regular_json(
        &ctx.state
            .join("operations")
            .join(format!("{operation}.json")),
    )?;
    let store = Store::lock(&ctx)?;
    assert_eq!(
        recover(&ctx, &store, &mut journal, false, true)?["journal"]["phase"],
        "succeeded"
    );
    Ok(())
}

#[test]
fn artifact_path_and_upstream_commit_are_binding_evidence_without_changing_logical_installation_id()
-> TestResult {
    let (_temp, ctx, mut recipe) = fixture()?;
    let first = package(
        &ctx,
        &recipe,
        None,
        Some(Path::new("/owned/artifacts/one/payload")),
    )?;
    recipe.source.commit = "b".repeat(40);
    let next = package(
        &ctx,
        &recipe,
        None,
        Some(Path::new("/owned/artifacts/two/payload")),
    )?;
    assert_eq!(first.id, next.id);
    assert_eq!(
        capability_local::canonical_identity(&first)?,
        capability_local::canonical_identity(&next)?
    );
    let Provider::CapabilityObserved {
        source: first_source,
        ..
    } = first.provider
    else {
        return Err("source".into());
    };
    let Provider::CapabilityObserved {
        source: next_source,
        ..
    } = next.provider
    else {
        return Err("source".into());
    };
    assert_ne!(first_source, next_source);
    Ok(())
}

#[test]
fn interrupted_adoption_finishes_only_original_owned_records_without_any_native_call() -> TestResult
{
    let (_temp, ctx, recipe) = fixture()?;
    let store = Store::lock(&ctx)?;
    scoped_files::private(&ctx.config)?;
    scoped_files::private(&ctx.data)?;
    scoped_files::private(&ctx.state.join("native-skills/receipts"))?;
    let (frozen, plan) = plan(&ctx, recipe, Action::Adopt, InputFence::default())?;
    let mut journal = crate::operations::new_journal(&ctx, plan)?;
    disk::write_new(&frozen_path(&journal), &frozen)?;
    persist_recipe(&ctx, &frozen)?;
    store.save(&journal)?;
    let mut progress = Progress {
        stage: "adopted-baseline".into(),
        rows_after: Some(frozen.snapshot.rows.clone()),
        native_after: Some(frozen.snapshot.selected.clone()),
        customization: None,
        commits: vec![],
    };
    for (witness, bytes) in record_images(&ctx, &journal, &frozen, &progress)? {
        progress
            .commits
            .push(disk::Commit::prepare(witness, bytes)?);
    }
    save_progress(&store, &journal, &progress)?;
    let first = &progress.commits[0];
    first.publish(&first.before.path, &first.after, || {
        store.verify_namespace()
    })?;
    drop(store);
    let store = Store::lock(&ctx)?;
    let result = recover(&ctx, &store, &mut journal, false, true)?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    assert_eq!(result["nativeInvoked"], false);
    assert_eq!(load_book(&ctx)?.1.recipes.len(), 1);
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}

#[test]
fn tracking_cache_uses_actual_native_folder_hash_and_ignores_repository_only_commit_changes()
-> TestResult {
    let (_temp, ctx, mut recipe) = fixture()?;
    recipe.source.tracking_ref = Some("main".into());
    let mut rows = policy::registry(scoped_files::regular(&disk::lock_path(&ctx))?.as_deref())?;
    let id = policy::installation_id(&disk::payload(&ctx, "alpha")?)?;
    let scope = cache_scope(&ctx);
    let now = crate::operations::store::now();
    let candidate = crate::source::rules::source_index::Candidate {
        member: id.clone(),
        repository: "owner/repo".into(),
        locator_sha256: policy::locator(&recipe, &id)?,
        requested_ref: Some("main".into()),
        commit: "c".repeat(40),
        skill_tree: "b".repeat(40),
        payload_sha256: "d".repeat(64),
        checked_at: now,
    };
    let cache = CheckCache {
        schema_version: 1,
        index: crate::source::rules::source_index::Index::empty(scope.clone())?.merge(
            &scope,
            vec![candidate],
            now,
        )?,
        baselines: BTreeMap::from([(id.clone(), "d".repeat(64))]),
        native_checks: BTreeMap::from([(
            id,
            NativeCheck {
                code: Some(0),
                uncertain: false,
                scope: "registered-sha-pin".into(),
            },
        )]),
    };
    let current = cached_row(&ctx, &recipe, &cache, &rows)?;
    assert_eq!(current["outdated"], false);
    assert_eq!(current["repositoryAhead"], true);
    rows.get_mut("alpha").ok_or("row")?["skillFolderHash"] = json!("e".repeat(40));
    assert_eq!(cached_row(&ctx, &recipe, &cache, &rows)?["outdated"], true);
    let row = rows.get_mut("alpha").ok_or("row")?;
    row["skillFolderHash"] = json!("e".repeat(64));
    row["commit"] = Value::Null;
    row["ref"] = json!("main");
    assert_eq!(
        cached_row(&ctx, &recipe, &cache, &rows)?["reason"],
        "native_baseline_unproven"
    );
    rows.clear();
    assert_eq!(
        cached_row(&ctx, &recipe, &cache, &rows)?["status"],
        "missing"
    );
    root_tracking_cases(&ctx, &recipe, &cache)?;
    Ok(())
}

#[test]
fn original_adapter_contract_rejects_a_different_package_action_argv_or_environment() -> TestResult
{
    let (_temp, ctx, recipe) = fixture()?;
    adopt(&ctx, &recipe)?;
    let store = Store::lock(&ctx)?;
    let (frozen, plan) = plan(&ctx, recipe, Action::Upgrade, InputFence::default())?;
    let journal = crate::operations::new_journal(&ctx, plan)?;
    disk::write_new(&frozen_path(&journal), &frozen)?;
    let invocation = FrozenInvocation {
        command: journal.plan.command.clone().ok_or("command")?,
        env: disk::frozen_environment(&ctx)?,
        cwd: ctx.home.clone(),
        timeout_secs: ctx.timeout_secs,
    };
    frozen_contract(&journal, &frozen, &invocation)?;
    let mut forged = journal.clone();
    forged.plan.package.name = "another-owner".into();
    assert!(frozen_contract(&forged, &frozen, &invocation).is_err());
    forged.plan_sha256 = fingerprint(&forged.plan)?;
    assert!(super::frozen(&forged).is_err());
    forged = journal.clone();
    forged.plan.action = Action::Uninstall;
    forged.plan.command = mutation_command(&frozen.recipe, Action::Uninstall, None)?;
    forged.plan_sha256 = fingerprint(&forged.plan)?;
    assert!(super::frozen(&forged).is_err());
    let mut changed = invocation.clone();
    changed.command.args.push("--all".into());
    assert!(frozen_contract(&journal, &frozen, &changed).is_err());
    changed = invocation;
    changed
        .env
        .insert("NODE_OPTIONS".into(), "--require foreign".into());
    assert!(frozen_contract(&journal, &frozen, &changed).is_err());
    store.verify_namespace()?;
    Ok(())
}

type ObservedNative = (tempfile::TempDir, Context, Store, Frozen, Journal);
fn observed_native_after() -> std::result::Result<ObservedNative, Box<dyn std::error::Error>> {
    let (temp, ctx, mut recipe) = fixture()?;
    recipe.manual_only = true;
    adopt(&ctx, &recipe)?;
    // An unrelated test child may inherit the released fixture lock until exec.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    let store = loop {
        match Store::lock(&ctx) {
            Err(error)
                if error.code == "operation_busy" && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => break result?,
        }
    };
    let (frozen, plan) = plan(&ctx, recipe, Action::Upgrade, InputFence::default())?;
    let mut journal = crate::operations::new_journal(&ctx, plan)?;
    disk::write_new(&frozen_path(&journal), &frozen)?;
    persist_recipe(&ctx, &frozen)?;
    journal.phase = Phase::Running;
    journal.process_group = Some(std::process::id());
    store.save(&journal)?;
    let invocation = FrozenInvocation {
        command: journal.plan.command.clone().ok_or("command")?,
        env: disk::frozen_environment(&ctx)?,
        cwd: ctx.home.clone(),
        timeout_secs: ctx.timeout_secs,
    };
    worker_after(
        &journal,
        &invocation,
        &NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        },
    )?;
    Ok((temp, ctx, store, frozen, journal))
}
#[test]
fn observed_native_success_advances_customization_and_record_stages_only_after_the_bound_after_proof()
-> TestResult {
    let (_temp, ctx, store, frozen, mut journal) = observed_native_after()?;
    let result = finish(&ctx, &store, &mut journal, &frozen)?;
    assert_eq!(result["deliveryStage"], "verified");
    assert_eq!(journal.phase, Phase::Succeeded);
    let raw = fs::read(disk::payload(&ctx, "alpha")?.join("SKILL.md"))?;
    assert!(crate::capability::rules::invocation::manual_frontmatter(
        &raw
    )?);
    let receipt: StepReceipt =
        policy::decode(&fs::read(receipt_path(&ctx, &journal.plan.package.id))?)?;
    assert_eq!(receipt.operation_id, journal.id);
    assert_eq!(receipt.stage, "verified");
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}
#[test]
fn external_payload_changes_after_native_observation_are_never_learned_as_a_new_customization_baseline()
-> TestResult {
    let (_temp, ctx, store, frozen, mut journal) = observed_native_after()?;
    let common = fs::read(ctx.state.join("receipts.json"))?;
    let skill = disk::payload(&ctx, "alpha")?.join("SKILL.md");
    fs::write(&skill, b"foreign change")?;
    let result = finish(&ctx, &store, &mut journal, &frozen)?;
    assert_eq!(result["deliveryStage"], "customization-pending");
    assert_eq!(journal.phase, Phase::Unknown);
    assert_eq!(fs::read(&skill)?, b"foreign change");
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, common);
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}
#[test]
fn interrupted_owned_metadata_publication_continues_recorded_steps_without_repeating_native()
-> TestResult {
    let (_temp, ctx, store, frozen, mut journal) = observed_native_after()?;
    let mut saved = super::progress(&journal)?;
    let root = disk::payload(&ctx, "alpha")?;
    let mut steps =
        native_skills_steps::prepare(&root, &frozen.recipe, &frozen.assets.licenses, false)?;
    saved.stage = "customization-pending".into();
    saved.customization = Some(steps.clone());
    save_progress(&store, &journal, &saved)?;
    steps.apply(0, |updated| {
        let mut checkpoint = saved.clone();
        checkpoint.customization = Some(updated.clone());
        save_progress(&store, &journal, &checkpoint)
    })?;
    saved.customization = Some(steps);
    save_progress(&store, &journal, &saved)?;
    drop(store);
    let store = Store::lock(&ctx)?;
    let result = finish(&ctx, &store, &mut journal, &frozen)?;
    assert_eq!(result["deliveryStage"], "verified");
    assert_eq!(journal.phase, Phase::Succeeded);
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}

#[test]
fn missing_or_changed_original_after_evidence_cannot_be_reconstructed_from_an_installed_payload()
-> TestResult {
    for variant in 0..3 {
        let (_temp, ctx, store, frozen, mut journal) = observed_native_after()?;
        let common = fs::read(ctx.state.join("receipts.json"))?;
        match variant {
            0 => {
                fs::remove_file(ctx.state.join(after_name(&journal)))?;
            }
            1 => {
                let file = ctx.state.join(after_name(&journal));
                let mut proof: Value = policy::decode(&fs::read(&file)?)?;
                proof["worker_pid"] = json!(std::process::id() + 1);
                fs::write(file, disk::encode(&proof)?)?;
            }
            _ => {
                let mut progress = super::progress(&journal)?;
                progress
                    .rows_after
                    .as_mut()
                    .ok_or("rows")?
                    .get_mut("alpha")
                    .ok_or("row")?["commit"] = json!("b".repeat(40));
                save_progress(&store, &journal, &progress)?;
            }
        }
        let result = finish(&ctx, &store, &mut journal, &frozen)?;
        assert_eq!(result["deliveryStage"], "native-unknown");
        assert_eq!(journal.phase, Phase::Unknown);
        assert_eq!(fs::read(ctx.state.join("receipts.json"))?, common);
        assert!(!ctx.home.join("check-called").exists());
    }
    Ok(())
}
#[test]
fn resumed_customization_with_changed_step_authority_stays_pending_and_preserves_common_records()
-> TestResult {
    let (_temp, ctx, store, frozen, mut journal) = observed_native_after()?;
    let common = fs::read(ctx.state.join("receipts.json"))?;
    let mut steps = native_skills_steps::prepare(
        &disk::payload(&ctx, "alpha")?,
        &frozen.recipe,
        &frozen.assets.licenses,
        false,
    )?;
    steps.slot.name = "other-installation".into();
    let mut progress = super::progress(&journal)?;
    progress.stage = "customization-pending".into();
    progress.customization = Some(steps);
    save_progress(&store, &journal, &progress)?;
    let result = finish(&ctx, &store, &mut journal, &frozen)?;
    assert_eq!(result["deliveryStage"], "customization-pending");
    assert_eq!(journal.phase, Phase::Unknown);
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, common);
    Ok(())
}

fn root_tracking_cases(ctx: &Context, recipe: &Recipe, cache: &CheckCache) -> TestResult {
    let mut recipe = recipe.clone();
    recipe.source.skill_directory = ".".into();
    let id = policy::installation_id(&disk::payload(ctx, "alpha")?)?;
    let mut cache = cache.clone();
    cache
        .index
        .entries
        .get_mut(&id)
        .ok_or("candidate")?
        .locator_sha256 = policy::locator(&recipe, &id)?;
    let mut rows = policy::registry(scoped_files::regular(&disk::lock_path(ctx))?.as_deref())?;
    rows.get_mut("alpha").ok_or("row")?["skillPath"] = json!("SKILL.md");
    rows.get_mut("alpha").ok_or("row")?["skillFolderHash"] = json!("a".repeat(40));
    let same = cached_row(ctx, &recipe, &cache, &rows)?;
    assert_eq!(same["outdated"], false);
    assert_eq!(same["repositoryAhead"], true);
    cache
        .index
        .entries
        .get_mut(&id)
        .ok_or("candidate")?
        .payload_sha256 = "e".repeat(64);
    assert_eq!(cached_row(ctx, &recipe, &cache, &rows)?["outdated"], true);
    cache
        .index
        .entries
        .get_mut(&id)
        .ok_or("candidate")?
        .payload_sha256 = "d".repeat(64);
    let row = rows.get_mut("alpha").ok_or("row")?;
    row["commit"] = json!("b".repeat(40));
    row["ref"] = json!("b".repeat(40));
    row["skillFolderHash"] = json!("b".repeat(40));
    assert_eq!(
        cached_row(ctx, &recipe, &cache, &rows)?["status"],
        "unknown"
    );
    let row = rows.get_mut("alpha").ok_or("row")?;
    row["commit"] = Value::Null;
    row["ref"] = json!("main");
    row["skillFolderHash"] = json!("a".repeat(40));
    assert_eq!(
        cached_row(ctx, &recipe, &cache, &rows)?["status"],
        "unknown"
    );
    rows.get_mut("alpha").ok_or("row")?["ref"] = json!("a".repeat(40));
    assert_eq!(cached_row(ctx, &recipe, &cache, &rows)?["outdated"], false);
    cache.native_checks.get_mut(&id).ok_or("check")?.code = Some(1);
    assert_eq!(
        cached_row(ctx, &recipe, &cache, &rows)?["status"],
        "unknown"
    );
    Ok(())
}
