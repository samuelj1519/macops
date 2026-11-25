#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;
#[test]
fn profile_assessment_builds_owned_changes_without_writing_configs_or_exposing_credentials()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("utf8")?,
        "skill",
        "sync",
        "--all",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::write(
        ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"plugins_off":["p@vendor"]}}}"#,
    )?;
    let profile = home.join(".claude-mi");
    fs::create_dir(&profile)?;
    let target = profile.join("settings.json");
    let before = br#"{ "model" : "custom-\u006dodel", "env":{"TOKEN":"PRIVATE_VALUE"} }"#;
    fs::write(&target, before)?;
    let authority = crate::operations::governance::capture(&ctx)?;
    let selected = crate::policy::profile_select::split(delivery::Selection::Explicit(vec![
        "profile:mi".into(),
    ]))?;
    let plan = assess(&ctx, &authority, &selected)?;
    assert_eq!(plan.patches.len(), 1);
    assert_eq!(plan.patches[0].path, target);
    assert_eq!(
        crate::policy::json::parse(&plan.patches[0].after)?["enabledPlugins"]["p@vendor"],
        false
    );
    assert_eq!(fs::read(&target)?, before);
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    let book = invocation::record(&ctx.home, plan.book_after.as_deref().ok_or("book")?)?;
    assert_eq!(book.schema_version, 2);
    assert_eq!(book.profiles.len(), 1);
    assert!(!String::from_utf8(plan.book_after.clone().ok_or("book")?)?.contains("PRIVATE_VALUE"));
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
        "skill",
        "sync",
        "--only",
        "profile:mi",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    fs::create_dir_all(ctx.settings.join("manifests"))?;
    fs::create_dir(ctx.home.join(".claude-mi"))?;
    fs::write(ctx.home.join(".claude-mi/settings.json"), b"{}")?;
    Ok((temp, ctx))
}
fn selected() -> Result<Selected> {
    crate::policy::profile_select::split(delivery::Selection::Explicit(vec!["profile:mi".into()]))
}
fn hash(plan: &Assessment, ctx: &Context, path: &Path) -> Result<String> {
    let book = invocation::record(&ctx.home, plan.book_after.as_deref().ok_or_else(invalid)?)?;
    let file = book.profiles.get(path).ok_or_else(invalid)?;
    let profile_registry::Fields::Claude { plugins, pstack } = &file.fields else {
        return Err(invalid());
    };
    plugins
        .values()
        .flat_map(|f| f.claims.values().map(|c| c.intent_sha256.clone()))
        .chain(
            pstack
                .iter()
                .flat_map(|p| p.claims.values().map(|c| c.intent_sha256.clone())),
        )
        .next()
        .ok_or_else(invalid)
}
#[test]
fn configuration_provenance_hashes_bind_live_and_source_independently() -> TestResult {
    let (_temp, mut ctx) = fixture()?;
    let source = ctx.home.join("source");
    let source_file = source.join("dot_claude-mi/settings.json");
    fs::create_dir_all(source_file.parent().ok_or("parent")?)?;
    fs::write(&source_file, b"{}")?;
    ctx.chezmoi.source = Some(source);
    ctx.chezmoi.destination = Some(ctx.home.clone());
    fs::write(
        ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"plugins_off":["p@vendor"]}}}"#,
    )?;
    let plan = assess(
        &ctx,
        &crate::operations::governance::capture(&ctx)?,
        &selected()?,
    )?;
    assert_ne!(
        hash(&plan, &ctx, &source_file)?,
        hash(&plan, &ctx, &ctx.home.join(".claude-mi/settings.json"))?
    );
    assert!(!ctx.state.exists());
    assert!(!ctx.cache.exists());
    Ok(())
}
#[test]
fn pstack_provenance_uses_the_actual_registered_release_even_with_the_same_skill_names()
-> TestResult {
    let (_temp, ctx) = fixture()?;
    let root = ctx.home.join(".claude-mi");
    fs::write(
        ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"pstack_allow":["tdd"]}}}"#,
    )?;
    let mut hashes = vec![];
    for version in ["1.0.0", "2.0.0"] {
        let payload = root
            .join("plugins/cache/cursor-plugins/pstack")
            .join(version);
        for skill in ["tdd", "why"] {
            let path = payload.join("skills").join(skill);
            fs::create_dir_all(&path)?;
            fs::write(path.join("SKILL.md"), b"---\nname: fixture\n---\n")?;
        }
        fs::write(
            root.join("plugins/installed_plugins.json"),
            serde_json::to_vec(
                &json!({"version":2,"plugins":{"pstack@cursor-plugins":[{"scope":"user","installPath":payload,"version":version}]}}),
            )?,
        )?;
        let plan = assess(
            &ctx,
            &crate::operations::governance::capture(&ctx)?,
            &selected()?,
        )?;
        hashes.push(hash(&plan, &ctx, &root.join("settings.json"))?);
    }
    assert_ne!(hashes[0], hashes[1]);
    assert_eq!(fs::read(root.join("settings.json"))?, b"{}");
    assert!(!ctx.state.exists());
    Ok(())
}
#[test]
fn ignored_git_objects_do_not_expand_or_invalidate_static_profile_source_scope() -> TestResult {
    let (_temp, mut ctx) = fixture()?;
    let source = ctx.home.join("source");
    let source_file = source.join("dot_claude-mi/settings.json");
    fs::create_dir_all(source_file.parent().ok_or("parent")?)?;
    fs::write(&source_file, b"{}")?;
    let deep = source
        .join(".git")
        .join((0..20).map(|_| "objects").collect::<PathBuf>());
    fs::create_dir_all(&deep)?;
    ctx.chezmoi.source = Some(source);
    ctx.chezmoi.destination = Some(ctx.home.clone());
    fs::write(
        ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"plugins_off":["p@vendor"]}}}"#,
    )?;
    let plan = assess(
        &ctx,
        &crate::operations::governance::capture(&ctx)?,
        &selected()?,
    )?;
    assert_eq!(plan.patches.len(), 2);
    fs::write(deep.join("pack"), b"unrelated Git metadata")?;
    plan.verify()?;
    Ok(())
}

#[test]
fn redirected_source_preserves_its_root_directive_but_rejects_unexamined_outer_controls()
-> TestResult {
    let (_temp, mut ctx) = fixture()?;
    let source = ctx.home.join("source");
    let target = source.join("inner/dot_claude-mi/settings.json");
    fs::create_dir_all(target.parent().ok_or("parent")?)?;
    fs::write(&target, b"{}")?;
    fs::write(source.join(".chezmoiroot"), b"inner\n")?;
    ctx.chezmoi.source = Some(source.clone());
    ctx.chezmoi.destination = Some(ctx.home.clone());
    fs::write(
        ctx.settings.join("manifests/skill-profiles.json"),
        br#"{"claude":{"mi":{"plugins_off":["p@vendor"]}}}"#,
    )?;
    let plan = assess(
        &ctx,
        &crate::operations::governance::capture(&ctx)?,
        &selected()?,
    )?;
    assert!(plan.patches.iter().any(|p| p.path == target));
    fs::write(source.join(".chezmoidata.json"), b"{}")?;
    assert!(plan.verify().is_err());
    assert_eq!(
        assess(
            &ctx,
            &crate::operations::governance::capture(&ctx)?,
            &selected()?
        )
        .err()
        .ok_or("unknown control")?
        .code,
        "profile_config_dynamic_source"
    );
    assert_eq!(fs::read(target)?, b"{}");
    assert!(!ctx.state.exists());
    Ok(())
}
