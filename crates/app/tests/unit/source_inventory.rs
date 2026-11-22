#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;
#[test]
fn declared_adoption_prepares_full_metadata_without_changing_payload_or_state() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "adopt",
        "asset:alpha",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::write(ctx.settings.join("manifests/managed-agent-assets.json"),br#"{"assets":{"alpha":{"kind":"github-skill","repository":"Owner/Repo","path":"skills/alpha","revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}"#)?;
    let payload = home.join(".agents/skills/alpha");
    fs::create_dir_all(&payload)?;
    fs::write(payload.join("SKILL.md"), b"body")?;
    let authority = crate::governance::capture(&ctx)?;
    let assessment =
        assess_adoption(&ctx, &authority, "asset:alpha", false)?.ok_or("assessment")?;
    assert_eq!(assessment.baseline.source.repository, "owner/repo");
    assert!(assessment.native.is_none());
    assert_eq!(fs::read(payload.join("SKILL.md"))?, b"body");
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    assert!(!authority.paths.managed.exists());
    fs::create_dir(payload.join("empty"))?;
    assert!(assessment.verify().is_err());
    Ok(())
}
#[test]
fn declared_adoption_cannot_infer_no_conflict_from_an_unknown_native_registry() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "adopt",
        "asset:alpha",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::create_dir_all(home.join(".agents/skills/alpha"))?;
    fs::write(home.join(".agents/skills/alpha/SKILL.md"), b"body")?;
    fs::write(ctx.settings.join("manifests/managed-agent-assets.json"),br#"{"assets":{"alpha":{"kind":"github-skill","repository":"owner/repo","path":"skills/alpha"}}}"#)?;
    for document in [
        serde_json::json!({"version":4,"skills":{}}),
        serde_json::json!({"version":3,"skills":[]}),
        serde_json::json!({}),
    ] {
        fs::write(
            home.join(".agents/.skill-lock.json"),
            serde_json::to_vec(&document)?,
        )?;
        assert_eq!(
            assess_adoption(
                &ctx,
                &crate::governance::capture(&ctx)?,
                "asset:alpha",
                false
            )
            .err()
            .ok_or("registry")?
            .code,
            "unsupported_native_source_registry"
        );
        assert!(!ctx.state.exists());
        assert!(!home.join(".agents/managed-assets.json").exists());
    }
    Ok(())
}
