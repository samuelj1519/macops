#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{fs, path::PathBuf};

#[test]
fn constructed_policy_commands_validate_change_id_before_missing_pending_or_noop() -> TestResult {
    let f = Fixture::new()?;
    fs::remove_file(
        f.ctx
            .settings
            .join("manifests/agent-invocation-policy.json"),
    )?;
    for command in [
        crate::cli::Policy::Sync {
            change_id: Some("../outside".into()),
        },
        crate::cli::Policy::Recover {
            change_id: Some("../outside".into()),
        },
    ] {
        let request = crate::cli::Command::Skills {
            command: Some(crate::cli::Skills::Policy { command }),
        };
        assert!(write(&f.ctx, &request, false, true).is_err());
        assert!(!f.ctx.state.exists());
    }
    Ok(())
}

struct Fixture {
    _temp: tempfile::TempDir,
    ctx: Context,
    skill: PathBuf,
}
impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir_in("/private/tmp")?;
        let home = temp.path().join("home");
        fs::create_dir(&home)?;
        let cli = Cli::try_parse_from([
            "macops",
            "--home",
            home.to_str().ok_or("utf8")?,
            "skills",
            "policy",
            "sync",
        ])?;
        let ctx = Context::from_cli(&cli, &mut vec![])?;
        let path = ctx.settings.join("manifests/agent-invocation-policy.json");
        fs::create_dir_all(path.parent().ok_or("parent")?)?;
        fs::write(
            path,
            br#"{"schema_version":1,"standalone_manual_only":["example"]}"#,
        )?;
        let skill = home.join(".agents/skills/example/SKILL.md");
        fs::create_dir_all(skill.parent().ok_or("parent")?)?;
        fs::write(&skill, "---\nname: example\n---\nbody\n")?;
        Ok(Self {
            _temp: temp,
            ctx,
            skill,
        })
    }
}
#[test]
fn policy_sync_prepares_a_missing_metadata_directory_and_is_idempotent() -> TestResult {
    let f = Fixture::new()?;
    let before = fs::read(&f.skill)?;
    let dry = sync(&f.ctx, Some("CASE-1"), true, true)?;
    assert_eq!(dry["plan"]["planned_changes"], 2);
    assert!(!f.ctx.state.exists());
    assert_eq!(fs::read(&f.skill)?, before);
    let result = sync(&f.ctx, Some("CASE-1"), false, true)?;
    assert_eq!(result["status"], "succeeded");
    assert!(crate::policy::invocation::manual_frontmatter(&fs::read(
        &f.skill
    )?)?);
    assert!(
        f.skill
            .parent()
            .ok_or("parent")?
            .join("agents/openai.yaml")
            .is_file()
    );
    assert!(
        !f.ctx
            .home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    assert_eq!(sync(&f.ctx, Some("CASE-2"), false, true)?["status"], "same");
    assert!(!f.ctx.home.join(".agents/backups/CASE-2").exists());
    Ok(())
}

struct Batch {
    store: Store,
    root: Directory,
    blobs: files::Blobs,
    manifest: Manifest,
    pending: Pending,
    journal: Journal,
}
fn batch(f: &Fixture, change: &str) -> std::result::Result<Batch, Box<dyn std::error::Error>> {
    let store = Store::lock(&f.ctx)?;
    let authority = crate::governance::capture(&f.ctx)?;
    let assessment = crate::invocation_inventory::assess(&f.ctx, &authority)?;
    let root = backup(&authority, change)?;
    let blobs = files::Blobs::create(&root)?;
    let operation = crate::store::operation_id();
    let manifest = manifest(&f.ctx, &authority, &assessment, &operation, change, &blobs)?;
    let raw = bytes(&manifest)?;
    let digest = files::hash(&raw);
    Store::atomic_bytes(root.file(), "capability-plan.json", &raw, true)?;
    let mut journal = journal(
        &f.ctx,
        &manifest,
        &root.path.join("capability-plan.json"),
        &digest,
    )?;
    store.save(&journal)?;
    let mut pending = empty_pending(&manifest, &digest);
    save_pending(&root, &manifest, &pending)?;
    prepare(&root, &manifest, &blobs, &mut pending)?;
    journal.phase = Phase::Running;
    store.save(&journal)?;
    Ok(Batch {
        store,
        root,
        blobs,
        manifest,
        pending,
        journal,
    })
}
fn advance(batch: &Batch, step: usize) -> Result<()> {
    if step >= 1 {
        for parent in &batch.pending.parents {
            files::publish_parent(parent)?;
        }
    }
    for (index, (file, candidate)) in batch
        .manifest
        .files
        .iter()
        .zip(&batch.pending.files)
        .enumerate()
    {
        if step >= index + 2 {
            files::publish(
                &file.namespace,
                &file.before,
                &candidate_for(candidate, &batch.pending)?,
            )?;
        }
    }
    if step >= 4 {
        for ledger in &batch.manifest.ledgers {
            if let Some(candidate) = batch
                .pending
                .ledgers
                .iter()
                .find(|c| c.target == ledger.path)
            {
                files::publish(
                    &ledger.namespace,
                    &ledger.before,
                    &candidate_for(candidate, &batch.pending)?,
                )?;
            }
        }
    }
    if step >= 5 {
        clear_pending(&batch.manifest, &batch.pending)?;
    }
    Ok(())
}
#[test]
fn each_physical_commit_boundary_recovers_without_rerunning_and_preserves_full_before() -> TestResult
{
    for step in 0..=5 {
        let f = Fixture::new()?;
        let before = fs::read(&f.skill)?;
        let mut b = batch(&f, &format!("BOUNDARY-{step}"))?;
        advance(&b, step)?;
        recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)?;
        assert!(!b.manifest.pending.exists(), "step {step}");
        if step < 4 {
            assert_eq!(b.journal.phase, Phase::Failed);
            assert_eq!(fs::read(&f.skill)?, before);
            assert!(!f.skill.parent().ok_or("parent")?.join("agents").exists());
            assert!(!f.ctx.home.join(".agents/invocation-overlays.json").exists());
        } else {
            assert_eq!(b.journal.phase, Phase::Succeeded);
            assert!(crate::policy::invocation::manual_frontmatter(&fs::read(
                &f.skill
            )?)?);
        }
        let phase = b.journal.phase;
        recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)?;
        assert_eq!(b.journal.phase, phase);
    }
    Ok(())
}
#[test]
fn recovery_keeps_external_payload_and_control_changes_and_never_abandons_pending() -> TestResult {
    for control in [false, true] {
        let f = Fixture::new()?;
        let mut b = batch(&f, "EXTERNAL")?;
        advance(&b, 2)?;
        let path = if control {
            f.ctx
                .settings
                .join("manifests/agent-invocation-policy.json")
        } else {
            f.skill.clone()
        };
        fs::write(&path, b"external-secret-bytes")?;
        assert!(recover_journal(&f.ctx, &b.store, &mut b.journal, false, true).is_err());
        assert!(recover_journal(&f.ctx, &b.store, &mut b.journal, true, true).is_err());
        assert_eq!(fs::read(path)?, b"external-secret-bytes");
        assert!(b.manifest.pending.is_file());
    }
    Ok(())
}
#[test]
fn a_second_recovery_recognizes_its_own_restored_inode() -> TestResult {
    let f = Fixture::new()?;
    let before = fs::read(&f.skill)?;
    let mut b = batch(&f, "ROLLBACK-CRASH")?;
    advance(&b, 2)?;
    let file = &b.manifest.files[0];
    rollback_one(
        &b.root,
        &b.manifest,
        &mut b.pending,
        &b.blobs,
        &file.namespace,
        &file.before,
        file.before_blob.as_ref(),
    )?;
    assert_ne!(files::observe(&file.namespace, 1024)?.0, file.before);
    assert_eq!(fs::read(&f.skill)?, before);
    recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)?;
    assert_eq!(b.journal.phase, Phase::Failed);
    assert!(!b.manifest.pending.exists());
    Ok(())
}
#[test]
fn no_pending_policy_recovery_is_zero_write_even_without_a_state_directory() -> TestResult {
    let f = Fixture::new()?;
    assert_eq!(
        recover_policy(&f.ctx, Some("NOT-PRESENT"), false, true)?["status"],
        "not-needed"
    );
    assert!(!f.ctx.state.exists());
    assert!(!f.ctx.home.join(".agents/backups").exists());
    Ok(())
}

#[test]
fn a_rollback_cleared_pending_before_its_final_journal_save_can_finish_later() -> TestResult {
    let f = Fixture::new()?;
    let before = fs::read(&f.skill)?;
    let mut b = batch(&f, "ROLLBACK-FINAL-GAP")?;
    advance(&b, 3)?;
    rollback(&b.root, &b.manifest, &mut b.pending, &b.blobs)?;
    assert!(!b.manifest.pending.exists());
    assert_eq!(b.journal.phase, Phase::Running);
    let operation = b.journal.id.clone();
    drop(b.store);
    let value = crate::operations::recover(&f.ctx, &operation, false, false, true)?;
    assert_eq!(value["status"], "failed");
    assert_eq!(fs::read(&f.skill)?, before);
    let store = Store::lock(&f.ctx)?;
    store.check_pending()?;
    Ok(())
}
#[test]
fn missing_only_declarations_are_attention_before_any_noop_or_business_write() -> TestResult {
    let f = Fixture::new()?;
    fs::remove_file(&f.skill)?;
    assert!(sync(&f.ctx, Some("MISSING"), false, true).is_err());
    assert!(!f.ctx.state.exists());
    assert!(!f.ctx.home.join(".agents/backups").exists());
    Ok(())
}
#[test]
fn an_unregistered_stage_is_preserved_and_cannot_be_reported_as_a_complete_rollback() -> TestResult
{
    let f = Fixture::new()?;
    let mut b = batch(&f, "UNREGISTERED-STAGE")?;
    let orphan = f.skill.parent().ok_or("parent")?.join(".OP-ORPHAN.cap.tmp");
    fs::write(&orphan, b"unregistered-private-stage")?;
    assert!(recover_journal(&f.ctx, &b.store, &mut b.journal, false, true).is_err());
    assert!(b.manifest.pending.exists());
    assert_eq!(fs::read(orphan)?, b"unregistered-private-stage");
    Ok(())
}

fn adopted_receipt(
    f: &Fixture,
) -> std::result::Result<crate::domain::Receipt, Box<dyn std::error::Error>> {
    use crate::domain::{CapabilityKind, QuerySpec, Receipt, Receipts};
    let record = f.ctx.home.join(".agents/.skill-lock.json");
    fs::write(
        &record,
        serde_json::to_vec(
            &json!({"version":3,"skills":{"example":{"source":"owner/repo","sourceType":"github","sourceUrl":"https://github.com/owner/repo","ref":"a".repeat(40),"skillFolderHash":"b".repeat(40)}}}),
        )?,
    )?;
    let id = crate::policy::capability_local::skill_capability_id(
        &record,
        "example",
        "github",
        "owner/repo",
    )?;
    let package = Package {
        id,
        name: "example".into(),
        kind: Kind::Skill,
        official_url: "https://github.com/owner/repo".into(),
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: "example".into(),
            manager: "skills-cli:github".into(),
            record,
            payload: f.skill.parent().ok_or("parent")?.into(),
            source: Some("owner/repo".into()),
            profile_id: None,
            settings: None,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let query = crate::capability_local::capture(&package)?;
    let receipt = Receipt {
        scope: Some(f.ctx.scope.clone()),
        observation: crate::capability_local::observe_default(&package.id, &query)?,
        query: Some(QuerySpec::LocalCapability(query)),
        package,
        operation_id: "OP-ADOPT".into(),
        updated_at: 1,
    };
    let store = Store::lock(&f.ctx)?;
    Store::atomic(
        &store.root,
        "receipts.json",
        &Receipts {
            schema_version: 1,
            installations: std::collections::BTreeMap::from([(
                receipt.package.id.clone(),
                receipt.clone(),
            )]),
        },
    )?;
    Ok(receipt)
}
#[test]
fn partial_commit_recovery_preserves_an_adopted_payload_and_its_full_before_receipt() -> TestResult
{
    let f = Fixture::new()?;
    let original = fs::read(&f.skill)?;
    let receipt = adopted_receipt(&f)?;
    let raw_before = fs::read(f.ctx.state.join("receipts.json"))?;
    let mut b = batch(&f, "ADOPTED-ROLLBACK")?;
    advance(&b, 3)?;
    let overlay = &b.manifest.ledgers[1];
    let candidate = b
        .pending
        .ledgers
        .iter()
        .find(|c| c.target == overlay.path)
        .ok_or("overlay")?;
    files::publish(
        &overlay.namespace,
        &overlay.before,
        &candidate_for(candidate, &b.pending)?,
    )?;
    recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)?;
    assert_eq!(fs::read(&f.skill)?, original);
    assert_eq!(fs::read(f.ctx.state.join("receipts.json"))?, raw_before);
    let crate::domain::QuerySpec::LocalCapability(query) = receipt.query.ok_or("query")? else {
        return Err("query".into());
    };
    assert_eq!(
        crate::capability_local::observe_default(&receipt.package.id, &query)?,
        receipt.observation
    );
    Ok(())
}
#[test]
fn unchanged_file_identity_does_not_hide_external_payload_content_during_rollback() -> TestResult {
    let f = Fixture::new()?;
    let notes = f.skill.parent().ok_or("parent")?.join("notes.txt");
    fs::write(&notes, b"before")?;
    adopted_receipt(&f)?;
    let mut b = batch(&f, "PAYLOAD-DRIFT")?;
    advance(&b, 3)?;
    fs::write(&notes, b"external-content")?;
    let error = recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)
        .err()
        .ok_or("must reject")?;
    assert_eq!(error.code, "capability_before_mismatch");
    assert!(b.manifest.pending.is_file());
    assert_eq!(fs::read(notes)?, b"external-content");
    Ok(())
}
#[test]
fn receipt_scope_or_baseline_drift_blocks_policy_writes_before_live_mutation() -> TestResult {
    let f = Fixture::new()?;
    let mut receipt = adopted_receipt(&f)?;
    let original = fs::read(&f.skill)?;
    fs::write(&f.skill, b"---\nname: example\n---\nexternal\n")?;
    assert_eq!(
        sync(&f.ctx, Some("DRIFT"), false, true)
            .err()
            .ok_or("must reject")?
            .code,
        "capability_baseline_drift"
    );
    fs::write(&f.skill, &original)?;
    receipt.scope.as_mut().ok_or("scope")?.home = "/elsewhere".into();
    let store = Store::lock(&f.ctx)?;
    Store::atomic(
        &store.root,
        "receipts.json",
        &crate::domain::Receipts {
            schema_version: 1,
            installations: std::collections::BTreeMap::from([(
                receipt.package.id.clone(),
                receipt,
            )]),
        },
    )?;
    drop(store);
    assert_eq!(
        sync(&f.ctx, Some("SCOPE"), false, true)
            .err()
            .ok_or("must reject")?
            .code,
        "capability_scope_conflict"
    );
    assert_eq!(fs::read(&f.skill)?, original);
    assert!(
        !f.ctx
            .home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    Ok(())
}
#[test]
fn positional_receipt_roots_cannot_be_treated_as_a_valid_batch_baseline() -> TestResult {
    let f = Fixture::new()?;
    let original = fs::read(&f.skill)?;
    let store = Store::lock(&f.ctx)?;
    Store::atomic_bytes(&store.root, "receipts.json", b"[1,{}]", false)?;
    drop(store);
    assert!(sync(&f.ctx, Some("BAD-RECEIPT"), false, true).is_err());
    assert_eq!(fs::read(&f.skill)?, original);
    assert!(
        !f.ctx
            .home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    Ok(())
}

#[test]
fn receipt_objects_and_nested_scope_package_shapes_are_required_before_any_mutation() -> TestResult
{
    for field in ["row", "scope", "package", "key"] {
        let f = Fixture::new()?;
        let receipt = adopted_receipt(&f)?;
        let value = serde_json::to_value(&receipt)?;
        let mut row = value.clone();
        match field {
            "row" => {
                row = json!([
                    value["scope"],
                    value["query"],
                    value["package"],
                    value["observation"],
                    value["operation_id"],
                    value["updated_at"]
                ]);
            }
            "scope" => {
                row["scope"] =
                    json!([value["scope"]["home"], value["scope"]["paths"], Value::Null]);
            }
            "package" => {
                row["package"] = json!([
                    value["package"]["id"],
                    value["package"]["name"],
                    value["package"]["kind"],
                    value["package"]["official_url"],
                    value["package"]["provider"],
                    value["package"]["paths"],
                    value["package"]["exact_version_required"]
                ]);
            }
            _ => (),
        }
        let id = if field == "key" {
            "capability:wrong"
        } else {
            &receipt.package.id
        };
        let book = json!({"schema_version":1,"installations":{id:row}});
        let store = Store::lock(&f.ctx)?;
        Store::atomic(&store.root, "receipts.json", &book)?;
        drop(store);
        let original = fs::read(&f.skill)?;
        assert!(
            sync(&f.ctx, Some("INVALID-SHAPE"), false, true).is_err(),
            "{field}"
        );
        assert_eq!(fs::read(&f.skill)?, original);
        assert!(
            !f.ctx
                .home
                .join(".agents/invocation-policy.pending.json")
                .exists()
        );
    }
    Ok(())
}

#[test]
fn corrupt_private_transaction_evidence_never_rebinds_or_clears_pending() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    for fault in [
        "manifest-bytes",
        "manifest-mode",
        "backup-mode",
        "pending-json",
        "pending-shape",
        "pending-binding",
    ] {
        let f = Fixture::new()?;
        let b = batch(&f, "CORRUPT")?;
        let manifest = b.root.path.join("capability-plan.json");
        match fault {
            "manifest-bytes" => fs::write(manifest, b"{}")?,
            "manifest-mode" => fs::set_permissions(manifest, fs::Permissions::from_mode(0o644))?,
            "backup-mode" => fs::set_permissions(&b.root.path, fs::Permissions::from_mode(0o755))?,
            "pending-json" => fs::write(&b.manifest.pending, b"{")?,
            "pending-shape" => fs::write(&b.manifest.pending, b"[]")?,
            "pending-binding" => {
                let mut pending = b.pending.clone();
                pending.manifest_sha256 = "f".repeat(64);
                save_pending(&b.root, &b.manifest, &pending)?;
            }
            _ => return Err("unknown fault".into()),
        }
        let before = fs::read(&f.skill)?;
        assert!(load(&b.store, &b.journal).is_err(), "{fault}");
        assert_eq!(fs::read(&f.skill)?, before);
        assert!(b.manifest.pending.exists());
    }
    Ok(())
}
#[test]
fn a_preparation_backup_failure_records_unknown_and_never_touches_live_metadata() -> TestResult {
    let f = Fixture::new()?;
    let mut b = batch(&f, "PREPARE-FAILURE")?;
    let after = b.manifest.files[0].after_blob.as_ref().ok_or("blob")?;
    fs::write(b.blobs.directory.path.join(&after.sha256), b"corrupt")?;
    let original = fs::read(&f.skill)?;
    assert!(
        execute(
            &b.store,
            &b.root,
            &b.blobs,
            &b.manifest,
            &mut b.pending,
            &mut b.journal
        )
        .is_err()
    );
    assert_eq!(b.journal.phase, Phase::Unknown);
    assert_eq!(fs::read(&f.skill)?, original);
    assert!(b.manifest.pending.is_file());
    let active: Journal = Store::read(&b.store.root, "active.json")?.ok_or("active")?;
    assert_eq!(active.phase, Phase::Unknown);
    Ok(())
}
#[test]
fn no_pending_cannot_finish_a_batch_with_unpublished_staging_or_missing_retained_evidence()
-> TestResult {
    for corrupt in [false, true] {
        let f = Fixture::new()?;
        let mut b = batch(&f, "NO-PENDING-UNSETTLED")?;
        clear_pending(&b.manifest, &b.pending)?;
        if corrupt {
            fs::write(b.root.path.join("capability-candidates.json"), b"[]")?;
        }
        assert!(recover_journal(&f.ctx, &b.store, &mut b.journal, false, true).is_err());
        assert_eq!(b.journal.phase, Phase::Running);
    }
    Ok(())
}
#[test]
fn duplicate_change_ids_do_not_replace_immutable_backups() -> TestResult {
    let f = Fixture::new()?;
    let authority = crate::governance::capture(&f.ctx)?;
    let root = backup(&authority, "EXCLUSIVE")?;
    Store::atomic_bytes(root.file(), "marker", b"retained", true)?;
    assert!(backup(&authority, "EXCLUSIVE").is_err());
    assert_eq!(fs::read(root.path.join("marker"))?, b"retained");
    Ok(())
}

#[test]
fn policy_recovery_matches_change_id_and_previews_the_frozen_operation_without_writes() -> TestResult
{
    let f = Fixture::new()?;
    let b = batch(&f, "SELECTED")?;
    let raw = fs::read(&b.manifest.pending)?;
    let error = recover_policy(&f.ctx, Some("OTHER"), false, true)
        .err()
        .ok_or("must reject")?;
    assert_eq!(error.code, "capability_change_id_mismatch");
    let value = recover_policy(&f.ctx, Some("SELECTED"), true, true)?;
    assert_eq!(value["mode"], "dry-run");
    assert_eq!(value["persistentWrites"], false);
    assert_eq!(fs::read(&b.manifest.pending)?, raw);
    let mut invalid = b.pending.clone();
    invalid.operation_id = "../outside".into();
    save_pending(&b.root, &b.manifest, &invalid)?;
    assert!(recover_policy(&f.ctx, Some("SELECTED"), false, true).is_err());
    Ok(())
}
#[test]
fn replaced_or_repermissioned_candidate_parents_are_never_adopted_during_recovery() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    for replace in [false, true] {
        let f = Fixture::new()?;
        let mut b = batch(&f, "PARENT-CHANGED")?;
        advance(&b, 1)?;
        let parent = &b.pending.parents[0].target;
        if replace {
            fs::rename(parent, parent.with_file_name("retained-parent"))?;
            fs::create_dir(parent)?;
            fs::write(parent.join("external"), b"keep")?;
        } else {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o755))?;
        }
        assert!(recover_journal(&f.ctx, &b.store, &mut b.journal, false, true).is_err());
        assert!(b.manifest.pending.exists());
        if replace {
            assert_eq!(fs::read(parent.join("external"))?, b"keep");
        }
    }
    Ok(())
}

#[test]
fn final_input_rejection_must_keep_the_commit_marker_until_after_validation() -> TestResult {
    let f = Fixture::new()?;
    let b = batch(&f, "FINAL-INPUT-CAS")?;
    advance(&b, 4)?;
    let original = fs::read(&b.manifest.pending)?;
    let result = clear_pending_checked(&b.manifest, &b.pending, || {
        fs::write(
            f.ctx
                .settings
                .join("manifests/agent-invocation-policy.json"),
            b"external-after-private-preparation",
        )?;
        guard::verify(&b.manifest, &b.pending)
    });
    assert!(result.is_err());
    assert!(
        b.manifest.pending.is_file(),
        "final input rejection must preserve pending"
    );
    assert_eq!(fs::read(&b.manifest.pending)?, original);
    Ok(())
}

#[test]
fn owned_path_records_in_receipts_require_objects_instead_of_positional_arrays() -> TestResult {
    let f = Fixture::new()?;
    let receipt = adopted_receipt(&f)?;
    let mut row = serde_json::to_value(&receipt)?;
    row["package"]["paths"] = json!([[f.skill.parent().ok_or("parent")?, "payload"]]);
    let book = json!({"schema_version":1,"installations":{&receipt.package.id:row}});
    let store = Store::lock(&f.ctx)?;
    Store::atomic(&store.root, "receipts.json", &book)?;
    drop(store);
    let original = fs::read(&f.skill)?;
    assert!(sync(&f.ctx, Some("INVALID-OWNED-PATH"), false, true).is_err());
    assert_eq!(fs::read(&f.skill)?, original);
    assert!(
        !f.ctx
            .home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    Ok(())
}

fn link_request(all: bool, change: Option<&str>) -> crate::cli::SkillSelection {
    crate::cli::SkillSelection {
        name: None,
        only: vec![],
        all,
        revision: None,
        change_id: change.map(str::to_owned),
    }
}
#[test]
fn native_link_batch_creates_missing_parent_retires_owned_links_and_keeps_model_bytes() -> TestResult
{
    let f = Fixture::new()?;
    let profile = f.ctx.home.join(".claude-mi");
    fs::create_dir(&profile)?;
    let config = profile.join("settings.json");
    fs::write(
        &config,
        b"{\"model\":\"private-model\",\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"SECRET_VALUE\"}}",
    )?;
    let model = fs::read(&config)?;
    let dry = sync_links(&f.ctx, &link_request(true, Some("LINK-DRY")), true, true)?;
    assert_eq!(dry["planned_changes"], 1);
    assert!(!f.ctx.state.exists());
    assert!(!profile.join("skills").exists());
    let value = sync_links(
        &f.ctx,
        &link_request(true, Some("LINK-CREATE")),
        false,
        true,
    )?;
    assert_eq!(value["status"], "succeeded");
    assert_eq!(value["incomplete"], false);
    let path = profile.join("skills/example");
    assert_eq!(
        fs::read_link(&path)?,
        Path::new("../../.agents/skills/example")
    );
    assert_eq!(fs::read(&config)?, model);
    assert_eq!(
        sync_links(&f.ctx, &link_request(true, Some("LINK-NOOP")), false, true)?["status"],
        "same"
    );
    let rules = f.ctx.settings.join("manifests/skill-profiles.json");
    fs::write(rules, br#"{"claude":{"mi":{"links":[]}}}"#)?;
    let value = sync_links(
        &f.ctx,
        &link_request(true, Some("LINK-RETIRE")),
        false,
        true,
    )?;
    assert_eq!(value["status"], "succeeded");
    assert!(fs::symlink_metadata(path).is_err());
    assert_eq!(fs::read(config)?, model);
    assert!(
        !f.ctx
            .home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    Ok(())
}

fn link_batch(f: &Fixture, change: &str) -> std::result::Result<Batch, Box<dyn std::error::Error>> {
    let store = Store::lock(&f.ctx)?;
    let authority = crate::governance::capture(&f.ctx)?;
    let plan =
        crate::link_inventory::assess(&f.ctx, &authority, crate::policy::delivery::Selection::All)?;
    let root = backup(&authority, change)?;
    let blobs = files::Blobs::create(&root)?;
    let operation = crate::store::operation_id();
    let manifest = links_manifest(&f.ctx, &authority, &plan, &operation, change, &blobs)?;
    let raw = bytes(&manifest)?;
    let digest = files::hash(&raw);
    Store::atomic_bytes(root.file(), "capability-plan.json", &raw, true)?;
    let mut journal = journal(
        &f.ctx,
        &manifest,
        &root.path.join("capability-plan.json"),
        &digest,
    )?;
    store.save(&journal)?;
    let mut pending = empty_pending(&manifest, &digest);
    save_pending(&root, &manifest, &pending)?;
    prepare(&root, &manifest, &blobs, &mut pending)?;
    journal.phase = Phase::Running;
    store.save(&journal)?;
    Ok(Batch {
        store,
        root,
        blobs,
        manifest,
        pending,
        journal,
    })
}
fn link_profile(f: &Fixture, retire: bool) -> TestResult {
    let profile = f.ctx.home.join(".claude-mi");
    fs::create_dir(&profile)?;
    fs::write(profile.join("settings.json"), br#"{"model":"isolated"}"#)?;
    if retire {
        fs::create_dir(profile.join("skills"))?;
        std::os::unix::fs::symlink(
            "../../.agents/skills/example",
            profile.join("skills/example"),
        )?;
        fs::write(
            f.ctx.settings.join("manifests/skill-profiles.json"),
            br#"{"claude":{"mi":{"links":[]}}}"#,
        )?;
    }
    Ok(())
}
#[test]
fn v3_link_creation_recovers_at_parent_publication_payload_and_log_boundaries() -> TestResult {
    for step in 0..4 {
        let f = Fixture::new()?;
        link_profile(&f, false)?;
        let mut b = link_batch(&f, &format!("LINK-CRASH-{step}"))?;
        if step >= 1 {
            for parent in &b.pending.parents {
                files::publish_parent(parent)?;
            }
        }
        if step >= 2 {
            for (file, candidate) in b.manifest.files.iter().zip(&b.pending.files) {
                files::publish_link(
                    &f.ctx.home,
                    &file.namespace,
                    &file.before,
                    &candidate_for(candidate, &b.pending)?,
                )?;
            }
        }
        if step >= 3 {
            clear_pending(&b.manifest, &b.pending)?;
        }
        let mut recovery = f.ctx.clone();
        recovery.cache = f.ctx.home.join("different-cache");
        recovery.scope.paths.insert(
            "XDG_CACHE_HOME".into(),
            Some(f.ctx.home.join("another-cache")),
        );
        recovery.scope.paths.insert(
            "NPM_CONFIG_PREFIX".into(),
            Some(f.ctx.home.join("unused-npm-prefix")),
        );
        recover_journal(&recovery, &b.store, &mut b.journal, false, true)?;
        let target = f.ctx.home.join(".claude-mi/skills/example");
        if step < 2 {
            assert_eq!(b.journal.phase, Phase::Failed);
            assert!(!target.exists());
            assert!(!target.parent().ok_or("parent")?.exists());
        } else {
            assert_eq!(b.journal.phase, Phase::Succeeded);
            assert_eq!(
                fs::read_link(target)?,
                Path::new("../../.agents/skills/example")
            );
        }
        assert!(!b.manifest.pending.exists());
    }
    Ok(())
}
#[test]
fn link_retirement_rollback_uses_prepared_mode_and_recognizes_its_new_inode_on_retry() -> TestResult
{
    let f = Fixture::new()?;
    link_profile(&f, true)?;
    let mut b = link_batch(&f, "LINK-RETIRE-ROLLBACK")?;
    let file = &b.manifest.files[0];
    let candidate = &b.pending.files[0];
    let original = file.before.clone();
    files::publish_link(&f.ctx.home, &file.namespace, &file.before, candidate)?;
    rollback_one(
        &b.root,
        &b.manifest,
        &mut b.pending,
        &b.blobs,
        &file.namespace,
        &file.before,
        file.before_blob.as_ref(),
    )?;
    let restored = files::observe(&file.namespace, 4096)?.0;
    assert_ne!(restored, original);
    assert!(same_before_content(&original, &restored));
    assert!(before_state(
        &file.path,
        &file.before,
        &restored,
        &b.pending
    ));
    cleanup(&b.root, &b.manifest, &mut b.pending)?;
    guard::verify(&b.manifest, &b.pending)?;
    clear_pending(&b.manifest, &b.pending)?;
    recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)?;
    assert_eq!(b.journal.phase, Phase::Failed);
    assert!(same_before_content(
        &original,
        &files::observe(&file.namespace, 4096)?.0
    ));
    Ok(())
}

fn reseal_fixture(b: &mut Batch) -> Result<()> {
    let raw = bytes(&b.manifest)?;
    let digest = files::hash(&raw);
    Store::atomic_bytes(b.root.file(), "capability-plan.json", &raw, false)?;
    b.pending.manifest_sha256 = digest.clone();
    let Provider::Governance {
        manifest_sha256, ..
    } = &mut b.journal.plan.package.provider
    else {
        return Err(invalid());
    };
    *manifest_sha256 = digest.clone();
    b.journal.plan.target = Some(digest);
    b.journal.plan.before = Observation::Governance {
        fingerprint: fingerprint(&b.manifest.inputs)?,
    };
    b.journal.plan.recipe_sha256 = fingerprint(&b.journal.plan.package)?;
    b.journal.plan_sha256 = fingerprint(&b.journal.plan)?;
    b.store.save(&b.journal)?;
    save_pending(&b.root, &b.manifest, &b.pending)
}

fn legacy_policy_version(b: &mut Batch) -> Result<()> {
    let recipe_book = policy::recipe_book_witness(&b.manifest)?.path.clone();
    b.manifest.inputs.retain(|input| input.path != recipe_book);
    b.manifest.schema_version = 2;
    b.pending.schema_version = 2;
    Ok(())
}

#[test]
fn original_v2_codex_policy_wire_and_mode_less_readonly_links_remain_fully_recoverable()
-> TestResult {
    for step in [0, 3, 4, 5] {
        let f = Fixture::new()?;
        let legacy = f.ctx.home.join("readonly-legacy-link");
        std::os::unix::fs::symlink("../../.agents//skills/example", &legacy)?;
        let mut b = batch(&f, &format!("V2-WIRE-{step}"))?;
        legacy_policy_version(&mut b)?;
        let (mut old, _) = witness(&legacy)?;
        let State::Link { mode, .. } = &mut old.state else {
            return Err("expected link".into());
        };
        *mode = None;
        b.manifest.inputs.push(old);
        reseal_fixture(&mut b)?;
        let raw = fs::read(b.root.path.join("capability-plan.json"))?;
        let value: Value = serde_json::from_slice(&raw)?;
        assert!(
            value["files"]
                .as_array()
                .ok_or("files")?
                .iter()
                .any(|f| f["kind"] == "codex-policy")
        );
        assert!(
            value["inputs"]
                .as_array()
                .ok_or("inputs")?
                .iter()
                .find(|r| r["path"] == json!(legacy))
                .ok_or("legacy")?["state"]
                .get("mode")
                .is_none()
        );
        advance(&b, step)?;
        recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)?;
        assert_eq!(
            b.journal.phase,
            if step < 4 {
                Phase::Failed
            } else {
                Phase::Succeeded
            }
        );
        assert!(!b.manifest.pending.exists());
    }
    Ok(())
}

#[test]
fn fixed_recipe_book_witness_is_unique_and_bound_to_the_recovery_config_root() -> TestResult {
    let f = Fixture::new()?;
    let mut b = batch(&f, "RECIPE-BOOK-SCOPE")?;
    let book_path = crate::native_skills::book_path(&f.ctx);
    assert_eq!(b.manifest.schema_version, policy::POLICY_MANIFEST_VERSION);
    let book = policy::recipe_book_witness(&b.manifest)?.clone();
    assert_eq!(book.path, book_path);
    assert_eq!(book.state, State::Missing);

    let mut invalid_manifest = b.manifest.clone();
    invalid_manifest
        .inputs
        .retain(|input| input.path != book_path);
    assert!(policy::validate_manifest(&invalid_manifest).is_err());
    invalid_manifest = b.manifest.clone();
    invalid_manifest.inputs.push(book.clone());
    assert!(policy::validate_manifest(&invalid_manifest).is_err());
    let alias = f.ctx.home.join("different-book");
    std::os::unix::fs::symlink(&f.skill, &alias)?;
    let (alias, _) = witness(&alias)?;
    invalid_manifest.inputs.pop();
    invalid_manifest
        .inputs
        .iter_mut()
        .find(|input| input.path == book_path)
        .ok_or("book witness")?
        .state = alias.state;
    assert!(policy::validate_manifest(&invalid_manifest).is_err());

    let before = fs::read(&b.manifest.pending)?;
    let mut different = f.ctx.clone();
    different.config = f.ctx.home.join("different-config");
    let error = recover_journal(&different, &b.store, &mut b.journal, false, true)
        .err()
        .ok_or("changed recipe book scope")?;
    assert_eq!(error.code, "capability_recovery_scope_changed");
    assert_eq!(fs::read(&b.manifest.pending)?, before);
    Ok(())
}

#[test]
fn legacy_policy_recovery_preserves_pending_when_a_fixed_recipe_owns_a_planned_target() -> TestResult
{
    let f = Fixture::new()?;
    let mut b = batch(&f, "FIXED-RECIPE-LEGACY")?;
    legacy_policy_version(&mut b)?;
    reseal_fixture(&mut b)?;
    advance(&b, 2)?;
    declare_example_recipe(&f.ctx)?;
    let marker = fs::read(&b.manifest.pending)?;
    let before = fs::read(&f.skill)?;
    let plan = fs::read(b.root.path.join("capability-plan.json"))?;
    let error = recover_journal(&f.ctx, &b.store, &mut b.journal, false, true)
        .err()
        .ok_or("fixed recipe conflict")?;
    assert_eq!(error.code, "fixed_skill_recipe_conflict");
    assert_eq!(fs::read(&b.manifest.pending)?, marker);
    assert_eq!(fs::read(&f.skill)?, before);
    assert_eq!(fs::read(b.root.path.join("capability-plan.json"))?, plan);
    Ok(())
}

fn declare_example_recipe(ctx: &Context) -> TestResult {
    let book = crate::native_skills::book_path(ctx);
    fs::create_dir_all(&ctx.config)?;
    fs::write(
        book,
        serde_json::to_vec(&json!({"schemaVersion":1,"recipes":[{
            "schemaVersion":1,"id":"example","owner":"skills-cli","cliVersion":"1.9.0",
            "source":{"repository":"owner/repo","commit":"a".repeat(40),"skillDirectory":"skills/example","memberName":"example","trackingRef":null},
            "installName":"example","scope":"global","clientScope":["codex"],"copy":true,"manualOnly":false,"licenseFiles":[],"derived":null,
            "runner":{"node":"/fixture/node","entry":"/fixture/skills/bin/cli.mjs","sha256":"f".repeat(64)}
        }]}))?,
    )?;
    Ok(())
}

#[test]
fn reserved_legacy_policy_rows_are_preserved_when_a_recipe_owns_the_same_metadata() -> TestResult {
    let f = Fixture::new()?;
    sync(&f.ctx, Some("OLD-OVERLAY"), false, true)?;
    let overlay = f.ctx.home.join(".agents/invocation-overlays.json");
    let before: Value = serde_json::from_slice(&fs::read(&overlay)?)?;
    declare_example_recipe(&f.ctx)?;
    let recipe_metadata = b"---\nname: example\n---\nfixed recipe payload\n";
    fs::write(&f.skill, recipe_metadata)?;
    fs::write(
        f.ctx
            .settings
            .join("manifests/agent-invocation-policy.json"),
        br#"{"schema_version":1,"standalone_manual_only":[]}"#,
    )?;
    let result = sync(&f.ctx, Some("KEEP-OLD-ROW"), false, true)?;
    assert_eq!(result["status"], "succeeded");
    assert_eq!(result["changed"], 0);
    let after: Value = serde_json::from_slice(&fs::read(overlay)?)?;
    assert_eq!(after["files"], before["files"]);
    assert_eq!(fs::read(&f.skill)?, recipe_metadata);
    Ok(())
}

#[test]
fn all_selection_assesses_configuration_and_completes_when_no_fields_are_declared() -> TestResult {
    let f = Fixture::new()?;
    link_profile(&f, false)?;
    let result = sync_links(&f.ctx, &link_request(true, None), true, true)?;
    assert_eq!(result["incomplete"], false);
    assert_eq!(result["profile_config_assessed"], true);
    assert_eq!(result["operation"], "delivery");
    assert!(!f.ctx.state.exists());
    Ok(())
}

#[test]
fn profile_configuration_and_metadata_policies_keep_each_others_independent_overlay_records()
-> TestResult {
    let f = Fixture::new()?;
    let profile = f.ctx.home.join(".claude-mi");
    fs::create_dir(&profile)?;
    let config = profile.join("settings.json");
    let model = br#"{"model":"private-\u006dodel"}"#;
    fs::write(&config, model)?;
    let rules = f.ctx.settings.join("manifests/skill-profiles.json");
    fs::write(&rules, br#"{"claude":{"mi":{"plugins_off":["p@vendor"]}}}"#)?;
    let request = crate::cli::SkillSelection {
        only: vec!["profile:mi".into()],
        ..link_request(false, None)
    };
    assert_eq!(
        sync_links(&f.ctx, &request, false, true)?["status"],
        "succeeded"
    );
    let book_path = f.ctx.home.join(".agents/invocation-overlays.json");
    let before: Value = serde_json::from_slice(&fs::read(&book_path)?)?;
    assert_eq!(before["schema_version"], 2);
    assert_eq!(
        sync(&f.ctx, Some("AFTER-PROFILE"), false, true)?["status"],
        "succeeded"
    );
    let after: Value = serde_json::from_slice(&fs::read(&book_path)?)?;
    assert_eq!(after["schema_version"], 2);
    assert_eq!(after["profiles"], before["profiles"]);
    assert_eq!(after["profile_policy_hash"], before["profile_policy_hash"]);
    let metadata = after["files"].clone();
    assert_eq!(metadata.as_object().ok_or("metadata")?.len(), 2);
    fs::write(rules, b"{}")?;
    assert_eq!(
        sync_links(&f.ctx, &request, false, true)?["status"],
        "succeeded"
    );
    let retired: Value = serde_json::from_slice(&fs::read(&book_path)?)?;
    assert_eq!(retired["files"], metadata);
    assert!(
        crate::policy::invocation::record(&f.ctx.home, &fs::read(&book_path)?)?
            .profiles
            .is_empty()
    );
    assert_eq!(
        crate::policy::json::parse(&fs::read(config)?)?,
        crate::policy::json::parse(model)?
    );
    Ok(())
}

#[test]
fn profile_configuration_and_links_converge_without_claiming_another_profiles_fields() -> TestResult
{
    let f = Fixture::new()?;
    let primary = f.ctx.home.join(".claude-mi/settings.json");
    let secondary = f.ctx.home.join(".claude-kimi/settings.json");
    for path in [&primary, &secondary] {
        fs::create_dir_all(path.parent().ok_or("profile")?)?;
    }
    let model = r#""model" : "private-\u006di""#;
    let before = format!(
        r#"{{ {model}, "enabledPlugins":{{"shared@vendor":true,"foreign@vendor":true}}, "env":{{"KEY":"SYNTHETIC_PRIVATE"}} }}"#
    );
    let other = br#"{"model":"private-kimi","enabledPlugins":{"shared@vendor":true}}"#;
    fs::write(&primary, &before)?;
    fs::write(&secondary, other)?;
    let rules = f.ctx.settings.join("manifests/skill-profiles.json");
    fs::write(
        &rules,
        br#"{"claude":{"mi":{"links":["example"],"plugins_off":["shared@vendor"]},"kimi":{"links":[],"plugins_off":["shared@vendor"]}}}"#,
    )?;
    let request = crate::cli::SkillSelection {
        only: vec!["profile:mi".into(), "example".into()],
        ..link_request(false, None)
    };
    let link = f.ctx.home.join(".claude-mi/skills/example");
    let preview = sync_links(&f.ctx, &request, true, true)?;
    assert_eq!(preview["planned_changes"], 2);
    assert_eq!(
        preview["pending_config_policies"],
        json!([{"intent":"claude:kimi:plugins_off","reason":"not_selected"}])
    );
    assert!(!preview.to_string().contains("SYNTHETIC_PRIVATE"));
    assert!(!f.ctx.state.exists());
    assert!(!link.exists());
    assert_eq!(fs::read_to_string(&primary)?, before);

    let result = sync_links(&f.ctx, &request, false, true)?;
    assert_eq!(result["status"], "succeeded");
    let configured = fs::read_to_string(&primary)?;
    assert!(configured.contains(model));
    let current = crate::policy::json::parse(configured.as_bytes())?;
    assert_eq!(
        current["enabledPlugins"],
        json!({"shared@vendor":false,"foreign@vendor":true})
    );
    assert_eq!(current["env"], json!({"KEY":"SYNTHETIC_PRIVATE"}));
    assert_eq!(fs::read(&secondary)?, other);
    assert_eq!(
        fs::read_link(&link)?,
        Path::new("../../.agents/skills/example")
    );
    let unchanged = sync_links(&f.ctx, &request, false, true)?;
    assert_eq!(unchanged["status"], "same");
    assert_eq!(unchanged["persistentWrites"], false);

    fs::write(
        rules,
        br#"{"claude":{"mi":{"links":["example"]},"kimi":{"links":[],"plugins_off":["shared@vendor"]}}}"#,
    )?;
    let retire = crate::cli::SkillSelection {
        only: vec!["profile:mi".into()],
        ..link_request(false, None)
    };
    assert_eq!(
        sync_links(&f.ctx, &retire, false, true)?["status"],
        "succeeded"
    );
    let restored = fs::read_to_string(&primary)?;
    assert!(restored.contains(model));
    assert_eq!(
        crate::policy::json::parse(restored.as_bytes())?,
        crate::policy::json::parse(before.as_bytes())?
    );
    assert_eq!(fs::read(secondary)?, other);
    assert_eq!(
        fs::read_link(link)?,
        Path::new("../../.agents/skills/example")
    );
    Ok(())
}

#[test]
fn a_missing_selected_link_stops_the_whole_batch_before_profile_configuration() -> TestResult {
    let f = Fixture::new()?;
    link_profile(&f, false)?;
    let rules = f.ctx.settings.join("manifests/skill-profiles.json");
    fs::write(
        rules,
        br#"{"claude":{"mi":{"links":["missing-skill"],"plugins_off":["shared@vendor"]}}}"#,
    )?;
    let settings = f.ctx.home.join(".claude-mi/settings.json");
    let original = fs::read(&settings)?;
    let request = link_request(true, None);
    let preview = sync_links(&f.ctx, &request, true, true)?;
    assert_eq!(preview["incomplete"], true);
    assert_eq!(preview["planned_changes"], 1);
    let error = sync_links(&f.ctx, &request, false, true)
        .err()
        .ok_or("batch must reject a missing target")?;
    assert_eq!(error.code, "link_targets_missing");
    assert_eq!(fs::read(settings)?, original);
    assert!(!f.ctx.home.join(".claude-mi/skills").exists());
    assert!(!f.ctx.home.join(".agents/invocation-overlays.json").exists());
    assert!(!f.ctx.state.exists());
    Ok(())
}

#[test]
fn configuration_only_sync_creates_governance_storage_for_an_unmanaged_profile() -> TestResult {
    let f = Fixture::new()?;
    fs::rename(
        f.ctx.home.join(".agents"),
        f.ctx.home.join("fixture-archive"),
    )?;
    link_profile(&f, false)?;
    fs::write(
        f.ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"plugins_off":["shared@vendor"]}}}"#,
    )?;
    let request = crate::cli::SkillSelection {
        only: vec!["profile:mi".into()],
        ..link_request(false, Some("PROFILE-FIRST"))
    };
    let preview = sync_links(&f.ctx, &request, true, true)?;
    assert_eq!(preview["planned_changes"], 1);
    assert!(!f.ctx.home.join(".agents").exists());
    assert!(!f.ctx.state.exists());

    let result = sync_links(&f.ctx, &request, false, true)?;
    assert_eq!(result["status"], "succeeded");
    let agents = f.ctx.home.join(".agents");
    let control = agents.join("invocation-overlays.json");
    let configured = fs::read(f.ctx.home.join(".claude-mi/settings.json"))?;
    let config = crate::policy::json::parse(&configured)?;
    assert_eq!(config["model"], "isolated");
    assert_eq!(config["enabledPlugins"]["shared@vendor"], false);
    let book = crate::policy::invocation::record(&f.ctx.home, &fs::read(control)?)?;
    assert!(!book.profiles.is_empty());
    assert!(
        agents
            .join("backups/PROFILE-FIRST/capability-plan.json")
            .is_file()
    );
    assert!(!agents.join("invocation-policy.pending.json").exists());
    assert!(!agents.join("skills").exists());
    assert_eq!(sync_links(&f.ctx, &request, false, true)?["status"], "same");
    Ok(())
}

#[test]
fn interrupted_link_rollback_reuses_its_recorded_stage_after_restart() -> TestResult {
    let f = Fixture::new()?;
    link_profile(&f, true)?;
    let sibling_skill = f.ctx.home.join(".agents/skills/sibling");
    fs::create_dir(&sibling_skill)?;
    fs::write(
        sibling_skill.join("SKILL.md"),
        "---\nname: sibling\n---\nbody\n",
    )?;
    let sibling_link = f.ctx.home.join(".claude-mi/skills/sibling");
    let sibling_text = Path::new("../../.agents/skills/sibling");
    std::os::unix::fs::symlink(sibling_text, &sibling_link)?;
    let mut b = link_batch(&f, "LINK-ROLLBACK-STAGED")?;
    assert_eq!(b.manifest.files.len(), 2);
    let change = b
        .manifest
        .files
        .iter()
        .find(|file| file.path.ends_with("example"))
        .ok_or("example")?;
    let namespace = change.namespace.clone();
    let original = change.before.clone();
    let link = change.path.clone();
    let settings = f.ctx.home.join(".claude-mi/settings.json");
    let configuration = fs::read(&settings)?;
    let candidate = b
        .pending
        .files
        .iter()
        .find(|candidate| candidate.target == link)
        .ok_or("example candidate")?;
    files::publish_link(&f.ctx.home, &namespace, &original, candidate)?;
    let staged =
        link_rollback_candidate(&b.root, &b.manifest, &mut b.pending, &namespace, &original)?;
    let marker_path = b.manifest.pending.clone();
    let marker = fs::read(&marker_path)?;
    let stage_path = staged.staged.ok_or("prepared rollback link")?;
    let op = b.journal.id.clone();
    drop(b.store);
    let refused = crate::operations::recover(&f.ctx, &op, false, true, true)
        .err()
        .ok_or("cancellation must keep the original recovery pending")?;
    assert_eq!(refused.code, "capability_pending_cannot_abandon");
    assert_eq!(fs::read(&marker_path)?, marker);
    assert_eq!(
        fs::read_link(&stage_path)?,
        Path::new("../../.agents/skills/example")
    );
    assert!(fs::symlink_metadata(&link).is_err());

    let result = crate::operations::recover(&f.ctx, &op, false, false, true)?;
    assert_eq!(result["status"], "failed");
    assert_eq!(files::observe(&namespace, 4096)?.0, staged.after);
    assert_eq!(
        fs::read_link(&link)?,
        Path::new("../../.agents/skills/example")
    );
    assert_eq!(fs::read(settings)?, configuration);
    assert_eq!(fs::read_link(sibling_link)?, sibling_text);
    assert!(fs::symlink_metadata(stage_path).is_err());
    assert!(!marker_path.exists());
    assert_eq!(
        crate::operations::recover(&f.ctx, &op, false, false, true)?["phase"],
        "failed"
    );
    assert_eq!(files::observe(&namespace, 4096)?.0, staged.after);
    Ok(())
}

#[test]
fn rollback_after_retiring_a_created_link_directory_can_finish_without_recreating_it() -> TestResult
{
    let f = Fixture::new()?;
    link_profile(&f, false)?;
    let mut b = link_batch(&f, "ROLLBACK-PARENT-REMOVED")?;
    let parent = &b.pending.parents[0];
    let target = parent.target.clone();
    let staged = parent.staged.clone();
    let settings = f.ctx.home.join(".claude-mi/settings.json");
    let original_settings = fs::read(&settings)?;
    files::publish_parent(parent)?;
    assert!(target.is_dir());
    cleanup(&b.root, &b.manifest, &mut b.pending)?;
    assert_eq!(b.pending.retired_parents, std::slice::from_ref(&target));
    assert!(b.manifest.pending.is_file());
    assert!(!target.exists());
    let op = b.journal.id.clone();
    drop(b.store);

    let result = crate::operations::recover(&f.ctx, &op, false, false, true)?;
    assert_eq!(result["status"], "failed");
    assert!(!b.manifest.pending.exists());
    assert!(!target.exists());
    assert!(!staged.exists());
    assert_eq!(fs::read(settings)?, original_settings);
    Ok(())
}

#[test]
fn an_owned_probe_with_the_wrong_mode_is_not_a_completed_before_state() -> TestResult {
    let f = Fixture::new()?;
    link_profile(&f, true)?;
    let mut b = link_batch(&f, "WRONG-MODE-PROBE")?;
    let file = &b.manifest.files[0];
    let mut current = file.before.clone();
    let State::Link {
        mode: Some(mode), ..
    } = &mut current
    else {
        return Err("link mode".into());
    };
    *mode ^= 0o040;
    b.pending.rollbacks.push(Candidate {
        target: file.path.clone(),
        staged: None,
        after: current.clone(),
    });
    assert!(!before_state(
        &file.path,
        &file.before,
        &current,
        &b.pending
    ));
    assert!(!same_before_content(&file.before, &current));
    Ok(())
}
#[test]
fn a_governance_journal_cannot_recover_another_categorys_manifest() -> TestResult {
    let f = Fixture::new()?;
    let mut b = batch(&f, "CROSS-CATEGORY")?;
    let Provider::Governance { category, .. } = &mut b.journal.plan.package.provider else {
        return Err("provider".into());
    };
    *category = GovernanceCategory::Links;
    b.journal.plan.package.id = "governance:links".into();
    b.journal.plan.recipe_sha256 = fingerprint(&b.journal.plan.package)?;
    b.journal.plan_sha256 = fingerprint(&b.journal.plan)?;
    crate::store::validate_journal(&b.journal)?;
    assert!(load(&b.store, &b.journal).is_err());
    assert!(b.manifest.pending.exists());
    Ok(())
}

#[test]
fn recovery_rejects_a_changed_settings_root_or_used_native_source_scope() -> TestResult {
    for native in [false, true] {
        let mut f = Fixture::new()?;
        link_profile(&f, false)?;
        let old = f.ctx.home.join("native-state-original");
        if native {
            fs::create_dir_all(old.join("skills"))?;
            fs::write(old.join("skills/.skill-lock.json"),br#"{"version":3,"skills":{"example":{"source":"owner/repo","sourceType":"github"}}}"#)?;
            f.ctx.scope.paths.insert("XDG_STATE_HOME".into(), Some(old));
        }
        let mut b = link_batch(&f, "RECOVERY-SCOPE")?;
        let before = fs::read(&b.manifest.pending)?;
        let mut changed = f.ctx.clone();
        if native {
            changed.scope.paths.insert(
                "XDG_STATE_HOME".into(),
                Some(f.ctx.home.join("native-state-other")),
            );
        } else {
            changed.settings = f.ctx.home.join("other-settings");
            fs::create_dir_all(&changed.settings)?;
        }
        let error = recover_journal(&changed, &b.store, &mut b.journal, false, true)
            .err()
            .ok_or("scope must reject")?;
        assert_eq!(error.code, "capability_recovery_scope_changed");
        assert_eq!(fs::read(&b.manifest.pending)?, before);
        assert!(!f.ctx.home.join(".claude-mi/skills").exists());
    }
    Ok(())
}

// Physical fault injection can erase pending in deliberately unsettled states.
fn clear_pending(manifest: &Manifest, pending: &Pending) -> Result<()> {
    clear_pending_checked(manifest, pending, || guard::verify(manifest, pending))
}
