#![cfg(test)]
use super::*;
use crate::{cli::Cli, platform::fs::Directory, test_support::TestResult};
use clap::Parser;
use std::fs;

const ID: &str = "widget@fixture-market";

struct Fixture {
    _root: tempfile::TempDir,
    ctx: Context,
    target: InstallTarget,
    binary: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(client: &str) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir_in("/private/tmp")?;
        let home = root.path().join("home");
        let profile = home.join(format!(".{client}"));
        fs::create_dir_all(profile.join("plugins"))?;
        let ctx = Context::from_cli(
            &Cli::try_parse_from(["mo", "--home", home.to_str().ok_or("home")?, "skill"])?,
            &mut vec![],
        )?;
        let target = InstallTarget {
            client: client.into(),
            name: format!("{client}:default"),
            directory: crate::capability::capability_store::binding(
                &Directory::open(&profile)?,
                &profile,
            )?,
            command: None,
        };
        let binary = home.join("fixture-client");
        fs::copy("/usr/bin/true", &binary)?;
        let source = home.join("market");
        fs::create_dir_all(source.join(".claude-plugin"))?;
        fs::write(
            source.join(".claude-plugin/marketplace.json"),
            br#"{"name":"fixture-market","plugins":[{"name":"widget","source":"./"}]}"#,
        )?;
        fs::write(
            source.join(".claude-plugin/plugin.json"),
            br#"{"name":"widget","version":"2.0.0"}"#,
        )?;
        Ok(Self {
            _root: root,
            ctx,
            target,
            binary,
            source,
        })
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.target.directory.canonical.join(relative)
    }

    fn json(&self, relative: &str, value: &Value) -> TestResult {
        fs::write(self.path(relative), serde_json::to_vec(value)?)?;
        Ok(())
    }

    fn claude_settings(&self, enabled: &Value) -> TestResult {
        self.json(
            "settings.json",
            &json!({
                "enabledPlugins": {ID: enabled},
                "model": "fixture-model"
            }),
        )
    }

    fn claude_install(&self, version: &str) -> TestResult {
        let payload = self.path(&format!("plugins/cache/fixture-market/widget/{version}"));
        if version == "2.0.0" {
            fs::create_dir_all(payload.join(".claude-plugin"))?;
            fs::copy(
                self.source.join(".claude-plugin/plugin.json"),
                payload.join(".claude-plugin/plugin.json"),
            )?;
            fs::copy(
                self.source.join(".claude-plugin/marketplace.json"),
                payload.join(".claude-plugin/marketplace.json"),
            )?;
        } else {
            fs::create_dir_all(payload.join(".claude-plugin"))?;
            fs::write(
                payload.join(".claude-plugin/plugin.json"),
                serde_json::to_vec(&json!({"name":"widget","version":version}))?,
            )?;
        }
        self.json("plugins/installed_plugins.json", &json!({
            "version":2,"plugins":{ID:[{"scope":"user","version":version,"installPath":payload}]}
        }))?;
        self.json("plugins/known_marketplaces.json", &json!({
            "fixture-market":{"source":{"source":"directory","path":self.source},"installLocation":self.source}
        }))
    }

    fn intent(&self, desired: Desired) -> Result<Intent> {
        Ok(Intent {
            target: self.target.clone(),
            id: ID.into(),
            desired,
            before: read_facts(&self.target, ID)?,
            binary: self.binary.clone(),
            binary_digest: digest_binary(&self.binary)?,
            market: None,
        })
    }
}

#[test]
fn claude_native_lifecycle_checks_versions_scope_and_postconditions() -> TestResult {
    let f = Fixture::new("claude")?;
    f.claude_install("1.0.0")?;
    f.claude_settings(&json!(true))?;
    assert!(Intent::plan(&f.ctx, f.target.clone(), ID, Operation::Enable, None, false)?.is_none());
    assert!(
        Intent::plan(
            &f.ctx,
            f.target.clone(),
            ID,
            Operation::Disable,
            None,
            false
        )
        .is_err()
    );
    assert!(Intent::plan(&f.ctx, f.target.clone(), ID, Operation::Upgrade, None, true).is_err());
    assert!(pinned_market(&f.target, ID, Some("3.0.0")).is_err());

    let market = pinned_market(&f.target, ID, Some("2.0.0"))?;
    let mut upgrade = f.intent(Desired::Version {
        version: market.version.clone(),
        enabled: Some(true),
        payload: market.payload.clone(),
    })?;
    upgrade.market = Some(market);
    let store = Store::lock(&f.ctx)?;
    let result = upgrade.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        worker_guard(journal, &binding.invocation)?;
        assert_eq!(
            binding.invocation.command.args,
            ["plugin", "update", ID, "--scope", "user"]
        );
        assert_eq!(
            binding.invocation.env["CLAUDE_CONFIG_DIR"],
            f.target.directory.canonical.display().to_string()
        );
        f.claude_install("2.0.0")
            .map_err(|e| changed_fixture(e.as_ref()))?;
        let exited = NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        };
        worker_after(journal, &binding.invocation, &exited)?;
        Ok(exited)
    })?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    assert_eq!(upgrade.target_version(), Some("2.0.0"));
    assert_eq!(upgrade.run_locked(&f.ctx, &store)?["status"], "current");
    assert!(
        Intent::plan(
            &f.ctx,
            f.target.clone(),
            ID,
            Operation::Upgrade,
            None,
            false
        )?
        .is_none()
    );

    Ok(())
}

#[test]
fn claude_failed_and_unknown_mutations_retain_the_original_operation() -> TestResult {
    let f = Fixture::new("claude")?;
    f.claude_install("2.0.0")?;
    f.claude_settings(&json!(true))?;
    let store = Store::lock(&f.ctx)?;
    let disable = f.intent(Desired::Enabled(false))?;
    let unchanged = disable.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        worker_guard(journal, &binding.invocation)?;
        assert_eq!(
            binding.invocation.command.args,
            ["plugin", "disable", ID, "--scope", "user"]
        );
        Ok(NativeOutcome::Exited {
            code: Some(1),
            signal: None,
        })
    })?;
    assert_eq!(unchanged["journal"]["phase"], "failed");

    let disabled = disable.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        worker_guard(journal, &binding.invocation)?;
        f.claude_settings(&json!(false))
            .map_err(|e| changed_fixture(e.as_ref()))?;
        Ok(NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    })?;
    assert_eq!(disabled["journal"]["phase"], "succeeded");
    assert!(disable.target_version().is_none());

    let enable = f.intent(Desired::Enabled(true))?;
    f.claude_settings(&json!(null))?;
    assert!(enable.run_locked(&f.ctx, &store).is_err());
    f.claude_settings(&json!(false))?;
    let stopped = enable.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        worker_guard(journal, &binding.invocation)?;
        assert_eq!(
            binding.invocation.command.args,
            ["plugin", "enable", ID, "--scope", "user"]
        );
        f.claude_settings(&json!(null))
            .map_err(|e| changed_fixture(e.as_ref()))?;
        Ok(NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    })?;
    assert_eq!(stopped["journal"]["phase"], "unknown");
    assert!(
        stopped["journal"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("启停配置"))
    );
    let mut journal: Journal = serde_json::from_value(stopped["journal"].clone())?;
    assert!(owns(&journal));
    assert!(enable.run_locked(&f.ctx, &store).is_err());
    f.claude_settings(&json!(true))?;
    assert_eq!(
        recover(&store, &mut journal, true, true)?["journal"]["phase"],
        "failed"
    );

    let removal = f.intent(Desired::Absent)?;
    let removed = removal.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        worker_guard(journal, &binding.invocation)?;
        assert_eq!(
            binding.invocation.command.args,
            ["plugin", "uninstall", ID, "--scope", "user", "--keep-data"]
        );
        fs::remove_file(f.path("plugins/installed_plugins.json"))?;
        Ok(NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    })?;
    assert_eq!(removed["journal"]["phase"], "succeeded");
    assert!(f.path("plugins/cache/fixture-market/widget/2.0.0").exists());
    assert!(
        Intent::plan(
            &f.ctx,
            f.target.clone(),
            ID,
            Operation::Uninstall,
            None,
            false
        )?
        .is_none()
    );
    Ok(())
}

fn changed_fixture(e: &dyn std::error::Error) -> Error {
    Error::new("fixture", e.to_string())
}

#[test]
fn codex_removal_requires_both_registration_and_installation_to_be_absent() -> TestResult {
    let f = Fixture::new("codex")?;
    let cache = f.path("plugins/cache/fixture-market/widget");
    fs::create_dir_all(&cache)?;
    let config = f.path("config.toml");
    fs::write(&config, "[plugins.'widget@fixture-market']\nenabled=true\n")?;
    assert!(
        Intent::plan(
            &f.ctx,
            f.target.clone(),
            ID,
            Operation::Upgrade,
            None,
            false
        )
        .is_err()
    );
    assert!(
        Intent::plan(
            &f.ctx,
            f.target.clone(),
            ID,
            Operation::Disable,
            None,
            false
        )
        .is_err()
    );
    let remove = f.intent(Desired::Absent)?;
    let store = Store::lock(&f.ctx)?;
    let result = remove.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        worker_guard(journal, &binding.invocation)?;
        assert_eq!(binding.invocation.command.args, ["plugin", "remove", ID]);
        assert_eq!(
            binding.invocation.env["CODEX_HOME"],
            f.target.directory.canonical.display().to_string()
        );
        fs::remove_file(&config)?;
        Ok(NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        })
    })?;
    assert_eq!(result["journal"]["phase"], "unknown");
    assert!(read_facts(&f.target, ID)?.installed);
    assert!(read_facts(&f.target, "missing-market").is_err());

    let mut journal: Journal = serde_json::from_value(result["journal"].clone())?;
    assert_eq!(
        recover(&store, &mut journal, false, true)?["journal"]["phase"],
        "unknown"
    );
    fs::remove_dir_all(&cache)?;
    journal.phase = Phase::Prepared;
    assert_eq!(
        recover(&store, &mut journal, false, true)?["journal"]["phase"],
        "succeeded"
    );
    assert_eq!(remove.run_locked(&f.ctx, &store)?["status"], "current");
    Ok(())
}

#[test]
fn native_admission_freezes_binary_identity_and_rejects_changed_scope_or_invocation() -> TestResult
{
    let f = Fixture::new("claude")?;
    f.claude_install("1.0.0")?;
    f.claude_settings(&json!(true))?;
    let plan = Intent::plan_using(
        &f.ctx,
        f.target.clone(),
        ID,
        Operation::Disable,
        None,
        false,
        |_, client, desired| {
            assert_eq!(client, "claude");
            assert_eq!(desired, &Desired::Enabled(false));
            Ok(f.binary.clone())
        },
    )?
    .ok_or("disable plan")?;
    assert!(plan.key().2.contains(ID));
    assert!(!matches_install(
        &f.ctx,
        &f.target,
        ID,
        "2.0.0",
        &"a".repeat(40)
    )?);
    assert!(
        Intent::plan(
            &f.ctx,
            f.target.clone(),
            ID,
            Operation::Upgrade,
            Some(("2.0.0", &"a".repeat(40))),
            true
        )
        .is_err()
    );
    let store = Store::lock(&f.ctx)?;
    let report = plan.run_locked_with(&f.ctx, &store, |_, journal, binding| {
        let mut invocation = binding.invocation.clone();
        invocation
            .env
            .insert("CLAUDE_CONFIG_DIR".into(), "/outside/fixture".into());
        assert!(worker_guard(journal, &invocation).is_err());
        assert!(worker_guard(journal, &binding.invocation).is_ok());
        let mut forged = journal.clone();
        forged.plan.warnings[0].push('x');
        assert!(read_intent(&forged).is_err());
        assert!(!owns(&{
            let mut unowned = journal.clone();
            unowned.plan.delegated = false;
            unowned
        }));
        fs::write(&f.binary, b"replacement")?;
        assert!(worker_guard(journal, &binding.invocation).is_err());
        Ok(NativeOutcome::Exited {
            code: Some(1),
            signal: None,
        })
    })?;
    assert_eq!(report["journal"]["phase"], "failed");
    assert!(plan.run_locked(&f.ctx, &store).is_err());
    assert_eq!(read_facts(&f.target, ID)?.enabled, Some(true));
    Ok(())
}

#[test]
fn invalid_or_non_unique_native_plugin_evidence_stays_read_only() -> TestResult {
    let f = Fixture::new("claude")?;
    f.claude_install("1.0.0")?;
    f.claude_settings(&json!(true))?;
    let registry = f.path("plugins/installed_plugins.json");
    let bytes = fs::read(&registry)?;
    let mut duplicate: Value = serde_json::from_slice(&bytes)?;
    let record = duplicate["plugins"][ID][0].clone();
    duplicate["plugins"][ID] = json!([record, record]);
    f.json("plugins/installed_plugins.json", &duplicate)?;
    assert!(read_facts(&f.target, ID).is_err());
    fs::write(&registry, bytes)?;

    let manifest = f.path("plugins/cache/fixture-market/widget/1.0.0/.claude-plugin/plugin.json");
    fs::write(&manifest, br#"{"name":"different","version":"1.0.0"}"#)?;
    assert!(read_facts(&f.target, ID).is_err());
    fs::remove_file(&manifest)?;
    assert!(read_facts(&f.target, ID).is_err());

    let known = f.path("plugins/known_marketplaces.json");
    fs::write(&known, b"{}")?;
    assert!(pinned_market(&f.target, ID, None).is_err());
    f.json(
        "plugins/known_marketplaces.json",
        &json!({
            "fixture-market":{
                "source":{"source":"directory","path":f.source},
                "installLocation":"/other/fixture"
            }
        }),
    )?;
    assert!(pinned_market(&f.target, ID, None).is_err());
    assert!(!f.ctx.state.exists());
    Ok(())
}
