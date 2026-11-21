use crate::{
    domain::{Catalog, Kind, OwnedPath, Package, PathKind, Provider, Receipts},
    error::{Error, Result},
};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub fn add_ai(catalog: &mut Catalog) {
    for name in [
        "codex",
        "claude",
        "cursor",
        "kimi",
        "dsh",
        "codegraph",
        "mimo",
    ] {
        let id = format!("ai:{name}");
        if !catalog.packages.iter().any(|p| p.id == id) {
            catalog.packages.push(Package {
                id,
                name: name.into(),
                kind: Kind::Cli,
                official_url: String::new(),
                provider: Provider::Legacy {
                    category: "ai".into(),
                    selector: name.into(),
                },
                paths: vec![],
                exact_version_required: matches!(name, "dsh" | "codegraph"),
            });
        }
    }
}

pub fn add_managers(cache: &Path, catalog: &mut Catalog) {
    for (manager, url) in [
        ("brew", "https://docs.brew.sh/"),
        ("mise", "https://mise.jdx.dev/"),
        ("uv", "https://docs.astral.sh/uv/"),
        ("rustup", "https://rust-lang.github.io/rustup/"),
        ("conda", "https://docs.conda.io/"),
        ("npm", "https://docs.npmjs.com/"),
        ("bun", "https://bun.com/docs"),
        ("cargo", "https://doc.rust-lang.org/cargo/"),
    ] {
        let id = format!("manager:{manager}");
        if !catalog.packages.iter().any(|p| p.id == id) {
            catalog.packages.push(Package {
                id,
                name: manager.into(),
                kind: Kind::Manager,
                official_url: url.into(),
                provider: Provider::Manager {
                    executable: manager.into(),
                },
                paths: vec![],
                exact_version_required: false,
            });
        }
    }
    if !catalog
        .packages
        .iter()
        .any(|p| p.id == "service:postgresql")
    {
        catalog.packages.push(crate::policy::service::package());
    }
    if !catalog.packages.iter().any(|p| p.id == "macops:cache") {
        catalog.packages.push(Package {
            id: "macops:cache".into(),
            name: "macops-cache".into(),
            kind: Kind::Manager,
            official_url: String::new(),
            provider: Provider::Macops,
            paths: vec![OwnedPath {
                path: cache.to_path_buf(),
                kind: PathKind::Cache,
            }],
            exact_version_required: false,
        });
    }
}

pub fn merge_receipts(catalog: &mut Catalog, state: Receipts) -> Result<()> {
    if state.schema_version != 1 {
        return Err(Error::new("state_version", "receipt schema 不支持"));
    }
    for receipt in state.installations.into_values() {
        if !catalog.packages.iter().any(|p| p.id == receipt.package.id) {
            catalog.packages.push(receipt.package);
        }
    }
    Ok(())
}

pub fn add_assets(catalog: &mut Catalog, value: &Value) -> Result<()> {
    let assets = value
        .get("assets")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::new("invalid_assets", "assets 必须是对象"))?;
    for (name, value) in assets {
        if !value.get("kind").is_some_and(Value::is_string) {
            return Err(Error::new("invalid_asset_kind", name));
        }
        let id = format!("skill:{name}");
        if !catalog.packages.iter().any(|p| p.id == id) {
            catalog.packages.push(Package {
                id,
                name: name.clone(),
                kind: if value
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|k| k.contains("plugin"))
                {
                    Kind::Plugin
                } else {
                    Kind::Skill
                },
                official_url: value
                    .get("repository")
                    .and_then(Value::as_str)
                    .map_or_else(String::new, |repo| format!("https://github.com/{repo}")),
                provider: Provider::Legacy {
                    category: "skills".into(),
                    selector: name.clone(),
                },
                paths: vec![],
                exact_version_required: false,
            });
        }
    }
    Ok(())
}

pub fn app_row(row: &BTreeMap<String, String>, path: &Path) -> Result<Option<Package>> {
    let bundle = row
        .get("bundle_id")
        .ok_or_else(|| Error::new("invalid_manifest", "bundle_id缺失"))?
        .clone();
    let id = format!("app:{bundle}");
    let name = path
        .file_stem()
        .and_then(|p| p.to_str())
        .ok_or_else(|| Error::new("invalid_app_path", path.display().to_string()))?
        .to_owned();
    Ok(Some(Package {
        id,
        name,
        kind: Kind::App,
        official_url: String::new(),
        provider: Provider::ObservedApp {
            path: path.to_path_buf(),
            bundle_id: bundle,
            historical_version: row.get("version").cloned().unwrap_or_default(),
            owner: row.get("owner").cloned().unwrap_or_default(),
            source: row.get("source").cloned().unwrap_or_default(),
            uninstaller: row.get("uninstaller").cloned(),
        },
        paths: vec![OwnedPath {
            path: path.to_path_buf(),
            kind: PathKind::Payload,
        }],
        exact_version_required: false,
    }))
}

#[cfg(test)]
#[path = "../../tests/unit/catalogue.rs"]
mod tests;
