use super::*;
use crate::{cli::Cli, migration_scan, test_support::TestResult};
use clap::Parser;
use std::{
    fs as disk,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

fn fixture() -> std::result::Result<(tempfile::TempDir, Context), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let settings = home.join(".local/settings");
    disk::create_dir_all(settings.join("manifests"))?;
    disk::create_dir_all(home.join(".agents/cache"))?;
    disk::write(settings.join("dirty-new.txt"), "original dirty content\n")?;
    let data = [
        (
            ".agents/managed-assets.json",
            "{\"vendor\":{\"version\":\"1\"}}",
        ),
        (
            ".agents/invocation-overlays.json",
            "{\"schema_version\":1,\"files\":{}}",
        ),
        (".agents/cache/managed-updates.json", "{}"),
        (".local/settings/manifests/skill-profiles.json", "{}"),
        (
            ".local/settings/manifests/managed-agent-assets.json",
            "{\"schema_version\":1,\"assets\":{}}",
        ),
        (
            ".local/settings/manifests/agent-invocation-policy.json",
            "{\"schema_version\":1,\"standalone_manual_only\":[]}",
        ),
        (
            ".local/settings/manifests/agent-source-channels.json",
            "{\"schema_version\":1}",
        ),
        (
            ".local/settings/manifests/mac-apps.tsv",
            "path\tbundle_id\tversion\towner\tsource\n",
        ),
    ];
    for (relative, bytes) in data {
        disk::write(home.join(relative), bytes)?;
    }
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    Ok((temp, ctx))
}

fn prepared(ctx: &Context) -> Result<(Store, Batch)> {
    let store = Store::lock(ctx)?;
    let scan = migration_scan::capture(ctx, true)?;
    Ok((store, prepare(ctx, scan, &crate::store::operation_id())?))
}

use crate::store::Store;

#[test]
fn old_writer_observation_is_rechecked_at_placement_and_commit() -> TestResult {
    for after_place in [false, true] {
        let (_temp, ctx) = fixture()?;
        let writer = ctx.home.join(".agents/bin/skillctl");
        disk::create_dir(writer.parent().ok_or("writer parent")?)?;
        let mask = b"#!/bin/sh\nexit 75\n";
        disk::write(&writer, mask)?;
        disk::set_permissions(&writer, disk::Permissions::from_mode(0o700))?;
        let (_store, batch) = prepared(&ctx)?;
        if after_place {
            batch.place()?;
        }
        let replacement = writer.with_file_name("replacement");
        disk::write(&replacement, mask)?;
        disk::set_permissions(&replacement, disk::Permissions::from_mode(0o700))?;
        disk::rename(replacement, &writer)?;

        let result = if after_place {
            batch.commit(&ctx)
        } else {
            batch.place()
        };
        assert_eq!(
            result.err().ok_or("writer change accepted")?.code,
            "migration_old_writer_active"
        );
        assert!(!ctx.state.join("migration-ledger.json").exists());
        assert_eq!(ctx.state.join("managed-assets.json").exists(), after_place);
        batch.rollback()?;
        assert!(!ctx.state.join("managed-assets.json").exists());
        assert_eq!(disk::read(&writer)?, mask);
    }
    Ok(())
}

#[test]
fn committed_cleanup_retains_ledger_when_a_legacy_writer_returns() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let (_store, batch) = prepared(&ctx)?;
    batch.place()?;
    batch.publish_ledger(&ctx)?;
    let writer = ctx.home.join(".agents/bin/skillctl");
    disk::create_dir(writer.parent().ok_or("writer parent")?)?;
    disk::write(&writer, b"#!/bin/sh\nexit 0\n")?;
    disk::set_permissions(&writer, disk::Permissions::from_mode(0o700))?;
    let batch = load(&ctx, batch.id())?;
    let result = batch.finish_committed(&ctx);
    assert_eq!(
        result.err().ok_or("cleanup progressed")?.code,
        "migration_old_writer_active"
    );
    let ledger = disk::read(ctx.state.join("migration-ledger.json"))?;
    assert!(pending(&ctx)?);
    disk::write(writer, b"#!/bin/sh\nexit 75\n")?;
    batch.finish_committed(&ctx)?;
    assert!(!pending(&ctx)?);
    assert_eq!(disk::read(ctx.state.join("migration-ledger.json"))?, ledger);
    Ok(())
}

fn seed_window(ctx: &Context, window: &str, count: usize) -> Result<Batch> {
    let id = crate::store::operation_id();
    let scan = migration_scan::capture(ctx, true)?;
    let batch = if window == "active-before-pending" {
        let result = prepare_with(ctx, scan, &id, || {
            Err(Error::attention("synthetic_stop", "after active"))
        });
        assert!(result.is_err());
        load(ctx, &id)?
    } else {
        prepare(ctx, scan, &id)?
    };
    for item in batch.manifest.placements.iter().take(count) {
        batch.publish(item)?;
    }
    if matches!(window, "after-ledger" | "clear-after") {
        batch.publish_ledger(ctx)?;
    }
    if matches!(window, "clear-before" | "clear-after") {
        assert!(
            batch
                .clear_with(|| Err(Error::attention("synthetic_stop", "after pending complete")))
                .is_err()
        );
    }
    Ok(batch)
}

#[test]
fn original_engine_can_emit_frozen_recovery_windows_for_a_separate_release_process() -> TestResult {
    use std::io::Write;
    let output = std::env::var_os("MACOPS_MIGRATION_FIXTURE_OUT").map(PathBuf::from);
    if let Some(path) = &output {
        assert!(path.starts_with("/private/tmp"));
        assert!(!path.exists());
    }
    let mut rows = Vec::new();
    let mut cases: Vec<_> = (0..=8)
        .map(|count| (format!("partial-{count}"), count))
        .collect();
    cases.extend([
        ("active-before-pending".into(), 0),
        ("after-ledger".into(), 8),
        ("clear-before".into(), 0),
        ("clear-after".into(), 8),
    ]);
    for (window, count) in cases {
        let (temp, ctx) = fixture()?;
        let _store = Store::lock(&ctx)?;
        let batch = seed_window(&ctx, &window, count)?;
        assert!(pending(&ctx)?);
        assert_eq!(pending_id(&ctx).as_deref(), Some(batch.id()));
        let copied: Vec<_> = batch
            .manifest
            .ledger
            .converted
            .iter()
            .filter_map(|(dataset, transfer)| match transfer {
                policy::Transfer::Copied { source, .. } => {
                    Some(serde_json::json!({"dataset":dataset,"source":source}))
                }
                policy::Transfer::Missing => None,
            })
            .collect();
        rows.push(serde_json::json!({"home":ctx.home,"id":batch.id(),"window":window,"committed":matches!(window.as_str(),"after-ledger"|"clear-after"),"stage":batch.stage.path,"targets":batch.manifest.placements,"copied":copied}));
        if output.is_some() {
            let _kept = temp.keep();
        }
    }
    if let Some(path) = output {
        let mut file = disk::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        file.write_all(&serde_json::to_vec_pretty(&rows)?)?;
        file.sync_all()?;
    }
    Ok(())
}

#[test]
fn atomic_no_replace_preserves_last_window_foreign_files_in_both_directions() -> TestResult {
    for rollback in [false, true] {
        let (_temp, ctx) = fixture()?;
        let (_store, batch) = prepared(&ctx)?;
        let item = &batch.manifest.placements[0];
        if rollback {
            batch.publish(item)?;
            let slot = batch.stage.path.join(&item.slot);
            let result = batch.restore_with(item, || {
                disk::write(&slot, "foreign last-window stage")?;
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(disk::read_to_string(&slot)?, "foreign last-window stage");
            assert!(fs::matches(
                batch.parent(item)?,
                item.target.file_name().ok_or("name")?,
                &item.identity,
                &item.sha256,
                item.size
            )?);
        } else {
            let result = batch.publish_with(item, || {
                disk::write(&item.target, "foreign last-window target")?;
                Ok(())
            });
            assert!(result.is_err());
            assert_eq!(
                disk::read_to_string(&item.target)?,
                "foreign last-window target"
            );
            assert!(fs::matches(
                &batch.stage,
                OsStr::new(&item.slot),
                &item.identity,
                &item.sha256,
                item.size
            )?);
        }
        assert!(pending(&ctx)?);
    }
    Ok(())
}

#[test]
fn frozen_destination_roots_cannot_be_replaced_or_relearned_before_staging() -> TestResult {
    for existing in [false, true] {
        let (_temp, ctx) = fixture()?;
        let _store = Store::lock(&ctx)?;
        if existing {
            disk::create_dir_all(&ctx.config)?;
            disk::set_permissions(&ctx.config, disk::Permissions::from_mode(0o700))?;
        }
        let scan = migration_scan::capture(&ctx, true)?;
        if existing {
            disk::rename(&ctx.config, ctx.config.with_extension("old"))?;
        }
        disk::create_dir_all(&ctx.config)?;
        disk::set_permissions(&ctx.config, disk::Permissions::from_mode(0o700))?;
        disk::write(ctx.config.join("foreign"), "preserve")?;
        assert!(prepare(&ctx, scan, &crate::store::operation_id()).is_err());
        assert_eq!(
            disk::read_to_string(ctx.config.join("foreign"))?,
            "preserve"
        );
        assert!(!pending(&ctx)?);
        assert!(committed(&ctx)?.is_none());
    }
    Ok(())
}

#[test]
fn root_witnesses_share_only_the_parent_created_by_their_own_plan() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let parent = ctx.home.join("migration-destinations");
    let configuration = parent.join("config");
    let data = parent.join("data");
    let config_witness = fs::RootWitness::capture(&configuration)?;
    let data_witness = fs::RootWitness::capture(&data)?;
    assert!(!parent.exists());

    let mut created = BTreeMap::new();
    let config_root = config_witness.create(&mut created)?;
    let parent_before = fs::binding(created.get(&parent).ok_or("created parent")?)?;
    disk::write(
        configuration.join("kept"),
        b"configuration from the original plan",
    )?;

    let mut unrelated_plan = BTreeMap::new();
    let error = data_witness
        .create(&mut unrelated_plan)
        .err()
        .ok_or("a different plan acquired the observed parent")?;
    assert_eq!(error.code, "migration_input_changed");
    assert!(unrelated_plan.is_empty());
    assert!(!data.exists());

    let data_root = data_witness.create(&mut created)?;
    assert_eq!(data_root.path, data);
    assert_eq!(config_root.path, configuration);
    assert_eq!(
        fs::binding(created.get(&parent).ok_or("created parent")?)?,
        parent_before
    );
    assert_eq!(
        created.keys().collect::<Vec<_>>(),
        [&parent, &configuration, &data]
    );
    assert!(!created.contains_key(&ctx.home));
    assert_eq!(
        disk::read(configuration.join("kept"))?,
        b"configuration from the original plan"
    );
    Ok(())
}

#[test]
fn frozen_existing_root_keeps_its_binding_and_is_never_registered_as_created() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let root = ctx.home.join("existing-root");
    disk::create_dir(&root)?;
    disk::set_permissions(&root, disk::Permissions::from_mode(0o700))?;
    let sentinel = root.join("retained");
    disk::write(&sentinel, b"previously owned file")?;
    let frozen = fs::RootWitness::capture(&root)?;
    let expected = fs::binding(&fs::directory(&root)?)?;
    let mut created = BTreeMap::new();
    assert_eq!(fs::binding(&frozen.create(&mut created)?)?, expected);
    assert!(created.is_empty());

    disk::set_permissions(&root, disk::Permissions::from_mode(0o750))?;
    let error = frozen
        .create(&mut created)
        .err()
        .ok_or("original mode changed")?;
    assert_eq!(error.code, "migration_input_changed");
    let observed_public = fs::RootWitness::capture(&root)?;
    let error = observed_public
        .create(&mut created)
        .err()
        .ok_or("existing root is not private")?;
    assert_eq!(error.code, "migration_private_directory_required");
    assert!(created.is_empty());
    assert_eq!(disk::metadata(&root)?.permissions().mode() & 0o7777, 0o750);
    assert_eq!(disk::read(sentinel)?, b"previously owned file");
    Ok(())
}

#[test]
fn a_replaced_shared_parent_never_receives_another_root_from_the_original_plan() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let parent = ctx.home.join("migration-destinations");
    let configuration = parent.join("config");
    let data = parent.join("data");
    let config_witness = fs::RootWitness::capture(&configuration)?;
    let data_witness = fs::RootWitness::capture(&data)?;
    let mut created = BTreeMap::new();
    config_witness.create(&mut created)?;
    disk::write(configuration.join("kept"), b"original plan")?;
    let original = ctx.home.join("original-destinations");
    disk::rename(&parent, &original)?;
    disk::create_dir(&parent)?;
    disk::set_permissions(&parent, disk::Permissions::from_mode(0o700))?;
    disk::write(parent.join("kept"), b"replacement owner")?;

    let error = data_witness
        .create(&mut created)
        .err()
        .ok_or("replacement directory")?;
    assert_eq!(error.code, "directory_changed");
    assert!(!created.contains_key(&data));
    assert!(!data.exists());
    assert!(!original.join("data").exists());
    assert_eq!(disk::read(original.join("config/kept"))?, b"original plan");
    assert_eq!(disk::read(parent.join("kept"))?, b"replacement owner");
    Ok(())
}

#[test]
fn reused_migration_parent_must_still_be_private_before_creating_another_root() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let parent = ctx.home.join("migration-destinations");
    let config = fs::RootWitness::capture(&parent.join("config"))?;
    let data = fs::RootWitness::capture(&parent.join("data"))?;
    let mut created = BTreeMap::new();
    let configuration = config.create(&mut created)?;
    disk::write(configuration.path.join("existing"), b"first root")?;
    disk::set_permissions(&parent, disk::Permissions::from_mode(0o755))?;
    let error = data
        .create(&mut created)
        .err()
        .ok_or("changed parent accepted")?;
    assert_eq!(error.code, "migration_private_directory_required");
    assert!(!parent.join("data").exists());
    assert_eq!(
        disk::read(configuration.path.join("existing"))?,
        b"first root"
    );
    assert_eq!(disk::metadata(parent)?.permissions().mode() & 0o7777, 0o755);
    Ok(())
}

#[test]
fn governance_observation_detects_a_migration_registered_after_its_initial_read() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let _store = Store::lock(&ctx)?;
    let observed = crate::governance::capture(&ctx)?;
    disk::write(ctx.state.join(ACTIVE), "unknown active record")?;
    assert_eq!(
        observed.verify().err().ok_or("late marker")?.code,
        "migration_recovery_required"
    );
    Ok(())
}

#[test]
fn partial_placement_crash_recovery_restores_missing_targets_and_preserves_old_sources()
-> TestResult {
    for count in 0..=8 {
        let (_temp, ctx) = fixture()?;
        let (store, batch) = prepared(&ctx)?;
        let id = batch.id().to_owned();
        for item in batch.manifest.placements.iter().take(count) {
            batch.publish(item)?;
        }
        assert!(pending(&ctx)?);
        drop(batch);
        drop(store);
        let _store = Store::lock(&ctx)?;
        let recovered = load(&ctx, &id)?;
        recovered.rollback()?;
        assert!(!pending(&ctx)?);
        assert!(committed(&ctx)?.is_none());
        for dataset in policy::DATASETS {
            assert!(
                !dataset
                    .target(&Layout {
                        config: ctx.config.clone(),
                        data: ctx.data.clone(),
                        state: ctx.state.clone(),
                        cache: ctx.cache.clone()
                    })
                    .exists()
            );
        }
        assert_eq!(
            disk::read_to_string(ctx.settings.join("dirty-new.txt"))?,
            "original dirty content\n"
        );
        for item in &recovered.manifest.placements {
            assert!(fs::matches(
                &recovered.stage,
                OsStr::new(&item.slot),
                &item.identity,
                &item.sha256,
                item.size
            )?);
        }
    }
    Ok(())
}

#[test]
fn commit_clear_gap_recovery_finishes_original_ledger_and_never_reverts_new_business() -> TestResult
{
    let (_temp, ctx) = fixture()?;
    let (store, batch) = prepared(&ctx)?;
    batch.place()?;
    migration_scan::verify_sources(&ctx, &batch.manifest.ledger)?;
    rustix::fs::renameat_with(
        batch.stage.file(),
        LEDGER_SLOT,
        batch.root.file(),
        "migration-ledger.json",
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(fs::os)?;
    batch.root.file().sync_all()?;
    let id = batch.id().to_owned();
    drop(batch);
    drop(store);
    let _store = Store::lock(&ctx)?;
    let recovered = load(&ctx, &id)?;
    recovered.finish_committed(&ctx)?;
    assert!(!pending(&ctx)?);
    disk::write(
        ctx.state.join("managed-assets.json"),
        "{\"new_business\":true}",
    )?;
    assert_eq!(crate::migration::verify(&ctx)?["phase"], "succeeded");
    assert_eq!(
        disk::read_to_string(ctx.state.join("managed-assets.json"))?,
        "{\"new_business\":true}"
    );
    Ok(())
}

#[test]
fn target_collision_wrong_inode_and_changed_mode_never_receive_rollback_authority() -> TestResult {
    for kind in ["collision", "inode", "mode", "symlink"] {
        let (_temp, ctx) = fixture()?;
        let (_store, batch) = prepared(&ctx)?;
        let item = &batch.manifest.placements[0];
        if kind == "collision" {
            disk::write(&item.target, "foreign")?;
            assert!(batch.publish(item).is_err());
        } else {
            batch.publish(item)?;
            if kind == "inode" {
                let bytes = disk::read(&item.target)?;
                disk::remove_file(&item.target)?;
                disk::write(&item.target, bytes)?;
                disk::set_permissions(&item.target, disk::Permissions::from_mode(0o600))?;
            }
            if kind == "mode" {
                disk::set_permissions(&item.target, disk::Permissions::from_mode(0o644))?;
            }
            if kind == "symlink" {
                disk::remove_file(&item.target)?;
                symlink("/never-follow", &item.target)?;
            }
        }
        assert!(batch.rollback().is_err(), "{kind}");
        assert!(pending(&ctx)?);
        if kind == "collision" {
            assert_eq!(disk::read_to_string(&item.target)?, "foreign");
        }
        if kind == "symlink" {
            assert_eq!(disk::read_link(&item.target)?, Path::new("/never-follow"));
        }
    }
    Ok(())
}

#[test]
fn changed_source_before_commit_rolls_back_without_creating_authority() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let (_store, batch) = prepared(&ctx)?;
    batch.place()?;
    disk::write(
        ctx.home.join(".agents/managed-assets.json"),
        "{\"external\":true}",
    )?;
    assert_eq!(
        batch.commit(&ctx).err().ok_or("commit")?.code,
        "migration_source_diverged"
    );
    batch.rollback()?;
    assert!(!pending(&ctx)?);
    assert!(committed(&ctx)?.is_none());
    assert_eq!(
        disk::read_to_string(ctx.home.join(".agents/managed-assets.json"))?,
        "{\"external\":true}"
    );
    Ok(())
}

#[test]
fn manifest_namespace_stage_and_marker_changes_remain_unknown_without_cleanup() -> TestResult {
    for kind in ["marker", "manifest", "stage", "parent", "missing-candidate"] {
        let (_temp, ctx) = fixture()?;
        let (_store, batch) = prepared(&ctx)?;
        let id = batch.id().to_owned();
        match kind {
            "marker" => disk::write(ctx.state.join(MARKER), "{}")?,
            "manifest" => disk::write(batch.stage.path.join(MANIFEST), "{}")?,
            "stage" => {
                disk::rename(
                    &batch.stage.path,
                    batch.stage.path.with_extension("foreign"),
                )?;
                disk::create_dir(&batch.stage.path)?;
                disk::set_permissions(&batch.stage.path, disk::Permissions::from_mode(0o700))?;
            }
            "parent" => {
                let path = ctx.config.join("manifests");
                disk::rename(&path, path.with_extension("old"))?;
                disk::create_dir(&path)?;
                disk::set_permissions(&path, disk::Permissions::from_mode(0o700))?;
            }
            _ => disk::remove_file(batch.stage.path.join(&batch.manifest.placements[0].slot))?,
        }
        assert!(batch.place().is_err(), "{kind}");
        assert!(pending(&ctx)?);
        if kind != "missing-candidate" {
            assert!(load(&ctx, &id).is_err(), "{kind}");
        }
    }
    Ok(())
}

#[test]
fn foreign_later_target_blocks_the_first_publication_without_any_partial_write() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let (_store, batch) = prepared(&ctx)?;
    let later = batch.manifest.placements.last().ok_or("last")?;
    disk::write(&later.target, "foreign")?;
    let first = batch.manifest.placements.first().ok_or("first")?;
    assert!(batch.publish(first).is_err());
    assert!(!first.target.exists());
    assert_eq!(disk::read_to_string(&later.target)?, "foreign");
    assert!(pending(&ctx)?);
    Ok(())
}

#[test]
fn committed_recovery_ignores_evicted_rebuilt_or_relocated_volatile_cache() -> TestResult {
    for mode in ["evicted", "rebuilt", "relocated", "conflict"] {
        let (_temp, mut ctx) = fixture()?;
        let (store, batch) = prepared(&ctx)?;
        batch.place()?;
        rustix::fs::renameat_with(
            batch.stage.file(),
            LEDGER_SLOT,
            batch.root.file(),
            "migration-ledger.json",
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(fs::os)?;
        batch.root.file().sync_all()?;
        let id = batch.id().to_owned();
        match mode {
            "evicted" => disk::remove_dir_all(&ctx.cache)?,
            "rebuilt" => {
                disk::rename(&ctx.cache, ctx.cache.with_extension("old"))?;
                disk::create_dir(&ctx.cache)?;
                disk::set_permissions(&ctx.cache, disk::Permissions::from_mode(0o700))?;
            }
            "relocated" => ctx.cache = ctx.home.join("replacement-cache"),
            _ => {
                disk::remove_dir_all(&ctx.cache)?;
                disk::write(&ctx.cache, "foreign non-directory cache")?;
            }
        }
        drop(batch);
        drop(store);
        let _store = Store::lock(&ctx)?;
        let recovered = load(&ctx, &id)?;
        recovered.finish_committed(&ctx)?;
        assert!(!pending(&ctx)?);
        assert_eq!(
            crate::migration::verify(&ctx)?["cache_miss"],
            true,
            "{mode}"
        );
    }
    Ok(())
}

#[test]
fn marker_identity_is_declared_before_publication_and_not_relearned_after_restart() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let (_store, batch) = prepared(&ctx)?;
    let marker = ctx.state.join(MARKER);
    let bytes = disk::read(&marker)?;
    disk::rename(&marker, ctx.state.join("retained-marker"))?;
    disk::write(&marker, bytes)?;
    disk::set_permissions(&marker, disk::Permissions::from_mode(0o600))?;
    assert!(load(&ctx, batch.id()).is_err());
    assert!(pending(&ctx)?);
    Ok(())
}

#[test]
fn migration_control_documents_reject_positional_nested_objects_and_duplicate_keys() -> TestResult {
    for shape in [
        "marker-array",
        "marker-duplicate",
        "placement-identity",
        "placement-array",
        "root-binding",
        "manifest-array",
    ] {
        let (_temp, ctx) = fixture()?;
        let (_store, batch) = prepared(&ctx)?;
        if shape == "marker-array" {
            disk::write(ctx.state.join(ACTIVE), "[]")?;
        } else if shape == "marker-duplicate" {
            disk::write(
                ctx.state.join(ACTIVE),
                "{\"schema_version\":1,\"schema_version\":1}",
            )?;
        } else {
            let mut document = serde_json::to_value(&batch.manifest)?;
            match shape {
                "placement-identity" => {
                    document["placements"][0]["identity"] = serde_json::json!([1, 2]);
                }
                "placement-array" => document["placements"][0] = serde_json::json!([]),
                "root-binding" => document["parents"][0] = serde_json::json!([]),
                _ => document = serde_json::json!([]),
            }
            let bytes = serde_json::to_vec(&document)?;
            disk::write(batch.stage.path.join(MANIFEST), &bytes)?;
            let mut pending = serde_json::to_value(&batch.pending)?;
            pending["manifest_sha256"] = fs::hash(&bytes).into();
            disk::write(ctx.state.join(MARKER), serde_json::to_vec(&pending)?)?;
            disk::write(ctx.state.join(ACTIVE), serde_json::to_vec(&pending)?)?;
        }
        assert!(load(&ctx, batch.id()).is_err(), "{shape}");
        assert!(pending(&ctx)?);
    }
    Ok(())
}

#[test]
fn two_crashes_across_registration_and_terminal_record_windows_keep_original_evidence() -> TestResult
{
    for committed in [false, true] {
        let (_temp, ctx) = fixture()?;
        let (store, batch) = prepared(&ctx)?;
        let id = batch.id().to_owned();
        if committed {
            batch.place()?;
            rustix::fs::renameat_with(
                batch.stage.file(),
                LEDGER_SLOT,
                batch.root.file(),
                "migration-ledger.json",
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(fs::os)?;
            batch.root.file().sync_all()?;
        } else {
            rustix::fs::renameat_with(
                batch.root.file(),
                MARKER,
                batch.stage.file(),
                MARKER_SLOT,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(fs::os)?;
            batch.root.file().sync_all()?;
            batch.stage.file().sync_all()?;
        }
        drop(batch);
        drop(store);
        let store = Store::lock(&ctx)?;
        let first = load(&ctx, &id)?;
        first.complete_record(MARKER, MARKER_SLOT, MARKER_DONE, &first.pending_identity)?;
        assert!(pending(&ctx)?);
        assert!(store.check_migration_pending().is_err());
        drop(first);
        drop(store);
        let _store = Store::lock(&ctx)?;
        let second = load(&ctx, &id)?;
        if committed {
            second.finish_committed(&ctx)?;
        } else {
            second.rollback()?;
        }
        assert!(!pending(&ctx)?);
        assert!(second.stage.metadata(OsStr::new(MARKER_DONE))?.is_some());
        assert!(second.stage.metadata(OsStr::new(ACTIVE_DONE))?.is_some());
    }
    Ok(())
}

#[test]
fn either_lost_or_replaced_registration_record_is_unknown_and_keeps_the_other_blocker() -> TestResult
{
    for (name, replace) in [
        (MARKER, false),
        (ACTIVE, false),
        (MARKER, true),
        (ACTIVE, true),
    ] {
        let (_temp, ctx) = fixture()?;
        let (store, batch) = prepared(&ctx)?;
        let id = batch.id().to_owned();
        let path = ctx.state.join(name);
        let bytes = disk::read(&path)?;
        disk::rename(&path, ctx.state.join("foreign-retained-record"))?;
        if replace {
            disk::write(&path, bytes)?;
            disk::set_permissions(&path, disk::Permissions::from_mode(0o600))?;
        }
        assert!(load(&ctx, &id).is_err());
        assert!(pending(&ctx)?);
        assert!(store.check_migration_pending().is_err());
    }
    Ok(())
}

#[test]
fn public_recover_accepts_v2_retained_entries_without_replaying_the_migration() -> TestResult {
    use crate::cli::Migrate;

    for (window, count, expected) in [
        ("partial-1", 1, "rolled-back"),
        ("after-ledger", 8, "committed"),
    ] {
        let (_temp, ctx) = fixture()?;
        let native = ctx.home.join(".local/state/gh");
        disk::create_dir_all(&native)?;
        let sentinel = native.join("kept");
        disk::write(&sentinel, "native before")?;
        let (id, targets, stage) = {
            let _store = Store::lock(&ctx)?;
            let batch = seed_window(&ctx, window, count)?;
            assert!(
                batch
                    .manifest
                    .ledger
                    .retained
                    .iter()
                    .any(|item| item.path == native)
            );
            (
                batch.id().to_owned(),
                batch.manifest.placements.clone(),
                batch.stage.path.clone(),
            )
        };
        let command = Migrate::Recover {
            migration_id: id.clone(),
        };
        let error = crate::migration::run(&ctx, &Migrate::Verify, false, false)
            .err()
            .ok_or("pending verify")?;
        assert_eq!(error.code, "migration_recovery_required");
        assert!(error.message.contains(&id));
        let error = crate::migration::run(&ctx, &Migrate::Run, false, true)
            .err()
            .ok_or("pending run")?;
        assert_eq!(error.code, "migration_recovery_required");
        assert_eq!(
            crate::migration::run(&ctx, &command, true, false)?["pending"],
            true
        );
        assert!(pending(&ctx)?);

        disk::write(&sentinel, "native change during recovery")?;
        let result = crate::migration::run(&ctx, &command, false, true)?;
        assert_eq!(result["result"], expected);
        assert!(!pending(&ctx)?);
        assert_eq!(
            disk::read_to_string(&sentinel)?,
            "native change during recovery"
        );
        for item in targets {
            if expected == "committed" {
                assert!(item.target.exists());
                assert!(!stage.join(item.slot).exists());
            } else {
                assert!(!item.target.exists());
                assert!(stage.join(item.slot).exists());
            }
        }
        if expected == "committed" {
            assert_eq!(
                crate::migration::run(&ctx, &command, false, true)?["mode"],
                "verify"
            );
        } else {
            assert_eq!(
                crate::migration::run(&ctx, &command, false, true)?["result"],
                "not-needed"
            );
        }
    }
    Ok(())
}

#[test]
fn recovery_validates_retained_paths_and_versions_in_the_frozen_placement_manifest() -> TestResult {
    use crate::cli::Migrate;

    for (version, owner, relative) in [
        (1, "native_state", ".local/state/gh"),
        (2, "native_state", ".local/state/macos-rebuild"),
        (2, "unknown_owner", ".local/state/gh"),
        (3, "native_state", ".local/state/gh"),
    ] {
        let (_temp, ctx) = fixture()?;
        let (id, before, active, marker, manifest) = {
            let (_store, batch) = prepared(&ctx)?;
            let mut document = serde_json::to_value(&batch.manifest)?;
            document["ledger"]["schema_version"] = serde_json::json!(version);
            document["ledger"]["retained"] = serde_json::json!([{
                "owner": owner, "path": ctx.home.join(relative), "entry": { "kind": "directory" },
            }]);
            let bytes = serde_json::to_vec(&document)?;
            let mut pending = batch.pending.clone();
            pending.manifest_sha256 = fs::hash(&bytes);
            let registration = json(&pending)?;
            let active = ctx.state.join(ACTIVE);
            let marker = ctx.state.join(MARKER);
            let manifest = batch.stage.path.join(MANIFEST);
            disk::write(&active, &registration)?;
            disk::write(&marker, &registration)?;
            disk::write(&manifest, &bytes)?;
            (
                batch.id().to_owned(),
                [registration.clone(), registration, bytes],
                active,
                marker,
                manifest,
            )
        };
        let command = Migrate::Recover { migration_id: id };
        for dry_run in [true, false] {
            let error = crate::migration::run(&ctx, &command, dry_run, true)
                .err()
                .ok_or("forged placement accepted")?;
            assert_eq!(
                error.code, "invalid_migration_ledger",
                "{version} {owner} {relative}"
            );
            assert_eq!(disk::read(&active)?, before[0]);
            assert_eq!(disk::read(&marker)?, before[1]);
            assert_eq!(disk::read(&manifest)?, before[2]);
            assert!(pending(&ctx)?);
            assert!(committed(&ctx)?.is_none());
            assert!(!ctx.state.join("managed-assets.json").exists());
        }
    }
    Ok(())
}

#[test]
fn recovery_preserves_committed_evidence_when_old_governance_diverges() -> TestResult {
    use crate::cli::Migrate;

    let (_temp, ctx) = fixture()?;
    let (id, ledger, active) = {
        let _store = Store::lock(&ctx)?;
        let batch = seed_window(&ctx, "after-ledger", 8)?;
        (
            batch.id().to_owned(),
            disk::read(ctx.state.join("migration-ledger.json"))?,
            disk::read(ctx.state.join(ACTIVE))?,
        )
    };
    let command = Migrate::Recover { migration_id: id };
    let old = ctx.home.join(".agents/managed-assets.json");
    let converted = ctx.state.join("managed-assets.json");
    let before = disk::read(&converted)?;
    disk::write(&old, br#"{"late_writer":true}"#)?;
    let diverged = crate::migration::run(&ctx, &command, false, true)
        .err()
        .ok_or("diverged")?;
    assert_eq!(diverged.code, "migration_source_diverged");
    assert!(pending(&ctx)?);
    assert_eq!(disk::read(&converted)?, before);
    assert_eq!(disk::read(&old)?, br#"{"late_writer":true}"#);
    assert_eq!(disk::read(ctx.state.join(ACTIVE))?, active);
    assert_eq!(disk::read(ctx.state.join("migration-ledger.json"))?, ledger);
    Ok(())
}

#[test]
fn recovery_rechecks_frozen_native_link_alias_evidence_before_writes() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let native = ctx.home.join(".local/state/gh");
    disk::create_dir_all(native.parent().ok_or("native parent")?)?;
    let relayed = ctx.home.join("peer");
    let external = ctx.home.join("separate");
    disk::create_dir(&external)?;
    symlink(&external, &relayed)?;
    symlink("../../peer", &native)?;
    let (id, active, pending_marker) = {
        let (_store, batch) = prepared(&ctx)?;
        assert!(
            batch
                .manifest
                .ledger
                .retained
                .iter()
                .any(|entry| entry.path == native)
        );
        (
            batch.id().to_owned(),
            disk::read(ctx.state.join(ACTIVE))?,
            disk::read(ctx.state.join(MARKER))?,
        )
    };
    disk::remove_file(&native)?;
    disk::remove_file(&relayed)?;
    symlink(".agents", &relayed)?;
    let command = crate::cli::Migrate::Recover { migration_id: id };
    for dry_run in [true, false] {
        let error = crate::migration::run(&ctx, &command, dry_run, true)
            .err()
            .ok_or("invalid retained audit")?;
        assert_eq!(error.code, "invalid_migration_ledger");
        assert_eq!(disk::read(ctx.state.join(ACTIVE))?, active);
        assert_eq!(disk::read(ctx.state.join(MARKER))?, pending_marker);
        assert!(!ctx.state.join("managed-assets.json").exists());
    }
    Ok(())
}

#[test]
fn recovery_checks_the_operation_id_before_creating_lock_metadata() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let batch = seed_window(&ctx, "partial-0", 0)?;
    let id = batch.id().to_owned();
    drop(batch);
    let write_lock = ctx.state.join("write.lock");
    let operations = ctx.state.join("operations");
    assert!(!write_lock.exists());
    assert!(!operations.exists());

    let command = crate::cli::Migrate::Recover {
        migration_id: "OP-123-456".into(),
    };
    let error = crate::migration::run(&ctx, &command, false, true)
        .err()
        .ok_or("unknown operation")?;
    assert_eq!(error.code, "migration_input_changed");
    assert!(!write_lock.exists());
    assert!(!operations.exists());
    assert_eq!(pending_id(&ctx).as_deref(), Some(id.as_str()));

    let command = crate::cli::Migrate::Recover { migration_id: id };
    assert_eq!(
        crate::migration::run(&ctx, &command, false, true)?["result"],
        "rolled-back"
    );
    assert!(!pending(&ctx)?);
    Ok(())
}
