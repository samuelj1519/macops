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
    offer_at(&ObjectId::parse(&"a".repeat(40))?)
}

fn offer_at(commit: &ObjectId) -> Result<plugin_install::Offer> {
    let candidate = crate::agent::claude_install_source::Offer::identify(
        "owner/repo",
        commit,
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

#[test]
fn plugin_catalog_keeps_only_verified_target_commit_and_uses_exact_fallback() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let profiles = resource_agents::profile_roots_matching(&ctx, |client, _| client == "claude")?;
    let agents = agents(&profiles);
    let names = SkillAgents::default();
    let mut request = Request {
        name: "owner/repo",
        kind: Operation::Install,
        agents: &names,
        revision: None,
        switch_source: false,
    };
    let sources = PluginSources {
        initial: |_, source| {
            assert_eq!(source, "owner/repo");
            Ok(vec![offer()?])
        },
        resolve: |repo| {
            assert_eq!(repo.repository.key(), "owner/repo");
            ObjectId::parse(&"b".repeat(40))
        },
        claude_at: |_, repo, commit| {
            assert_eq!(repo.repository.key(), "owner/repo");
            assert_eq!(commit.as_str(), "b".repeat(40));
            Ok(Some(offer_at(commit)?))
        },
    };
    let repo = public_skill::Request::parse(request.name)?;
    let first = verified_offers(&ctx, &request, Some(&repo), &agents, &[0], &[], &sources)?;
    assert_eq!(first[0].plugin_info()?.3, "a".repeat(40));

    request.name = "owner/repo@v2.0.0";
    let pinned = public_skill::Request::parse(request.name)?;
    let tagged = verified_offers(&ctx, &request, Some(&pinned), &agents, &[0], &[], &sources)?;
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].plugin_info()?.3, "b".repeat(40));
    let a = "a".repeat(40);
    request.revision = Some(a.as_str());
    assert!(verified_offers(&ctx, &request, Some(&pinned), &agents, &[0], &[], &sources).is_err());

    request.name = "owner/repo";
    let exact = verified_offers(&ctx, &request, Some(&repo), &agents, &[0], &[], &sources)?;
    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].plugin_info()?.3, a);
    request.kind = Operation::Upgrade;
    let no_install = verified_offers(&ctx, &request, Some(&repo), &agents, &[0], &[], &sources)?;
    assert_eq!(no_install.len(), 0);
    assert!(!ctx.state.exists());
    Ok(())
}

fn observed_claude_plugin(profile: &Path) -> TestResult {
    let path = profile.join("plugins/cache/fixture/widget/1.0.0");
    let manifest = path.join(".claude-plugin");
    fs::create_dir_all(&manifest)?;
    fs::write(
        manifest.join("plugin.json"),
        br#"{"name":"widget","version":"1.0.0"}"#,
    )?;
    fs::write(
        profile.join("settings.json"),
        br#"{"enabledPlugins":{"widget@fixture":true}}"#,
    )?;
    fs::write(
        profile.join("plugins/installed_plugins.json"),
        serde_json::to_vec(&json!({
            "version":2,"plugins":{"widget@fixture":[{"scope":"user","version":"1.0.0","installPath":path}]}
        }))?,
    )?;
    Ok(())
}

#[test]
fn source_switch_requires_authorization_and_reuses_one_failed_source_check_for_all_selected_sites()
-> TestResult {
    let (_temp, ctx) = fixture()?;
    observed_claude_plugin(&ctx.home.join(".claude-mi"))?;
    observed_claude_plugin(&ctx.home.join(".claude-glm"))?;
    let profiles = resource_agents::profile_roots_matching(&ctx, |client, _| client == "claude")?;
    let agents = agents(&profiles);
    let (local, _) =
        crate::capability::resource_capabilities::collect_observed(&ctx, &profiles, None, false)?;
    let names = SkillAgents::default();
    let request = Request {
        name: "owner/repo",
        kind: Operation::Upgrade,
        agents: &names,
        revision: None,
        switch_source: true,
    };
    let offers = [offer()?];
    for allow_change in [false, true] {
        let mut sites = agents
            .iter()
            .map(|agent| classify(&ctx, &request, agent, &offers, &local, Some("owner/repo")))
            .collect::<Vec<_>>();
        let mut reads = 0;
        plan_source_switches(
            &ctx,
            &request,
            &mut sites,
            &offers,
            &local,
            SwitchRequest {
                repository: "owner/repo",
                allow_change,
            },
            |_, _| {
                reads += 1;
                Err(Error::attention("fixture", "固定来源不可核验"))
            },
        );
        assert_eq!(reads, usize::from(allow_change));
        assert!(
            sites
                .iter()
                .all(|site| site.status == "blocked" && site.work.is_none())
        );
        if allow_change {
            assert!(sites.iter().all(|site| site.reason == "固定来源不可核验"));
        }
    }
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn selected_plugin_plans_are_blocked_if_configuration_roots_overlap() -> TestResult {
    let (_temp, ctx) = fixture()?;
    let profiles = resource_agents::profile_roots_matching(&ctx, |client, _| client == "claude")?;
    let mut agents = agents(&profiles);
    let nested = agents[0].path.join("another-config");
    agents[1].path = &nested;
    let installed = [offer()?];
    let names = SkillAgents::default();
    let request = Request {
        name: "owner/repo",
        kind: Operation::Install,
        agents: &names,
        revision: None,
        switch_source: false,
    };
    let mut sites = agents
        .iter()
        .map(|agent| classify(&ctx, &request, agent, &installed, &[], Some("owner/repo")))
        .collect::<Vec<_>>();
    assert!(sites.iter().all(|site| site.work.is_some()));
    check_target_scope(&mut sites, &agents, true);
    assert!(sites.iter().all(|site| site.status == "blocked"
        && site.work.is_none()
        && site.reason.contains("目录重叠")));
    assert!(!ctx.state.exists());
    Ok(())
}
