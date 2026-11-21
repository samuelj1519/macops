#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::{fs, os::unix::fs::symlink};

#[test]
fn destination_alias_binding_is_retained_through_later_metadata_queries() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-dest-epoch-")
        .tempdir_in("/private/tmp")?;
    let source = temp.path().join("source");
    let outside = temp.path().join("outside");
    let alias = temp.path().join("alias");
    fs::create_dir_all(&source)?;
    fs::create_dir_all(&outside)?;
    symlink(&source, &alias)?;
    fs::write(source.join(".leaf"), "old")?;
    fs::write(outside.join(".leaf"), "outside")?;
    let destination = Destination::capture(&alias)?;
    let mut bindings = Bindings::new(8);
    let Destination::Present(root) = &destination else {
        return Err("missing root".into());
    };
    let leaf = bindings.relative(root, Path::new(".leaf"))?;
    fs::remove_file(&alias)?;
    symlink(&outside, &alias)?;
    assert!(destination.verify().is_err());
    assert!(leaf.metadata().is_err());
    Ok(())
}

#[test]
fn missing_destination_cannot_be_rebound_when_it_appears() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-dest-epoch-")
        .tempdir_in("/private/tmp")?;
    let root = temp.path().join("missing");
    let destination = Destination::capture(&root)?;
    destination.verify()?;
    fs::create_dir(&root)?;
    assert!(destination.verify().is_err());
    Ok(())
}
