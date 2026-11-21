use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::fs;

#[test]
fn distinct_claude_registrations_stay_read_only_and_a_changed_profile_invalidates_the_view()
-> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let first = home.join(".claude-one/plugins");
    let second = home.join(".claude-two/plugins");
    let third = home.join(".claude-three/plugins");
    for plugins in [&first, &second, &third] {
        fs::create_dir_all(plugins)?;
    }
    let registry = |plugins: &std::path::Path| plugins.join("installed_plugins.json");
    for plugins in [&first, &second] {
        fs::write(
            registry(plugins),
            serde_json::to_vec(&json!({
                "version": 2,
                "plugins": {
                    "shared@official": [{
                        "scope": "user",
                        "installPath": plugins.join("cache/official/shared/1.0.0"),
                        "version": "1.0.0"
                    }],
                    "empty@official": []
                }
            }))?,
        )?;
    }
    let malformed = b"invalid registry";
    fs::write(registry(&third), malformed)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let view = observe(&ctx)?;
    let shared: Vec<_> = view
        .unsupported
        .iter()
        .filter(|item| item["native_id"] == "shared@official")
        .collect();
    assert_eq!(shared.len(), 2);
    assert_ne!(shared[0]["id"], shared[1]["id"]);
    assert_ne!(shared[0]["profile"], shared[1]["profile"]);
    assert!(shared.iter().all(|item| {
        item["scope"] == "user" && item["reason"] == "unsupported_native_plugin_source"
    }));
    assert_eq!(
        view.unsupported
            .iter()
            .filter(|item| item["reason"] == "unknown_native_plugin_registration")
            .count(),
        2
    );
    assert!(
        view.unsupported
            .iter()
            .any(|item| item["reason"] == "unknown_native_plugin_registry")
    );
    view.verify()?;

    fs::create_dir(home.join(".claude-added"))?;
    assert!(view.verify().is_err());
    fs::remove_dir(home.join(".claude-added"))?;
    view.verify()?;
    let unchanged = fs::read(registry(&second))?;
    fs::write(registry(&first), br#"{"version":2,"plugins":{}}"#)?;
    assert!(view.verify().is_err());
    assert_eq!(fs::read(registry(&second))?, unchanged);
    assert_eq!(fs::read(registry(&third))?, malformed);
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn a_configured_claude_root_with_an_absent_registry_is_still_bound_to_the_view() -> TestResult {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let root = temp.path().join("custom-model");
    fs::create_dir(&home)?;
    fs::create_dir_all(root.join("plugins"))?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let mut ctx = Context::from_cli(&cli, &mut vec![])?;
    ctx.harness.claude = Some(root.clone());
    let before = observe(&ctx)?;
    assert_eq!(before.unsupported, [] as [Value; 0]);
    before.verify()?;

    let registry = root.join("plugins/installed_plugins.json");
    fs::write(&registry, br#"{"version":2,"plugins":{}}"#)?;
    assert!(before.verify().is_err());
    assert_eq!(observe(&ctx)?.unsupported, [] as [Value; 0]);
    let unknown = home.join(".claude-broken");
    fs::write(&unknown, b"foreign file")?;
    assert_eq!(
        observe(&ctx).err().ok_or("unsupported profile shape")?.code,
        "source_plugin_scope_changed"
    );
    assert_eq!(fs::read(&unknown)?, b"foreign file");
    assert!(!ctx.state.exists());
    Ok(())
}
