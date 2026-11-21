use super::*;
use std::os::unix::fs::MetadataExt;
#[test]
fn leaf_rejects_symlink_fifo_and_directory() -> Result<()> {
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let root = Directory::open(tmp.path())?;
    std::fs::write(tmp.path().join("good"), b"data")?;
    std::os::unix::fs::symlink("good", tmp.path().join("link"))?;
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(tmp.path().join("pipe"))
            .status()?
            .success()
    );
    std::fs::create_dir(tmp.path().join("dir"))?;
    for name in ["link", "pipe", "dir"] {
        assert!(
            root.leaf(OsStr::new(name), FILE_LIMIT, &mut TOTAL_LIMIT.clone())
                .is_err()
        );
    }
    assert_eq!(
        root.leaf(OsStr::new("good"), FILE_LIMIT, &mut TOTAL_LIMIT.clone())?
            .0,
        b"data"
    );
    Ok(())
}

#[test]
fn leaf_canary_is_never_opened_and_races_use_original_fd() -> Result<()> {
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let home = tmp.path();
    let credential = std::env::var_os("MACOPS_UV_RACE_CANARY")
        .map_or_else(|| home.join("credential"), PathBuf::from);
    if !credential.exists() {
        std::fs::write(&credential, b"SECRET-CANARY")?;
    }
    std::fs::create_dir(home.join("parent"))?;
    std::fs::write(home.join("parent/data"), b"original")?;
    let root = Directory::open(&home.join("parent"))?;
    let meta = std::fs::metadata(&credential)?;
    let canary = FileId {
        device: meta.dev(),
        inode: meta.ino(),
    };
    let mut opened = vec![];
    // Leaf changes into a symlink between validation and open. Opened-FD IDs,
    // rather than output strings, prove whether the external canary was opened.
    let result = root.leaf_with(
        OsStr::new("data"),
        FILE_LIMIT,
        &mut TOTAL_LIMIT.clone(),
        |event, identity| {
            if matches!(event, LeafEvent::BeforeOpen) {
                std::fs::remove_file(home.join("parent/data"))?;
                std::os::unix::fs::symlink(&credential, home.join("parent/data"))?;
            }
            if let Some(identity) = identity {
                opened.push(identity.clone());
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!opened.contains(&canary));
    std::fs::remove_file(home.join("parent/data"))?;
    std::fs::write(home.join("parent/data"), b"original")?;
    opened.clear();
    let result = root.leaf_with(
        OsStr::new("data"),
        FILE_LIMIT,
        &mut TOTAL_LIMIT.clone(),
        |event, identity| {
            if let Some(identity) = identity {
                opened.push(identity.clone());
            }
            if matches!(event, LeafEvent::BeforeRead) {
                std::fs::rename(home.join("parent/data"), home.join("parent/old"))?;
                std::os::unix::fs::symlink(&credential, home.join("parent/data"))?;
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(opened.len(), 1);
    assert!(!opened.contains(&canary));
    std::fs::create_dir(home.join("foreign"))?;
    std::fs::write(home.join("foreign/old"), b"PARENT-CANARY")?;
    let parent_canary = FileId::from_file(&File::open(home.join("foreign/old"))?)?;
    opened.clear();
    // Frozen parent directory is replaced by a symlink before final open.
    let result = root.leaf_with(
        OsStr::new("old"),
        FILE_LIMIT,
        &mut TOTAL_LIMIT.clone(),
        |event, identity| {
            if let Some(identity) = identity {
                opened.push(identity.clone());
            }
            if matches!(event, LeafEvent::BeforeOpen) {
                std::fs::rename(home.join("parent"), home.join("original-parent"))?;
                std::os::unix::fs::symlink(home.join("foreign"), home.join("parent"))?;
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!opened.contains(&parent_canary));
    Ok(())
}

fn tool_fixture() -> Result<(tempfile::TempDir, UvToolBinding)> {
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let tool_dir = tmp.path().join("tools");
    let bin_dir = tmp.path().join("bin");
    let env = tool_dir.join("demo-tool");
    std::fs::create_dir_all(env.join("lib/python3.12/site-packages/demo_tool-1.0.dist-info"))?;
    std::fs::create_dir_all(env.join("bin"))?;
    std::fs::create_dir(&bin_dir)?;
    std::fs::write(
        env.join("uv-receipt.toml"),
        format!(
            "[tool]\nrequirements=[{{name='demo-tool'}}]\nentrypoints=[{{name='demo',install-path='{}'}}]\n",
            bin_dir.join("demo").display()
        ),
    )?;
    std::fs::write(env.join("pyvenv.cfg"), "home = /synthetic-python\n")?;
    std::fs::write(
        env.join("lib/python3.12/site-packages/demo_tool-1.0.dist-info/METADATA"),
        "Metadata-Version: 2.4\nName: demo-tool\nVersion: 1.0\n",
    )?;
    std::fs::write(env.join("bin/demo"), "#!/synthetic-python\n")?;
    std::os::unix::fs::symlink(env.join("bin/demo"), bin_dir.join("demo"))?;
    std::os::unix::fs::symlink("/synthetic-python", env.join("bin/python"))?;
    Ok((tmp, UvToolBinding { tool_dir, bin_dir }))
}
#[test]
fn stable_tool_and_each_data_leaf_fail_closed() -> Result<()> {
    let (tmp, binding) = tool_fixture()?;
    let query = capture(&binding, "Demo_Tool")?;
    let present = observe(&query, &mut TOTAL_LIMIT.clone())?;
    assert_eq!(present.version(), Some("1.0"));
    assert!(!binding.tool_dir.join(".lock").exists());
    for relative in [
        "demo-tool/uv-receipt.toml",
        "demo-tool/pyvenv.cfg",
        "demo-tool/lib/python3.12/site-packages/demo_tool-1.0.dist-info/METADATA",
        "demo-tool/bin/demo",
        ".lock",
    ] {
        let path = binding.tool_dir.join(relative);
        let original = std::fs::read(&path).ok();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        std::os::unix::fs::symlink(tmp.path().join("credentials-canary"), &path)?;
        assert!(
            observe(&query, &mut TOTAL_LIMIT.clone()).is_err(),
            "{relative} symlink"
        );
        std::fs::remove_file(&path)?;
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(&path)
                .status()?
                .success()
        );
        let start = std::time::Instant::now();
        assert!(
            observe(&query, &mut TOTAL_LIMIT.clone()).is_err(),
            "{relative} FIFO"
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        std::fs::remove_file(&path)?;
        std::fs::create_dir(&path)?;
        assert!(
            observe(&query, &mut TOTAL_LIMIT.clone()).is_err(),
            "{relative} directory"
        );
        std::fs::remove_dir(&path)?;
        if let Some(bytes) = original {
            std::fs::write(path, bytes)?;
        }
    }
    let restored = observe(&query, &mut TOTAL_LIMIT.clone())?;
    assert_eq!(restored.version(), Some("1.0"));
    assert_ne!(restored, present);
    std::fs::rename(&binding.tool_dir, tmp.path().join("old-tools"))?;
    std::fs::create_dir(&binding.tool_dir)?;
    std::fs::rename(
        tmp.path().join("old-tools/demo-tool"),
        binding.tool_dir.join("demo-tool"),
    )?;
    assert!(observe(&query, &mut TOTAL_LIMIT.clone()).is_err());
    Ok(())
}

#[test]
fn budgets_duplicates_receipt_escape_and_lock_competition() -> Result<()> {
    let (tmp, binding) = tool_fixture()?;
    let query = capture(&binding, "demo-tool")?;
    assert!(observe(&query, &mut 0).is_err());
    let meta = binding
        .tool_dir
        .join("demo-tool/lib/python3.12/site-packages/demo_tool-1.0.dist-info/METADATA");
    let original = std::fs::read(&meta)?;
    std::fs::write(&meta, "Name: demo-tool\nName:\tdemo-tool\nVersion: 1\n")?;
    assert!(observe(&query, &mut TOTAL_LIMIT.clone()).is_err());
    std::fs::write(&meta, &original)?;
    let extra = meta
        .parent()
        .and_then(Path::parent)
        .ok_or_else(unknown)?
        .join("demo_tool-2.dist-info");
    std::fs::create_dir(&extra)?;
    std::fs::write(extra.join("METADATA"), &original)?;
    assert!(observe(&query, &mut TOTAL_LIMIT.clone()).is_err());
    std::fs::remove_dir_all(extra)?;
    let receipt_path = binding.tool_dir.join("demo-tool/uv-receipt.toml");
    let original = std::fs::read(&receipt_path)?;
    std::fs::write(
        &receipt_path,
        "[tool]\nrequirements=[{name='demo-tool'}]\nentrypoints=[{name='demo',install-path='/outside/demo'}]\n",
    )?;
    assert!(observe(&query, &mut TOTAL_LIMIT.clone()).is_err());
    std::fs::write(&receipt_path, &original)?;
    let lock = File::create(binding.tool_dir.join(".lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    assert!(observe(&query, &mut TOTAL_LIMIT.clone()).is_err());
    fs2::FileExt::unlock(&lock)?;
    assert!(matches!(
        observe(&query, &mut TOTAL_LIMIT.clone())?,
        Observation::UvTool { .. }
    ));
    let leaf = tmp.path().join("budget");
    std::fs::create_dir(&leaf)?;
    let dir = Directory::open(&leaf)?;
    for limit in [PYVENV_LIMIT, FILE_LIMIT] {
        std::fs::write(
            leaf.join("data"),
            vec![0; usize::try_from(limit).map_err(|_| unknown())?],
        )?;
        assert_eq!(
            dir.leaf(OsStr::new("data"), limit, &mut limit.clone())?
                .0
                .len() as u64,
            limit
        );
        assert!(
            dir.leaf(OsStr::new("data"), limit, &mut (limit - 1))
                .is_err()
        );
        std::fs::write(
            leaf.join("data"),
            vec![0; usize::try_from(limit + 1).map_err(|_| unknown())?],
        )?;
        assert!(
            dir.leaf(OsStr::new("data"), limit, &mut TOTAL_LIMIT.clone())
                .is_err()
        );
    }
    for n in 0..TOOL_LIMIT {
        std::fs::write(leaf.join(format!("entry-{n}")), [])?;
    }
    assert_eq!(dir.names(TOOL_LIMIT + 1)?.len(), TOOL_LIMIT + 1);
    assert!(dir.names(TOOL_LIMIT).is_err());
    std::fs::remove_file(leaf.join("data"))?;
    std::fs::write(leaf.join(".lock"), [])?;
    assert_eq!(dir.tool_names()?.len(), TOOL_LIMIT);
    std::fs::write(leaf.join("over-limit"), [])?;
    assert!(dir.tool_names().is_err());
    Ok(())
}

#[test]
fn site_packages_entry_budget_is_cumulative_across_python_layouts() -> Result<()> {
    let (_tmp, binding) = tool_fixture()?;
    let query = capture(&binding, "demo-tool")?;
    let extra = binding
        .tool_dir
        .join("demo-tool/lib/python3.13/site-packages");
    std::fs::create_dir_all(&extra)?;
    for index in 0..SITE_LIMIT {
        std::fs::write(extra.join(format!("ignored-{index}")), [])?;
    }
    assert!(observe(&query, &mut TOTAL_LIMIT.clone()).is_err());
    Ok(())
}
