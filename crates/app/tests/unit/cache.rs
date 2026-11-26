#![cfg(test)]
use super::*;
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn directory_exchange_does_not_redirect_unlink() -> TestResult {
    let tmp = tempfile::Builder::new()
        .prefix("macops-dirfd-")
        .tempdir_in("/private/tmp")?;
    let root = tmp.path().join("cache");
    std::fs::create_dir(&root)?;
    std::fs::write(root.join("updates.json"), "owned")?;
    let fd = File::from(rustix::fs::open(&root, directory_flags(), Mode::empty())?);
    let item = candidate(&fd, "updates.json")?;
    std::fs::rename(&root, tmp.path().join("original-cache"))?;
    std::fs::create_dir(&root)?;
    std::fs::write(root.join("updates.json"), "replacement-user-data")?;
    let mut owned = Ownership {
        schema_version: 1,
        files: BTreeMap::from([("updates.json".into(), item.sha256.clone())]),
    };
    purge(&fd, "isolation", &[item], &mut owned)?;
    assert_eq!(
        std::fs::read_to_string(root.join("updates.json"))?,
        "replacement-user-data"
    );
    assert!(!tmp.path().join("original-cache/updates.json").exists());
    Ok(())
}

#[test]
fn changed_inode_is_quarantined_and_never_deleted() -> TestResult {
    let tmp = tempfile::Builder::new()
        .prefix("macops-quarantine-")
        .tempdir_in("/private/tmp")?;
    std::fs::write(tmp.path().join("updates.json"), "owned")?;
    let fd = File::from(rustix::fs::open(
        tmp.path(),
        directory_flags(),
        Mode::empty(),
    )?);
    let item = candidate(&fd, "updates.json")?;
    std::fs::rename(
        tmp.path().join("updates.json"),
        tmp.path().join("old-object"),
    )?;
    std::fs::write(tmp.path().join("updates.json"), "new-user-content")?;
    let mut owned = Ownership {
        schema_version: 1,
        files: BTreeMap::from([("updates.json".into(), item.sha256.clone())]),
    };
    let error = purge(&fd, "isolation", &[item], &mut owned)
        .err()
        .ok_or("expected rejection")?;
    assert_eq!(error.code, "quarantine_mismatch");
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("isolation/updates.json"))?,
        "new-user-content"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("old-object"))?,
        "owned"
    );
    Ok(())
}

fn cleanup_fixture()
-> std::result::Result<(tempfile::TempDir, Context, Package), Box<dyn std::error::Error>> {
    use clap::Parser;
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let home = tmp.path().join("home");
    std::fs::create_dir(&home)?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("home path")?,
        "cleanup",
        "macops:cache",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let mut catalog = crate::domain::Catalog {
        schema_version: 1,
        packages: vec![],
    };
    crate::policy::catalog::catalogue::add_managers(&ctx.cache, &mut catalog);
    let package = catalog.select("macops:cache")?.clone();
    Ok((tmp, ctx, package))
}

#[test]
fn cache_cleanup_clears_its_references_and_deletes_only_present_owned_content() -> TestResult {
    use std::fs;
    for owned_present in [true, false] {
        let (_tmp, ctx, package) = cleanup_fixture()?;
        let mut registry = Registry::load(&ctx)?;
        let namespace = Namespace::open(&ctx, true, &registry)?.ok_or("namespace")?;
        let (owned, created) = namespace.publish(&json!({"version":"owned"}))?;
        assert!(created);
        let index = ctx.cache.join("indexes");
        let owned_name = format!("{owned}.json");
        if !owned_present {
            fs::remove_file(index.join(&owned_name))?;
        }
        let borrowed = json!({"version":"borrowed"});
        let mut borrowed_bytes = serde_json::to_vec_pretty(&borrowed)?;
        borrowed_bytes.push(b'\n');
        let borrowed_hash = crate::catalog::index_store::digest(&borrowed_bytes);
        let borrowed_name = format!("{borrowed_hash}.json");
        fs::write(index.join(&borrowed_name), &borrowed_bytes)?;
        assert_eq!(
            namespace.publish(&borrowed)?,
            (borrowed_hash.clone(), false)
        );
        fs::write(index.join("keep.txt"), b"opaque index neighbor")?;
        fs::write(ctx.cache.join("keep.txt"), b"opaque cache neighbor")?;
        let mut book = registry.book(&namespace.path, &namespace.identity, &namespace.key);
        assert!(crate::policy::catalog::cache::register_current(
            &mut book, &owned, true
        ));
        assert!(!crate::policy::catalog::cache::register_current(
            &mut book,
            &borrowed_hash,
            false
        ));
        registry.scopes.insert(namespace.key.clone(), book);
        let store = Store::lock(&ctx)?;
        Store::atomic(&store.root, BOOK_FILE, &registry)?;
        drop(store);

        let registry_bytes = fs::read(ctx.state.join(BOOK_FILE))?;
        let preview = cleanup(&ctx, &package, true, true)?;
        assert_eq!(preview["persistentWrites"], false);
        assert_eq!(
            preview["review"],
            json!([
                format!("indexes/{borrowed_name}"),
                "indexes/keep.txt",
                "keep.txt"
            ])
        );
        let candidates = preview["candidates"].as_array().ok_or("candidates")?;
        assert_eq!(candidates.len(), usize::from(owned_present));
        if owned_present {
            assert_eq!(candidates[0]["name"], owned_name);
            assert_eq!(candidates[0]["sha256"], owned);
        }
        assert_eq!(fs::read(ctx.state.join(BOOK_FILE))?, registry_bytes);

        let result = cleanup(&ctx, &package, false, true)?;
        assert_eq!(result["journal"]["phase"], "succeeded");
        assert_eq!(result["review"], preview["review"]);
        assert!(!index.join(owned_name).exists());
        assert_eq!(fs::read(index.join(borrowed_name))?, borrowed_bytes);
        assert_eq!(fs::read(index.join("keep.txt"))?, b"opaque index neighbor");
        assert_eq!(
            fs::read(ctx.cache.join("keep.txt"))?,
            b"opaque cache neighbor"
        );
        let remaining = Registry::load(&ctx)?;
        let after = remaining.scopes.get(&namespace.key).ok_or("ledger scope")?;
        assert!(after.current.is_none());
        assert!(after.owned.is_empty());
        assert_eq!(cleanup(&ctx, &package, false, true)?["mode"], "unchanged");
    }
    Ok(())
}

#[test]
fn copied_cache_directory_has_no_cleanup_authority_even_with_the_original_marker() -> TestResult {
    use std::fs;
    let (tmp, ctx, package) = cleanup_fixture()?;
    let mut registry = Registry::load(&ctx)?;
    let namespace = Namespace::open(&ctx, true, &registry)?.ok_or("namespace")?;
    let (owned, created) = namespace.publish(&json!({"version":"owned"}))?;
    assert!(created);
    let name = format!("{owned}.json");
    let mut book = registry.book(&namespace.path, &namespace.identity, &namespace.key);
    assert!(crate::policy::catalog::cache::register_current(
        &mut book, &owned, true
    ));
    registry.scopes.insert(namespace.key, book);
    let store = Store::lock(&ctx)?;
    Store::atomic(&store.root, BOOK_FILE, &registry)?;
    drop(store);
    assert_eq!(
        cleanup(&ctx, &package, true, true)?["candidates"][0]["name"],
        name
    );

    let original = tmp.path().join("relocated-cache");
    fs::rename(&ctx.cache, &original)?;
    fs::create_dir_all(ctx.cache.join("indexes"))?;
    for leaf in [".macops-scope.json", name.as_str()] {
        fs::copy(
            original.join("indexes").join(leaf),
            ctx.cache.join("indexes").join(leaf),
        )?;
    }
    let ledger = fs::read(ctx.state.join(BOOK_FILE))?;
    for dry in [true, false] {
        let error = cleanup(&ctx, &package, dry, true)
            .err()
            .ok_or("replacement scope")?;
        assert_eq!(error.code, "cache_scope_changed");
        assert_eq!(fs::read(ctx.state.join(BOOK_FILE))?, ledger);
        assert_eq!(
            fs::read(original.join("indexes").join(&name))?,
            fs::read(ctx.cache.join("indexes").join(&name))?
        );
        assert!(!ctx.state.join("active.json").exists());
    }
    Ok(())
}

#[test]
fn in_place_edit_after_snapshot_is_retained_with_its_original_ownership_entry() -> TestResult {
    use std::fs;
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let path = tmp.path().join("updates.json");
    fs::write(&path, b"known content")?;
    let root = File::from(rustix::fs::open(
        tmp.path(),
        directory_flags(),
        Mode::empty(),
    )?);
    let planned = candidate(&root, "updates.json")?;
    fs::write(&path, b"new user edit")?;
    assert_eq!(fs::metadata(&path)?.ino(), planned.inode);
    let mut ownership = Ownership {
        schema_version: 1,
        files: BTreeMap::from([(planned.name.clone(), planned.sha256.clone())]),
    };
    let error = purge(
        &root,
        "review",
        std::slice::from_ref(&planned),
        &mut ownership,
    )
    .err()
    .ok_or("edited candidate must be retained")?;
    assert_eq!(error.code, "quarantine_mismatch");
    assert_eq!(
        fs::read(tmp.path().join("review/updates.json"))?,
        b"new user edit"
    );
    assert_eq!(ownership.files.get(&planned.name), Some(&planned.sha256));
    Ok(())
}

#[test]
fn cleanup_without_an_index_namespace_only_reviews_the_existing_cache_files() -> TestResult {
    use std::fs;
    let (_tmp, ctx, package) = cleanup_fixture()?;
    fs::create_dir_all(&ctx.cache)?;
    fs::write(
        ctx.cache.join("upstream-download"),
        b"belongs to another owner",
    )?;
    for (dry, mode) in [(true, "dry-run"), (false, "unchanged")] {
        let result = cleanup(&ctx, &package, dry, true)?;
        assert_eq!(result["mode"], mode);
        assert_eq!(result["candidates"], json!([]));
        assert_eq!(result["review"], json!(["upstream-download"]));
        assert_eq!(
            fs::read(ctx.cache.join("upstream-download"))?,
            b"belongs to another owner"
        );
        assert!(!ctx.cache.join("indexes").exists());
        assert!(!ctx.state.exists());
    }
    Ok(())
}
