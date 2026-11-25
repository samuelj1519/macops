use super::*;
use crate::test_support::TestResult;
use serde_json::json;
use std::os::unix::fs::PermissionsExt as _;

const MARKET_DOC: &[u8] = br#"{"name":"market","plugins":[{"name":"plugin","source":"./"}]}"#;
const PLUGIN_DOC: &[u8] = br#"{"name":"plugin","version":"1.0.0"}"#;

#[test]
fn claude_offer_rejects_other_source_types_and_bindings() -> TestResult {
    let revision = ObjectId::parse(&"a".repeat(40))?;
    let offer = Offer::identify("Example/Plugin", &revision, MARKET_DOC, PLUGIN_DOC)?;
    assert_eq!(offer.repository, "example/plugin");
    assert_eq!(offer.native_id(), "plugin@market");
    assert_eq!(offer.url()?, "https://github.com/example/plugin.git");
    assert_eq!(Offer::parse(&json!(offer))?.commit, revision.as_str());
    for altered in [
        br#"{"name":"market","plugins":[]}"#.as_slice(),
        br#"{"name":"market","plugins":[{"name":"wrong","source":"./"}]}"#.as_slice(),
        br#"{"name":"market","plugins":[{"name":"plugin","source":"../outside"}]}"#.as_slice(),
        br#"{"name":"market","plugins":[{"name":"plugin","source":{"source":"command"}}]}"#
            .as_slice(),
    ] {
        assert!(Offer::identify("example/plugin", &revision, altered, PLUGIN_DOC).is_err());
    }
    let mut altered = json!(offer);
    altered["pluginSha256"] = json!("unverified");
    assert!(Offer::parse(&altered).is_err());
    assert!(Offer::identify("wrong", &revision, MARKET_DOC, PLUGIN_DOC).is_err());
    Ok(())
}

#[test]
fn claude_checkout_verifies_exact_manifest_before_writing_and_discards_failed_git_scratch()
-> TestResult {
    let root = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/private/tmp")?;
    let target = Directory::open(root.path())?;
    let revision = ObjectId::parse(&"a".repeat(40))?;
    let offer = Offer::identify("example/plugin", &revision, MARKET_DOC, PLUGIN_DOC)?;
    let failed = checkout_using(&offer, &target, |_, repository, commit| {
        assert_eq!(repository.key(), "example/plugin");
        assert_eq!(commit, &revision);
        Err(invalid())
    });
    assert!(failed.is_err());
    assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    let changed = br#"{"name":"plugin","version":"2.0.0"}"#;
    assert!(
        materialize(
            &offer,
            &target,
            &[(MARKET, false, MARKET_DOC), (PLUGIN, false, changed)]
        )
        .is_err()
    );
    assert!(!target.path.join("checkout").exists());
    let (slot, tree, files) = materialize(
        &offer,
        &target,
        &[
            (MARKET, false, MARKET_DOC),
            (PLUGIN, false, PLUGIN_DOC),
            ("hooks/fixture", true, b"fixture"),
        ],
    )?;
    assert_eq!(slot.name, "checkout");
    assert_eq!(crate::source::source_tree::observe(&slot)?, Some(tree));
    assert!(files.contains(&("hooks/fixture".into(), true, sha(b"fixture"))));
    assert!(!target.path.join("checkout/.git").exists());
    Ok(())
}

fn git_in(path: &std::path::Path, argv: &[&str]) -> Result<String> {
    let captured = std::process::Command::new("/usr/bin/git")
        .args(argv)
        .current_dir(path)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", path)
        .env("GIT_ALLOW_PROTOCOL", "file")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()?;
    if !captured.status.success() {
        return Err(Error::new(
            "fixture_git",
            String::from_utf8_lossy(&captured.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&captured.stdout).trim().into())
}

#[test]
fn checked_out_marketplace_is_materialized_from_exact_tracked_git_objects() -> TestResult {
    let source = tempfile::tempdir_in("/private/tmp")?;
    let allocation = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/private/tmp")?;
    let at = source.path();
    std::fs::create_dir(at.join(".claude-plugin"))?;
    std::fs::write(at.join(MARKET), MARKET_DOC)?;
    std::fs::write(at.join(PLUGIN), PLUGIN_DOC)?;
    git_in(at, &["init", "-q", "--object-format=sha1"])?;
    git_in(at, &["add", "--", MARKET, PLUGIN])?;
    git_in(
        at,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.com",
            "commit",
            "--no-gpg-sign",
            "-qm",
            "Synthetic plugin",
        ],
    )?;
    let pinned = ObjectId::parse(&git_in(at, &["rev-parse", "HEAD"])?)?;
    let offer = Offer::identify("example/plugin", &pinned, MARKET_DOC, PLUGIN_DOC)?;
    let directory = Directory::open(allocation.path())?;
    let (slot, tree, files) = checkout_using(&offer, &directory, |_, _, commit| {
        assert_eq!(commit, &pinned);
        let scratch = std::fs::read_dir(allocation.path())?
            .filter_map(std::result::Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("claude-source-")
            })
            .ok_or_else(invalid)?;
        let bare = scratch.path().join("git/objects.git");
        git_in(
            &bare,
            &[
                "fetch",
                "-q",
                "--no-tags",
                "--",
                at.to_str().ok_or_else(invalid)?,
                pinned.as_str(),
            ],
        )?;
        ObjectId::parse(&git_in(&bare, &["rev-parse", "FETCH_HEAD^{tree}"])?)
    })?;
    assert_eq!(files.len(), 2);
    assert_eq!(tree.entries.len(), 3);
    assert_eq!(std::fs::read(slot.path().join(MARKET))?, MARKET_DOC);
    assert_eq!(std::fs::read_dir(allocation.path())?.count(), 1);
    Ok(())
}

#[test]
fn manifest_field_types_and_missing_checkout_entries_fail_before_materialization() -> TestResult {
    let pin = ObjectId::parse(&"a".repeat(40))?;
    let checkout = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/private/tmp")?;
    let dir = Directory::open(checkout.path())?;
    let offer = Offer::identify("example/plugin", &pin, MARKET_DOC, PLUGIN_DOC)?;
    assert!(materialize(&offer, &dir, &[(MARKET, false, MARKET_DOC)]).is_err());
    assert!(materialize(&offer, &dir, &[(PLUGIN, false, PLUGIN_DOC)]).is_err());
    assert!(Offer::identify("example/plugin", &pin, b"invalid json", PLUGIN_DOC).is_err());
    assert!(Offer::identify("example/plugin", &pin, MARKET_DOC, b"invalid json").is_err());
    let missing_market = br#"{"plugins":[{"name":"plugin","source":"."}]}"#;
    assert!(Offer::identify("example/plugin", &pin, missing_market, PLUGIN_DOC).is_err());
    let unversioned = br#"{"name":"plugin"}"#;
    assert!(Offer::identify("example/plugin", &pin, MARKET_DOC, unversioned).is_err());
    let mut unknown = json!(offer);
    unknown["commit"] = json!("not-a-commit");
    assert!(Offer::parse(&unknown).is_err());
    assert!(!dir.path.join("checkout").exists());
    Ok(())
}
