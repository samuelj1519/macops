use super::*;
use crate::test_support::TestResult;
use serde_json::json;

fn offer() -> Result<Offer> {
    let pin = "a".repeat(40);
    Offer::parse(&json!({
        "repository":"vendor/fixture","marketplace":"market","plugin":"item",
        "declaredVersion":"1.0.0","marketplaceCommit":pin,"payloadCommit":pin,
        "payloadRef":"main","marketplaceSha256":"a".repeat(64),"pluginManifestSha256":"b".repeat(64)
    }))
}

fn row(mode: &str, path: &str, size: &str) -> String {
    format!("{mode} blob {} {size}\t{path}\0", "a".repeat(40))
}

#[test]
fn mirror_listing_and_batch_require_exact_regular_tree_and_object_frames() -> TestResult {
    let a = "a".repeat(40);
    let raw = row("100755", ".agents/plugins/marketplace.json", "1")
        + &row("100644", ".agents/plugins/item", "3");
    let files = parse_listing(raw.as_bytes())?;
    let assembled = format!("{a} blob 1\na\n{a} blob 3\na\0b\n");
    let data = parse_batch(&files, assembled.as_bytes())?;
    assert!(data[0].0.executable);
    assert_eq!(data[1].1, b"a\0b");
    for bad in [
        row("120000", "link", "2"),
        row("160000", "submodule", "2"),
        row("100644", "./file", "2"),
        row("100644", ".git/config", "2"),
        row("100644", "Big", "99999999"),
        row("100644", "文件", "2"),
        row("100644", "A/one", "2") + &row("100644", "a/two", "2"),
        row("100644", "A", "2") + &row("100644", "A/c", "2"),
        row("100644", "same", "2") + &row("100644", "same", "2"),
        "missing-delimiter".into(),
    ] {
        assert!(parse_listing(bad.as_bytes()).is_err());
    }
    for bad in [
        assembled.clone() + "trailing",
        assembled.replace("blob", "tree"),
        assembled.trim_end().to_owned(),
        assembled.replace(&a, &"b".repeat(40)),
        String::new(),
    ] {
        assert!(parse_batch(&files, bad.as_bytes()).is_err());
    }
    Ok(())
}

#[test]
fn install_offer_and_git_member_reject_source_or_identity_changes() -> TestResult {
    let approved = offer()?;
    assert_eq!(approved.url()?, "https://github.com/vendor/fixture.git");
    let market = json!({"name":"market","plugins":[{"name":"item",
        "source":{"source":"url","ref":"main","url":"https://github.com/vendor/fixture.git"}}]});
    let manifest = br#"{"name":"item","version":"1.0.0"}"#;
    verify_manifests(&approved, &serde_json::to_vec(&market)?, manifest)?;
    for (field, value) in [
        ("url", json!("https://github.com/vendor/other.git")),
        ("source", json!("local")),
        ("ref", json!("other")),
        ("path", json!("subdir")),
        ("sha", json!("a".repeat(40))),
    ] {
        let mut altered = market.clone();
        altered["plugins"][0]["source"][field] = value;
        assert!(verify_manifests(&approved, &serde_json::to_vec(&altered)?, manifest).is_err());
    }
    let mut invalid = approved.clone();
    invalid.payload_commit = "b".repeat(40);
    assert!(invalid.validate().is_err());
    let mut invalid = approved.clone();
    invalid.payload_ref = "--flag".into();
    assert!(invalid.validate().is_err());
    let mut invalid = approved;
    invalid.marketplace = "path/to".into();
    assert!(invalid.validate().is_err());
    Ok(())
}

#[test]
fn malformed_offer_proof_and_invalid_batch_boundaries_remain_advisory() -> TestResult {
    assert!(Offer::parse(&Value::Null).is_err());
    let mut candidate = offer()?;
    candidate.repository = "https://github.com/vendor/fixture".into();
    assert!(candidate.url().is_err());
    assert!(candidate.validate().is_err());
    candidate = offer()?;
    candidate.marketplace_sha256 = "invalid digest".into();
    assert!(candidate.validate().is_err());
    let id = "a".repeat(40);
    assert!(parse_listing(format!("100644 blob {id} not-a-size\tx\0").as_bytes()).is_err());
    assert!(parse_listing(b"100644 blob not-a-sha 1\tx\0").is_err());
    assert!(parse_listing(format!("100644 blob {id} 1 x\0").as_bytes()).is_err());
    let listing = parse_listing(row("100644", "x", "8").as_bytes())?;
    assert!(parse_batch(&listing, format!("{id} blob 8\nshort\n").as_bytes()).is_err());
    assert!(parse_batch(&listing, format!("{id} blob 8\n12345678wrong\n").as_bytes()).is_err());
    Ok(())
}
