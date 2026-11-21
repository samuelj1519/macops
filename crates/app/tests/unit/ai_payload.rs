#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn streaming_tree_and_file_evidence_detects_content_identity_link_and_directory_changes()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path().join("payload");
    fs::create_dir(&root)?;
    let file = root.join("binary");
    fs::write(&file, vec![b'a'; 200_000])?;
    symlink("missing-target", root.join("optional-link"))?;
    let mut evidence = Evidence::default();
    evidence.tree(&root)?;
    evidence.file(&file)?;
    evidence.file(&file)?;
    assert_eq!(evidence.header(&file)?.len(), 512);
    evidence.verify()?;
    fs::write(&file, vec![b'b'; 200_000])?;
    assert_eq!(
        evidence.verify().err().ok_or("content")?.code,
        "ai_payload_changed"
    );
    let mut evidence = Evidence::default();
    evidence.tree(&root)?;
    let candidate = root.join("candidate");
    fs::write(&candidate, fs::read(&file)?)?;
    fs::rename(candidate, &file)?;
    assert!(evidence.verify().is_err());
    let mut evidence = Evidence::default();
    evidence.tree(&root)?;
    fs::remove_file(root.join("optional-link"))?;
    symlink("other-target", root.join("optional-link"))?;
    assert!(evidence.verify().is_err());
    let mut evidence = Evidence::default();
    evidence.file(&file)?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700))?;
    assert!(evidence.verify().is_err());
    let mut evidence = Evidence::default();
    evidence.tree(&root)?;
    fs::rename(&root, temp.path().join("held"))?;
    fs::create_dir(&root)?;
    assert!(evidence.verify().is_err());
    Ok(())
}

#[test]
fn payload_evidence_rejects_oversize_depth_and_special_files_without_reading_them() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path().join("payload");
    fs::create_dir(&root)?;
    let sparse = root.join("oversize");
    fs::File::create(&sparse)?.set_len(MAX_FILE + 1)?;
    assert_eq!(
        Evidence::default().tree(&root).err().ok_or("size")?.code,
        "ai_payload_limit"
    );
    fs::remove_file(sparse)?;
    let pipe = root.join("pipe");
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(&pipe)
            .status()?
            .success()
    );
    assert_eq!(
        Evidence::default().tree(&root).err().ok_or("special")?.code,
        "ai_payload_unsupported"
    );
    fs::remove_file(pipe)?;
    let mut deepest = root.clone();
    for _ in 0..=MAX_DEPTH {
        deepest.push("child");
    }
    fs::create_dir_all(deepest)?;
    assert_eq!(
        Evidence::default().tree(&root).err().ok_or("depth")?.code,
        "ai_payload_limit"
    );
    Ok(())
}
