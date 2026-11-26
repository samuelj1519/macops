use super::*;
use crate::{domain::*, test_support::TestResult};
use clap::Parser;
use std::fs;

fn fixture() -> Result<(tempfile::TempDir, Context, Package)> {
    let tmp = tempfile::tempdir_in("/private/tmp")?;
    let home = tmp.path().join("home");
    fs::create_dir(&home)?;
    let catalog = tmp.path().join("catalog.json");
    fs::write(&catalog, br#"{"schema_version":1,"packages":[]}"#)?;
    let cli = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(|| Error::new("test", "home"))?,
        "--catalog",
        catalog
            .to_str()
            .ok_or_else(|| Error::new("test", "catalog"))?,
        "app",
        "adopt",
        "demo-tool",
    ])
    .map_err(|e| Error::new("test", e.to_string()))?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let root = home.join("tools");
    let bin = home.join("uv-bin");
    let env = root.join("demo-tool");
    let dist = env.join("lib/python3.12/site-packages/demo_tool-1.dist-info");
    fs::create_dir_all(&dist)?;
    fs::create_dir_all(env.join("bin"))?;
    fs::create_dir(&bin)?;
    fs::write(dist.join("METADATA"), "Name: demo-tool\nVersion: 1\n")?;
    fs::write(env.join("pyvenv.cfg"), "home = /synthetic\n")?;
    fs::write(env.join("bin/demo"), "#!/synthetic\n")?;
    std::os::unix::fs::symlink(env.join("bin/demo"), bin.join("demo"))?;
    fs::write(
        env.join("uv-receipt.toml"),
        format!(
            "[tool]\nrequirements=[{{name='demo-tool'}}]\nentrypoints=[{{name='demo',install-path='{}'}}]\n",
            bin.join("demo").display()
        ),
    )?;
    let package = Package {
        id: crate::software::rules::uv_local::tool_id(&root, "demo-tool")?,
        name: "demo-tool".into(),
        kind: Kind::Cli,
        official_url: String::new(),
        provider: Provider::UvObserved {
            package: "demo-tool".into(),
            binding: UvToolBinding {
                tool_dir: root,
                bin_dir: bin,
            },
        },
        paths: vec![],
        exact_version_required: false,
    };
    Ok((tmp, ctx, package))
}
#[test]
fn uv_plan_drift_owner_change_and_unknown_namespace_never_commit() -> TestResult {
    for change in ["metadata", "bin", "root", "declaration", "receipt"] {
        let (_tmp, ctx, package) = fixture()?;
        let planned = plan(&ctx, &package, Action::Adopt, None, false)?;
        let (name, binding) =
            crate::software::rules::uv_local::descriptor(&package.provider).ok_or("binding")?;
        match change {
            "metadata" => fs::write(
                binding
                    .tool_dir
                    .join(name)
                    .join("lib/python3.12/site-packages/demo_tool-1.dist-info/METADATA"),
                "Name: demo-tool\nVersion: 2\n",
            )?,
            "bin" => {
                fs::rename(&binding.bin_dir, ctx.home.join("old-bin"))?;
                fs::create_dir(&binding.bin_dir)?;
            }
            "root" => {
                fs::rename(&binding.tool_dir, ctx.home.join("old-tools"))?;
                fs::create_dir(&binding.tool_dir)?;
            }
            "declaration" => {
                let mut other = package.clone();
                other.id = "another-owner".into();
                fs::write(
                    &ctx.catalog,
                    serde_json::to_vec(&Catalog {
                        schema_version: 1,
                        packages: vec![other],
                    })?,
                )?;
            }
            _ => {
                let mut other = package.clone();
                other.id = "another-owner".into();
                let receipts = Receipts {
                    schema_version: 1,
                    installations: std::collections::BTreeMap::from([(
                        other.id.clone(),
                        Receipt {
                            scope: Some(ctx.scope.clone()),
                            query: planned.query.clone(),
                            package: other,
                            observation: planned.before.clone(),
                            operation_id: "OP-OTHER".into(),
                            updated_at: 0,
                        },
                    )]),
                };
                fs::create_dir_all(&ctx.state)?;
                fs::write(
                    ctx.state.join("receipts.json"),
                    serde_json::to_vec(&receipts)?,
                )?;
            }
        }
        assert!(execute(&ctx, planned, false, true).is_err(), "{change}");
        if ctx.state.join("receipts.json").exists() {
            let receipts = crate::catalog::target::receipts(&ctx)?;
            assert!(!receipts.installations.contains_key(&package.id));
        }
        assert!(!ctx.state.join("active.json").exists(), "{change}");
    }
    Ok(())
}
#[test]
fn uv_prepared_receipt_cas_and_old_uv_recover_remain_no_spawn() -> TestResult {
    let (_tmp, ctx, package) = fixture()?;
    let planned = plan(&ctx, &package, Action::Adopt, None, false)?;
    let journal = new_journal_with_id(&ctx, planned, "OP-UV".into())?;
    let store = Store::lock(&ctx)?;
    store.save(&journal)?;
    let mut other = journal.plan.package.clone();
    other.id = "foreign-owner".into();
    let receipts = Receipts {
        schema_version: 1,
        installations: std::collections::BTreeMap::from([(
            other.id.clone(),
            Receipt {
                scope: Some(ctx.scope.clone()),
                query: journal.plan.query.clone(),
                package: other,
                observation: journal.plan.before.clone(),
                operation_id: "OP-OTHER".into(),
                updated_at: 0,
            },
        )]),
    };
    Store::atomic(&store.root, "receipts.json", &receipts)?;
    assert!(store.commit(&journal, journal.plan.before.clone()).is_err());
    assert!(
        store
            .receipts()?
            .installations
            .contains_key("foreign-owner")
    );
    drop(store);
    let mut legacy = crate::test_support::journal()?;
    legacy.scope = ctx.scope.clone();
    legacy.home = ctx.home.clone();
    legacy.scope_sha256 = fingerprint(&legacy.scope)?;
    legacy.state = ctx.state.clone();
    legacy.cache = ctx.cache.clone();
    legacy.settings = ctx.settings.clone();
    legacy.plan.package.provider = Provider::Uv {
        package: "demo-tool".into(),
        binding: None,
    };
    legacy.plan.recipe_sha256 = fingerprint(&legacy.plan.package)?;
    legacy.plan_sha256 = fingerprint(&legacy.plan)?;
    legacy.phase = Phase::Unknown;
    legacy.process_group = None;
    let store = Store::lock(&ctx)?;
    store.save(&legacy)?;
    drop(store);
    let before = fs::read(ctx.state.join("active.json"))?;
    let result = recover(&ctx, &legacy.id, false, false, false)?;
    assert_eq!(result["reason"], "legacy_uv_query_unbound");
    assert_eq!(fs::read(ctx.state.join("active.json"))?, before);
    Ok(())
}

#[test]
fn uv_resolve_and_final_publication_recheck_declaration_owner_history() -> TestResult {
    let (_tmp, ctx, package) = fixture()?;
    let mut owner = package.clone();
    owner.id = "owner:alice".into();
    fs::write(
        &ctx.catalog,
        serde_json::to_vec(&Catalog {
            schema_version: 1,
            packages: vec![owner.clone()],
        })?,
    )?;
    let resolved = crate::catalog::target::resolve(&ctx, &owner.id)?;
    assert_eq!(resolved.id, "owner:alice");
    let planned = plan(&ctx, &resolved, Action::Adopt, None, false)?;
    let journal = new_journal_with_id(&ctx, planned, "OP-CAS".into())?;
    let store = Store::lock(&ctx)?;
    store.save(&journal)?;
    let mut changed = owner.clone();
    changed.name = "changed declaration".into();
    fs::write(
        &ctx.catalog,
        serde_json::to_vec(&Catalog {
            schema_version: 1,
            packages: vec![changed],
        })?,
    )?;
    assert!(
        store
            .commit_checked(&journal, journal.plan.before.clone(), || {
                crate::software::uv_ownership::check(&ctx, &journal.plan, None)
            })
            .is_err()
    );
    assert!(store.receipts()?.installations.is_empty());
    let mut late = owner.clone();
    late.id = "owner:bob".into();
    fs::write(
        &ctx.catalog,
        serde_json::to_vec(&Catalog {
            schema_version: 1,
            packages: vec![late.clone()],
        })?,
    )?;
    assert!(
        store
            .commit_checked(&journal, journal.plan.before.clone(), || {
                crate::software::uv_ownership::check(&ctx, &journal.plan, None)
            })
            .is_err()
    );
    assert!(store.receipts()?.installations.is_empty());
    fs::write(
        &ctx.catalog,
        serde_json::to_vec(&Catalog {
            schema_version: 1,
            packages: vec![owner.clone(), late],
        })?,
    )?;
    assert!(crate::catalog::target::resolve(&ctx, &owner.id).is_err());
    assert!(crate::catalog::target::resolve(&ctx, &package.id).is_err());
    Ok(())
}

#[test]
fn uv_adopted_namespace_replacement_cannot_be_silently_recaptured() -> TestResult {
    let (_tmp, ctx, package) = fixture()?;
    let planned = plan(&ctx, &package, Action::Adopt, None, false)?;
    execute(&ctx, planned, false, true)?;
    let (_, binding) =
        crate::software::rules::uv_local::descriptor(&package.provider).ok_or("binding")?;
    fs::rename(&binding.tool_dir, ctx.home.join("old-tool-root"))?;
    fs::create_dir(&binding.tool_dir)?;
    fs::rename(
        ctx.home.join("old-tool-root/demo-tool"),
        binding.tool_dir.join("demo-tool"),
    )?;
    assert!(matches!(
        inventory::observe(&ctx, &package),
        Observation::Unknown { .. }
    ));
    assert!(plan(&ctx, &package, Action::Adopt, None, false).is_err());
    Ok(())
}
