#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::fs;
#[test]
fn namespace_snapshots_and_content_addressed_backups_never_follow_replaced_targets() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = Directory::open(temp.path())?;
    let missing = namespace(&temp.path().join("missing/file"))?;
    assert_eq!(observe(&missing, 1024)?.0, State::Missing);
    let path = temp.path().join("file");
    fs::write(&path, "before")?;
    let frozen = namespace(&path)?;
    let (before, bytes) = observe(&frozen, 1024)?;
    let blobs = Blobs::create(&root)?;
    let blob = blobs.put(bytes.as_deref().ok_or("bytes")?)?;
    assert_eq!(blobs.get(&blob)?, b"before");
    assert_eq!(blobs.put(b"before")?, blob);
    fs::write(&path, "external")?;
    assert!(cas(&frozen, &before).is_err());
    Ok(())
}

#[test]
fn staged_publication_preserves_an_external_edit_and_records_its_own_inode() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let path = temp.path().join("file");
    fs::write(&path, b"before")?;
    let ns = namespace(&path)?;
    let before = observe(&ns, 100)?.0;
    let candidate = stage(&ns, Some(b"after"), 0o640)?;
    assert_eq!(fs::read(&path)?, b"before");
    fs::write(&path, b"external")?;
    assert!(publish(&ns, &before, &candidate).is_err());
    assert_eq!(fs::read(&path)?, b"external");
    let external = observe(&ns, 100)?.0;
    publish(&ns, &external, &candidate)?;
    assert_eq!(observe(&ns, 100)?.0, candidate.after);
    assert_eq!(fs::read(path)?, b"after");
    Ok(())
}

#[test]
fn content_addressed_backups_reject_corruption_modes_and_special_files() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = Directory::open(temp.path())?;
    let blobs = Blobs::create(&root)?;
    assert!(Blobs::create(&root).is_err());
    let blob = blobs.put(b"private-before")?;
    let path = blobs.directory.path.join(&blob.sha256);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    assert!(blobs.get(&blob).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    fs::write(&path, b"external-after")?;
    assert!(blobs.put(b"private-before").is_err());
    fs::remove_file(&path)?;
    symlink("/does-not-exist", &path)?;
    assert!(blobs.get(&blob).is_err());
    assert!(
        blobs
            .get(&Blob {
                sha256: "invalid".into(),
                size: 0
            })
            .is_err()
    );
    let file = temp.path().join("file");
    fs::write(&file, b"safe")?;
    let ns = namespace(&file)?;
    let before = observe(&ns, 10)?.0;
    let mut candidate = stage(&ns, Some(b"after"), 0o644)?;
    let original_target = candidate.target.clone();
    candidate.target = temp.path().join("other");
    assert!(publish(&ns, &before, &candidate).is_err());
    candidate.target = original_target;
    candidate.after = State::Missing;
    assert!(publish(&ns, &before, &candidate).is_err());
    candidate.staged = None;
    candidate.after = before.clone();
    assert!(publish(&ns, &before, &candidate).is_err());
    assert_eq!(fs::read(&file)?, b"safe");
    assert!(stage(&ns, Some(b"x"), u32::MAX).is_err());
    Ok(())
}
#[test]
fn namespaces_and_new_parents_detect_links_directory_replacement_and_unsupported_shapes()
-> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let parent_path = temp.path().join("parent");
    fs::create_dir(&parent_path)?;
    let path = parent_path.join("file");
    fs::write(&path, b"before")?;
    let frozen = namespace(&path)?;
    let mode = fs::metadata(&parent_path)?.permissions().mode();
    fs::set_permissions(
        &parent_path,
        fs::Permissions::from_mode(if mode & 0o7777 == 0o700 { 0o755 } else { 0o700 }),
    )?;
    assert!(open(&frozen).is_err());
    fs::set_permissions(&parent_path, fs::Permissions::from_mode(mode))?;
    let link = parent_path.join("link");
    symlink("file", &link)?;
    assert!(
        matches!(observe(&namespace(&link)?, 1024)?.0, State::Link {text, ..} if text.as_str() == "file")
    );
    let directory = parent_path.join("directory");
    fs::create_dir(&directory)?;
    assert!(observe(&namespace(&directory)?, 1024).is_err());
    assert!(prepare_parent(&namespace(&parent_path.join("a/b/c"))?).is_err());
    assert!(prepare_parent(&frozen)?.is_none());
    let missing = namespace(&parent_path.join("agents/openai.yaml"))?;
    let candidate = prepare_parent(&missing)?.ok_or("candidate")?;
    assert!(!parent_published(&candidate)?);
    publish_parent(&candidate)?;
    publish_parent(&candidate)?;
    assert!(parent_published(&candidate)?);
    fs::rename(&parent_path, temp.path().join("old-parent"))?;
    fs::create_dir(&parent_path)?;
    assert!(open(&frozen).is_err());
    Ok(())
}

#[test]
fn dedicated_link_adapter_creates_and_retires_only_exact_owned_leaves() -> TestResult {
    use crate::capability::rules::delivery::LinkText;
    use std::os::unix::fs::MetadataExt;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path();
    let path = home.join(".claude-mi/skills/demo");
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    let ns = namespace(&path)?;
    let text = LinkText::managed("demo")?;
    let candidate = stage_link(&ns, &text)?;
    assert_eq!(observe(&ns, 4096)?.0, State::Missing);
    publish_link(home, &ns, &State::Missing, &candidate)?;
    let (before, raw) = observe(&ns, 4096)?;
    assert_eq!(raw.as_deref(), Some(text.as_str().as_bytes()));
    assert!(
        matches!(&before,State::Link{mode:Some(mode),..} if *mode == fs::symlink_metadata(&path).map(|m|m.mode()&0o7777)?)
    );
    let remove = crate::capability::rules::capability_tx::Candidate {
        target: path.clone(),
        staged: None,
        after: State::Missing,
    };
    assert!(publish(&ns, &before, &remove).is_err());
    assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
    publish_link(home, &ns, &before, &remove)?;
    assert_eq!(observe(&ns, 4096)?.0, State::Missing);
    let restore = stage_link(&ns, &text)?;
    publish_link(home, &ns, &State::Missing, &restore)?;
    assert_eq!(observe(&ns, 4096)?.0, restore.after);
    Ok(())
}
#[test]
fn equivalent_path_text_and_replaced_same_text_links_are_never_claimed() -> TestResult {
    use crate::capability::rules::delivery::LinkText;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let path = temp.path().join(".claude-mi/skills/demo");
    fs::create_dir_all(path.parent().ok_or("parent")?)?;
    let text = LinkText::managed("demo")?;
    for foreign in [
        "../../.agents//skills/demo",
        "../../.agents/skills/./demo",
        "../../.agents/skills/demo/",
    ] {
        symlink(foreign, &path)?;
        let ns = namespace(&path)?;
        let before = observe(&ns, 4096)?.0;
        let remove = crate::capability::rules::capability_tx::Candidate {
            target: path.clone(),
            staged: None,
            after: State::Missing,
        };
        assert!(publish_link(temp.path(), &ns, &before, &remove).is_err());
        assert_eq!(
            fs::read_link(&path)?.as_os_str().as_bytes(),
            foreign.as_bytes()
        );
        fs::remove_file(&path)?;
    }
    symlink(text.as_str(), &path)?;
    let ns = namespace(&path)?;
    let before = observe(&ns, 4096)?.0;
    fs::rename(&path, path.with_file_name("retained"))?;
    symlink(text.as_str(), &path)?;
    let remove = crate::capability::rules::capability_tx::Candidate {
        target: path.clone(),
        staged: None,
        after: State::Missing,
    };
    assert!(publish_link(temp.path(), &ns, &before, &remove).is_err());
    assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
    Ok(())
}
