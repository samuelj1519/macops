#![cfg(test)]
use super::*;
use crate::{
    domain::{Action, ConfigAuthority, Provider},
    test_support::TestResult,
};
use std::path::Path;

fn context(home: &Path) -> std::result::Result<context::Context, Box<dyn std::error::Error>> {
    let cli =
        cli::Cli::try_parse_from(["mo", "--home", home.to_str().ok_or("fixture home")?, "app"])?;
    Ok(context::Context::from_cli(&cli, &mut vec![])?)
}

fn command(args: &[&str]) -> std::result::Result<cli::Command, clap::Error> {
    Ok(parse_entry(
        std::iter::once("mo")
            .chain(args.iter().copied())
            .map(Into::into)
            .collect(),
    )?
    .command)
}

#[test]
fn recovered_agent_upgrade_cannot_be_reused_for_another_tool_version_or_local_mode() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let ctx = context(temp.path())?;
    let mut original = test_support::journal()?;
    original.plan.action = Action::Upgrade;
    original.plan.package.id = "ai-native:codex".into();
    original.plan.target = Some("2.0.0".into());
    let mut recovered = operations::Recovered {
        original,
        result: json!({"phase":"succeeded"}),
        succeeded: true,
        local_ai: Some(true),
    };
    for (args, expected) in [
        (&["agent", "upgrade", "codex", "--local"][..], true),
        (&["agent", "upgrade", "codex"][..], false),
        (&["agent", "upgrade", "claude", "--local"][..], false),
        (&["agent", "install", "codex", "2.0.0", "--local"][..], true),
        (
            &["agent", "install", "codex", "3.0.0", "--local"][..],
            false,
        ),
        (&["agent", "upgrade"][..], false),
    ] {
        assert_eq!(
            repeats_recovered(&ctx, &command(args)?, &recovered)?,
            expected,
            "{args:?}"
        );
    }
    recovered.succeeded = false;
    assert!(!repeats_recovered(
        &ctx,
        &command(&["agent", "upgrade", "codex", "--local"])?,
        &recovered
    )?);
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn replay_matches_approved_application_recipe_and_does_not_treat_missing_upgrade_as_success()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let ctx = context(temp.path())?;
    let mut previous = test_support::journal()?;
    let requested = previous.plan.package.name.clone();
    std::fs::create_dir_all(ctx.catalog.parent().ok_or("catalog parent")?)?;
    let catalog = domain::Catalog {
        schema_version: 1,
        packages: vec![previous.plan.package.clone()],
    };
    std::fs::write(&ctx.catalog, serde_json::to_vec(&catalog)?)?;
    previous.plan.action = Action::Upgrade;
    previous.plan.target = Some("2.0.0".into());
    assert!(repeats_resource(
        &ctx,
        &command(&["app", "upgrade", "app:demo", "--version", "2.0.0"])?,
        &previous,
        None,
        true
    )?);
    assert!(!repeats_resource(
        &ctx,
        &command(&["app", "upgrade", "app:demo", "--version", "3.0.0"])?,
        &previous,
        None,
        true
    )?);
    assert!(!repeats_resource(
        &ctx,
        &command(&["app", "install", "app:demo"])?,
        &previous,
        None,
        true
    )?);

    std::fs::remove_file(&ctx.catalog)?;
    let checked = repeats_resource(
        &ctx,
        &command(&["app", "upgrade", &requested])?,
        &previous,
        None,
        true,
    );
    assert_eq!(
        checked
            .err()
            .ok_or("missing target was silently selected")?
            .code,
        "operation_checked"
    );
    assert!(!repeats_resource(
        &ctx,
        &command(&["app", "upgrade", "different-object"])?,
        &previous,
        None,
        true
    )?);
    previous.plan.action = Action::Uninstall;
    assert!(repeats_resource(
        &ctx,
        &command(&["app", "uninstall", &requested])?,
        &previous,
        None,
        true
    )?);
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn replay_matches_configuration_by_physical_path_and_does_not_cross_to_service_actions()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let ctx = context(temp.path())?;
    let mut journal = test_support::journal()?;
    journal.plan.action = Action::Apply;
    journal.plan.package.provider = Provider::ConfigObserved {
        target: "/fixture/settings".into(),
        source: None,
        authority: ConfigAuthority::ChezmoiNative,
    };
    assert!(repeats_resource(
        &ctx,
        &command(&["config", "apply", "/fixture/settings"])?,
        &journal,
        None,
        false
    )?);
    assert!(!repeats_resource(
        &ctx,
        &command(&["config", "apply", "/fixture/other"])?,
        &journal,
        None,
        false
    )?);
    assert!(!repeats_resource(
        &ctx,
        &command(&["app", "start", "/fixture/settings"])?,
        &journal,
        None,
        false
    )?);

    journal.plan.package.provider = Provider::Service {
        formula: "demo".into(),
    };
    journal.plan.package.name = "demo".into();
    journal.plan.action = Action::Start;
    assert!(repeats_resource(
        &ctx,
        &command(&["app", "start", "demo"])?,
        &journal,
        None,
        false
    )?);
    assert!(!repeats_resource(
        &ctx,
        &command(&["app", "stop", "demo"])?,
        &journal,
        None,
        false
    )?);
    assert!(!repeats_resource(
        &ctx,
        &command(&["app", "start", "another"])?,
        &journal,
        None,
        false
    )?);
    assert!(!ctx.state.exists());
    Ok(())
}
