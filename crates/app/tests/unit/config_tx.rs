#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;
#[path = "config_legacy_fixture.rs"]
mod legacy_fixture;
use legacy_fixture::apply;

fn test_ctx(home: &Path) -> Result<Context> {
    let mut warnings = Vec::new();
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(|| Error::new("utf8", "home"))?,
        "app",
    ])
    .map_err(|e| Error::new("parse", e.to_string()))?;
    Context::from_cli(&cli, &mut warnings)
}

#[test]
fn anchored_apply_restore_and_mise_reconcile_preserve_backups_and_cas_guards() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let cz = home.join(".local/share/chezmoi");
    fs::create_dir_all(cz.join("dot_config/mise"))?;
    fs::create_dir_all(home.join(".config/mise"))?;

    fs::write(cz.join("private_dot_netrc"), "machine example.invalid\n")?;
    let ctx = test_ctx(&home)?;
    let target = home.join(".netrc");
    let target_str = target.to_str().ok_or("utf8")?;

    let preview = apply(&ctx, target_str, true, true)?;
    assert_eq!(preview["mode"], "dry-run");
    assert!(!target.exists());

    let applied = apply(&ctx, target_str, false, true)?;
    assert_eq!(applied["phase"], "succeeded");
    let op_id = applied["id"].as_str().ok_or("op")?.to_owned();
    assert_eq!(fs::read_to_string(&target)?, "machine example.invalid\n");
    assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o077, 0);

    let noop = apply(&ctx, target_str, false, true)?;
    assert_eq!(noop["mode"], "no-op");

    fs::write(&target, "externally_edited\n")?;
    assert_eq!(
        restore(&ctx, &op_id, false, true).err().ok_or("ext")?.code,
        "external_modification_detected"
    );

    fs::write(&target, "machine example.invalid\n")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
    let restored = restore(&ctx, &op_id, false, true)?;
    assert_eq!(restored["phase"], "succeeded");
    assert!(!target.exists());
    assert_eq!(restore(&ctx, &op_id, false, true)?["mode"], "no-op");

    let live_mise = home.join(".config/mise/config.toml");
    let src_mise = cz.join("dot_config/mise/private_config.toml");
    fs::write(&src_mise, "[tools]\n\"aqua:openai/codex\" = \"0.9.0\"\n")?;
    fs::write(&live_mise, "[tools]\n\"aqua:openai/codex\" = \"0.10.0\"\n")?;

    let check = reconcile(&ctx, None, false, true)?;
    assert_eq!(check["reconcile_state"], "drift");
    let dry = reconcile(&ctx, Some("codex"), true, true)?;
    assert_eq!(dry["reconcile_state"], "would_reconcile");
    let rec = reconcile(&ctx, Some("codex"), false, true)?;
    assert_eq!(rec["reconcile_state"], "reconciled");
    assert_eq!(
        fs::read_to_string(&src_mise)?,
        fs::read_to_string(&live_mise)?
    );

    fs::write(home.join(".config/mise/config.local.toml"), "[tools]\n")?;
    assert_eq!(
        reconcile(&ctx, None, false, true)
            .err()
            .ok_or("layered")?
            .code,
        "mise_layered_config_present"
    );
    Ok(())
}

#[test]
fn locked_reconcile_rejects_rewritten_content_based_on_an_old_source() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let cz = home.join(".local/share/chezmoi/dot_config/mise");
    fs::create_dir_all(&cz)?;
    fs::create_dir_all(home.join(".config/mise"))?;
    let live = home.join(".config/mise/config.toml");
    let source = cz.join("private_config.toml");
    let before = "[tools]\n\"aqua:openai/codex\" = \"1.0.0\"\n";
    let desired = before.replace("1.0.0", "2.0.0");
    fs::write(&live, &desired)?;
    fs::write(&source, before)?;
    let ctx = test_ctx(&home)?;
    let fence = capture_reconcile_fence(&ctx, &live, &source)?;
    fs::write(&source, format!("{before}[env]\nEDITOR = 'external'\n"))?;
    let store = Store::lock(&ctx)?;
    let result = reconcile_source_locked(
        &store,
        &ctx,
        &ReconcileWrite {
            guard: None,
            operation: None,
            live_path: &live,
            src_path: &source,
            spec: "aqua:openai/codex",
            from: "1.0.0",
            to: "2.0.0",
            rewritten: &desired,
            fence: &fence,
        },
    );
    assert!(
        result.is_err(),
        "a stale source rewrite must be rejected before writing"
    );
    assert!(fs::read_to_string(&source)?.contains("external"));
    Ok(())
}

#[test]
fn restore_records_its_own_recoverable_transaction() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let cz = home.join(".local/share/chezmoi");
    fs::create_dir_all(&cz)?;
    fs::write(cz.join("dot_rc"), "after\n")?;
    fs::write(home.join(".rc"), "before\n")?;
    let ctx = test_ctx(&home)?;
    let applied = apply(&ctx, home.join(".rc").to_str().ok_or("utf8")?, false, true)?;
    let id = applied["id"].as_str().ok_or("op")?;
    restore(&ctx, id, false, true)?;
    let active: Journal = paths::regular_json(&ctx.state.join("active.json"))?;
    assert_eq!(active.plan.action, Action::Restore);
    assert_eq!(active.phase, Phase::Succeeded);
    assert_eq!(fs::read_to_string(home.join(".rc"))?, "before\n");
    assert!(
        backup_dir(&ctx.state, &active.id)
            .join("before.bin")
            .exists()
    );
    Ok(())
}

struct TxFixture {
    temp: tempfile::TempDir,
    ctx: Context,
    target: PathBuf,
    source: PathBuf,
}

impl TxFixture {
    fn new(before: Option<&str>) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir_in("/private/tmp")?;
        let home = temp.path().join("home");
        let source = home.join(".local/share/chezmoi/dot_rc");
        fs::create_dir_all(source.parent().ok_or("parent")?)?;
        fs::write(&source, "new\n")?;
        let target = home.join(".rc");
        if let Some(before) = before {
            fs::write(&target, before)?;
        }
        let ctx = test_ctx(&home)?;
        Ok(Self {
            temp,
            ctx,
            target,
            source,
        })
    }
    fn plan(&self) -> std::result::Result<(Plan, LocalConfigQuery), Box<dyn std::error::Error>> {
        let value = apply(&self.ctx, self.target.to_str().ok_or("utf8")?, true, true)?;
        let plan: Plan = serde_json::from_value(value["plan"].clone())?;
        let Some(QuerySpec::LocalConfig(query)) = &plan.query else {
            return Err("query".into());
        };
        Ok((plan.clone(), query.clone()))
    }
}

#[test]
fn interrupted_apply_and_restore_recover_without_repeating_writes() -> TestResult {
    for original in [None, Some("old\n")] {
        let f = TxFixture::new(original)?;
        let (plan, query) = f.plan()?;
        let store = Store::lock(&f.ctx)?;
        let mut staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
        write_candidate(
            &store,
            &f.ctx,
            &staged,
            &ConfigWrite {
                guard: None,
                query: &query,
                bytes: Some(b"new\n"),
                mode: 0o644,
                reconcile: None,
            },
        )?;
        assert_eq!(
            recover_journal(&store, &mut staged.journal, false, true)?["phase"],
            "succeeded"
        );
        let source_id = staged.journal.id.clone();
        assert_eq!(
            recover_journal(&store, &mut staged.journal, false, true)?["phase"],
            "succeeded"
        );
        let (backup, raw) = load_backup(&store.path, &source_id)?;
        let (restore_plan, restore_query) = restore_plan(&f.ctx, &backup, ".rc")?;
        let mode = backup.before_stamp.as_ref().map_or(0o600, |s| s.mode);
        let mut restore =
            stage_config_tx(&store, &f.ctx, restore_plan, &restore_query, mode, None)?;
        write_candidate(
            &store,
            &f.ctx,
            &restore,
            &ConfigWrite {
                guard: None,
                query: &restore_query,
                bytes: raw.as_deref(),
                mode,
                reconcile: None,
            },
        )?;
        assert_eq!(
            recover_journal(&store, &mut restore.journal, false, true)?["phase"],
            "succeeded"
        );
        assert!(
            !store
                .receipts()?
                .installations
                .contains_key(&backup.config_id)
        );
        assert_eq!(
            recover_journal(&store, &mut restore.journal, false, true)?["phase"],
            "succeeded"
        );
        assert_eq!(fs::read_to_string(&f.target).ok().as_deref(), original);
    }
    Ok(())
}

#[test]
fn only_the_original_temporary_inode_can_be_committed_or_rolled_back() -> TestResult {
    let f = TxFixture::new(Some("old\n"))?;
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let mut staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    let candidate = write_candidate(
        &store,
        &f.ctx,
        &staged,
        &ConfigWrite {
            guard: None,
            query: &query,
            bytes: Some(b"new\n"),
            mode: 0o644,
            reconcile: None,
        },
    )?
    .ok_or("candidate")?;
    fs::write(&f.target, "external\n")?;
    assert!(verify_written(&staged, &query, Some(&candidate)).is_err());
    assert!(
        rollback_to_backup(
            &staged.parent_dir,
            ".rc",
            &staged.backup,
            staged.before_bytes.as_deref(),
            &candidate
        )
        .is_err()
    );
    assert_eq!(
        recover_journal(&store, &mut staged.journal, false, true)?["phase"],
        "unknown"
    );
    assert_eq!(fs::read_to_string(&f.target)?, "external\n");
    assert!(store.check_pending().is_err());
    assert_eq!(
        recover_journal(&store, &mut staged.journal, true, true)?["phase"],
        "failed"
    );
    assert_eq!(fs::read_to_string(&f.target)?, "external\n");
    Ok(())
}

#[test]
fn apply_rechecks_dynamic_controls_and_source_before_prepared_write() -> TestResult {
    let f = TxFixture::new(Some("old\n"))?;
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let mut staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    let control = f.source.parent().ok_or("parent")?.join(".chezmoiignore");
    fs::write(&control, ".rc\n")?;
    let attempt = write_candidate(
        &store,
        &f.ctx,
        &staged,
        &ConfigWrite {
            guard: None,
            query: &query,
            bytes: Some(b"new\n"),
            mode: 0o644,
            reconcile: None,
        },
    );
    assert_eq!(
        attempt.err().ok_or("blocked")?.code,
        "dynamic_chezmoi_control_present"
    );
    assert_eq!(fs::read_to_string(&f.target)?, "old\n");
    assert_eq!(
        recover_journal(&store, &mut staged.journal, false, true)?["phase"],
        "failed"
    );
    fs::remove_file(control)?;
    drop(store);
    let (plan, query) = f.plan()?;
    fs::write(&f.source, "late\n")?;
    let store = Store::lock(&f.ctx)?;
    assert_eq!(
        stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)
            .err()
            .ok_or("stale")?
            .code,
        "plan_stale"
    );
    Ok(())
}

#[test]
fn source_drift_after_write_can_only_restore_the_owned_candidate() -> TestResult {
    let f = TxFixture::new(Some("old\n"))?;
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    let candidate = write_candidate(
        &store,
        &f.ctx,
        &staged,
        &ConfigWrite {
            guard: None,
            query: &query,
            bytes: Some(b"new\n"),
            mode: 0o644,
            reconcile: None,
        },
    )?
    .ok_or("candidate")?;
    fs::write(&f.source, "later\n")?;
    assert_eq!(
        verify_written(&staged, &query, Some(&candidate))
            .err()
            .ok_or("drift")?
            .code,
        "config_source_changed"
    );
    rollback_to_backup(
        &staged.parent_dir,
        ".rc",
        &staged.backup,
        staged.before_bytes.as_deref(),
        &candidate,
    )?;
    assert_eq!(fs::read_to_string(&f.target)?, "old\n");
    assert_eq!(fs::read_to_string(&f.source)?, "later\n");
    Ok(())
}

#[test]
fn backups_reject_corruption_missing_payloads_and_retargeted_metadata() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let f = TxFixture::new(Some("old\n"))?;
    assert_eq!(
        load_backup(&f.ctx.state, "invalid").err().ok_or("id")?.code,
        "invalid_operation_id"
    );
    assert_eq!(
        load_backup(&f.ctx.state, "OP-missing")
            .err()
            .ok_or("missing")?
            .code,
        "backup_not_found"
    );
    let applied = apply(&f.ctx, f.target.to_str().ok_or("utf8")?, false, true)?;
    let id = applied["id"].as_str().ok_or("id")?;
    let dir = backup_dir(&f.ctx.state, id);
    assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(
        fs::metadata(dir.join("before.bin"))?.permissions().mode() & 0o777,
        0o600
    );
    let (backup, _) = load_backup(&f.ctx.state, id)?;
    let original = fs::read(dir.join("config-backup.json"))?;
    let mut damaged = backup.clone();
    damaged.operation_id = "OP-other".into();
    fs::write(
        dir.join("config-backup.json"),
        serde_json::to_vec(&damaged)?,
    )?;
    assert_eq!(
        load_backup(&f.ctx.state, id).err().ok_or("op")?.code,
        "invalid_config_backup"
    );
    fs::write(dir.join("config-backup.json"), original)?;
    fs::write(dir.join("before.bin"), "corrupt\n")?;
    assert_eq!(
        restore(&f.ctx, id, false, true).err().ok_or("digest")?.code,
        "invalid_config_backup"
    );
    fs::write(dir.join("before.bin"), "old\n")?;
    let dry = restore(&f.ctx, id, true, true)?;
    assert_eq!(dry["persistentWrites"], false);
    assert_eq!(fs::read_to_string(&f.target)?, "new\n");
    let store = Store::lock(&f.ctx)?;
    assert!(write_backup(&store, &backup, Some(b"old\n")).is_err());
    let mut journal: Journal = paths::regular_json(&f.ctx.state.join("active.json"))?;
    journal.phase = Phase::Running;
    let Some(QuerySpec::LocalConfig(query)) = &journal.plan.query else {
        return Err("query".into());
    };
    let mut retarget = backup;
    retarget.target = f.ctx.home.join(".other");
    assert!(validate_backup_binding(&retarget, &journal, query).is_err());
    fs::write(
        dir.join("candidate.json"),
        r#"{"identity":{"device":1,"inode":1},"mode":420,"content_digest":"bad","fingerprint":"bad"}"#,
    )?;
    assert_eq!(
        load_candidate(&store, id).err().ok_or("candidate")?.code,
        "invalid_config_backup"
    );
    Ok(())
}

#[test]
fn unchanged_prepared_and_unproven_same_content_recovery_are_distinct() -> TestResult {
    let f = TxFixture::new(Some("old\n"))?;
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let mut staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    assert_eq!(
        recover_journal(&store, &mut staged.journal, false, true)?["phase"],
        "failed"
    );
    drop(store);
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let mut staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    fs::write(&f.target, "new\n")?;
    assert_eq!(
        recover_journal(&store, &mut staged.journal, false, true)?["phase"],
        "unknown"
    );
    assert_eq!(fs::read_to_string(&f.target)?, "new\n");
    Ok(())
}

#[test]
fn reconcile_recovery_checks_frozen_live_and_rolls_back_only_its_candidate() -> TestResult {
    let f = TxFixture::new(None)?;
    let live = f.ctx.home.join(".config/mise/config.toml");
    let source = f
        .source
        .parent()
        .ok_or("parent")?
        .join("dot_config/mise/private_config.toml");
    fs::create_dir_all(source.parent().ok_or("parent")?)?;
    fs::create_dir_all(live.parent().ok_or("parent")?)?;
    let old = "[tools]\n\"aqua:openai/codex\"='1.0.0'\n";
    let new = old
        .replace("1.0.0", "2.0.0")
        .replace("'2.0.0'", "\"2.0.0\"");
    fs::write(&source, old)?;
    fs::write(&live, &new)?;
    let value = reconcile(&f.ctx, Some("codex"), false, true)?;
    assert_eq!(value["reconcile_state"], "reconciled");
    let store = Store::lock(&f.ctx)?;
    let mut journal: Journal = paths::regular_json(&f.ctx.state.join("active.json"))?;
    journal.phase = Phase::Running;
    let mut receipts = store.receipts()?;
    receipts.installations.remove(&journal.plan.package.id);
    Store::atomic(&store.root, "receipts.json", &receipts)?;
    fs::write(&live, format!("{new}[env]\nEDITOR='external'\n"))?;
    assert_eq!(
        recover_journal(&store, &mut journal, false, true)?["phase"],
        "failed"
    );
    assert_eq!(fs::read_to_string(&source)?, old);
    assert!(fs::read_to_string(&live)?.contains("external"));
    Ok(())
}

#[test]
fn applying_unowned_missing_or_special_config_sources_never_writes() -> TestResult {
    use std::os::unix::fs::symlink;
    let f = TxFixture::new(Some("old\n"))?;
    let orphan = f.ctx.home.join(".orphan");
    fs::write(&orphan, "keep\n")?;
    assert_eq!(
        apply(&f.ctx, orphan.to_str().ok_or("utf8")?, false, true)
            .err()
            .ok_or("unowned")?
            .code,
        "unsupported_config_apply"
    );
    assert!(apply(&f.ctx, "config:unknown", true, true).is_err());
    let (_, query) = f.plan()?;
    let mut anchor = query.source.clone().ok_or("source")?;
    anchor.parent_identity.inode += 1;
    let mut tampered = query.clone();
    tampered.source = Some(anchor);
    assert_eq!(
        config_local::observe(&tampered, &mut (2 * FILE_LIMIT))
            .err()
            .ok_or("anchor")?
            .code,
        "config_namespace_changed"
    );
    let dir = Directory::open(&f.ctx.home)?;
    fs::remove_file(&f.target)?;
    symlink(&orphan, &f.target)?;
    assert_eq!(
        observe_anchor_stamp("config:x", &dir, ".rc")
            .err()
            .ok_or("link")?
            .code,
        "unsupported_config_file"
    );
    assert!(apply(&f.ctx, f.target.to_str().ok_or("utf8")?, false, true).is_err());
    assert_eq!(fs::read_to_string(&orphan)?, "keep\n");
    assert!(!f.ctx.state.exists());
    Ok(())
}

#[test]
fn mise_reads_reject_unknown_layers_and_templates_without_evaluation() -> TestResult {
    let f = TxFixture::new(None)?;
    assert_eq!(
        read_utf8_regular(Path::new("/"), "missing")
            .err()
            .ok_or("root")?
            .code,
        "invalid_config_target"
    );
    assert!(read_utf8_regular(Path::new("relative"), "missing").is_err());
    let missing = f.ctx.home.join("missing/file");
    assert_eq!(
        read_utf8_regular(&missing, "missing")
            .err()
            .ok_or("parent")?
            .code,
        "missing"
    );
    assert_eq!(
        read_utf8_regular(&f.ctx.home.join("absent"), "missing")
            .err()
            .ok_or("file")?
            .code,
        "missing"
    );
    fs::write(f.ctx.home.join("invalid"), b"\xff")?;
    assert_eq!(
        read_utf8_regular(&f.ctx.home.join("invalid"), "missing")
            .err()
            .ok_or("utf8")?
            .code,
        "invalid_mise_toml"
    );
    let mise = f.ctx.home.join(".config/mise");
    fs::create_dir_all(mise.parent().ok_or("parent")?)?;
    fs::write(&mise, "not a directory")?;
    assert!(check_mise_layers(&mise).is_err());
    fs::remove_file(&mise)?;
    fs::create_dir_all(mise.join("conf.d"))?;
    fs::write(mise.join("conf.d/notes.txt"), "ignored")?;
    check_mise_layers(&mise)?;
    fs::write(mise.join("conf.d/extra.toml"), "[tools]\n")?;
    assert_eq!(
        check_mise_layers(&mise).err().ok_or("layer")?.code,
        "mise_layered_config_present"
    );
    fs::remove_file(mise.join("conf.d/extra.toml"))?;
    fs::write(mise.join("config.prod.toml"), "[tools]\n")?;
    assert_eq!(
        check_mise_layers(&mise).err().ok_or("env-layer")?.code,
        "mise_layered_config_present"
    );
    fs::remove_file(mise.join("config.prod.toml"))?;
    let template = f
        .source
        .parent()
        .ok_or("parent")?
        .join("dot_config/mise/private_config.toml.tmpl");
    fs::create_dir_all(template.parent().ok_or("parent")?)?;
    fs::write(&template, "{{ exec `never` }}")?;
    assert_eq!(
        reconcile(&f.ctx, None, false, true)?["reason"],
        "source_not_editable"
    );
    assert_eq!(
        reconcile(&f.ctx, Some("codex"), false, true)
            .err()
            .ok_or("template")?
            .code,
        "unsupported_config_source"
    );
    assert!(!f.ctx.state.exists());
    Ok(())
}

#[test]
fn restore_refuses_replaced_directories_and_backup_payload_links() -> TestResult {
    use std::os::unix::fs::symlink;
    let f = TxFixture::new(Some("old\n"))?;
    let applied = apply(&f.ctx, f.target.to_str().ok_or("utf8")?, false, true)?;
    let id = applied["id"].as_str().ok_or("id")?;
    let dir = backup_dir(&f.ctx.state, id);
    fs::remove_file(dir.join("before.bin"))?;
    symlink(&f.source, dir.join("before.bin"))?;
    assert!(load_backup(&f.ctx.state, id).is_err());
    fs::remove_file(dir.join("before.bin"))?;
    fs::write(dir.join("before.bin"), "old\n")?;
    let mut record: ConfigBackupRecord = paths::regular_json(&dir.join("config-backup.json"))?;
    record.parent_identity.inode += 1;
    fs::write(dir.join("config-backup.json"), serde_json::to_vec(&record)?)?;
    assert_eq!(
        restore(&f.ctx, id, false, true)
            .err()
            .ok_or("parent-id")?
            .code,
        "config_namespace_changed"
    );
    assert_eq!(fs::read_to_string(&f.target)?, "new\n");
    Ok(())
}

type MiseFixture = (TxFixture, PathBuf, PathBuf, String, String);

fn mise_fixture() -> std::result::Result<MiseFixture, Box<dyn std::error::Error>> {
    let f = TxFixture::new(None)?;
    let live = f.ctx.home.join(".config/mise/config.toml");
    let source = f
        .source
        .parent()
        .ok_or("parent")?
        .join("dot_config/mise/private_config.toml");
    fs::create_dir_all(live.parent().ok_or("parent")?)?;
    fs::create_dir_all(source.parent().ok_or("parent")?)?;
    let old = "[tools]\n\"aqua:openai/codex\" = \"1.0.0\"\n".to_owned();
    let new = old.replace("1.0.0", "2.0.0");
    fs::write(&source, &old)?;
    fs::write(&live, &new)?;
    Ok((f, live, source, old, new))
}

#[test]
fn mise_layers_follow_the_effective_destination_and_not_an_unrelated_home() -> TestResult {
    let (mut f, live, source, old, new) = mise_fixture()?;
    let destination = f.temp.path().join("destination");
    let actual_live = destination.join(".config/mise/config.toml");
    fs::create_dir_all(actual_live.parent().ok_or("parent")?)?;
    fs::write(&actual_live, &new)?;
    fs::write(
        live.parent().ok_or("parent")?.join("config.local.toml"),
        "unrelated\n",
    )?;
    f.ctx.chezmoi.destination = Some(destination);
    assert_eq!(
        reconcile(&f.ctx, Some("codex"), true, true)?["reconcile_state"],
        "would_reconcile"
    );
    fs::write(
        actual_live
            .parent()
            .ok_or("parent")?
            .join("config.local.toml"),
        "[tools]\n",
    )?;
    assert_eq!(
        reconcile(&f.ctx, Some("codex"), false, true)
            .err()
            .ok_or("layer")?
            .code,
        "mise_layered_config_present"
    );
    assert_eq!(fs::read_to_string(source)?, old);
    assert!(!f.ctx.state.exists());
    Ok(())
}

#[test]
fn reconcile_rejects_root_switches_before_write_and_during_recovery() -> TestResult {
    let (f, live, source, old, new) = mise_fixture()?;
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    let configured = f.ctx.home.join(".local/share/chezmoi");
    let alternate = configured.join("alternate/dot_config/mise/private_config.toml");
    fs::create_dir_all(alternate.parent().ok_or("parent")?)?;
    fs::write(&alternate, &old)?;
    fs::write(configured.join(".chezmoiroot"), "alternate\n")?;
    let store = Store::lock(&f.ctx)?;
    let request = ReconcileWrite {
        guard: None,
        operation: None,
        live_path: &live,
        src_path: &source,
        spec: "aqua:openai/codex",
        from: "1.0.0",
        to: "2.0.0",
        rewritten: &new,
        fence: &fence,
    };
    assert_eq!(
        reconcile_source_locked(&store, &f.ctx, &request)
            .err()
            .ok_or("root")?
            .code,
        "plan_stale"
    );
    assert_eq!(fs::read_to_string(&source)?, old);
    assert_eq!(fs::read_to_string(&alternate)?, old);
    fs::remove_file(configured.join(".chezmoiroot"))?;
    drop(store);
    assert_eq!(
        reconcile(&f.ctx, Some("codex"), false, true)?["reconcile_state"],
        "reconciled"
    );
    let store = Store::lock(&f.ctx)?;
    let mut journal: Journal = paths::regular_json(&f.ctx.state.join("active.json"))?;
    journal.phase = Phase::Running;
    let mut receipts = store.receipts()?;
    receipts.installations.remove(&journal.plan.package.id);
    Store::atomic(&store.root, "receipts.json", &receipts)?;
    fs::write(configured.join(".chezmoiroot"), "alternate\n")?;
    assert_eq!(
        recover_journal(&store, &mut journal, false, true)?["phase"],
        "failed"
    );
    assert_eq!(fs::read_to_string(&source)?, old);
    assert_eq!(fs::read_to_string(&alternate)?, old);
    Ok(())
}

#[test]
fn equal_bytes_under_a_replaced_source_inode_do_not_refresh_the_confirmation_fence() -> TestResult {
    let (f, live, source, old, new) = mise_fixture()?;
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    let replacement = source.with_file_name("replacement");
    fs::write(&replacement, &old)?;
    fs::rename(replacement, &source)?;
    let store = Store::lock(&f.ctx)?;
    let request = ReconcileWrite {
        guard: None,
        operation: None,
        live_path: &live,
        src_path: &source,
        spec: "aqua:openai/codex",
        from: "1.0.0",
        to: "2.0.0",
        rewritten: &new,
        fence: &fence,
    };
    assert_eq!(
        reconcile_source_locked(&store, &f.ctx, &request)
            .err()
            .ok_or("inode")?
            .code,
        "plan_stale"
    );
    assert_eq!(fs::read_to_string(&source)?, old);
    assert!(!f.ctx.state.join("active.json").exists());
    Ok(())
}

#[test]
fn frozen_config_lookup_is_reused_and_config_metadata_changes_block_recovery() -> TestResult {
    let (mut f, live, source, _, _) = mise_fixture()?;
    let xdg = f.temp.path().join("xdg");
    let config = xdg.join("chezmoi/chezmoi.json");
    fs::create_dir_all(config.parent().ok_or("parent")?)?;
    fs::write(
        &config,
        serde_json::to_vec(&json!({
            "sourceDir":f.ctx.home.join(".local/share/chezmoi"), "destDir":f.ctx.home
        }))?,
    )?;
    f.ctx.isolated = false;
    f.ctx.chezmoi.env.insert("XDG_CONFIG_HOME".into(), xdg);
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    verify_root_guard(&f.ctx.home, Some(&fence.roots))?;
    assert_eq!(
        verify_root_guard(&f.ctx.home, None)
            .err()
            .ok_or("missing")?
            .code,
        "reconcile_guard_missing"
    );
    assert_eq!(
        reconcile(&f.ctx, Some("codex"), false, true)?["reconcile_state"],
        "reconciled"
    );
    let store = Store::lock(&f.ctx)?;
    let mut journal: Journal = paths::regular_json(&f.ctx.state.join("active.json"))?;
    journal.phase = Phase::Running;
    let mut bytes = fs::read(&config)?;
    bytes.push(b'\n');
    fs::write(&config, bytes)?;
    assert_eq!(
        verify_root_guard(&f.ctx.home, Some(&fence.roots))
            .err()
            .ok_or("changed")?
            .code,
        "config_roots_changed"
    );
    assert_eq!(
        recover_journal(&store, &mut journal, false, true)?["phase"],
        "unknown"
    );
    assert!(fs::read_to_string(&source)?.contains("2.0.0"));
    fs::remove_file(config)?;
    assert!(verify_root_guard(&f.ctx.home, Some(&fence.roots)).is_err());
    Ok(())
}

#[test]
fn checked_receipt_commit_runs_last_guard_after_temp_sync_and_never_publishes_on_rejection()
-> TestResult {
    let f = TxFixture::new(Some("old\n"))?;
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    let candidate = write_candidate(
        &store,
        &f.ctx,
        &staged,
        &ConfigWrite {
            guard: None,
            query: &query,
            bytes: Some(b"new\n"),
            mode: 0o644,
            reconcile: None,
        },
    )?
    .ok_or("candidate")?;
    let observed = verify_written(&staged, &query, Some(&candidate))?;
    let result = store.commit_checked(&staged.journal, observed, || {
        let mut durable_candidate_seen = false;
        for entry in fs::read_dir(&f.ctx.state)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().ends_with(".tmp") {
                let pending: crate::domain::Receipts = paths::regular_json(&entry.path())?;
                durable_candidate_seen |= pending
                    .installations
                    .contains_key(&staged.journal.plan.package.id);
            }
        }
        assert!(
            durable_candidate_seen,
            "last guard must run after receipt serialization and file preparation"
        );
        fs::write(&f.target, "external\n")?;
        check_stamp_cas(
            &staged.backup.config_id,
            &staged.parent_dir,
            ".rc",
            Some(&candidate),
        )
    });
    assert!(matches!(result, Err(CommitFailure::Check(_))));
    assert!(!f.ctx.state.join("receipts.json").exists());
    assert_eq!(fs::read_to_string(&f.target)?, "external\n");
    assert!(
        fs::read_dir(&f.ctx.state)?
            .all(|entry| entry.is_ok_and(|e| !e.file_name().to_string_lossy().ends_with(".tmp")))
    );
    Ok(())
}

#[test]
fn checked_receipt_commit_distinguishes_failed_checks_from_unknown_io_and_guards_noop() -> TestResult
{
    let f = TxFixture::new(Some("old\n"))?;
    let (plan, query) = f.plan()?;
    let store = Store::lock(&f.ctx)?;
    let staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
    let candidate = write_candidate(
        &store,
        &f.ctx,
        &staged,
        &ConfigWrite {
            guard: None,
            query: &query,
            bytes: Some(b"new\n"),
            mode: 0o644,
            reconcile: None,
        },
    )?
    .ok_or("candidate")?;
    let observed = verify_written(&staged, &query, Some(&candidate))?;
    fs::create_dir(f.ctx.state.join("receipts.json"))?;
    let called = std::cell::Cell::new(false);
    let result = store.commit_checked(&staged.journal, observed.clone(), || {
        called.set(true);
        Ok(())
    });
    assert!(matches!(result, Err(CommitFailure::Outcome(_))));
    assert!(!called.get());
    fs::remove_dir(f.ctx.state.join("receipts.json"))?;
    store.commit(&staged.journal, observed.clone())?;
    let original = fs::read(f.ctx.state.join("receipts.json"))?;
    let result = store.commit_checked(&staged.journal, observed, || {
        called.set(true);
        Err(Error::attention("guard_rejected", "synthetic rejection"))
    });
    assert!(matches!(result, Err(CommitFailure::Check(_))));
    assert!(called.get());
    assert_eq!(fs::read(f.ctx.state.join("receipts.json"))?, original);
    Ok(())
}

#[test]
fn rejected_final_commit_can_only_roll_back_a_still_owned_candidate() -> TestResult {
    for external_target in [false, true] {
        let f = TxFixture::new(Some("old\n"))?;
        let (plan, query) = f.plan()?;
        let store = Store::lock(&f.ctx)?;
        let mut staged = stage_config_tx(&store, &f.ctx, plan, &query, 0o644, None)?;
        let candidate = write_candidate(
            &store,
            &f.ctx,
            &staged,
            &ConfigWrite {
                guard: None,
                query: &query,
                bytes: Some(b"new\n"),
                mode: 0o644,
                reconcile: None,
            },
        )?
        .ok_or("candidate")?;
        let observed = verify_written(&staged, &query, Some(&candidate))?;
        fs::write(&f.source, "source-edited\n")?;
        if external_target {
            fs::write(&f.target, "target-edited\n")?;
        }
        let result = commit_config(
            &store,
            Some(&f.ctx),
            &staged,
            &query,
            Some(&candidate),
            observed,
        );
        let Err(CommitFailure::Check(error)) = result else {
            return Err("expected pre-commit rejection".into());
        };
        assert!(!f.ctx.state.join("receipts.json").exists());
        fail_config_verification(&mut staged, &query, Some(&candidate), &error);
        assert_eq!(
            staged.journal.phase,
            if external_target {
                Phase::Unknown
            } else {
                Phase::Failed
            }
        );
        assert_eq!(
            fs::read_to_string(&f.target)?,
            if external_target {
                "target-edited\n"
            } else {
                "old\n"
            }
        );
        assert_eq!(fs::read_to_string(&f.source)?, "source-edited\n");
    }
    Ok(())
}

#[test]
fn reconcile_final_readonly_guard_cannot_publish_source_after_payload_is_rejected() -> TestResult {
    let (f, live, source, old, new) = mise_fixture()?;
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    let store = Store::lock(&f.ctx)?;
    let reject = || {
        Err(Error::attention(
            "ai_payload_changed",
            "synthetic payload unavailable",
        ))
    };
    let result = reconcile_source_locked(
        &store,
        &f.ctx,
        &ReconcileWrite {
            guard: Some(&reject),
            operation: None,
            live_path: &live,
            src_path: &source,
            spec: "aqua:openai/codex",
            from: "1.0.0",
            to: "2.0.0",
            rewritten: &new,
            fence: &fence,
        },
    )?;
    assert_eq!(result["reconcile_state"], "unknown");
    assert_eq!(result["journal"]["phase"], "unknown");
    assert_eq!(fs::read_to_string(&source)?, old);
    assert_eq!(fs::read_to_string(&live)?, new);
    Ok(())
}

fn prepare_original_operation(ctx: &Context, store: &Store) -> Result<Journal> {
    let mut plan = crate::test_support::journal()?.plan;
    plan.action = Action::Upgrade;
    let journal = operations::new_journal(ctx, plan)?;
    store.save(&journal)?;
    Ok(journal)
}

#[test]
fn mise_reconcile_in_original_operation_changes_only_the_authorized_claude_pin() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let (f, live, source, _, _) = mise_fixture()?;
    let old = concat!(
        "# Selected CLI pin and separately managed runtimes\n",
        "[tools]\n",
        "\"aqua:anthropics/claude-code\" = \"1.0.0\" # exact CLI version\n",
        "\"aqua:openai/codex\" = \"1.0.0\"\n",
        "node = \"22.14.0\"\n",
        "[env]\nEDITOR = 'fixture-editor'\n",
    );
    let new = old.replacen("\"1.0.0\" # exact CLI", "\"2.0.0\" # exact CLI", 1);
    fs::write(&source, old)?;
    fs::set_permissions(&source, fs::Permissions::from_mode(0o640))?;
    fs::write(&live, &new)?;
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    let store = Store::lock(&f.ctx)?;
    let operation = prepare_original_operation(&f.ctx, &store)?;
    let active_path = f.ctx.state.join("active.json");
    let original_record = fs::read(&active_path)?;
    let backup = backup_dir(&f.ctx.state, &operation.id);
    let mut request = ReconcileWrite {
        guard: None,
        operation: Some(&operation),
        live_path: &live,
        src_path: &source,
        spec: "aqua:anthropics/claude-code",
        from: "1.0.0",
        to: "2.0.1",
        rewritten: &new,
        fence: &fence,
    };
    assert_eq!(
        reconcile_source_locked(&store, &f.ctx, &request)
            .err()
            .ok_or("unexpected pin")?
            .code,
        "plan_stale"
    );
    assert!(!backup.exists());
    assert_eq!(fs::read_to_string(&source)?, old);
    assert_eq!(fs::read(&active_path)?, original_record);

    request.to = "2.0.0";
    let applied = reconcile_source_locked(&store, &f.ctx, &request)?;
    assert_eq!(applied["reconcile_state"], "reconciled");
    assert_eq!(applied["journal"]["id"], operation.id);
    assert_eq!(fs::read_to_string(&source)?, new);
    assert_eq!(fs::read_to_string(&live)?, new);
    assert_eq!(fs::metadata(&source)?.permissions().mode() & 0o7777, 0o640);
    assert_eq!(fs::read_to_string(backup.join("before.bin"))?, old);
    assert_eq!(fs::read(active_path)?, original_record);
    assert_eq!(fs::read_dir(f.ctx.state.join("operations"))?.count(), 1);
    assert!(store.receipts()?.installations.is_empty());
    ai_reconcile_completed(&store, &operation)?;
    Ok(())
}

#[test]
fn mise_source_in_original_operation_stays_unpublished_when_a_live_field_changes() -> TestResult {
    let (f, live, source, old, new) = mise_fixture()?;
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    let store = Store::lock(&f.ctx)?;
    let operation = prepare_original_operation(&f.ctx, &store)?;
    let active_path = f.ctx.state.join("active.json");
    let before = fs::read(&active_path)?;
    let external = format!("{new}[env]\nEDITOR = 'user-editor'\n");
    let edit_live = || {
        fs::write(&live, &external)?;
        Ok(())
    };
    let result = reconcile_source_locked(
        &store,
        &f.ctx,
        &ReconcileWrite {
            guard: Some(&edit_live),
            operation: Some(&operation),
            live_path: &live,
            src_path: &source,
            spec: "aqua:openai/codex",
            from: "1.0.0",
            to: "2.0.0",
            rewritten: &new,
            fence: &fence,
        },
    );
    assert_eq!(
        result.err().ok_or("expected live drift")?.code,
        "concurrent_target_modified"
    );
    assert_eq!(fs::read_to_string(&source)?, old);
    assert_eq!(fs::read_to_string(&live)?, external);
    assert_eq!(fs::read(active_path)?, before);
    assert_eq!(
        fs::read_to_string(backup_dir(&f.ctx.state, &operation.id).join("before.bin"))?,
        old
    );
    assert!(ai_reconcile_completed(&store, &operation).is_err());
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn mise_reconcile_requires_original_root_selection_even_when_resolved_files_are_unchanged()
-> TestResult {
    let (mut f, live, source, old, new) = mise_fixture()?;
    let fence = capture_reconcile_fence(&f.ctx, &live, &source)?;
    f.ctx.chezmoi.source = Some(f.ctx.home.join(".local/share/chezmoi"));
    let store = Store::lock(&f.ctx)?;
    let result = reconcile_source_locked(
        &store,
        &f.ctx,
        &ReconcileWrite {
            guard: None,
            operation: None,
            live_path: &live,
            src_path: &source,
            spec: "aqua:openai/codex",
            from: "1.0.0",
            to: "2.0.0",
            rewritten: &new,
            fence: &fence,
        },
    );
    assert_eq!(
        result.err().ok_or("changed root selection")?.code,
        "config_roots_changed"
    );
    assert_eq!(fs::read_to_string(&source)?, old);
    assert_eq!(fs::read_to_string(&live)?, new);
    assert!(!f.ctx.state.join("active.json").exists());
    assert!(!f.ctx.state.join("backups").exists());
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn legacy_restore_preview_and_commit_use_the_original_backup_with_current_source_unresolved()
-> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let f = TxFixture::new(Some("original contents\n"))?;
    fs::set_permissions(&f.target, fs::Permissions::from_mode(0o640))?;
    let applied = apply(&f.ctx, f.target.to_str().ok_or("target")?, false, true)?;
    assert_eq!(applied["phase"], "succeeded");
    let original_id = applied["id"].as_str().ok_or("legacy operation")?;
    fs::remove_file(&f.source)?;
    let template = f.source.with_extension("tmpl");
    let dynamic = b"{{ .fixture_value }}\n";
    fs::write(&template, dynamic)?;
    let active = f.ctx.state.join("active.json");
    let before = fs::read(&active)?;

    let preview = restore(&f.ctx, original_id, true, false)?;
    assert_eq!(preview["mode"], "dry-run");
    assert_eq!(preview["from_operation"], original_id);
    assert_eq!(preview["persistentWrites"], false);
    assert_eq!(fs::read_to_string(&f.target)?, "new\n");
    assert_eq!(fs::read(&active)?, before);

    let restored = restore(&f.ctx, original_id, false, true)?;
    assert_eq!(restored["phase"], "succeeded");
    assert_eq!(fs::read_to_string(&f.target)?, "original contents\n");
    assert_eq!(
        fs::metadata(&f.target)?.permissions().mode() & 0o7777,
        0o640
    );
    assert_eq!(fs::read(template)?, dynamic);
    assert!(!f.source.exists());
    assert_eq!(
        fs::read_to_string(backup_dir(&f.ctx.state, original_id).join("before.bin"))?,
        "original contents\n"
    );
    Ok(())
}

#[test]
fn cursor_reconcile_preserves_http_options_and_writes_only_the_confirmed_version() -> TestResult {
    let (f, live, source, _, _) = mise_fixture()?;
    let original = r#"# installed through an HTTP package
[tools]
"http:cursor-agent" = { version = "1.0.0", url = "https://example.test/cursor/{{ version }}.tgz" }
node = "26.13.1"
"#;
    let wanted = original.replace("1.0.0", "2.0.0");
    fs::write(&source, original)?;
    fs::write(&live, wanted.replace("cursor/{{", "foreign/{{"))?;
    let blocked = reconcile(&f.ctx, Some("cursor"), false, true)?;
    assert_eq!(blocked["reason"], "tool_options_drifted");
    assert_eq!(fs::read_to_string(&source)?, original);
    assert!(!f.ctx.state.exists());

    fs::write(&live, &wanted)?;
    let preview = reconcile(&f.ctx, Some("cursor"), true, true)?;
    assert_eq!(preview["reconcile_state"], "would_reconcile");
    assert_eq!(preview["persistentWrites"], false);
    assert_eq!(fs::read_to_string(&source)?, original);
    let result = reconcile(&f.ctx, Some("cursor"), false, true)?;
    assert_eq!(result["reconcile_state"], "reconciled");
    assert_eq!(fs::read_to_string(source)?, wanted);
    assert_eq!(fs::read_to_string(live)?, wanted);
    Ok(())
}

#[test]
fn layered_mise_detection_handles_conf_d_and_nonconfiguration_files() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path().join("mise");
    check_mise_layers(&root)?;
    let includes = root.join("conf.d");
    fs::create_dir_all(&includes)?;
    fs::write(root.join("config.toml"), b"[tools]\n")?;
    fs::write(root.join("config.backup"), b"old")?;
    fs::write(includes.join("README"), b"not loaded")?;
    check_mise_layers(&root)?;
    let config = includes.join("override.TOML");
    fs::write(&config, b"[tools]\n")?;
    assert_eq!(
        check_mise_layers(&root).err().ok_or("layer accepted")?.code,
        "mise_layered_config_present"
    );
    fs::remove_file(config)?;
    fs::write(root.join("config.team.toml"), b"[tools]\n")?;
    assert_eq!(
        check_mise_layers(&root).err().ok_or("layer accepted")?.code,
        "mise_layered_config_present"
    );
    fs::remove_file(root.join("config.team.toml"))?;
    fs::remove_dir_all(&includes)?;
    fs::write(includes, b"not a directory")?;
    assert!(check_mise_layers(&root).is_err());
    Ok(())
}
