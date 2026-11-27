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
