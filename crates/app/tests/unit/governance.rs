#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;

fn context(home: &std::path::Path) -> Result<Context> {
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(|| Error::new("utf8", "home"))?,
        "list",
    ])
    .map_err(|_| Error::new("parse", "fixture"))?;
    Context::from_cli(&cli, &mut vec![])
}
#[test]
fn missing_ledger_is_read_only_legacy_and_a_late_ledger_cannot_rebind_the_snapshot() -> TestResult {
    let dir = tempfile::tempdir_in("/private/tmp")?;
    let home = dir.path().join("home");
    fs::create_dir_all(&home)?;
    let ctx = context(&home)?;
    let authority = capture(&ctx)?;
    assert_eq!(
        authority.paths.managed,
        home.join(".agents/managed-assets.json")
    );
    assert!(authority.read(Dataset::ManagedAssets, 1024)?.is_none());
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    fs::create_dir_all(&ctx.state)?;
    fs::write(ctx.state.join("migration-ledger.json"), "{}")?;
    assert!(authority.verify().is_err());
    assert_eq!(
        capture(&ctx).err().ok_or("invalid ledger")?.code,
        "invalid_migration_ledger"
    );
    Ok(())
}

fn directory_binding(path: &std::path::Path) -> Result<DirectoryBinding> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    let dir = Directory::open(path)?;
    Ok(DirectoryBinding {
        requested: path.into(),
        canonical: dir.path.clone(),
        identity: dir.identity().clone(),
        lineage: dir.lineage()?,
        uid: dir.file().metadata()?.uid(),
        mode: 0o700,
    })
}

fn committed(ctx: &Context) -> Result<policy::CommittedMigration> {
    Ok(policy::CommittedMigration {
        schema_version: 1,
        phase: policy::MigrationPhase::Committed,
        migration_id: "OP-100-1".into(),
        home: ctx.home.clone(),
        legacy_settings: ctx.settings.clone(),
        layout: policy::Layout {
            config: ctx.config.clone(),
            data: ctx.data.clone(),
            state: ctx.state.clone(),
            cache: ctx.cache.clone(),
        },
        root_bindings: policy::DurableBindings {
            config: directory_binding(&ctx.config)?,
            data: directory_binding(&ctx.data)?,
            state: directory_binding(&ctx.state)?,
        },
        cache_audit: Some(directory_binding(&ctx.cache)?),
        converted: policy::DATASETS
            .into_iter()
            .map(|key| (key, policy::Transfer::Missing))
            .collect(),
        legacy_links: vec![],
        archives: vec![],
        retained: vec![],
    })
}

fn save(ctx: &Context, ledger: &policy::CommittedMigration) -> Result<()> {
    fs::write(
        ctx.state.join("migration-ledger.json"),
        serde_json::to_vec(ledger)?,
    )?;
    Ok(())
}

#[test]
fn committed_authority_survives_cache_eviction_override_and_never_uses_the_legacy_dataset()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(&home)?;
    let mut ctx = context(&home)?;
    let ledger = committed(&ctx)?;
    save(&ctx, &ledger)?;
    fs::create_dir_all(home.join(".agents"))?;
    fs::write(home.join(".agents/managed-assets.json"), "legacy-data")?;
    let authority = capture(&ctx)?;
    assert!(matches!(
        authority.paths.authority,
        policy::Authority::Migrated { .. }
    ));
    assert!(authority.read(Dataset::ManagedAssets, 1024)?.is_none());
    fs::write(ctx.state.join("managed-assets.json"), "new-data")?;
    assert_eq!(
        authority.read(Dataset::ManagedAssets, 1024)?,
        Some(b"new-data".to_vec())
    );
    fs::rename(&ctx.cache, temp.path().join("old-cache"))?;
    assert!(
        capture(&ctx)?
            .read(Dataset::ManagedUpdates, 1024)?
            .is_none()
    );
    directory_binding(&ctx.cache)?;
    assert_ne!(
        Directory::open(&ctx.cache)?.identity(),
        &ledger.cache_audit.as_ref().ok_or("audit")?.identity
    );
    assert!(
        capture(&ctx)?
            .read(Dataset::ManagedUpdates, 1024)?
            .is_none()
    );
    ctx.cache = temp.path().join("override-cache");
    let overridden = capture(&ctx)?;
    assert_eq!(
        overridden.paths.updates,
        ctx.cache.join("managed-updates.json")
    );
    assert!(overridden.read(Dataset::ManagedUpdates, 1024)?.is_none());
    assert!(!ctx.cache.exists());
    Ok(())
}

#[test]
fn durable_root_permission_lineage_or_ledger_replacement_invalidates_an_existing_epoch()
-> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(&home)?;
    let ctx = context(&home)?;
    let ledger = committed(&ctx)?;
    save(&ctx, &ledger)?;
    let authority = capture(&ctx)?;
    fs::set_permissions(&ctx.data, fs::Permissions::from_mode(0o755))?;
    assert!(authority.verify().is_err());
    fs::set_permissions(&ctx.data, fs::Permissions::from_mode(0o700))?;
    authority.verify()?;
    let parent = ctx.config.parent().ok_or("parent")?;
    let moved = home.join("old-config-parent");
    fs::rename(parent, &moved)?;
    fs::create_dir_all(parent)?;
    fs::rename(moved.join("macops"), &ctx.config)?;
    assert_eq!(
        Directory::open(&ctx.config)?.identity(),
        &ledger.root_bindings.config.identity
    );
    assert!(authority.verify().is_err());
    assert!(capture(&ctx).is_err());
    let mut repaired = ledger;
    repaired.root_bindings.config = directory_binding(&ctx.config)?;
    save(&ctx, &repaired)?;
    let authority = capture(&ctx)?;
    let replacement = ctx.state.join("new-ledger");
    fs::write(&replacement, serde_json::to_vec(&repaired)?)?;
    fs::rename(replacement, ctx.state.join("migration-ledger.json"))?;
    assert!(authority.verify().is_err());
    Ok(())
}

#[test]
fn non_regular_ledger_or_dataset_is_never_interpreted_as_absent() -> TestResult {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(&home)?;
    let ctx = context(&home)?;
    let ledger = committed(&ctx)?;
    save(&ctx, &ledger)?;
    let path = ctx.state.join("migration-ledger.json");
    let outside = temp.path().join("outside");
    fs::write(&outside, "untouched")?;
    fs::remove_file(&path)?;
    symlink(&outside, &path)?;
    assert_eq!(
        capture(&ctx).err().ok_or("link")?.code,
        "invalid_migration_ledger"
    );
    fs::remove_file(&path)?;
    let created = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&path)
        .status()?;
    assert!(created.success());
    let started = std::time::Instant::now();
    assert_eq!(
        capture(&ctx).err().ok_or("fifo")?.code,
        "invalid_migration_ledger"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    fs::remove_file(&path)?;
    save(&ctx, &ledger)?;
    let authority = capture(&ctx)?;
    fs::create_dir(ctx.state.join("managed-assets.json"))?;
    assert_eq!(
        authority
            .read(Dataset::ManagedAssets, 1024)
            .err()
            .ok_or("directory")?
            .code,
        "invalid_governance_dataset"
    );
    assert_eq!(fs::read_to_string(outside)?, "untouched");
    Ok(())
}

#[test]
fn pending_state_is_frozen_and_inspection_does_not_clear_or_bypass_it() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(&home)?;
    let ctx = context(&home)?;
    let authority = capture(&ctx)?;
    let pending = home.join(".agents/invocation-policy.pending.json");
    fs::create_dir_all(pending.parent().ok_or("parent")?)?;
    fs::write(&pending, "pending")?;
    assert!(authority.verify().is_err());
    assert_eq!(
        read(&ctx, |_| Ok(())).err().ok_or("blocked")?.code,
        "capability_recovery_required"
    );
    inspect(&ctx, |state| {
        assert!(state.ensure_settled().is_err());
        Ok(())
    })?;
    assert_eq!(fs::read_to_string(pending)?, "pending");
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn v2_committed_authority_checks_anchored_native_aliases_and_requires_converted_bytes() -> TestResult
{
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir_all(&home)?;
    let mut ctx = context(&home)?;
    let original = home.join(".agents/managed-assets.json");
    fs::create_dir_all(home.join(".agents"))?;
    fs::write(&original, br#"{"legacy":true}"#)?;
    let (_, original_proof) = crate::migration_fs::snapshot(&original)?.ok_or("original proof")?;
    let mut ledger = committed(&ctx)?;
    ledger.schema_version = policy::LEDGER_VERSION;
    ledger.converted.insert(
        Dataset::ManagedAssets,
        policy::Transfer::Copied {
            source: original,
            target: ctx.state.join("managed-assets.json"),
            target_sha256: original_proof.sha256.clone(),
            original: Box::new(original_proof),
            target_mode: 0o600,
        },
    );
    save(&ctx, &ledger)?;
    let missing = capture(&ctx)?
        .read(Dataset::ManagedAssets, 1024)
        .err()
        .ok_or("missing")?;
    assert_eq!(missing.code, "missing_governance_dataset");

    let native_parent = temp.path().join("native-state");
    let native_root = native_parent.join("gh");
    fs::create_dir_all(&native_root)?;
    let alternate_home_state = home.join(".local/state");
    let anchored_state = temp.path().join("actual-state");
    fs::rename(&alternate_home_state, &anchored_state)?;
    symlink(&native_parent, &alternate_home_state)?;
    ctx.state = anchored_state.join("macops");
    ledger.layout.state.clone_from(&ctx.state);
    ledger.root_bindings.state = directory_binding(&ctx.state)?;
    if let Some(policy::Transfer::Copied { target, .. }) =
        ledger.converted.get_mut(&Dataset::ManagedAssets)
    {
        *target = ctx.state.join("managed-assets.json");
    }
    save(&ctx, &ledger)?;
    ctx.cache = native_root.join("missing");
    assert_eq!(
        capture(&ctx).err().ok_or("aliased cache")?.code,
        "invalid_migration_ledger"
    );
    assert!(!ctx.cache.exists());
    Ok(())
}

#[test]
fn committed_v1_can_verify_a_historical_native_named_state_without_reopening_writes() -> TestResult
{
    use crate::cli::Migrate;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let mut ctx = context(&home)?;
    ctx.state = home.join(".local/state/gh");
    let ledger = committed(&ctx)?;
    save(&ctx, &ledger)?;
    let ledger_path = ctx.state.join("migration-ledger.json");
    let before = fs::read(&ledger_path)?;
    let verified = crate::migration::run(&ctx, &Migrate::Verify, false, false)?;
    assert_eq!(verified["mode"], "verify");
    assert_eq!(verified["authority"], "migrated");
    let recover = Migrate::Recover {
        migration_id: ledger.migration_id,
    };
    for (command, dry_run) in [
        (&Migrate::Preflight, false),
        (&Migrate::Run, true),
        (&Migrate::Run, false),
        (&recover, true),
        (&recover, false),
    ] {
        let error = crate::migration::run(&ctx, command, dry_run, true)
            .err()
            .ok_or("write scope")?;
        assert_eq!(error.code, "migration_input_changed");
    }
    let marker = ctx.state.join("migration.pending.json");
    fs::write(&marker, b"keep pending")?;
    let error = crate::migration::run(&ctx, &Migrate::Verify, false, false)
        .err()
        .ok_or("pending")?;
    assert_eq!(error.code, "migration_recovery_required");
    assert_eq!(fs::read(&marker)?, b"keep pending");
    assert_eq!(fs::read(&ledger_path)?, before);
    assert!(!ctx.state.join("write.lock").exists());
    Ok(())
}
