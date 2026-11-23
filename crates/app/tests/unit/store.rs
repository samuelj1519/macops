#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
fn fixture() -> std::result::Result<(tempfile::TempDir, Context), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    Ok((temp, ctx))
}
#[test]
fn same_home_writes_remain_exclusive_across_state_roots_without_creating_another_state()
-> TestResult {
    let (_temp, ctx) = fixture()?;
    let first = Store::lock(&ctx)?;
    let mut other = ctx.clone();
    other.state = ctx.home.join("other-state");
    assert_eq!(
        Store::lock(&other).err().ok_or("busy")?.code,
        "operation_busy"
    );
    assert!(!other.state.exists());
    drop(first);
    let _second = Store::lock(&other)?;
    Ok(())
}
#[test]
fn deleting_the_lock_file_cannot_allow_another_writer_while_the_first_store_is_alive() -> TestResult
{
    let (_temp, ctx) = fixture()?;
    let first = Store::lock(&ctx)?;
    std::fs::remove_file(ctx.state.join("write.lock"))?;
    assert_eq!(
        Store::lock(&ctx).err().ok_or("busy")?.code,
        "operation_busy"
    );
    drop(first);
    let _second = Store::lock(&ctx)?;
    Ok(())
}
#[test]
fn different_homes_sharing_a_state_root_and_same_inode_home_state_are_handled_without_deadlock()
-> TestResult {
    let (temp, ctx) = fixture()?;
    let first = Store::lock(&ctx)?;
    let mut other = ctx.clone();
    other.home = temp.path().join("other-home");
    std::fs::create_dir(&other.home)?;
    assert_eq!(
        Store::lock(&other).err().ok_or("busy")?.code,
        "operation_busy"
    );
    drop(first);
    let second = Store::lock(&other)?;
    drop(second);
    let mut same = ctx.clone();
    same.state = same.home.clone();
    same.cache = temp.path().join("outside-cache");
    let store = Store::lock(&same)?;
    store.verify_namespace()?;
    assert_eq!(
        crate::domain::FileId::from_file(&store.root)?,
        store.home_guard.directory.identity().clone()
    );
    Ok(())
}
#[test]
fn replaced_home_state_operations_and_lock_are_rejected_without_writing_to_the_new_namespace()
-> TestResult {
    use std::os::unix::fs::symlink;
    for variant in 0..4 {
        let (temp, ctx) = fixture()?;
        let store = Store::lock(&ctx)?;
        let journal = crate::test_support::journal()?;
        let target = match variant {
            0 => ctx.home.clone(),
            1 => ctx.state.clone(),
            2 => ctx.state.join("operations"),
            _ => ctx.state.join("write.lock"),
        };
        let held = temp.path().join("held");
        std::fs::rename(&target, &held)?;
        if variant == 3 {
            symlink(&held, &target)?;
        } else {
            std::fs::create_dir(&target)?;
        }
        assert!(store.verify_namespace().is_err());
        assert!(store.save(&journal).is_err());
        if variant == 0 {
            assert!(!ctx.state.exists());
        } else if variant == 1 {
            assert!(!target.join("active.json").exists());
        } else if variant == 2 {
            assert_eq!(std::fs::read_dir(&target)?.count(), 0);
        }
        assert!(store.receipts().is_err());
        assert!(store.check_pending().is_err());
    }
    Ok(())
}
#[test]
fn each_journal_part_rechecks_the_namespace_and_keeps_the_original_terminal_order() -> TestResult {
    for terminal in [false, true] {
        let (_temp, ctx) = fixture()?;
        let store = Store::lock(&ctx)?;
        let mut journal = crate::test_support::journal()?;
        journal.phase = if terminal {
            Phase::Succeeded
        } else {
            Phase::Running
        };
        let retained = ctx.home.join("retained-state");
        let outcome = store.save_pair(&journal, || {
            std::fs::rename(&ctx.state, &retained)?;
            std::fs::create_dir(&ctx.state)?;
            Ok(())
        });
        assert!(outcome.is_err());
        assert!(!ctx.state.join("active.json").exists());
        assert!(!ctx.state.join("operations").exists());
        assert_eq!(retained.join("active.json").exists(), !terminal);
        assert_eq!(retained.join("operations/OP-1.json").exists(), terminal);
    }
    Ok(())
}
#[test]
fn recovered_terminal_publication_preserves_matching_history_and_refuses_a_different_attempt()
-> TestResult {
    let (_temp, ctx) = fixture()?;
    let store = Store::lock(&ctx)?;
    let active = crate::operations::new_journal(&ctx, crate::test_support::journal()?.plan)?;
    assert!(store.publish_existing_terminal(&active)?.is_none());
    store.save(&active)?;
    assert!(store.publish_existing_terminal(&active)?.is_none());
    let filename = format!("{}.json", active.id);
    let active_path = ctx.state.join("active.json");
    let history_path = ctx.state.join("operations").join(&filename);
    let pending = std::fs::read(&active_path)?;

    let mut terminal = active.clone();
    terminal.phase = Phase::Failed;
    terminal.message = "original operation failed".into();
    terminal.started_at += 1;
    store.save_part(&store.operations, &filename, &terminal)?;
    let mismatch = store
        .publish_existing_terminal(&active)
        .err()
        .ok_or("different attempt")?;
    assert_eq!(mismatch.code, "operation_conflict");
    assert_eq!(std::fs::read(&active_path)?, pending);

    terminal.started_at = active.started_at;
    store.save_part(&store.operations, &filename, &terminal)?;
    let original_history = std::fs::read(&history_path)?;
    let recovered = store
        .publish_existing_terminal(&active)?
        .ok_or("published")?;
    assert_eq!(recovered.phase, Phase::Failed);
    assert_eq!(recovered.started_at, active.started_at);
    assert_eq!(std::fs::read(&active_path)?, original_history);
    assert_eq!(std::fs::read(&history_path)?, original_history);
    assert!(!ctx.state.join("receipts.json").exists());
    Ok(())
}

#[test]
fn final_receipt_callback_and_no_change_early_return_cannot_use_a_replaced_root() -> TestResult {
    for unchanged in [false, true] {
        let (_temp, ctx) = fixture()?;
        let store = Store::lock(&ctx)?;
        let mut journal = crate::test_support::journal()?;
        if unchanged {
            journal.plan.action = crate::domain::Action::Uninstall;
        }
        let retained = ctx.home.join("retained-state");
        let result = store.commit_checked(
            &journal,
            Observation::Present {
                version: "1.0.0".into(),
                evidence: None,
            },
            || {
                std::fs::rename(&ctx.state, &retained)?;
                std::fs::create_dir(&ctx.state)?;
                Ok(())
            },
        );
        assert!(matches!(result, Err(CommitFailure::Check(_))));
        assert!(!ctx.state.join("receipts.json").exists());
        assert!(!retained.join("receipts.json").exists());
    }
    Ok(())
}
#[test]
fn special_lock_entries_are_not_opened_as_mutex_files_and_failed_attempts_release_directory_locks()
-> TestResult {
    let (_temp, ctx) = fixture()?;
    std::fs::create_dir_all(&ctx.state)?;
    std::fs::create_dir(ctx.state.join("write.lock"))?;
    assert_eq!(
        Store::lock(&ctx).err().ok_or("invalid lock")?.code,
        "invalid_state_lock"
    );
    std::fs::remove_dir(ctx.state.join("write.lock"))?;
    let _store = Store::lock(&ctx)?;
    Ok(())
}
