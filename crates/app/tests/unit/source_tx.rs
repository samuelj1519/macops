#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{fs, os::unix::fs::PermissionsExt};
fn adopt(ctx: &Context, key: &str, dry_run: bool, yes: bool) -> Result<Option<Value>> {
    super::adopt(
        ctx,
        crate::cli::AdoptTarget { key, exact: false },
        dry_run,
        yes,
    )
}
fn context(home: &std::path::Path) -> Result<Context> {
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(|| Error::new("utf8", "home"))?,
        "app",
        "adopt",
        "asset:alpha",
    ])
    .map_err(|e| Error::new("cli", e.to_string()))?;
    Context::from_cli(&cli, &mut vec![])
}
#[test]
fn declared_adoption_is_atomic_metadata_only_and_dry_run_is_read_only() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let ctx = context(&home)?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    let raw=br#"{"future": {"kept":true},"assets":{"alpha":{"kind":"github-skill","repository":"Owner/Repo","path":"skills/alpha","revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}"#;
    fs::write(
        ctx.settings.join("manifests/managed-agent-assets.json"),
        raw,
    )?;
    let payload = home.join(".agents/skills/alpha");
    fs::create_dir_all(payload.join("empty"))?;
    fs::write(payload.join("SKILL.md"), b"body")?;
    let slot = super::super::source_inventory::assess_adoption(
        &ctx,
        &crate::governance::capture(&ctx)?,
        "asset:alpha",
        false,
    )?
    .ok_or("assessment")?
    .baseline
    .slot;
    let before = crate::source_tree::observe(&slot)?;
    let result = adopt(&ctx, "asset:alpha", true, true)?.ok_or("result")?;
    assert_eq!(result["persistentWrites"], false);
    assert!(!ctx.state.exists());
    assert!(!home.join(".agents/backups").exists());
    let result = adopt(&ctx, "asset:alpha", false, true)?.ok_or("result")?;
    assert_eq!(result["status"], "succeeded");
    assert_eq!(crate::source_tree::observe(&slot)?, before);
    assert!(!home.join(".agents/.skill-lock.json").exists());
    assert!(!ctx.state.join("receipts.json").exists());
    assert!(!home.join(".agents/invocation-overlays.json").exists());
    assert!(!home.join(".agents/invocation-pending.json").exists());
    assert_eq!(
        fs::read(ctx.settings.join("manifests/managed-agent-assets.json"))?,
        raw
    );
    let managed = home.join(".agents/managed-assets.json");
    assert_eq!(fs::metadata(&managed)?.permissions().mode() & 0o7777, 0o600);
    let doc: serde_json::Value = serde_json::from_slice(&fs::read(&managed)?)?;
    assert_eq!(
        doc["_macops_sources"]["roots"]["asset:alpha"]["shape"]["entries"]["empty"]["content"]["type"],
        "directory"
    );
    Ok(())
}
fn native_fixture() -> std::result::Result<(tempfile::TempDir, Context), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let ctx = context(&home)?;
    fs::create_dir_all(home.join(".agents/skills/demo/empty"))?;
    fs::write(
        home.join(".agents/skills/demo/SKILL.md"),
        b"---\nname: demo\n---\nbody",
    )?;
    fs::write(home.join(".agents/.skill-lock.json"),br#"{"version":3,"future": {"raw":"\u0061"},"skills":{"demo":{"source":"Owner/Repo","sourceType":"github","sourceUrl":"https://github.com/Owner/Repo","skillPath":"skills/demo","skillFolderHash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","installedAt":"original","future":{"keep":true}}}}"#)?;
    fs::write(home.join(".agents/managed-assets.json"),br#"{"assets":{"old":{"revision":"artifact-sha256","recordHash":"unrelated"}}, "future":"\u0062"}"#)?;
    Ok((temp, ctx))
}
fn prepared(ctx: &Context) -> Result<(Store, Batch, Journal)> {
    let authority = crate::governance::capture(ctx)?;
    let assessment =
        crate::source_inventory::assess_adoption(ctx, &authority, "skill:demo", false)?
            .ok_or_else(invalid)?;
    let store = Store::lock(ctx)?;
    let authority = crate::governance::capture(ctx)?;
    let (mut batch, mut journal) = initialize(ctx, &authority, &assessment, &store)?;
    journal.phase = Phase::Running;
    store.save(&journal)?;
    batch.save(&store)?;
    batch.prepare(&store)?;
    Ok((store, batch, journal))
}
fn publish_slot(batch: &Batch, index: usize) -> Result<()> {
    let ledger = &batch.manifest.ledgers[index];
    files::publish(
        &ledger.witness.namespace,
        &ledger.witness.state,
        batch.pending.ledgers[index]
            .forward
            .as_ref()
            .ok_or_else(invalid)?,
    )
}
#[test]
fn native_adoption_preserves_native_raw_bytes_and_existing_heterogeneous_assets() -> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let lock = ctx.home.join(".agents/.skill-lock.json");
    let raw = fs::read(&lock)?;
    let result = adopt(&ctx, "skill:demo", false, true)?.ok_or("result")?;
    assert_eq!(result["status"], "succeeded");
    assert_eq!(fs::read(&lock)?, raw);
    let managed = fs::read(ctx.home.join(".agents/managed-assets.json"))?;
    assert!(String::from_utf8_lossy(&managed).contains(r#""future":"\u0062""#));
    let doc: Value = serde_json::from_slice(&managed)?;
    assert_eq!(doc["assets"]["old"]["recordHash"], "unrelated");
    let receipts: Receipts = crate::paths::regular_json(&ctx.state.join("receipts.json"))?;
    assert_eq!(receipts.installations.len(), 1);
    let receipt = receipts.installations.values().next().ok_or("receipt")?;
    assert_eq!(receipt.operation_id, result["operation_id"]);
    let Some(QuerySpec::LocalCapability(query)) = &receipt.query else {
        return Err("query".into());
    };
    assert_eq!(
        crate::capability_local::observe_default(&receipt.package.id, query)?,
        receipt.observation
    );
    Ok(())
}
#[test]
fn recovery_requires_all_five_ledgers_and_rolls_back_partial_adoption() -> TestResult {
    for stage in 0..=2 {
        let (_temp, ctx) = native_fixture()?;
        let managed = ctx.home.join(".agents/managed-assets.json");
        let before = fs::read(&managed)?;
        let (store, batch, mut journal) = prepared(&ctx)?;
        if stage >= 1 {
            publish_slot(&batch, 2)?;
        }
        if stage >= 2 {
            publish_slot(&batch, 4)?;
        }
        let result = recover_journal(&ctx, &store, &mut journal, false, true)?;
        assert_eq!(
            result["status"],
            if stage == 2 { "succeeded" } else { "failed" }
        );
        assert!(!batch.manifest.pending.exists());
        if stage < 2 {
            assert_eq!(fs::read(&managed)?, before);
            assert!(!ctx.state.join("receipts.json").exists());
        } else {
            assert!(ctx.state.join("receipts.json").exists());
            assert_ne!(fs::read(&managed)?, before);
        }
        assert!(ctx.home.join(".agents/skills/demo/empty").is_dir());
    }
    Ok(())
}
#[test]
fn own_restored_identity_survives_repeated_crash_and_pending_clear_journal_gap() -> TestResult {
    for clear in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        let managed = ctx.home.join(".agents/managed-assets.json");
        let before = fs::read(&managed)?;
        let (store, mut batch, mut journal) = prepared(&ctx)?;
        publish_slot(&batch, 2)?;
        let ledger = &batch.manifest.ledgers[2];
        let candidate = files::stage(
            &ledger.witness.namespace,
            Some(&before),
            policy::mode(&ledger.witness.state).ok_or("mode")?,
        )?;
        let restored = candidate.after.clone();
        batch.pending.ledgers[2].restored = Some(candidate.clone());
        batch.save(&store)?;
        files::publish(
            &batch.manifest.ledgers[2].witness.namespace,
            &batch.pending.ledgers[2]
                .forward
                .as_ref()
                .ok_or("candidate")?
                .after,
            &candidate,
        )?;
        if clear {
            batch.clear(&store, false)?;
        }
        assert_eq!(
            recover_journal(&ctx, &store, &mut journal, false, true)?["status"],
            "failed"
        );
        assert_eq!(fs::read(&managed)?, before);
        assert_eq!(
            files::observe(&files::namespace(&managed)?, 16 * 1024 * 1024)?.0,
            restored
        );
        assert!(!ctx.state.join("receipts.json").exists());
        assert!(!batch.manifest.pending.exists());
    }
    Ok(())
}
#[test]
fn foreign_same_bytes_identity_or_payload_empty_directory_blocks_recovery_without_writes()
-> TestResult {
    for root_change in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        let (store, batch, mut journal) = prepared(&ctx)?;
        publish_slot(&batch, 2)?;
        let managed = ctx.home.join(".agents/managed-assets.json");
        if root_change {
            fs::create_dir(ctx.home.join(".agents/skills/demo/foreign-empty"))?;
        } else {
            let replacement = managed.with_extension("foreign");
            fs::write(&replacement, fs::read(&managed)?)?;
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600))?;
            fs::rename(replacement, &managed)?;
        }
        let managed_before = fs::read(&managed)?;
        let marker = fs::read(&batch.manifest.pending)?;
        assert!(recover_journal(&ctx, &store, &mut journal, false, true).is_err());
        assert_eq!(fs::read(&managed)?, managed_before);
        assert_eq!(fs::read(&batch.manifest.pending)?, marker);
        assert!(!ctx.state.join("receipts.json").exists());
        assert_eq!(journal.phase, Phase::Running);
    }
    Ok(())
}
#[test]
fn initial_journal_before_marker_gap_can_recover_exact_before_without_retrying_adoption()
-> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let initial = crate::governance::capture(&ctx)?;
    let assessment = crate::source_inventory::assess_adoption(&ctx, &initial, "skill:demo", false)?
        .ok_or("assessment")?;
    let store = Store::lock(&ctx)?;
    let authority = crate::governance::capture(&ctx)?;
    let (batch, mut journal) = initialize(&ctx, &authority, &assessment, &store)?;
    assert_eq!(journal.phase, Phase::Prepared);
    assert!(!batch.manifest.pending.exists());
    assert!(batch.root.path.join("source-candidates.json").is_file());
    assert_eq!(
        recover_journal(&ctx, &store, &mut journal, false, true)?["status"],
        "failed"
    );
    assert!(!ctx.state.join("receipts.json").exists());
    assert!(!batch.manifest.pending.exists());
    assert_eq!(fs::read(ctx.home.join(".agents/managed-assets.json"))?,br#"{"assets":{"old":{"revision":"artifact-sha256","recordHash":"unrelated"}}, "future":"\u0062"}"#);
    Ok(())
}
#[test]
fn ambiguous_declared_and_native_alias_refuses_before_state_creation_but_explicit_native_id_works()
-> TestResult {
    let (_temp, ctx) = native_fixture()?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::write(ctx.settings.join("manifests/managed-agent-assets.json"),br#"{"assets":{"demo":{"kind":"github-skill","repository":"different/repo","path":"skills/demo"}}}"#)?;
    assert!(crate::target::resolve(&ctx, "skill:demo").is_err());
    assert_eq!(
        adopt(&ctx, "skill:demo", false, true)
            .err()
            .ok_or("ambiguous")?
            .code,
        "ambiguous"
    );
    assert!(!ctx.state.exists());
    assert_eq!(
        adopt(&ctx, "demo", false, true)
            .err()
            .ok_or("bare ambiguity")?
            .code,
        "ambiguous"
    );
    assert!(!ctx.state.exists());
    let package = crate::capability_local::resolve_package(&ctx, "skill:demo", false, false)?
        .ok_or("native package")?;
    let exact_alias = crate::cli::AdoptTarget {
        key: "skill:demo",
        exact: true,
    };
    assert!(super::adopt(&ctx, exact_alias, false, true)?.is_none());
    assert!(!ctx.state.exists());
    let exact_id = crate::cli::AdoptTarget {
        key: &package.id,
        exact: true,
    };
    assert!(super::adopt(&ctx, exact_id, true, true)?.is_some());
    assert!(crate::capability_local::resolve_package(&ctx, "skill:demo", false, true)?.is_none());
    assert_eq!(
        crate::capability_local::resolve_package(&ctx, &package.id, false, true)?
            .ok_or("exact ID")?
            .id,
        package.id
    );
    assert_eq!(
        adopt(&ctx, &package.id, false, true)?.ok_or("adopt")?["status"],
        "succeeded"
    );
    assert_eq!(
        adopt(&ctx, "asset:demo", false, true)
            .err()
            .ok_or("conflicting source")?
            .code,
        "ambiguous_source_registration"
    );
    Ok(())
}
#[test]
fn clearing_pending_requires_the_complete_intended_terminal_state_in_both_directions() -> TestResult
{
    for after in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        let (store, mut batch, _journal) = prepared(&ctx)?;
        if after {
            batch.publish(&store)?;
        }
        assert!(batch.matches(after)?);
        let index = if after { 2 } else { 4 };
        let ledger = &batch.manifest.ledgers[index];
        if after {
            let raw = ledger
                .before
                .as_ref()
                .map(|b| batch.blobs.get(b))
                .transpose()?;
            let candidate = files::stage(
                &ledger.witness.namespace,
                raw.as_deref(),
                policy::mode(&ledger.witness.state).unwrap_or(0o600),
            )?;
            batch.pending.ledgers[index].restored = Some(candidate.clone());
            batch.save(&store)?;
            files::publish(
                &batch.manifest.ledgers[index].witness.namespace,
                &batch.pending.ledgers[index]
                    .forward
                    .as_ref()
                    .ok_or("forward")?
                    .after,
                &candidate,
            )?;
        } else {
            publish_slot(&batch, index)?;
        }
        assert!(!batch.matches(after)?);
        assert!(!batch.matches(!after)?);
        let marker = fs::read(&batch.manifest.pending)?;
        assert!(batch.clear(&store, after).is_err());
        assert_eq!(fs::read(&batch.manifest.pending)?, marker);
    }
    Ok(())
}
#[test]
fn native_expected_rejects_unknown_fields_arrays_null_and_wrong_observation_variants() -> TestResult
{
    let (_temp, ctx) = native_fixture()?;
    let authority = crate::governance::capture(&ctx)?;
    let spec = crate::source_inventory::assess_adoption(&ctx, &authority, "skill:demo", false)?
        .ok_or("assessment")?
        .native
        .ok_or("native")?;
    let original = serde_json::to_value(&spec)?;
    let mut changed = original.clone();
    changed["expected"]["future_behavior"] = json!({"ignore":false});
    assert!(serde_json::from_value::<policy::NativeSpec>(changed).is_err());
    for value in [
        Value::Null,
        json!(["capability-pack", "skills-cli", {}]),
        json!({"status":"missing"}),
    ] {
        let mut changed = original.clone();
        changed["expected"] = value;
        assert!(serde_json::from_value::<policy::NativeSpec>(changed).is_err());
    }
    assert!(serde_json::from_value::<policy::NativeSpec>(original).is_ok());
    Ok(())
}
#[test]
fn github_full_adoption_requires_the_registered_skill_path_and_never_falls_back() -> TestResult {
    for invalid_path in [Value::Null, json!("../demo"), json!(""), json!("/demo")] {
        let (_temp, ctx) = native_fixture()?;
        let lock = ctx.home.join(".agents/.skill-lock.json");
        let mut doc: Value = serde_json::from_slice(&fs::read(&lock)?)?;
        doc["skills"]["demo"]["skillPath"] = invalid_path;
        fs::write(&lock, serde_json::to_vec(&doc)?)?;
        assert!(adopt(&ctx, "skill:demo", false, true).is_err());
        assert!(!ctx.state.exists());
        assert_eq!(fs::read(&lock)?, serde_json::to_vec(&doc)?);
    }
    Ok(())
}
#[test]
fn policy_write_and_retirement_advance_full_source_baseline_using_exact_patch_modes() -> TestResult
{
    let (_temp, ctx) = native_fixture()?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    let rules = ctx.settings.join("manifests/agent-invocation-policy.json");
    fs::write(
        &rules,
        br#"{"schema_version":1,"standalone_manual_only":["demo"]}"#,
    )?;
    let adopted = adopt(&ctx, "skill:demo", false, true)?.ok_or("adopt")?;
    let id = adopted["member"].as_str().ok_or("id")?;
    let authority = crate::governance::capture(&ctx)?;
    let before = fs::read(&authority.paths.managed)?;
    assert_eq!(
        crate::capability_tx::sync(&ctx, Some("SOURCE-POLICY"), false, true)?["status"],
        "succeeded"
    );
    let book = crate::policy::source_baseline::read(
        Some(&fs::read(&authority.paths.managed)?),
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    let root = &book.roots[id];
    let observed = crate::source_tree::observe(&root.slot)?.ok_or("root")?;
    root.authorize(
        &root.source,
        &root.slot,
        &observed,
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    assert_eq!(root.shape.entries["agents"].mode, 0o700);
    assert_eq!(
        root.shape.entries["agents/openai.yaml"].mode,
        fs::metadata(root.slot.path().join("agents/openai.yaml"))?
            .permissions()
            .mode()
            & 0o7777
    );
    assert_ne!(fs::read(&authority.paths.managed)?, before);
    fs::write(
        &rules,
        br#"{"schema_version":1,"standalone_manual_only":[]}"#,
    )?;
    assert_eq!(
        crate::capability_tx::sync(&ctx, Some("SOURCE-RETIRE"), false, true)?["status"],
        "succeeded"
    );
    let book = crate::policy::source_baseline::read(
        Some(&fs::read(&authority.paths.managed)?),
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    let root = &book.roots[id];
    root.authorize(
        &root.source,
        &root.slot,
        &crate::source_tree::observe(&root.slot)?.ok_or("root")?,
        &ctx.home,
        nix::unistd::geteuid().as_raw(),
    )?;
    assert!(!root.shape.entries.contains_key("agents/openai.yaml"));
    assert!(root.shape.entries.contains_key("empty"));
    Ok(())
}
#[test]
fn policy_projection_cannot_bless_preexisting_full_mode_or_empty_directory_drift() -> TestResult {
    for mode_change in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        fs::create_dir_all(ctx.settings.join("manifests"))?;
        fs::write(
            ctx.settings.join("manifests/agent-invocation-policy.json"),
            br#"{"schema_version":1,"standalone_manual_only":["demo"]}"#,
        )?;
        adopt(&ctx, "skill:demo", false, true)?.ok_or("adopt")?;
        let root = ctx.home.join(".agents/skills/demo");
        if mode_change {
            fs::set_permissions(root.join("SKILL.md"), fs::Permissions::from_mode(0o600))?;
        } else {
            fs::create_dir(root.join("foreign-empty"))?;
        }
        let managed = fs::read(ctx.home.join(".agents/managed-assets.json"))?;
        let receipts = fs::read(ctx.state.join("receipts.json"))?;
        let skill = fs::read(root.join("SKILL.md"))?;
        assert_eq!(
            crate::capability_tx::sync(&ctx, Some("SOURCE-DRIFT"), false, true)
                .err()
                .ok_or("drift")?
                .code,
            "source_baseline_drift"
        );
        assert_eq!(
            fs::read(ctx.home.join(".agents/managed-assets.json"))?,
            managed
        );
        assert_eq!(fs::read(ctx.state.join("receipts.json"))?, receipts);
        assert_eq!(fs::read(root.join("SKILL.md"))?, skill);
        assert!(!root.join("agents").exists());
        assert!(
            !ctx.home
                .join(".agents/invocation-policy.pending.json")
                .exists()
        );
    }
    Ok(())
}
#[test]
fn strict_v5_manifest_rejects_invalid_identity_paths_ledger_order_and_candidate_evidence()
-> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let (_store, batch, _journal) = prepared(&ctx)?;
    let authority = crate::governance::capture(&ctx)?;
    let owner = nix::unistd::geteuid().as_raw();
    let original = serde_json::to_value(&batch.manifest)?;
    for (pointer, value) in [
        ("/schema_version", json!(4)),
        ("/operation_id", json!("../bad")),
        ("/change_id", json!("../bad")),
        ("/home", json!("relative")),
        ("/settings", json!("relative")),
        ("/scope/home", json!("/other")),
        ("/pending", json!("/other")),
        ("/layout/config", json!("relative")),
        ("/layout/data", json!("relative")),
        ("/layout/state", json!("relative")),
        ("/layout/cache", json!("relative")),
        ("/ledgers", json!([])),
        ("/ledgers/0/slot", json!("managed")),
        ("/ledgers/0/witness/path", json!("/different")),
        ("/ledgers/0/witness/namespace/remaining", json!([])),
        (
            "/ledgers/0/witness/namespace/ancestor/uid",
            json!(u64::from(owner) + 1),
        ),
        ("/ledgers/0/witness/namespace/ancestor/mode", json!(0o1777)),
        ("/ledgers/0/witness/namespace/ancestor/mode", json!(0o777)),
        (
            "/ledgers/0/witness/namespace/ancestor/canonical",
            json!("/different"),
        ),
        ("/ledgers/0/before", Value::Null),
        ("/ledgers/0/after", Value::Null),
        ("/ledgers/0/after_mode", json!(0o1777)),
        ("/ledgers/0/after_mode", Value::Null),
        ("/ledgers/0/after/sha256", json!("bad")),
        ("/ledgers/0/after/size", json!(16 * 1024 * 1024 + 1)),
        ("/ledgers/0/after_mode", json!(0o600)),
        ("/action/root/native/package/id", json!("other")),
        (
            "/action/root/native/package/provider/manager",
            json!("skills-cli:other"),
        ),
        (
            "/action/root/native/package/provider/source",
            json!("other/repo"),
        ),
        (
            "/action/root/native/package/provider/record",
            json!("/different"),
        ),
        (
            "/action/root/native/package/provider/payload",
            json!("/different"),
        ),
        ("/action/root/native", Value::Null),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).ok_or(pointer.to_owned())? = value;
        let decoded = decode::<Manifest>(&serde_json::to_vec(&changed)?);
        assert!(
            decoded.is_err()
                || decoded
                    .as_ref()
                    .is_ok_and(|m| policy::validate(m, &authority.paths, owner).is_err()),
            "{pointer}"
        );
    }
    let original = serde_json::to_value(&batch.pending)?;
    let hash = &batch.pending.manifest_sha256;
    for (pointer, value) in [
        ("/schema_version", json!(4)),
        ("/operation_id", json!("OP-other")),
        ("/manifest_sha256", json!("bad")),
        ("/ledgers", json!([])),
        ("/ledgers/2/forward/target", json!("/different")),
        ("/ledgers/2/forward/staged", json!("/private/tmp/unowned")),
        ("/ledgers/2/forward/staged", Value::Null),
        ("/ledgers/2/forward/after/mode", json!(0o777)),
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).ok_or(pointer.to_owned())? = value;
        let pending: Pending = decode(&serde_json::to_vec(&changed)?)?;
        assert!(
            policy::validate_pending(&batch.manifest, &pending, hash).is_err(),
            "{pointer}"
        );
    }
    Ok(())
}
#[test]
fn v5_rejects_forged_read_witnesses_mismatched_native_targets_and_unrecorded_restore_candidates()
-> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let (store, mut batch, _journal) = prepared(&ctx)?;
    let authority = crate::governance::capture(&ctx)?;
    let owner = nix::unistd::geteuid().as_raw();
    let good = batch.manifest.clone();
    for variant in 0..8 {
        let mut manifest = good.clone();
        let native = adopted_mut(&mut manifest)?
            .native
            .as_mut()
            .ok_or("native")?;
        match variant {
            0 => {
                native.package.provider = Provider::Brew {
                    package: "demo".into(),
                    cask: false,
                }
            }
            1 => native.expected = Observation::Missing { evidence: None },
            2 => adopted_mut(&mut manifest)?.baseline.source.member = "different".into(),
            3 => {
                if let Provider::CapabilityObserved { native_id, .. } = &mut native.package.provider
                {
                    *native_id = "different".into();
                }
            }
            4 => {
                if let Provider::CapabilityObserved { source, .. } = &mut native.package.provider {
                    *source = Some("invalid".into());
                }
            }
            5 => manifest.inputs = vec![manifest.ledgers[0].witness.clone()],
            6 => {
                let mut w = manifest.ledgers[0].witness.clone();
                w.path = ctx.home.join("input");
                w.namespace = files::namespace(&w.path)?;
                w.state = State::File {
                    identity: crate::domain::FileId {
                        device: 0,
                        inode: 0,
                    },
                    sha256: "bad".into(),
                    mode: 0o644,
                };
                manifest.inputs = vec![w];
            }
            _ => {
                manifest.inputs[0].namespace.remaining.clear();
            }
        }
        assert!(
            policy::validate(&manifest, &authority.paths, owner).is_err(),
            "{variant}"
        );
    }
    let ledger = &good.ledgers[2];
    let raw = ledger
        .before
        .as_ref()
        .map(|b| batch.blobs.get(b))
        .transpose()?;
    let restored = files::stage(
        &ledger.witness.namespace,
        raw.as_deref(),
        policy::mode(&ledger.witness.state).unwrap_or(0o600),
    )?;
    let mut pending = batch.pending.clone();
    pending.ledgers[2].restored = Some(restored.clone());
    pending.ledgers[2].forward = None;
    assert!(policy::validate_pending(&good, &pending, &pending.manifest_sha256).is_err());
    let mut pending = batch.pending.clone();
    pending.ledgers[0].forward = Some(restored);
    let forward = pending.ledgers[0].forward.as_mut().ok_or("forward")?;
    forward.target = good.ledgers[0].witness.path.clone();
    forward.after = good.ledgers[0].witness.state.clone();
    forward.staged = Some(
        forward
            .target
            .with_file_name(format!(".{}.cap.tmp", crate::store::operation_id())),
    );
    assert!(policy::validate_pending(&good, &pending, &pending.manifest_sha256).is_err());
    for staged in [
        "relative".into(),
        good.ledgers[2].witness.path.with_file_name("unowned.tmp"),
    ] {
        let mut pending = batch.pending.clone();
        pending.ledgers[2].forward.as_mut().ok_or("forward")?.staged = Some(staged);
        assert!(policy::validate_pending(&good, &pending, &pending.manifest_sha256).is_err());
    }
    batch.publish(&store)?;
    batch.rollback(&store)?;
    policy::validate_pending(
        &batch.manifest,
        &batch.pending,
        &batch.pending.manifest_sha256,
    )?;
    assert!(batch.matches(false)?);
    assert!(!ctx.state.join("receipts.json").exists());
    Ok(())
}
#[test]
fn source_scope_overlap_is_rejected_before_lock_preparation_can_write_inside_the_payload()
-> TestResult {
    for ancestor in [false, true] {
        let (_temp, mut ctx) = native_fixture()?;
        let payload = ctx.home.join(".agents/skills/demo");
        let slot = crate::source_inventory::assess_adoption(
            &ctx,
            &crate::governance::capture(&ctx)?,
            "skill:demo",
            false,
        )?
        .ok_or("assessment")?
        .baseline
        .slot;
        let before = crate::source_tree::observe(&slot)?;
        ctx.state = if ancestor {
            ctx.home.join(".agents/skills")
        } else {
            payload.join("state")
        };
        assert_eq!(
            adopt(&ctx, "skill:demo", false, true)
                .err()
                .ok_or("overlap")?
                .code,
            "source_path_overlap"
        );
        assert_eq!(crate::source_tree::observe(&slot)?, before);
        assert!(!ctx.state.join("write.lock").exists());
        assert!(!ctx.home.join(".agents/backups").exists());
    }
    Ok(())
}
#[test]
fn v5_authority_rejects_full_tree_mismatch_layout_overlap_and_invalid_native_binding() -> TestResult
{
    let (_temp, ctx) = native_fixture()?;
    let (_store, batch, _journal) = prepared(&ctx)?;
    let authority = crate::governance::capture(&ctx)?;
    let owner = nix::unistd::geteuid().as_raw();
    for variant in 0..4 {
        let mut manifest = batch.manifest.clone();
        match variant {
            0 => adopted_mut(&mut manifest)?.tree.mode = 0o700,
            1 => manifest.layout.state = adopted_mut(&mut manifest)?.baseline.slot.path(),
            2 => {
                adopted_mut(&mut manifest)?
                    .native
                    .as_mut()
                    .ok_or("native")?
                    .package
                    .exact_version_required = true;
            }
            _ => {
                adopted_mut(&mut manifest)?
                    .native
                    .as_mut()
                    .ok_or("native")?
                    .query
                    .record
                    .name = "different".into();
            }
        }
        assert!(policy::validate(&manifest, &authority.paths, owner).is_err());
    }
    Ok(())
}
#[test]
fn known_candidate_receipt_matches_native_observation_after_exchange_without_learning_the_new_live_identity()
-> TestResult {
    use crate::policy::{
        source_payload::{Entry, Payload},
        source_receipts,
        source_tree::Change,
    };
    let (_temp, ctx) = native_fixture()?;
    let assessment = crate::source_inventory::assess_adoption(
        &ctx,
        &crate::governance::capture(&ctx)?,
        "skill:demo",
        false,
    )?
    .ok_or("assessment")?;
    let spec = assessment.native.as_ref().ok_or("spec")?;
    let native_path = ctx.home.join(".agents/.skill-lock.json");
    let before = fs::read(&native_path)?;
    let owner = nix::unistd::geteuid().as_raw();
    assert_eq!(
        source_receipts::known_skill(
            &spec.package.id,
            &spec.query,
            &before,
            &assessment.baseline.slot,
            &assessment.tree,
            owner
        )?,
        spec.expected
    );
    let private = ctx.home.join(".agents/skills/.private-test");
    fs::create_dir(&private)?;
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700))?;
    let payload = Payload {
        entries: std::collections::BTreeMap::from([
            (
                "SKILL.md".into(),
                Entry::File {
                    mode: 0o644,
                    bytes: b"---\nname: demo\n---\nnew body\n".to_vec(),
                },
            ),
            ("empty".into(), Entry::Directory { mode: 0o755 }),
        ]),
    };
    let (retained, after_tree) = crate::source_payload::materialize(
        &Directory::open(&private)?,
        "payload",
        &payload,
        0o755,
    )?;
    let mut after_doc: Value = serde_json::from_slice(&before)?;
    after_doc["skills"]["demo"]["skillFolderHash"] = json!("b".repeat(40));
    after_doc["skills"]["demo"]["ref"] = json!("c".repeat(40));
    let after = serde_json::to_vec(&after_doc)?;
    let expected = source_receipts::known_skill(
        &spec.package.id,
        &spec.query,
        &after,
        &assessment.baseline.slot,
        &after_tree,
        owner,
    )?;
    assert_ne!(expected, spec.expected);
    let change = Change {
        live: assessment.baseline.slot.clone(),
        retained,
        before: assessment.tree.clone(),
        after: after_tree,
    };
    crate::source_tree::publish_checked(&change, || Ok(()))?;
    fs::write(&native_path, &after)?;
    assert_eq!(
        crate::capability_local::observe_default(&spec.package.id, &spec.query)?,
        expected
    );
    fs::write(&native_path, &before)?;
    crate::source_tree::restore_checked(&change, || Ok(()))?;
    assert_eq!(
        crate::capability_local::observe_default(&spec.package.id, &spec.query)?,
        spec.expected
    );
    Ok(())
}
#[test]
fn source_recovery_uses_persistent_frozen_evidence_without_requiring_the_old_cache_location()
-> TestResult {
    for committed in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        let (store, batch, mut journal) = prepared(&ctx)?;
        publish_slot(&batch, 2)?;
        if committed {
            publish_slot(&batch, 4)?;
        }
        let mut recovery = ctx.clone();
        recovery.cache = ctx.home.join("alternate-cache");
        assert_eq!(
            recover_journal(&recovery, &store, &mut journal, false, true)?["status"],
            if committed { "succeeded" } else { "failed" }
        );
        assert!(!recovery.cache.exists());
        assert!(!batch.manifest.pending.exists());
    }
    Ok(())
}
#[test]
fn recovery_checks_the_used_native_registry_scope_and_ignores_unrelated_manager_environment()
-> TestResult {
    for used_scope in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        let (store, batch, mut journal) = prepared(&ctx)?;
        publish_slot(&batch, 2)?;
        let marker = fs::read(&batch.manifest.pending)?;
        let mut recovery = ctx.clone();
        recovery.scope.paths.insert(
            if used_scope {
                "XDG_STATE_HOME"
            } else {
                "NPM_CONFIG_PREFIX"
            }
            .into(),
            Some(ctx.home.join("changed-scope")),
        );
        let result = recover_journal(&recovery, &store, &mut journal, false, true);
        if used_scope {
            assert!(result.is_err());
            assert_eq!(fs::read(&batch.manifest.pending)?, marker);
            assert_eq!(journal.phase, Phase::Running);
        } else {
            assert_eq!(result?["status"], "failed");
            assert!(!batch.manifest.pending.exists());
        }
        assert!(!recovery.home.join("changed-scope").exists());
    }
    Ok(())
}
#[test]
fn a_replaced_state_root_cannot_be_learned_between_lock_and_source_journal_initialization()
-> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let initial = crate::governance::capture(&ctx)?;
    let assessment = crate::source_inventory::assess_adoption(&ctx, &initial, "skill:demo", false)?
        .ok_or("assessment")?;
    let store = Store::lock(&ctx)?;
    let authority = crate::governance::capture(&ctx)?;
    let retained = ctx.home.join("retained-state");
    fs::rename(&ctx.state, &retained)?;
    fs::create_dir(&ctx.state)?;
    fs::set_permissions(&ctx.state, fs::Permissions::from_mode(0o700))?;
    assert!(initialize(&ctx, &authority, &assessment, &store).is_err());
    assert!(!ctx.state.join("receipts.json").exists());
    assert!(
        !ctx.home
            .join(".agents/invocation-policy.pending.json")
            .exists()
    );
    Ok(())
}
#[test]
fn source_forward_and_rollback_recheck_store_at_the_final_receipt_publication_boundary()
-> TestResult {
    for rollback in [false, true] {
        let (_temp, ctx) = native_fixture()?;
        let (store, mut batch, _journal) = prepared(&ctx)?;
        if rollback {
            batch.publish(&store)?;
        }
        let mutate = |slot: Slot| -> Result<()> {
            if slot == Slot::Receipts {
                fs::rename(
                    ctx.state.join("operations"),
                    ctx.state.join("retained-operations"),
                )?;
                fs::create_dir(ctx.state.join("operations"))?;
            }
            Ok(())
        };
        let outcome = if rollback {
            batch.rollback_with(&store, mutate)
        } else {
            batch.publish_with(&store, mutate).map(|_| ())
        };
        assert!(outcome.is_err());
        assert_eq!(ctx.state.join("receipts.json").exists(), rollback);
        assert!(batch.manifest.pending.exists());
        assert_eq!(fs::read_dir(ctx.state.join("operations"))?.count(), 0);
    }
    Ok(())
}
#[test]
fn a_replaced_lock_cannot_be_ignored_when_clearing_a_complete_source_batch() -> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let (store, mut batch, _journal) = prepared(&ctx)?;
    batch.publish(&store)?;
    assert!(batch.matches(true)?);
    let marker = fs::read(&batch.manifest.pending)?;
    fs::rename(
        ctx.state.join("write.lock"),
        ctx.state.join("retained-lock"),
    )?;
    fs::write(ctx.state.join("write.lock"), b"")?;
    assert!(batch.clear(&store, true).is_err());
    assert_eq!(fs::read(&batch.manifest.pending)?, marker);
    assert!(ctx.state.join("receipts.json").exists());
    Ok(())
}
#[test]
fn a_terminal_rollback_retires_only_its_recorded_unused_metadata_staging_files() -> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let (store, batch, mut journal) = prepared(&ctx)?;
    publish_slot(&batch, 2)?;
    let own: Vec<_> = batch
        .pending
        .ledgers
        .iter()
        .filter_map(|p| p.forward.as_ref().and_then(|c| c.staged.clone()))
        .collect();
    let unrelated = ctx.state.join(".unrelated.cap.tmp");
    fs::write(&unrelated, b"other owner")?;
    assert_eq!(
        recover_journal(&ctx, &store, &mut journal, false, true)?["status"],
        "failed"
    );
    assert!(own.iter().all(|p| !p.exists()));
    assert_eq!(fs::read(&unrelated)?, b"other owner");
    Ok(())
}

fn adopted_mut(
    manifest: &mut Manifest,
) -> std::result::Result<&mut ObservedRoot, Box<dyn std::error::Error>> {
    match &mut manifest.action {
        SourceAction::Adopt { root } => Ok(root),
        SourceAction::Upgrade { .. } => Err("expected adoption".into()),
    }
}

type PreparedUpgrade = (tempfile::TempDir, Context, Store, Batch, Journal);
fn complete_upgrade_context()
-> std::result::Result<(tempfile::TempDir, Context), Box<dyn std::error::Error>> {
    let (temp, ctx) = native_fixture()?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::write(ctx.settings.join("manifests/managed-agent-assets.json"),br#"{"assets":{"alpha":{"kind":"github-skill","repository":"owner/repo","path":"skills/alpha","branch":"main","revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","license_files":[]}}}"#)?;
    let alpha = ctx.home.join(".agents/skills/alpha");
    fs::create_dir_all(alpha.join("empty"))?;
    fs::write(alpha.join("SKILL.md"), b"old asset")?;
    let skill = ctx.home.join(".agents/skills/demo/SKILL.md");
    let raw = fs::read(&skill)?;
    let overlay = crate::policy::invocation::propose(
        crate::policy::invocation::MetadataKind::Frontmatter,
        "demo",
        "policy",
        Some(&raw),
        Some(0o644),
        None,
    )?;
    fs::write(&skill, overlay.after.ok_or("applied")?)?;
    let authority = crate::governance::capture(&ctx)?;
    let rules = serde_json::to_vec(
        &json!({"schema_version":1,"standalone_manual_only":["demo"],"plugins":{},"mirror_standalone_upstream_manual":false}),
    )?;
    fs::write(
        authority
            .paths
            .dataset(crate::policy::governance::Dataset::InvocationRules),
        &rules,
    )?;
    let codex = crate::policy::invocation::propose(
        crate::policy::invocation::MetadataKind::CodexPolicy,
        "demo",
        "policy",
        None,
        None,
        None,
    )?;
    let agents = ctx.home.join(".agents/skills/demo/agents");
    fs::create_dir(&agents)?;
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o700))?;
    fs::write(agents.join("openai.yaml"), codex.after.ok_or("codex")?)?;
    fs::set_permissions(
        agents.join("openai.yaml"),
        fs::Permissions::from_mode(0o600),
    )?;
    fs::write(
        &authority.paths.overlays,
        serde_json::to_vec(
            &json!({"schema_version":1,"policy_hash":crate::policy::invocation::hash(&rules),"files":{skill.to_str().ok_or("skill path")?:overlay.row,agents.join("openai.yaml").to_str().ok_or("codex path")?:codex.row}}),
        )?,
    )?;
    adopt(&ctx, "skill:demo", false, true)?.ok_or("native adoption")?;
    adopt(&ctx, "asset:alpha", false, true)?.ok_or("asset adoption")?;
    Ok((temp, ctx))
}
#[path = "legacy_source_v5_fixture.rs"]
mod legacy_v5_fixture;
fn prepared_complete_upgrade() -> std::result::Result<PreparedUpgrade, Box<dyn std::error::Error>> {
    legacy_v5_fixture::legacy_v5_fixture()
}

#[test]
fn legacy_v5_upgrade_recovers_each_actual_root_and_five_ledger_boundary_without_generating_restored_inodes()
-> TestResult {
    for boundary in 0usize..=7 {
        let (_temp, ctx, store, batch, mut journal) = prepared_complete_upgrade()?;
        let SourceAction::Upgrade { roots, .. } = &batch.manifest.action else {
            return Err("upgrade".into());
        };
        assert_eq!(roots.len(), 2);
        for root in roots.iter().take(boundary.min(2)) {
            crate::source_tree::publish_checked(&root.change, || batch.guard(&store))?;
        }
        for ledger in batch
            .manifest
            .ledgers
            .iter()
            .take(boundary.saturating_sub(2))
        {
            crate::source_files::publish_checked(ledger.pair.as_ref().ok_or("pair")?, || {
                batch.guard(&store)
            })?;
        }
        let result = recover_journal(&ctx, &store, &mut journal, false, true)?;
        let after = boundary == 7;
        assert_eq!(result["phase"], if after { "succeeded" } else { "failed" });
        for ledger in &batch.manifest.ledgers {
            let pair = ledger.pair.as_ref().ok_or("pair")?;
            assert_eq!(
                crate::source_files::position(pair)?,
                if after {
                    crate::policy::source_tree::Position::OwnAfter
                } else {
                    crate::policy::source_tree::Position::Before
                }
            );
            assert_eq!(
                crate::source_files::observe(&pair.live)?,
                if after {
                    Some(pair.after.clone())
                } else {
                    pair.before.clone()
                }
            );
        }
        for root in roots {
            assert_eq!(
                crate::source_tree::position(&root.change)?,
                if after {
                    crate::policy::source_tree::Position::OwnAfter
                } else {
                    crate::policy::source_tree::Position::Before
                }
            );
        }
        let archived: Pending = decode(&fs::read(batch.root.path.join("source-candidates.json"))?)?;
        assert!(
            archived
                .ledgers
                .iter()
                .all(|progress| progress.forward.is_none() && progress.restored.is_none())
        );
        assert!(!batch.manifest.pending.exists());
    }
    Ok(())
}
#[test]
fn legacy_v5_upgrade_full_before_and_after_clear_gap_use_frozen_n_and_preserve_terminal_pair_after_repeated_restarts()
-> TestResult {
    for after in [false, true] {
        let (_temp, ctx, store, mut batch, mut journal) = prepared_complete_upgrade()?;
        if after {
            batch.publish(&store)?;
        } else {
            let SourceAction::Upgrade { roots, .. } = &batch.manifest.action else {
                return Err("upgrade".into());
            };
            for root in roots {
                crate::source_tree::publish_checked(&root.change, || batch.guard(&store))?;
            }
            for ledger in batch.manifest.ledgers.iter().take(3) {
                crate::source_files::publish_checked(ledger.pair.as_ref().ok_or("pair")?, || {
                    batch.guard(&store)
                })?;
            }
            batch.rollback(&store)?;
            for _ in 0..2 {
                let mut restarted = load(&ctx, &store, &journal)?;
                restarted.rollback(&store)?;
                assert!(restarted.matches(false)?);
            }
        }
        batch.clear(&store, after)?;
        assert!(!batch.manifest.pending.exists());
        let result = recover_journal(&ctx, &store, &mut journal, false, true)?;
        assert_eq!(result["phase"], if after { "succeeded" } else { "failed" });
        let reloaded = load(&ctx, &store, &journal)?;
        assert!(reloaded.matches(after)?);
        assert!(
            reloaded
                .pending
                .ledgers
                .iter()
                .all(|p| p.forward.is_none() && p.restored.is_none())
        );
    }
    Ok(())
}
#[test]
fn legacy_v5_upgrade_unknown_retained_or_lost_frozen_candidate_evidence_never_rebuilds_or_clears_pending()
-> TestResult {
    for variant in 0..3 {
        let (_temp, ctx, store, batch, mut journal) = prepared_complete_upgrade()?;
        let ledger = &batch.manifest.ledgers[0];
        let pair = ledger.pair.as_ref().ok_or("pair")?;
        match variant {
            0 => {
                fs::rename(pair.retained.path(), batch.root.path.join("saved-N"))?;
                fs::write(
                    pair.retained.path(),
                    batch.blobs.get(ledger.after.as_ref().ok_or("after")?)?,
                )?;
                fs::set_permissions(
                    pair.retained.path(),
                    fs::Permissions::from_mode(pair.after.mode),
                )?;
            }
            1 => fs::remove_file(pair.retained.path())?,
            _ => fs::remove_file(batch.root.path.join("capability-plan.json"))?,
        }
        assert!(recover_journal(&ctx, &store, &mut journal, false, true).is_err());
        assert!(batch.manifest.pending.exists());
        assert_eq!(
            files::observe(&ledger.witness.namespace, 16 * 1024 * 1024)?.0,
            ledger.witness.state
        );
    }
    Ok(())
}

#[test]
fn legacy_v5_upgrade_frozen_authority_rejects_corrupt_roots_pairs_invocation_witness_and_dynamic_progress()
-> TestResult {
    let (_temp, ctx, _store, batch, _journal) = prepared_complete_upgrade()?;
    let authority = crate::governance::capture(&ctx)?;
    let owner = nix::unistd::geteuid().as_raw();
    let original = serde_json::to_value(&batch.manifest)?;
    for (pointer, value) in [
        ("/action/roots", json!([])),
        ("/action/invocation/path", json!("/other")),
        ("/action/invocation/state", json!({"type":"missing"})),
        ("/action/roots/0/commit", json!("bad")),
        ("/action/roots/0/skill_tree", json!("bad")),
        ("/action/roots/0/requested_ref", json!("main~1")),
        ("/action/roots/0/baseline/root_identity/inode", json!(999)),
        ("/action/roots/0/change/after/mode", json!(0o700)),
        ("/action/roots/0/base_shape/mode", json!(0o700)),
        (
            "/action/roots/0/base_shape/uid",
            json!(u64::from(owner) + 1),
        ),
        ("/action/roots/0/base_sha256", json!("f".repeat(64))),
        ("/action/roots/0/explicit_pin", json!(true)),
        ("/action/roots/0/change/retained/parent/mode", json!(0o755)),
        (
            "/action/roots/0/native_after",
            original["action"]["roots"][1]["native_after"].clone(),
        ),
        (
            "/action/roots/1/native_after/package/name",
            json!("renamed"),
        ),
        (
            "/action/roots/1/native_after/query/payload/parent_identity/inode",
            json!(999),
        ),
        (
            "/action/roots/1/native_after/query/record/name",
            json!("different"),
        ),
        ("/ledgers/0/pair", Value::Null),
        ("/ledgers/0/pair/live/name", json!("different")),
        ("/ledgers/0/pair/before/size", json!(0)),
        ("/ledgers/0/pair/after/size", json!(0)),
        ("/ledgers/0/pair/after/mode", json!(0o600)),
        ("/ledgers/0/pair/after/sha256", json!("f".repeat(64))),
        ("/ledgers/0/pair/after/uid", json!(u64::from(owner) + 1)),
    ] {
        let mut value_doc = original.clone();
        *value_doc.pointer_mut(pointer).ok_or(pointer)? = value;
        let decoded = decode::<Manifest>(&serde_json::to_vec(&value_doc)?);
        assert!(
            decoded.is_err()
                || decoded.as_ref().is_ok_and(|manifest| policy::validate(
                    manifest,
                    &authority.paths,
                    owner
                )
                .is_err()),
            "{pointer}"
        );
    }
    let mut pending = batch.pending.clone();
    let pair = batch.manifest.ledgers[0].pair.as_ref().ok_or("pair")?;
    for restored in [false, true] {
        let candidate = crate::policy::capability_tx::Candidate {
            target: pair.live.path(),
            staged: Some(pair.retained.path()),
            after: pair.after.state(),
        };
        if restored {
            pending.ledgers[0].restored = Some(candidate);
        } else {
            pending.ledgers[0].forward = Some(candidate);
        }
        assert!(
            policy::validate_pending(&batch.manifest, &pending, &batch.pending.manifest_sha256)
                .is_err()
        );
    }
    Ok(())
}
#[test]
fn legacy_v5_upgrade_authority_closes_root_overlap_duplicate_member_and_nonserializable_package_edges()
-> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let (_temp, ctx, _store, batch, _journal) = prepared_complete_upgrade()?;
    let authority = crate::governance::capture(&ctx)?;
    let owner = nix::unistd::geteuid().as_raw();
    for variant in 0..8 {
        let mut manifest = batch.manifest.clone();
        let SourceAction::Upgrade { roots, .. } = &mut manifest.action else {
            return Err("action".into());
        };
        match variant {
            0 => roots[0].change.after.identity = roots[0].change.before.identity.clone(),
            1 => {
                let root = &roots[0];
                let mut witness = manifest.inputs[0].clone();
                witness.path = root.change.live.path();
                witness.namespace.ancestor = root.change.live.parent.clone();
                witness.namespace.remaining = vec![root.change.live.name.clone()];
                manifest.inputs.push(witness);
            }
            2 => {
                let root = &roots[0];
                let mut witness = manifest.inputs[0].clone();
                witness.path = root.change.retained.path();
                witness.namespace.ancestor = root.change.retained.parent.clone();
                witness.namespace.remaining = vec![root.change.retained.name.clone()];
                manifest.inputs.push(witness);
            }
            3 => {
                let raw = std::ffi::OsString::from_vec(vec![b'/', 255]);
                roots[1]
                    .native_before
                    .as_mut()
                    .ok_or("native")?
                    .package
                    .paths
                    .push(crate::domain::OwnedPath {
                        path: raw.into(),
                        kind: crate::domain::PathKind::Data,
                    });
            }
            4 => {
                let raw = std::ffi::OsString::from_vec(vec![b'/', 255]);
                roots[1]
                    .native_after
                    .as_mut()
                    .ok_or("native")?
                    .package
                    .paths
                    .push(crate::domain::OwnedPath {
                        path: raw.into(),
                        kind: crate::domain::PathKind::Data,
                    });
            }
            5 => {
                let mut duplicate = roots[1].clone();
                duplicate.baseline.source.canonical_name = "duplicate".into();
                duplicate.baseline.slot.name = "duplicate".into();
                duplicate.change.live = duplicate.baseline.slot.clone();
                duplicate.change.before.identity.inode = 100_001;
                duplicate.baseline.root_identity = duplicate.change.before.identity.clone();
                duplicate.change.after.identity.inode = 100_002;
                duplicate.change.retained.name = "payload-duplicate".into();
                *roots = vec![roots[1].clone(), duplicate];
            }
            6 => {
                roots[0].change.after.entries.remove("SKILL.md");
            }
            _ => {
                manifest.ledgers[0]
                    .pair
                    .as_mut()
                    .ok_or("pair")?
                    .retained
                    .parent
                    .requested = ctx.state.join("receipts.json");
            }
        }
        assert!(
            policy::validate(&manifest, &authority.paths, owner).is_err(),
            "{variant}"
        );
    }
    Ok(())
}
#[test]
fn adoption_rejects_upgrade_pair_authority_and_unchanged_ledger_still_has_exact_after_semantics()
-> TestResult {
    let (_temp, ctx) = native_fixture()?;
    let (_store, mut batch, _journal) = prepared(&ctx)?;
    let ledger = &batch.manifest.ledgers[0];
    let progress = &batch.pending.ledgers[0];
    assert!(policy::is_after(ledger, progress, &ledger.witness.state));
    assert!(!policy::is_after(ledger, progress, &State::Missing));
    let witness = batch.manifest.ledgers[2].witness.clone();
    let raw = batch
        .blobs
        .get(batch.manifest.ledgers[2].after.as_ref().ok_or("after")?)?;
    batch.manifest.ledgers[2].pair = Some(crate::source_files::stage(&witness, &raw, 0o600)?);
    assert!(
        policy::validate(
            &batch.manifest,
            &crate::governance::capture(&ctx)?.paths,
            nix::unistd::geteuid().as_raw()
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn historical_native_projection_checks_original_row_pin_and_saved_after_receipt_before_recovery()
-> TestResult {
    let (_temp, ctx, _store, batch, _journal) = prepared_complete_upgrade()?;
    let SourceAction::Upgrade { roots, .. } = &batch.manifest.action else {
        return Err("upgrade".into());
    };
    let projection = &crate::policy::source_projection::native;
    let raw = batch.manifest.ledgers[0]
        .before
        .as_ref()
        .map(|b| batch.blobs.get(b))
        .transpose()?
        .ok_or("native")?;
    assert!(projection(None, roots, 1).is_err());
    assert!(projection(Some(b"{"), roots, 1).is_err());
    for (key, value) in [
        ("version", json!(4)),
        ("sourceType", json!("local")),
        ("skillPath", json!("other")),
        ("skillFolderHash", json!("changed")),
    ] {
        let mut doc: Value = serde_json::from_slice(&raw)?;
        if key == "version" {
            doc[key] = value;
        } else {
            doc["skills"]["demo"][key] = value;
        }
        assert!(projection(Some(&serde_json::to_vec(&doc)?), roots, 1).is_err());
    }
    let native = roots
        .iter()
        .find(|r| r.native_before.is_some())
        .ok_or("native")?
        .clone();
    let asset = roots
        .iter()
        .find(|r| r.native_before.is_none())
        .ok_or("asset")?
        .clone();
    assert_eq!(
        projection(Some(&raw), std::slice::from_ref(&asset), 1)?,
        Some(raw.clone())
    );
    let mut pinned = native.clone();
    pinned.explicit_pin = true;
    pinned.requested_ref = Some(pinned.commit.clone());
    let after = projection(Some(&raw), std::slice::from_ref(&pinned), 1)?.ok_or("after")?;
    assert_eq!(
        serde_json::from_slice::<Value>(&after)?["skills"]["demo"]["ref"],
        pinned.commit
    );
    assert!(projection(Some(&raw), std::slice::from_ref(&native), u64::MAX).is_err());
    let mut mismatch = native.clone();
    mismatch.requested_ref = Some("different".into());
    assert!(projection(Some(&raw), &[mismatch], 1).is_err());
    let after = batch.manifest.ledgers[0]
        .after
        .as_ref()
        .map(|b| batch.blobs.get(b))
        .transpose()?
        .ok_or("native")?;
    assert!(
        crate::policy::source_projection::receipt_specs(std::slice::from_ref(&native), None)
            .is_err()
    );
    let mut mismatch = native;
    mismatch.native_after.as_mut().ok_or("spec")?.expected =
        crate::domain::Observation::CapabilityPack {
            capability: crate::domain::CapabilityKind::SkillsCli,
            pack: None,
        };
    let _ = ctx;
    assert!(crate::policy::source_projection::receipt_specs(&[mismatch], Some(&after)).is_err());
    Ok(())
}

#[test]
fn historical_projection_refuses_foreign_declarations_missing_baseline_and_inconsistent_no_change_ledger()
-> TestResult {
    use crate::policy::source_projection as project;
    let (_temp, ctx, _store, batch, _journal) = prepared_complete_upgrade()?;
    let SourceAction::Upgrade { roots, .. } = &batch.manifest.action else {
        return Err("upgrade".into());
    };
    let asset = roots
        .iter()
        .find(|r| r.native_before.is_none())
        .ok_or("asset")?
        .clone();
    let native = roots
        .iter()
        .find(|r| r.native_before.is_some())
        .ok_or("native")?
        .clone();
    let definitions = batch.manifest.ledgers[1]
        .before
        .as_ref()
        .map(|b| batch.blobs.get(b))
        .transpose()?
        .ok_or("definitions")?;
    assert_eq!(
        project::definitions(Some(&definitions), std::slice::from_ref(&native))?,
        Some(definitions.clone())
    );
    assert!(project::definitions(None, std::slice::from_ref(&asset)).is_err());
    for bytes in [b"{".as_slice(), b"{}", br#"{"assets":{"alpha":null}}"#] {
        assert!(project::definitions(Some(bytes), std::slice::from_ref(&asset)).is_err());
    }
    for variant in 0..3 {
        let mut wrong = asset.clone();
        match variant {
            0 => wrong.baseline.source.repository = "other/repo".into(),
            1 => wrong.baseline.source.skill_path = "other".into(),
            _ => wrong.baseline.source.member = "asset:other".into(),
        }
        assert!(project::definitions(Some(&definitions), &[wrong]).is_err());
    }
    let managed = batch.manifest.ledgers[2]
        .before
        .as_ref()
        .map(|b| batch.blobs.get(b))
        .transpose()?
        .ok_or("managed")?;
    let uid = nix::unistd::geteuid().as_raw();
    assert!(project::managed(None, std::slice::from_ref(&asset), &ctx.home, uid).is_err());
    let mut wrong = asset.clone();
    wrong.baseline.root_identity.inode += 1;
    assert!(project::managed(Some(&managed), &[wrong], &ctx.home, uid).is_err());
    let mut wrong = asset.clone();
    wrong.change.before.identity.inode += 1;
    assert!(project::managed(Some(&managed), &[wrong], &ctx.home, uid).is_err());
    let mut wrong = asset.clone();
    wrong.change.after.mode = 0o1777;
    assert!(project::managed(Some(&managed), &[wrong], &ctx.home, uid).is_err());
    project::overlays(None, None, std::slice::from_ref(&asset), &ctx.home)?;
    assert!(project::overlays(None, Some(b"{}"), &[], &ctx.home).is_err());
    assert!(project::overlays(None, None, std::slice::from_ref(&native), &ctx.home).is_err());
    let mut wrong = asset;
    wrong.base_shape.mode = 0o700;
    assert!(project::overlays(None, None, &[wrong], &ctx.home).is_err());
    let overlays = batch.manifest.ledgers[3]
        .before
        .as_ref()
        .map(|b| batch.blobs.get(b))
        .transpose()?
        .ok_or("overlay")?;
    assert!(project::overlays(Some(&overlays), None, roots, &ctx.home).is_err());
    let mut manifest = batch.manifest.clone();
    let before_image = manifest.ledgers[0].before.clone();
    manifest.ledgers[0].after = before_image;
    manifest.ledgers[0].after_mode = policy::mode(&manifest.ledgers[0].witness.state);
    manifest.ledgers[0].pair = None;
    let authority = crate::governance::capture(&ctx)?;
    policy::validate(&manifest, &authority.paths, uid)?;
    assert!(verify_projection(&manifest, &batch.blobs).is_err());
    Ok(())
}

#[test]
fn completed_historical_placement_refuses_to_abandon_and_preserves_its_original_receipt()
-> TestResult {
    let (_temp, ctx, store, mut batch, mut journal) = prepared_complete_upgrade()?;
    batch.publish(&store)?;
    let marker_path = &batch.manifest.pending;
    let marker = fs::read(marker_path)?;
    let receipt = &batch.manifest.ledgers[4];
    let pair = receipt.pair.as_ref().ok_or("frozen receipt pair")?;
    let original = batch
        .blobs
        .get(receipt.before.as_ref().ok_or("before receipt")?)?;
    let published = batch
        .blobs
        .get(receipt.after.as_ref().ok_or("after receipt")?)?;
    assert_ne!(published, original);
    assert_eq!(fs::read(&receipt.witness.path)?, published);
    assert_eq!(fs::read(pair.retained.path())?, original);
    let active = fs::read(ctx.state.join("active.json"))?;

    let refused = recover_journal(&ctx, &store, &mut journal, true, true)
        .err()
        .ok_or("historical operation must not skip verification")?;
    assert_eq!(refused.code, "source_pending_cannot_abandon");
    assert_eq!(fs::read(marker_path)?, marker);
    assert_eq!(fs::read(ctx.state.join("active.json"))?, active);
    assert_eq!(fs::read(&receipt.witness.path)?, published);
    assert_eq!(fs::read(pair.retained.path())?, original);

    let recovered = recover_journal(&ctx, &store, &mut journal, false, true)?;
    assert_eq!(recovered["phase"], "succeeded");
    assert!(!marker_path.exists());
    assert_eq!(fs::read(&receipt.witness.path)?, published);
    assert_eq!(fs::read(pair.retained.path())?, original);
    let completed = fs::read(ctx.state.join("active.json"))?;
    let repeated = recover_journal(&ctx, &store, &mut journal, false, true)?;
    assert_eq!(repeated["phase"], "succeeded");
    assert_eq!(fs::read(ctx.state.join("active.json"))?, completed);
    Ok(())
}

#[test]
fn historical_progress_permissions_block_recovery_on_either_side_of_pending_removal() -> TestResult
{
    for removed in [false, true] {
        let (_temp, ctx, store, mut batch, mut journal) = prepared_complete_upgrade()?;
        batch.publish(&store)?;
        let progress = if removed {
            batch.clear(&store, true)?;
            batch.root.path.join("source-candidates.json")
        } else {
            batch.manifest.pending.clone()
        };
        let progress_bytes = fs::read(&progress)?;
        let active = ctx.state.join("active.json");
        let active_bytes = fs::read(&active)?;
        let receipt = &batch.manifest.ledgers[4].witness.path;
        let delivered = fs::read(receipt)?;
        fs::set_permissions(&progress, fs::Permissions::from_mode(0o640))?;
        let refused = recover_journal(&ctx, &store, &mut journal, false, true)
            .err()
            .ok_or("a non-private progress record must stop recovery")?;
        assert_eq!(refused.code, invalid().code);
        assert_eq!(journal.phase, Phase::Running);
        assert_eq!(fs::read(&active)?, active_bytes);
        assert_eq!(fs::read(&progress)?, progress_bytes);
        assert_eq!(
            fs::metadata(&progress)?.permissions().mode() & 0o7777,
            0o640
        );
        assert_eq!(fs::read(receipt)?, delivered);
        assert_eq!(batch.manifest.pending.exists(), !removed);

        fs::set_permissions(&progress, fs::Permissions::from_mode(0o600))?;
        assert_eq!(
            recover_journal(&ctx, &store, &mut journal, false, true)?["phase"],
            "succeeded"
        );
        assert_eq!(fs::read(receipt)?, delivered);
        assert!(!batch.manifest.pending.exists());
    }
    Ok(())
}

#[test]
fn historical_projection_requires_the_original_after_metadata_and_receipt_for_placed_roots()
-> TestResult {
    let (_temp, ctx, store, mut batch, _journal) = prepared_complete_upgrade()?;
    batch.publish(&store)?;
    let marker = fs::read(&batch.manifest.pending)?;
    let authority = crate::governance::capture(&ctx)?;
    let owner = nix::unistd::geteuid().as_raw();
    for slot in [Slot::Definitions, Slot::Managed, Slot::Receipts] {
        let mut stale = batch.manifest.clone();
        let ledger = stale
            .ledgers
            .iter_mut()
            .find(|l| l.slot == slot)
            .ok_or("ledger")?;
        let path = ledger.witness.path.clone();
        let after = fs::read(&path)?;
        let before = batch
            .blobs
            .get(ledger.before.as_ref().ok_or("before evidence")?)?;
        assert_ne!(after, before);
        ledger.after = ledger.before.clone();
        ledger.after_mode = policy::mode(&ledger.witness.state);
        ledger.pair = None;
        policy::validate(&stale, &authority.paths, owner)?;

        let error = verify_projection(&stale, &batch.blobs)
            .err()
            .ok_or("stale projection")?;
        assert_eq!(error.code, invalid().code, "{slot:?}");
        assert_eq!(fs::read(path)?, after);
        assert_eq!(fs::read(&batch.manifest.pending)?, marker);
    }
    verify_projection(&batch.manifest, &batch.blobs)?;
    assert!(batch.matches(true)?);
    Ok(())
}

#[test]
fn historical_declared_only_upgrade_keeps_native_and_receipt_images_unchanged() -> TestResult {
    let (_temp, ctx, _store, batch, _journal) = prepared_complete_upgrade()?;
    let mut declared_only = batch.manifest.clone();
    let SourceAction::Upgrade { roots, .. } = &mut declared_only.action else {
        return Err("upgrade".into());
    };
    let native_member = roots
        .iter()
        .find(|root| root.native_before.is_some())
        .ok_or("native fixture")?
        .baseline
        .source
        .member
        .clone();
    roots.retain(|root| root.native_before.is_none());
    assert_eq!(roots.len(), 1);
    let selected = &roots[0];
    let owner = nix::unistd::geteuid().as_raw();
    let baseline = &mut declared_only.ledgers[2];
    let before = batch
        .blobs
        .get(baseline.before.as_ref().ok_or("managed before")?)?;
    let after = crate::policy::source_projection::managed(Some(&before), roots, &ctx.home, owner)?
        .ok_or("projected managed images")?;
    let before_book = crate::policy::source_baseline::read(Some(&before), &ctx.home, owner)?;
    let after_book = crate::policy::source_baseline::read(Some(&after), &ctx.home, owner)?;
    assert_eq!(
        before_book.roots[&native_member],
        after_book.roots[&native_member]
    );
    assert_eq!(
        after_book.roots[&selected.baseline.source.member]
            .revision
            .as_ref()
            .ok_or("asset revision")?
            .commit,
        selected.commit
    );
    baseline.after = Some(batch.blobs.put(&after)?);
    baseline.pair = Some(crate::source_files::stage(
        &baseline.witness,
        &after,
        baseline.after_mode.ok_or("managed mode")?,
    )?);

    for index in [0, 3, 4] {
        let unchanged = &mut declared_only.ledgers[index];
        unchanged.after = unchanged.before.clone();
        unchanged.after_mode = policy::mode(&unchanged.witness.state);
        unchanged.pair = None;
    }
    let authority = crate::governance::capture(&ctx)?;
    policy::validate(&declared_only, &authority.paths, owner)?;
    verify_projection(&declared_only, &batch.blobs)?;
    for index in [0, 3, 4] {
        let ledger = &declared_only.ledgers[index];
        let original = batch
            .blobs
            .get(ledger.before.as_ref().ok_or("prior evidence")?)?;
        assert_eq!(fs::read(&ledger.witness.path)?, original);
    }
    assert!(batch.manifest.pending.is_file());
    Ok(())
}

fn historical_near_document_budget(
    raw: &[u8],
) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut document: Value = serde_json::from_slice(raw)?;
    document["future_padding"] = json!("");
    let overhead = serde_json::to_vec(&document)?.len();
    document["future_padding"] = json!("x".repeat(16 * 1024 * 1024 - overhead));
    let encoded = serde_json::to_vec(&document)?;
    assert_eq!(encoded.len(), 16 * 1024 * 1024);
    Ok(encoded)
}
#[test]
fn historical_projection_never_grows_frozen_native_definitions_or_baseline_images_past_the_document_budget()
-> TestResult {
    use crate::policy::source_projection as project;
    let (_temp, ctx, _store, batch, _journal) = prepared_complete_upgrade()?;
    let SourceAction::Upgrade { roots, .. } = &batch.manifest.action else {
        return Err("upgrade".into());
    };
    let raw = |index: usize| -> Result<Vec<u8>> {
        batch.blobs.get(
            batch.manifest.ledgers[index]
                .before
                .as_ref()
                .ok_or_else(invalid)?,
        )
    };
    let native = raw(0)?;
    assert!(project::native(Some(b"{}"), roots, 1).is_err());
    let mut opaque: Value = serde_json::from_slice(&native)?;
    let mut nested = Value::Null;
    for _ in 0..40 {
        nested = json!({"nested":nested});
    }
    opaque["future_opaque"] = nested;
    let opaque = serde_json::to_vec(&opaque)?;
    let selected = roots
        .iter()
        .find(|r| r.native_before.is_some())
        .ok_or("native")?;
    let spec = selected.native_before.as_ref().ok_or("spec")?;
    assert_eq!(
        crate::policy::source_receipts::known_skill(
            &spec.package.id,
            &spec.query,
            &opaque,
            &selected.change.live,
            &selected.change.before,
            selected.change.before.uid
        )?,
        spec.expected
    );
    assert!(project::native(Some(&opaque), roots, 1).is_err());
    assert!(project::native(Some(&historical_near_document_budget(&native)?), roots, 1).is_err());
    let mut definitions: Value = serde_json::from_slice(&raw(1)?)?;
    definitions["assets"]["alpha"]
        .as_object_mut()
        .ok_or("asset")?
        .remove("revision");
    let definitions = historical_near_document_budget(&serde_json::to_vec(&definitions)?)?;
    assert!(project::definitions(Some(&definitions), roots).is_err());
    assert!(
        project::managed(
            Some(b"{"),
            roots,
            &ctx.home,
            nix::unistd::geteuid().as_raw()
        )
        .is_err()
    );
    let managed = historical_near_document_budget(&raw(2)?)?;
    assert!(
        project::managed(
            Some(&managed),
            roots,
            &ctx.home,
            nix::unistd::geteuid().as_raw()
        )
        .is_err()
    );
    let mut changed = roots.clone();
    changed
        .iter_mut()
        .find(|r| r.native_after.is_some())
        .ok_or("native")?
        .native_after
        .as_mut()
        .ok_or("spec")?
        .query
        .native_id = "missing".into();
    let native_after = batch
        .blobs
        .get(batch.manifest.ledgers[0].after.as_ref().ok_or("native")?)?;
    assert!(project::receipt_specs(&changed, Some(&native_after)).is_err());
    Ok(())
}
