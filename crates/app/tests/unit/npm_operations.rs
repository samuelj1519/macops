#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use clap::Parser;
use serde_json::json;
use std::fs;

#[test]
fn native_adoption_rechecks_source_version_links_and_canonical_declarations() -> TestResult {
    for change in ["source", "version", "link", "ancestor", "declaration"] {
        let root = tempfile::tempdir_in("/private/tmp")?;
        let home = root.path();
        let catalog = home.join("catalog.json");
        fs::write(&catalog, br#"{"schema_version":1,"packages":[]}"#)?;
        let cli = crate::cli::Cli::try_parse_from([
            "macops",
            "--home",
            home.to_str().ok_or("home")?,
            "--catalog",
            catalog.to_str().ok_or("catalog")?,
            "adopt",
            "demo",
        ])?;
        let ctx = Context::from_cli(&cli, &mut vec![])?;
        let prefix = home.join("npm-root");
        let module = prefix.join("lib/node_modules/demo");
        fs::create_dir_all(&module)?;
        let manifest = module.join("package.json");
        fs::write(
            &manifest,
            br#"{"name":"demo","version":"1.0.0","_resolved":"first"}"#,
        )?;
        let package = Package {
            id: crate::policy::targets::npm_id(&prefix, "demo")?,
            name: "demo".into(),
            kind: crate::domain::Kind::Cli,
            provider: Provider::NpmObserved {
                prefix: prefix.clone(),
                name: "demo".into(),
            },
            official_url: "https://example.org".into(),
            paths: vec![],
            exact_version_required: false,
        };
        let planned = plan(&ctx, &package, Action::Adopt, None, false)?;
        match change {
            "source" => fs::write(
                &manifest,
                br#"{"name":"demo","version":"1.0.0","_resolved":"second"}"#,
            )?,
            "version" => fs::write(&manifest, br#"{"name":"demo","version":"2.0.0"}"#)?,
            "link" | "ancestor" => {
                let old = if change == "link" {
                    module.clone()
                } else {
                    prefix.join("lib/node_modules")
                };
                let relocated = home.join("relocated");
                fs::rename(&old, &relocated)?;
                std::os::unix::fs::symlink(relocated, old)?;
            }
            "declaration" => fs::write(
                &catalog,
                serde_json::to_vec(
                    &json!({"schema_version":1,"packages":[{"id":"new:demo","name":"demo","kind":"cli","official_url":"https://example.org","provider":{"type":"npm","prefix":prefix,"package":"demo"}}]}),
                )?,
            )?,
            _ => return Err("unknown fixture".into()),
        }
        assert!(execute(&ctx, planned, false, true).is_err(), "{change}");
        assert!(!ctx.state.join("active.json").exists(), "{change}");
        assert!(!ctx.state.join("receipts.json").exists(), "{change}");
    }
    Ok(())
}

#[test]
fn replacing_an_ancestor_changes_empty_namespace_observation_even_with_same_leaf() -> TestResult {
    let root = tempfile::tempdir_in("/private/tmp")?;
    let ancestor = root.path().join("ancestor");
    let prefix = ancestor.join("prefix");
    fs::create_dir_all(&prefix)?;
    let query = crate::npm_local::capture(
        &prefix,
        "demo",
        crate::domain::NpmContract::DeclaredV1,
        false,
    )?;
    let before = crate::npm_local::observe(&query, false, &mut 1024)?;
    let relocated = root.path().join("relocated");
    fs::rename(&ancestor, &relocated)?;
    fs::create_dir(&ancestor)?;
    fs::rename(relocated.join("prefix"), &prefix)?;
    let current = crate::npm_local::observe(&query, false, &mut 1024)?;
    assert!(matches!(&before, Observation::Missing { .. }));
    assert!(matches!(&current, Observation::Missing { .. }));
    assert_ne!(before, current);
    Ok(())
}
