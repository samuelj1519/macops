#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;
#[test]
fn link_assessment_never_changes_models_default_claude_cursor_or_foreign_placements() -> TestResult
{
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "skills",
        "sync",
        "--all",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let definitions = ctx.settings.join("manifests");
    fs::create_dir_all(&definitions)?;
    fs::write(
        definitions.join("skill-profiles.json"),
        br#"{"claude":{"mi":{"links":["alpha"],"pstack_allow":["tdd"]}}}"#,
    )?;
    for name in ["alpha", "beta"] {
        let root = home.join(".agents/skills").join(name);
        fs::create_dir_all(&root)?;
        fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\n---\nbody\n"),
        )?;
    }
    fs::create_dir_all(home.join(".claude-mi/skills"))?;
    let model = home.join(".claude-mi/settings.json");
    fs::write(
        &model,
        b"{\"model\":\"private-model\",\"env\":{\"ANTHROPIC_AUTH_TOKEN\":\"PRIVATE_VALUE\"}}",
    )?;
    std::os::unix::fs::symlink(
        "../../.agents/skills/beta",
        home.join(".claude-mi/skills/beta"),
    )?;
    fs::create_dir_all(home.join(".claude/skills/alpha"))?;
    fs::create_dir_all(home.join(".cursor/skills/alpha"))?;
    let before = fs::read(&model)?;
    let authority = crate::governance::capture(&ctx)?;
    let plan = assess(&ctx, &authority, Selection::All)?;
    assert_eq!(plan.patches.len(), 2);
    assert_eq!(plan.summary()["incomplete"], true);
    assert_eq!(
        plan.summary()["pending_config_policies"],
        json!(["claude:mi:pstack_allow"])
    );
    assert!(!plan.summary().to_string().contains("PRIVATE_VALUE"));
    assert_eq!(fs::read(model)?, before);
    assert!(home.join(".claude/skills/alpha").is_dir());
    assert!(home.join(".cursor/skills/alpha").is_dir());
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    plan.verify()?;
    Ok(())
}

fn fixture() -> std::result::Result<(tempfile::TempDir, Context), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "skills",
        "sync",
        "--all",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::create_dir_all(home.join(".claude-mi/skills"))?;
    for name in ["alpha", "beta", "devnorm", "native-owned"] {
        let root = home.join(".agents/skills").join(name);
        fs::create_dir_all(&root)?;
        fs::write(root.join("SKILL.md"), "---\nname: fixture\n---\nbody\n")?;
    }
    Ok((temp, ctx))
}
#[test]
fn partial_selection_keeps_other_sources_and_never_claims_equivalent_foreign_link_text()
-> TestResult {
    let (_temp, ctx) = fixture()?;
    fs::write(
        ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"links":[]}}}"#,
    )?;
    let alpha = ctx.home.join(".claude-mi/skills/alpha");
    let beta = ctx.home.join(".claude-mi/skills/beta");
    std::os::unix::fs::symlink("../../.agents//skills/alpha", &alpha)?;
    std::os::unix::fs::symlink("../../.agents/skills/beta", &beta)?;
    let authority = crate::governance::capture(&ctx)?;
    let plan = assess(&ctx, &authority, Selection::Explicit(vec!["alpha".into()]))?;
    assert_eq!(plan.patches.len(), 0);
    assert_eq!(plan.conflicts.len(), 1);
    assert!(plan.conflicts.iter().all(|r| r["name"] == "alpha"));
    assert_eq!(fs::read_link(beta)?, Path::new("../../.agents/skills/beta"));
    assert_eq!(
        fs::read_link(alpha)?.as_os_str(),
        OsStr::new("../../.agents//skills/alpha")
    );
    assert!(!ctx.state.exists());
    Ok(())
}
#[test]
fn native_copies_devnorm_and_unknown_legacy_placements_stay_in_place() -> TestResult {
    let (_temp, ctx) = fixture()?;
    fs::write(ctx.settings.join("manifests/managed-agent-assets.json"),br#"{"schema_version":1,"assets":{"native-owned":{"kind":"local-release-plugin","claude_profiles":["mi"]}}}"#)?;
    fs::create_dir(ctx.home.join(".claude-mi/skills/native-owned"))?;
    fs::create_dir(ctx.home.join(".claude-mi/skills/devnorm"))?;
    std::os::unix::fs::symlink(
        "../../.agents/skills/obsolete",
        ctx.home.join(".claude-mi/skills/obsolete"),
    )?;
    let authority = crate::governance::capture(&ctx)?;
    let plan = assess(&ctx, &authority, Selection::All)?;
    assert_eq!(plan.patches.len(), 2);
    assert!(
        plan.patches
            .iter()
            .all(|p| !p.path.ends_with("native-owned")
                && !p.path.ends_with("devnorm")
                && !p.path.ends_with("obsolete"))
    );
    assert!(plan.conflicts.iter().any(|r| r["name"] == "obsolete"));
    assert!(ctx.home.join(".claude-mi/skills/native-owned").is_dir());
    assert!(!ctx.state.exists());
    Ok(())
}
#[test]
fn native_lock_missing_payload_and_unknown_channel_schema_block_preparation() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let path = ctx.home.join(".agents/.skill-lock.json");
    fs::write(
        &path,
        br#"{"version":3,"skills":{"absent":{"source":"owner/repo","sourceType":"github"}}}"#,
    )?;
    let authority = crate::governance::capture(&ctx)?;
    assert_eq!(
        assess(&ctx, &authority, Selection::All)
            .err()
            .ok_or("must reject")?
            .code,
        "native_skill_payload_missing"
    );
    fs::remove_file(path)?;
    fs::write(
        ctx.settings.join("manifests/agent-source-channels.json"),
        br#"{"schema_version":99,"source_ids":{}}"#,
    )?;
    assert!(assess(&ctx, &authority, Selection::All).is_err());
    assert!(!ctx.state.exists());
    Ok(())
}
