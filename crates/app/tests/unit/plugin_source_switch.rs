#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn source_edit_preserves_every_unrelated_setting_byte() -> TestResult {
    let prefix = br#"{
  "env": {"SECRET": "fixture"},
  "extraKnownMarketplaces": {"devnorm-local": {"autoUpdate": false, "source": "#;
    let current = br#"{"source":"directory","path":"/private/tmp/old"}"#;
    let suffix = br#"}},
  "enabledPlugins": {"devnorm@devnorm-local": false}
}
"#;
    let mut bytes = [prefix.as_slice(), current.as_slice(), suffix.as_slice()].concat();
    let next = json!({"source":"github","repo":"owner/repo","ref":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"});
    change_source(&mut bytes, "devnorm-local", &next)?;
    assert_eq!(
        bytes,
        [
            prefix.as_slice(),
            &serde_json::to_vec(&next)?,
            suffix.as_slice()
        ]
        .concat()
    );
    Ok(())
}

#[test]
fn a_second_plugin_from_the_market_blocks_the_source_switch() -> TestResult {
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let profile = tmp.path().join(".claude-mi");
    let plugins = profile.join("plugins");
    std::fs::create_dir_all(&plugins)?;
    let root = Directory::open(&profile)?;
    let target = InstallTarget {
        name: "claude:mi".into(),
        client: "claude".into(),
        directory: crate::capability::capability_store::binding(&root, &profile)?,
        command: None,
    };
    let declaration = json!({"source":"directory","path":tmp.path().join("old")});
    let settings = serde_json::to_vec(&json!({
        "extraKnownMarketplaces":{"devnorm-local":{"source":declaration}},
        "enabledPlugins":{"devnorm@devnorm-local":false}
    }))?;
    std::fs::write(profile.join(SETTINGS), &settings)?;
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        serde_json::to_vec(&json!({
            "devnorm-local":{"source":declaration,"installLocation":tmp.path().join("old")}
        }))?,
    )?;
    let record = |name: &str| {
        json!([{
            "scope":"user","version":"1.0.0",
            "installPath":plugins.join("cache/devnorm-local").join(name).join("1.0.0"),
        }])
    };
    let book = plugins.join("installed_plugins.json");
    std::fs::write(
        &book,
        serde_json::to_vec(&json!({
            "version":2,"plugins":{"devnorm@devnorm-local":record("devnorm")}
        }))?,
    )?;
    let id = "devnorm@devnorm-local";
    let old = tmp.path().join("old");
    let catalog = old.join(".claude-plugin/marketplace.json");
    let manifest = old.join("plugin/.claude-plugin/plugin.json");
    std::fs::create_dir_all(catalog.parent().ok_or("market directory")?)?;
    std::fs::create_dir_all(manifest.parent().ok_or("plugin manifest directory")?)?;
    std::fs::write(
        catalog,
        br#"{"name":"devnorm-local","plugins":[{"name":"devnorm","source":"./plugin"}]}"#,
    )?;
    std::fs::write(manifest, br#"{"name":"devnorm","version":"1.0.0"}"#)?;
    let wanted = GithubMarket {
        repo: "owner/repo".into(),
        commit: "a".repeat(40),
        files: "b".repeat(64),
    };
    Scope::read(&target, id)?.verify_old(&target, id, &old, &wanted)?;
    let installed_manifest =
        plugins.join("cache/devnorm-local/devnorm/1.0.0/.claude-plugin/plugin.json");
    std::fs::create_dir_all(installed_manifest.parent().ok_or("installed directory")?)?;
    std::fs::write(
        &installed_manifest,
        br#"{"name":"devnorm","version":"1.0.0"}"#,
    )?;
    let offer = Offer::identify(
        &wanted.repo,
        &crate::policy::source_git::ObjectId::parse(&wanted.commit)?,
        br#"{"name":"devnorm-local","plugins":[{"name":"devnorm","source":"./"}]}"#,
        br#"{"name":"devnorm","version":"1.2.0"}"#,
    )?;
    let basis = SwitchOffer {
        offer,
        market: wanted.clone(),
        binary: "/fixture/claude".into(),
        binary_digest: "c".repeat(64),
    };
    let denied = SourceSwitch::plan(target.clone(), &basis, false)
        .err()
        .ok_or("undeclared source change")?;
    assert!(denied.message.contains("来源已改变"));
    assert!(SourceSwitch::plan(target.clone(), &basis, true)?.is_some());
    let destination = plugins.join("marketplaces/devnorm-local");
    std::fs::create_dir_all(&destination)?;
    assert!(
        Scope::read(&target, id)?
            .verify_old(&target, id, &old, &wanted)
            .is_err()
    );
    std::fs::remove_dir(&destination)?;
    std::fs::write(
        &book,
        serde_json::to_vec(&json!({
            "version":2,"plugins":{"devnorm@devnorm-local":record("devnorm"),"another@devnorm-local":record("another")}
        }))?,
    )?;
    let blocked = Scope::read(&target, "devnorm@devnorm-local")
        .err()
        .ok_or("market scope was accepted")?;
    assert_eq!(blocked.code, "native_plugin_action_unsupported");
    assert_eq!(std::fs::read(profile.join(SETTINGS))?, settings);
    Ok(())
}

const ID: &str = "widget@fixture-market";
const MARKET: &str = "fixture-market";
const CATALOG: &[u8] = br#"{"name":"fixture-market","plugins":[{"name":"widget","source":"./"}]}"#;
const INITIAL: &[u8] = br#"{"name":"widget","version":"1.0.0"}"#;
const UPDATED: &[u8] = br#"{"name":"widget","version":"2.0.0"}"#;

struct SwitchFixture {
    _temp: tempfile::TempDir,
    profile: PathBuf,
    old: PathBuf,
    ctx: Context,
    target: InstallTarget,
    basis: SwitchOffer,
}

impl SwitchFixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        use crate::cli::Cli;
        use clap::Parser;
        use std::fs;
        let temp = tempfile::tempdir_in("/private/tmp")?;
        let home = temp.path().join("home");
        let profile = home.join(".claude");
        let old = home.join("old-market");
        let upstream = home.join("fixed-commit");
        Self::populate(&old, INITIAL)?;
        Self::populate(&upstream, UPDATED)?;
        let ctx = Context::from_cli(
            &Cli::try_parse_from([
                "mo",
                "--home",
                home.to_str().ok_or("fixture home")?,
                "skill",
            ])?,
            &mut vec![],
        )?;
        fs::create_dir_all(profile.join("plugins"))?;
        let target = InstallTarget {
            client: "claude".into(),
            name: "claude:default".into(),
            directory: crate::capability::capability_store::binding(
                &Directory::open(&profile)?,
                &profile,
            )?,
            command: None,
        };
        let commit = crate::policy::source_git::ObjectId::parse(&"a".repeat(40))?;
        let offer = Offer::identify("owner/repo", &commit, CATALOG, UPDATED)?;
        let binary = home.join("synthetic-claude");
        fs::copy("/usr/bin/true", &binary)?;
        let checkout = Directory::open(&upstream)?;
        let mut files = capability_local::claude_market_files(&checkout)?;
        files.reverse();
        let basis = SwitchOffer::from_checkout(&offer, &binary, &checkout, files)?;
        let fixture = Self {
            _temp: temp,
            profile,
            old,
            ctx,
            target,
            basis,
        };
        Self::populate(&fixture.cache("1.0.0"), INITIAL)?;
        fixture.register("1.0.0")?;
        let directory = json!({"source":"directory","path":fixture.old});
        fixture.register_market(&fixture.old, &directory)?;
        fs::write(
            fixture.profile.join(SETTINGS),
            serde_json::to_vec(&json!({
                "extraKnownMarketplaces":{MARKET:{"source":directory,"autoUpdate":false}},
                "enabledPlugins":{ID:false},"model":"keep"
            }))?,
        )?;
        Ok(fixture)
    }

    fn populate(root: &std::path::Path, manifest: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(root.join(".claude-plugin"))?;
        std::fs::write(root.join(".claude-plugin/plugin.json"), manifest)?;
        std::fs::write(root.join(".claude-plugin/marketplace.json"), CATALOG)
    }

    fn cache(&self, version: &str) -> PathBuf {
        self.profile
            .join("plugins/cache/fixture-market/widget")
            .join(version)
    }

    fn register(&self, version: &str) -> Result<()> {
        std::fs::write(
            self.profile.join("plugins/installed_plugins.json"),
            serde_json::to_vec(&json!({
                "version":2,"plugins":{ID:[{"scope":"user","version":version,"installPath":self.cache(version)}]}
            }))?,
        )?;
        Ok(())
    }

    fn register_market(&self, location: &std::path::Path, source: &Value) -> Result<()> {
        std::fs::write(
            self.profile.join("plugins/known_marketplaces.json"),
            serde_json::to_vec(&json!({
                MARKET:{"source":source,"installLocation":location}
            }))?,
        )?;
        Ok(())
    }

    fn settings(&self) -> Result<Value> {
        Ok(serde_json::from_slice(&std::fs::read(
            self.profile.join(SETTINGS),
        )?)?)
    }
}

#[test]
fn switch_publishes_verified_market_before_plugin_update_and_retry_is_idempotent() -> TestResult {
    for complete_import in [true, false] {
        let f = SwitchFixture::new()?;
        let wanted = &f.basis.market;
        assert!(!SourceSwitch::is_bound(&f.target, &f.basis.offer)?);
        let plan = SourceSwitch::plan(f.target.clone(), &f.basis, true)?.ok_or("source switch")?;
        assert_eq!(plan.version(), "2.0.0");
        assert!(plan.detail().contains(f.old.to_str().ok_or("old path")?));
        assert!(plan.key().2.contains(&wanted.commit));
        let mut calls = Vec::new();
        let report = plan.run_with(&f.ctx, |_, journal, binding| {
            let invocation = &binding.invocation;
            super::super::worker_guard(journal, invocation)?;
            let args = &invocation.command.args;
            assert_eq!(invocation.env["GIT_CONFIG_GLOBAL"], "/dev/null");
            calls.push(args[1].clone());
            if args[1] == "marketplace" {
                let modified = f.settings()?;
                assert_eq!(
                    modified["extraKnownMarketplaces"][MARKET]["source"],
                    wanted.declaration()
                );
                assert_eq!(
                    modified["extraKnownMarketplaces"][MARKET]["autoUpdate"],
                    false
                );
                assert_eq!(modified["model"], "keep");
                if !complete_import {
                    return Ok(NativeOutcome::Unknown {
                        reason: "fixture interruption".into(),
                    });
                }
                let slot = f.profile.join("plugins/marketplaces").join(MARKET);
                SwitchFixture::populate(&slot, UPDATED)?;
                f.register_market(&slot, &wanted.declaration())?;
            } else {
                assert_eq!(args, &["plugin", "update", ID, "--scope", "user"]);
                assert!(wanted.verify_cache(&f.target, ID)?);
                SwitchFixture::populate(&f.cache("2.0.0"), UPDATED)?;
                f.register("2.0.0")?;
            }
            let exited = NativeOutcome::Exited {
                code: Some(0),
                signal: None,
            };
            super::super::worker_after(journal, invocation, &exited)?;
            Ok(exited)
        })?;
        if complete_import {
            assert_eq!(calls, ["marketplace", "update"]);
            assert_eq!(report["journal"]["phase"], "succeeded");
            assert!(SourceSwitch::is_bound(&f.target, &f.basis.offer)?);
            assert!(SourceSwitch::plan(f.target.clone(), &f.basis, false)?.is_none());
        } else {
            assert_eq!(calls, ["marketplace"]);
            assert_eq!(report["journal"]["phase"], "unknown");
            assert!(!SourceSwitch::is_bound(&f.target, &f.basis.offer)?);
            assert!(!f.cache("2.0.0").exists() && f.cache("1.0.0").exists());
        }
        assert_eq!(f.settings()?["enabledPlugins"][ID], false);
    }
    Ok(())
}

#[test]
fn an_already_verified_market_needs_only_plugin_update_and_is_rechecked_before_write() -> TestResult
{
    let f = SwitchFixture::new()?;
    let market = &f.basis.market;
    let slot = market_slot(&f.target, MARKET)?;
    SwitchFixture::populate(&slot, UPDATED)?;
    f.register_market(&slot, &market.declaration())?;
    let settings = f.profile.join(SETTINGS);
    let mut bytes = std::fs::read(&settings)?;
    change_source(&mut bytes, MARKET, &market.declaration())?;
    std::fs::write(settings, bytes)?;
    assert!(SourceSwitch::is_bound(&f.target, &f.basis.offer)?);

    let plan = SourceSwitch::plan(f.target.clone(), &f.basis, false)?.ok_or("update remaining")?;
    assert!(plan.detail().contains("owner/repo"));
    let mut calls = 0;
    let result = plan.run_with(&f.ctx, |_, journal, binding| {
        calls += 1;
        assert_eq!(
            binding.invocation.command.args,
            ["plugin", "update", ID, "--scope", "user"]
        );
        super::super::worker_guard(journal, &binding.invocation)?;
        SwitchFixture::populate(&f.cache("2.0.0"), UPDATED)?;
        f.register("2.0.0")?;
        Ok(NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    })?;
    assert_eq!(calls, 1);
    assert_eq!(result["journal"]["phase"], "succeeded");
    assert!(SourceSwitch::plan(f.target.clone(), &f.basis, false)?.is_none());

    std::fs::write(slot.join(".claude-plugin/plugin.json"), INITIAL)?;
    assert!(SourceSwitch::plan(f.target.clone(), &f.basis, false).is_err());
    assert_eq!(f.settings()?["enabledPlugins"][ID], false);
    Ok(())
}

#[test]
fn switch_refuses_stale_settings_and_new_market_with_additional_plugins() -> TestResult {
    let f = SwitchFixture::new()?;
    let plan = SourceSwitch::plan(f.target.clone(), &f.basis, true)?.ok_or("plan")?;
    let settings = f.profile.join(SETTINGS);
    let mut modified = f.settings()?;
    modified["model"] = json!("user-edited");
    std::fs::write(&settings, serde_json::to_vec(&modified)?)?;
    let result = plan.run_with(&f.ctx, |_, journal, binding| {
        super::super::worker_guard(journal, &binding.invocation)?;
        Ok(NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    });
    assert!(result.is_err());
    assert_eq!(f.settings()?, modified);

    let scratch = f.ctx.home.join("extra-plugin");
    SwitchFixture::populate(&scratch, UPDATED)?;
    let checkout = Directory::open(&scratch)?;
    let market = scratch.join(".claude-plugin/marketplace.json");
    let mut invalid: Value = serde_json::from_slice(CATALOG)?;
    invalid["plugins"]
        .as_array_mut()
        .ok_or("catalog members")?
        .push(json!({"name":"second","source":"./second"}));
    std::fs::write(&market, serde_json::to_vec(&invalid)?)?;
    let facts = capability_local::claude_market_files(&checkout)?;
    assert!(SwitchOffer::from_checkout(&f.basis.offer, &f.basis.binary, &checkout, facts).is_err());
    std::fs::write(&market, CATALOG)?;
    let metadata = scratch.join(".claude-plugin/plugin.json");
    std::fs::write(
        metadata,
        br#"{"name":"widget","version":"2.0.0","dependencies":["not-included"]}"#,
    )?;
    let facts = capability_local::claude_market_files(&checkout)?;
    assert!(SwitchOffer::from_checkout(&f.basis.offer, &f.basis.binary, &checkout, facts).is_err());
    Ok(())
}
