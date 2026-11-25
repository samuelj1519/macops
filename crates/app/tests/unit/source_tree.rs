#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};
fn slot(path: &Path, name: &str) -> Result<Slot> {
    let dir = Directory::open(path)?;
    Ok(Slot {
        parent: capability_store::binding(&dir, path)?,
        name: name.into(),
    })
}
#[test]
fn actual_directory_exchange_and_repeated_recovery_keep_original_identity_mode_and_content()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let live_parent = temp.path().join("live");
    let retained_parent = temp.path().join("retained");
    fs::create_dir(&live_parent)?;
    fs::create_dir(&retained_parent)?;
    let live = live_parent.join("alpha");
    let candidate = retained_parent.join("candidate");
    fs::create_dir(&live)?;
    fs::create_dir(&candidate)?;
    fs::set_permissions(&live, fs::Permissions::from_mode(0o751))?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o750))?;
    fs::write(live.join("SKILL.md"), b"old")?;
    fs::write(candidate.join("SKILL.md"), b"new")?;
    let live_slot = slot(&live_parent, "alpha")?;
    let stage_slot = slot(&retained_parent, "candidate")?;
    let change = Change {
        before: observe(&live_slot)?.ok_or("old")?,
        after: observe(&stage_slot)?.ok_or("new")?,
        live: live_slot,
        retained: stage_slot,
    };
    assert_eq!(position(&change)?, Position::Before);
    let result = publish(&change)?;
    assert!(result.exchanged);
    assert!(result.syscall_error.is_none());
    assert_eq!(position(&change)?, Position::OwnAfter);
    assert_eq!(fs::read(live.join("SKILL.md"))?, b"new");
    assert!(!publish(&change)?.exchanged);
    assert!(restore(&change)?.exchanged);
    assert_eq!(position(&change)?, Position::Before);
    assert!(!restore(&change)?.exchanged);
    assert_eq!(observe(&change.live)?.ok_or("restored")?, change.before);
    assert_eq!(fs::read(live.join("SKILL.md"))?, b"old");
    Ok(())
}
#[test]
fn complete_tree_observation_includes_empty_dirs_modes_and_rejects_links_and_special_entries()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let p = temp.path();
    fs::create_dir(p.join("root"))?;
    fs::create_dir(p.join("root/empty"))?;
    fs::write(p.join("root/SKILL.md"), b"content")?;
    let s = slot(p, "root")?;
    let before = observe(&s)?.ok_or("tree")?;
    assert!(matches!(
        before.entries["empty"].content,
        Content::Directory
    ));
    assert_eq!(
        before.entries["SKILL.md"].uid,
        nix::unistd::geteuid().as_raw()
    );
    fs::set_permissions(p.join("root/SKILL.md"), fs::Permissions::from_mode(0o600))?;
    assert_ne!(
        observe(&s)?.ok_or("tree")?.shape().digest()?,
        before.shape().digest()?
    );
    symlink("SKILL.md", p.join("root/alias"))?;
    assert!(observe(&s).is_err());
    fs::remove_file(p.join("root/alias"))?;
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(p.join("root/fifo"))
            .status()?
            .success()
    );
    assert!(observe(&s).is_err());
    assert!(observe(&slot(p, "absent")?)?.is_none());
    Ok(())
}

#[test]
fn foreign_same_bytes_or_changed_candidate_never_grants_a_swap() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let p = temp.path();
    for name in ["live", "retained"] {
        fs::create_dir(p.join(name))?;
        fs::write(p.join(name).join("SKILL.md"), name.as_bytes())?;
    }
    let live = slot(p, "live")?;
    let retained = slot(p, "retained")?;
    let frozen = Change {
        before: observe(&live)?.ok_or("before")?,
        after: observe(&retained)?.ok_or("after")?,
        live,
        retained,
    };
    fs::rename(p.join("live"), p.join("original"))?;
    fs::create_dir(p.join("live"))?;
    fs::write(p.join("live/SKILL.md"), b"live")?;
    assert_eq!(position(&frozen)?, Position::Unknown);
    assert!(publish(&frozen).is_err());
    assert_eq!(fs::read(p.join("live/SKILL.md"))?, b"live");
    assert_eq!(fs::read(p.join("retained/SKILL.md"))?, b"retained");
    fs::remove_dir_all(p.join("live"))?;
    fs::rename(p.join("original"), p.join("live"))?;
    fs::write(p.join("retained/SKILL.md"), b"external modification")?;
    assert_eq!(position(&frozen)?, Position::Unknown);
    assert!(publish(&frozen).is_err());
    assert_eq!(fs::read(p.join("live/SKILL.md"))?, b"live");
    Ok(())
}
#[test]
fn snapshots_refuse_unsafe_directory_modes_and_detect_new_empty_directories() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let p = temp.path();
    fs::create_dir(p.join("root"))?;
    fs::write(p.join("root/SKILL.md"), b"body")?;
    let target = slot(p, "root")?;
    let original = observe(&target)?.ok_or("root")?;
    fs::create_dir(p.join("root/empty"))?;
    assert_ne!(
        observe(&target)?.ok_or("root")?.shape().digest()?,
        original.shape().digest()?
    );
    fs::set_permissions(p.join("root/empty"), fs::Permissions::from_mode(0o777))?;
    assert!(observe(&target).is_err());
    fs::set_permissions(p.join("root/empty"), fs::Permissions::from_mode(0o755))?;
    fs::set_permissions(p.join("root"), fs::Permissions::from_mode(0o777))?;
    assert!(observe(&target).is_err());
    Ok(())
}

#[test]
fn legacy_invocation_metadata_is_read_only_from_the_recorded_tree() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path().join("skill");
    fs::create_dir_all(root.join("agents"))?;
    let skill = b"---\nname: fixture\n---\n";
    let policy = b"policy:\n  allow_implicit_invocation: false\n";
    fs::write(root.join("SKILL.md"), skill)?;
    fs::write(root.join("agents/openai.yaml"), policy)?;
    let slot = slot(temp.path(), "skill")?;
    let tree = observe(&slot)?.ok_or("recorded skill tree")?;
    let read = |relative| crate::source::source_invocation::metadata(&slot, &tree, relative);
    assert_eq!(read("SKILL.md")?.as_deref(), Some(skill.as_slice()));
    assert_eq!(
        read("agents/openai.yaml")?.as_deref(),
        Some(policy.as_slice())
    );
    assert!(read("agents/absent.yaml")?.is_none());
    assert!(read("agents").is_err());

    fs::write(root.join("SKILL.md"), b"new owner content")?;
    assert!(read("agents/openai.yaml").is_err());
    assert_eq!(fs::read(root.join("agents/openai.yaml"))?, policy);
    Ok(())
}

pub fn publish(change: &Change) -> Result<ExchangeOutcome> {
    publish_checked(change, || Ok(()))
}
pub fn restore(change: &Change) -> Result<ExchangeOutcome> {
    restore_checked(change, || Ok(()))
}
