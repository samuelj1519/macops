#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;

fn fixture() -> std::result::Result<(tempfile::TempDir, Context), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let home = tmp.path().join("home");
    fs::create_dir_all(home.join(".claude-mi"))?;
    fs::create_dir_all(home.join(".claude-glm"))?;
    let cli = Cli::try_parse_from([
        "mo",
        "--home",
        home.to_str().ok_or("fixture home")?,
        "skill",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    Ok((tmp, ctx))
}

fn offer() -> Result<plugin_install::Offer> {
    let commit = crate::policy::source_git::ObjectId::parse(&"a".repeat(40))?;
    let candidate = crate::agent::claude_install_source::Offer::identify(
        "owner/repo",
        &commit,
        br#"{"name":"fixture","plugins":[{"name":"widget","source":"./"}]}"#,
        br#"{"name":"widget","version":"2.0.0"}"#,
    )?;
    Ok(plugin_install::Offer::Claude(
        Box::new(candidate),
        "/fixture/claude".into(),
    ))
}

#[test]
fn physical_install_is_executed_once_and_unknown_stops_only_unattempted_writes() -> TestResult {
    let (_tmp, ctx) = fixture()?;
    let profiles = resource_agents::profile_roots_matching(&ctx, |client, _| client == "claude")?;
    let agents = agents(&profiles);
    assert_eq!(agents.len(), 2);
    let options = SkillAgents::default();
    let request = Request {
        name: "owner/repo",
        kind: Operation::Install,
        agents: &options,
        revision: None,
        switch_source: false,
    };
    let candidates = [offer()?];
    for pending in [false, true] {
        let sites = [0, 0, 1]
            .into_iter()
            .map(|index| {
                classify(
                    &ctx,
                    &request,
                    &agents[index],
                    &candidates,
                    &[],
                    Some("owner/repo"),
                )
            })
            .collect::<Vec<_>>();
        assert!(sites.iter().all(|site| site.status == "planned"));
        let preview = preview(&ctx, &request, &sites, &candidates, &agents);
        assert_eq!(preview[0]["targetVersion"], "2.0.0");
        let mut called = Vec::new();
        let result = perform_with(&ctx, &request, sites, &candidates, preview, |work| {
            let Work::Install(_, target) = work else {
                return Err(selector_error());
            };
            called.push(target.directory.canonical.clone());
            if called.len() == 1 {
                return Ok(if pending {
                    json!({"continuationRequired":true})
                } else {
                    json!({"journal":{"phase":"failed","message":"fixture rejected plugin"}})
                });
            }
            Ok(json!({"journal":{"phase":"succeeded"}}))
        })?;
        let rows = result["results"].as_array().ok_or("batch results")?;
        let first = if pending { "unknown" } else { "failed" };
        assert_eq!(rows[0]["status"], first);
        assert_eq!(rows[1]["status"], first);
        assert_eq!(rows[0]["reason"], rows[1]["reason"]);
        if pending {
            assert_eq!(rows[2]["status"], "unattempted");
            assert_eq!(called, [agents[0].path.to_path_buf()]);
        } else {
            assert_eq!(rows[2]["status"], "changed");
            assert_eq!(
                called,
                [agents[0].path.to_path_buf(), agents[1].path.to_path_buf()]
            );
        }
        assert_eq!(result["incomplete"], true);
    }
    Ok(())
}

#[test]
fn batch_results_distinguish_local_failure_from_original_pending_operation() -> TestResult {
    let (_tmp, ctx) = fixture()?;
    for (value, expected) in [
        (json!({"skipped":true}), "blocked"),
        (
            json!({"skipped":true,"message":"existing plugin"}),
            "blocked",
        ),
        (json!({"incomplete":true}), "failed"),
        (json!({"journal":{"phase":"failed"}}), "failed"),
        (json!({"status":"current"}), "current"),
    ] {
        assert_eq!(result_status(&ctx, Ok(value))?.0, expected);
    }
    let rejected = || Error::attention("fixture", "native read failed");
    assert_eq!(result_status(&ctx, Err(rejected()))?.0, "failed");

    let mut journal = crate::test_support::journal()?;
    journal.state.clone_from(&ctx.state);
    journal.home.clone_from(&ctx.home);
    journal.scope.clone_from(&ctx.scope);
    journal.scope_sha256 = crate::domain::fingerprint(&ctx.scope)?;
    journal.settings.clone_from(&ctx.settings);
    journal.cache.clone_from(&ctx.cache);
    let store = crate::operations::store::Store::lock(&ctx)?;
    store.save(&journal)?;
    assert_eq!(result_status(&ctx, Err(rejected()))?.0, "unknown");
    assert_eq!(
        result_status(&ctx, Ok(json!({"incomplete":true})))?.0,
        "unknown"
    );
    Ok(())
}
