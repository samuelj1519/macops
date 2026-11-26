use super::*;
use crate::{
    domain::{Kind, Package},
    test_support::TestResult,
};
use clap::Parser;
use std::fs;

#[test]
fn last_commit_check_fences_recipe_namespace_and_receipt_changes() -> TestResult {
    for change in [
        "catalog",
        "second-recipe",
        "old-receipt",
        "another-owner",
        "manifest",
        "scope",
    ] {
        let root = tempfile::tempdir_in("/private/tmp")?;
        let home = root.path();
        let catalog = home.join("catalog.json");
        let prefix = home.join("npm");
        let module = prefix.join("lib/node_modules/demo");
        fs::create_dir_all(&module)?;
        let manifest = module.join("package.json");
        fs::write(&manifest, br#"{"name":"demo","version":"1.0.0"}"#)?;
        fs::write(&catalog, br#"{"schema_version":1,"packages":[]}"#)?;
        let cli = crate::cli::Cli::try_parse_from([
            "macops",
            "--home",
            home.to_str().ok_or("home")?,
            "--catalog",
            catalog.to_str().ok_or("catalog")?,
            "app",
            "adopt",
            "demo",
        ])?;
        let ctx = Context::from_cli(&cli, &mut vec![])?;
        let id = crate::policy::catalog::targets::npm_id(&prefix, "demo")?;
        let observed = Package {
            id: id.clone(),
            name: "demo".into(),
            kind: Kind::Cli,
            provider: Provider::NpmObserved {
                prefix: prefix.clone(),
                name: "demo".into(),
            },
            official_url: "https://example.org/project".into(),
            paths: vec![],
            exact_version_required: false,
        };
        let previous = operations::plan(&ctx, &observed, Action::Adopt, None, false)?;
        operations::execute(&ctx, previous, false, true)?;
        let declared = json!({
            "schema_version":1,
            "packages":[{
                "id":id,"kind":"cli","name":"demo","official_url":"https://example.org/project",
                "provider":{"type":"npm","prefix":prefix,"package":"demo","authority":{"allow_remove":true}},
            }],
        });
        fs::write(&catalog, serde_json::to_vec(&declared)?)?;

        let store = Store::lock(&ctx)?;
        let candidate = prepare_plan(&ctx, &id, &store.receipts()?)?;
        let mut journal = operations::new_journal(&ctx, candidate)?;
        recheck(&ctx, &store, &journal)?;
        store.save(&journal)?;
        let receipt = ctx.state.join("receipts.json");
        change_input(change, &store, &journal)?;
        let before = fs::read(&receipt)?;
        let active = ctx.state.join("active.json");
        let active_before = fs::read(&active)?;
        let error = commit(&ctx, &store, &journal)
            .err()
            .ok_or("commit accepted stale inputs")?;
        let code = match change {
            "another-owner" | "second-recipe" => "owner_conflict",
            "manifest" => "installation_drift",
            "scope" => "manager_scope_changed",
            _ => "plan_stale",
        };
        assert_eq!(error.code, code, "{change}");
        assert_eq!(fs::read(&receipt)?, before);
        assert_eq!(fs::read(&active)?, active_before);

        let mut elsewhere = ctx.clone();
        elsewhere.state = home.join("invalid-state");
        let unknown = recover(&elsewhere, &store, &mut journal, false)
            .err()
            .ok_or("wrong state recovered")?;
        assert_eq!(unknown.code, "npm_binding_outcome_unknown");
        assert_eq!(fs::read(active)?, active_before);
    }
    Ok(())
}

fn change_input(change: &str, store: &Store, journal: &Journal) -> TestResult {
    let home = &journal.home;
    let prefix = home.join("npm");
    let catalog = home.join("catalog.json");
    let module = prefix.join("lib/node_modules/demo");
    let manifest = module.join("package.json");
    let receipt = journal.state.join("receipts.json");
    let id = &journal.plan.package.id;
    match change {
        "catalog" | "second-recipe" => {
            let mut declared: Value = serde_json::from_slice(&fs::read(&catalog)?)?;
            if change == "catalog" {
                declared["packages"][0]["provider"]["authority"]["allow_remove"] = json!(false);
            } else {
                let mut second = declared["packages"][0].clone();
                second["id"] = json!("npm:second");
                declared["packages"]
                    .as_array_mut()
                    .ok_or("packages")?
                    .push(second);
            }
            fs::write(&catalog, serde_json::to_vec(&declared)?)?;
        }
        "old-receipt" | "another-owner" => {
            let mut records = store.receipts()?;
            let mut replacement = records.installations.get(id).ok_or("receipt")?.clone();
            if change == "old-receipt" {
                replacement.updated_at += 1;
                records.installations.insert(id.to_owned(), replacement);
            } else {
                replacement.package.id = "npm:other".into();
                records
                    .installations
                    .insert("npm:other".into(), replacement);
            }
            fs::write(&receipt, serde_json::to_vec(&records)?)?;
        }
        "manifest" => fs::write(&manifest, br#"{"name":"demo","version":"2.0.0"}"#)?,
        "scope" => {
            fs::rename(&prefix, home.join("relocated"))?;
            fs::create_dir_all(&module)?;
            fs::write(&manifest, br#"{"name":"demo","version":"1.0.0"}"#)?;
        }
        _ => return Err("unknown change".into()),
    }
    Ok(())
}
