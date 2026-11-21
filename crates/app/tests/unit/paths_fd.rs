#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{OpenOptionsExt, symlink},
};

#[test]
fn anchored_directories_never_reopen_replacement_links() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("macops-fd-")
        .tempdir_in("/private/tmp")?;
    let source = root.path().join("source");
    let outside = root.path().join("outside");
    fs::create_dir_all(source.join("child"))?;
    fs::create_dir_all(&outside)?;
    fs::write(source.join("child/old"), "original")?;
    fs::write(outside.join("MUST_NOT_SCAN"), "outside")?;
    let directory = Directory::open(&source)?;
    let child = directory.child(OsStr::new("child"))?;
    fs::rename(source.join("child"), source.join("old-child"))?;
    symlink(&outside, source.join("child"))?;
    assert!(directory.verify_child(OsStr::new("child"), &child).is_err());
    assert!(directory.child(OsStr::new("child")).is_err());
    let names = child
        .names()?
        .map(|entry| entry.map(|e| e.file_name().to_bytes().to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert!(names.contains(&b"old".to_vec()));
    assert!(!names.contains(&b"MUST_NOT_SCAN".to_vec()));
    fs::rename(&source, root.path().join("old-source"))?;
    symlink(&outside, &source)?;
    assert!(directory.verify().is_err());
    assert!(
        Bindings::new(8)
            .relative(&directory, Path::new("missing/file"))?
            .metadata()
            .is_err()
    );
    assert!(directory.read_regular(OsStr::new("missing"), 100).is_err());
    assert!(directory.metadata(OsStr::new("MUST_NOT_SCAN"))?.is_none());
    assert_eq!(
        fs::read_to_string(outside.join("MUST_NOT_SCAN"))?,
        "outside"
    );
    Ok(())
}

#[test]
fn anchored_metadata_and_bounded_reads_do_not_follow_final_links_or_fifos() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("macops-fd-")
        .tempdir_in("/private/tmp")?;
    let dir = Directory::open(root.path())?;
    fs::write(root.path().join("regular"), "opaque")?;
    symlink("regular", root.path().join("link"))?;
    assert_eq!(
        dir.metadata(OsStr::new("link"))?.ok_or("metadata")?.kind,
        EntryKind::Symlink
    );
    assert!(dir.read_regular(OsStr::new("link"), 100).is_err());
    assert!(dir.read_regular(OsStr::new("regular"), 2).is_err());
    assert_eq!(
        dir.read_regular(OsStr::new("regular"), 100)?,
        Some(b"opaque".to_vec())
    );
    assert!(dir.read_regular(OsStr::new("missing"), 100)?.is_none());
    assert!(dir.metadata(OsStr::new("../escape")).is_err());
    assert!(
        Bindings::new(8)
            .target(&root.path().join("no-parent/missing"))?
            .metadata()?
            .is_none()
    );
    let out = std::process::Command::new("/usr/bin/mkfifo")
        .arg(root.path().join("pipe"))
        .output()?;
    assert!(out.status.success());
    assert_eq!(
        dir.metadata(OsStr::new("pipe"))?
            .ok_or("pipe metadata")?
            .kind,
        EntryKind::Other
    );
    let pipe = root.path().join("pipe");
    let release = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_NOFOLLOW)
            .open(pipe)?;
        std::thread::sleep(std::time::Duration::from_millis(100));
        drop(file);
        Ok::<_, std::io::Error>(())
    });
    let start = std::time::Instant::now();
    let result = dir.read_regular(OsStr::new("pipe"), 100);
    let elapsed = start.elapsed();
    release.join().map_err(|_| "fifo release thread failed")??;
    assert!(result.is_err());
    assert!(elapsed < std::time::Duration::from_millis(500));
    symlink("/Users/example/job", root.path().join("excluded"))?;
    assert!(
        Bindings::new(8)
            .target(&root.path().join("excluded/never-read"))
            .is_err()
    );
    Ok(())
}

#[test]
fn retained_target_bindings_reject_parent_and_alias_replacements_without_refreshing() -> TestResult
{
    let root = tempfile::Builder::new()
        .prefix("macops-target-bind-")
        .tempdir_in("/private/tmp")?;
    let inside = root.path().join("inside");
    let outside = root.path().join("outside");
    fs::create_dir_all(inside.join("profile"))?;
    fs::create_dir_all(&outside)?;
    fs::write(inside.join("profile/config.toml"), "old")?;
    fs::write(outside.join("config.toml"), "new")?;
    let alias = root.path().join("alias");
    symlink(inside.join("profile"), &alias)?;
    let mut bindings = Bindings::new(8);
    let base = bindings.root(&inside)?;
    let relative = bindings.relative(&base, Path::new("profile/config.toml"))?;
    let absolute = bindings.target(&inside.join("profile/config.toml"))?;
    let via_alias = bindings.target(&alias.join("config.toml"))?;
    assert_eq!(
        relative.metadata()?.ok_or("metadata")?.kind,
        EntryKind::File
    );
    assert!(absolute.metadata()?.is_some());
    assert!(via_alias.metadata()?.is_some());
    fs::rename(inside.join("profile"), inside.join("old-profile"))?;
    symlink(&outside, inside.join("profile"))?;
    for target in [relative, absolute, via_alias] {
        assert!(target.metadata().is_err());
    }
    assert!(bindings.target(&alias.join("config.toml")).is_err());
    assert!(
        bindings
            .relative(&base, Path::new("profile/config.toml"))
            .is_err()
    );
    Ok(())
}

#[test]
fn retained_missing_directory_and_shared_descriptor_budget_do_not_fall_back() -> TestResult {
    let root = tempfile::Builder::new()
        .prefix("macops-target-bind-")
        .tempdir_in("/private/tmp")?;
    let mut bindings = Bindings::new(2);
    let base = bindings.root(root.path())?;
    for _ in 0..100 {
        assert!(
            bindings
                .relative(&base, Path::new("absent"))?
                .metadata()?
                .is_none()
        );
    }
    let missing = bindings.target(&root.path().join("future/config"))?;
    let missing_root = bindings.target(&root.path().join("new-root"))?;
    let under_missing = missing_root.missing_descendant(Path::new("settings.json"))?;
    assert!(missing.metadata()?.is_none());
    assert!(under_missing.metadata()?.is_none());
    fs::create_dir(root.path().join("future"))?;
    fs::write(root.path().join("future/config"), "new")?;
    fs::create_dir(root.path().join("new-root"))?;
    assert!(missing.metadata().is_err());
    assert!(under_missing.metadata().is_err());
    fs::create_dir(root.path().join("one"))?;
    fs::create_dir(root.path().join("two"))?;
    bindings.relative(&base, Path::new("one/config"))?;
    assert_eq!(
        bindings
            .relative(&base, Path::new("two/config"))
            .err()
            .ok_or("limit")?
            .code,
        "metadata_binding_limit"
    );
    assert!(bindings.relative(&base, Path::new("../escape")).is_err());
    Ok(())
}
