use crate::{
    context::Context,
    domain::{Catalog, CommandSpec, Observation, Package, Provider},
    error::{Error, Result},
    platform::fs as paths,
    policy::{
        catalog::catalogue,
        operation::{commands, parsers},
    },
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn declared_catalog(ctx: &Context) -> Result<Catalog> {
    let catalog = if ctx.catalog.exists() || ctx.catalog_explicit {
        let bytes = paths::regular_bytes(&ctx.catalog)?;
        let value = crate::policy::json::parse(&bytes)?;
        let crate::policy::json::Object(catalog) =
            serde_json::from_value::<crate::policy::json::Object<Catalog>>(value)?;
        catalog
    } else {
        Catalog {
            schema_version: 1,
            packages: vec![],
        }
    };
    catalog.validate()?;
    Ok(catalog)
}

pub fn load(ctx: &Context) -> Result<Catalog> {
    crate::operations::governance::read(ctx, |authority| load_with_authority(ctx, authority))
}

fn load_with_authority(
    ctx: &Context,
    authority: &crate::operations::governance::Governance,
) -> Result<Catalog> {
    let mut catalog = declared_catalog(ctx)?;
    let receipts = ctx.state.join("receipts.json");
    if paths::read_path(&receipts)?.exists() {
        let state = crate::catalog::target::receipts(ctx)?;
        catalogue::merge_receipts(&mut catalog, state)?;
    }
    import_apps(ctx, authority, &mut catalog)?;
    add_ai(&mut catalog);
    if let Some(bytes) = authority.read(
        crate::policy::governance::Dataset::AssetDefinitions,
        1024 * 1024,
    )? {
        let value = crate::policy::json::parse(&bytes)?;
        catalogue::add_assets(&mut catalog, &value)?;
    }
    add_managers(&ctx.cache, &mut catalog);
    catalog.validate()?;
    Ok(catalog)
}

fn app_rows_with(
    authority: &crate::operations::governance::Governance,
) -> Result<Vec<BTreeMap<String, String>>> {
    let Some(bytes) =
        authority.read(crate::policy::governance::Dataset::MacApps, 8 * 1024 * 1024)?
    else {
        return Ok(vec![]);
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::new("invalid_manifest", "应用清单必须为 UTF-8"))?;
    parsers::tsv(&text, &["path", "bundle_id", "version", "owner", "source"])
}

pub fn app_rows(ctx: &Context) -> Result<Vec<BTreeMap<String, String>>> {
    crate::operations::governance::read(ctx, app_rows_with)
}

pub fn observe(ctx: &Context, p: &Package) -> Observation {
    match observe_result(ctx, p) {
        Ok(v) => v,
        Err(e) => Observation::Unknown {
            reason: e.to_string(),
        },
    }
}

fn observe_result(ctx: &Context, p: &Package) -> Result<Observation> {
    match &p.provider {
        Provider::ObservedApp {
            path, bundle_id, ..
        } => {
            let path = paths::read_path(path)?;
            if !path.exists() {
                return Ok(Observation::Missing { evidence: None });
            }
            let file = paths::read_path(&path.join("Contents/Info.plist"))?;
            let value =
                plist::Value::from_reader(std::io::Cursor::new(paths::regular_bytes(&file)?))
                    .map_err(|e| Error::new("invalid_plist", e.to_string()))?;
            parsers::app_plist(&value, bundle_id, &p.id)
        }
        Provider::Legacy { category, selector } if category == "skills" => {
            crate::operations::governance::read(ctx, |authority| {
                let Some(bytes) = authority.read(
                    crate::policy::governance::Dataset::ManagedAssets,
                    16 * 1024 * 1024,
                )?
                else {
                    return Ok(Observation::Unknown {
                        reason: "未找到治理安装记录；使用 skills list 检查原生来源".into(),
                    });
                };
                let value = crate::policy::json::parse(&bytes)?;
                Ok(parsers::managed_asset(&value, selector))
            })
        }
        Provider::Manager { executable }
        | Provider::Legacy {
            selector: executable,
            ..
        } => {
            let name = if executable == "cursor" {
                "cursor-agent"
            } else {
                executable
            };
            let program = match crate::operations::command::executable(name) {
                Ok(p) => p,
                Err(e) if e.code == "manager_missing" => {
                    return Ok(Observation::Missing { evidence: None });
                }
                Err(e) => return Err(e),
            };
            let output = crate::operations::command::query(
                &ctx.scope,
                &CommandSpec {
                    program,
                    args: vec![if name == "mimo" { "-v" } else { "--version" }.into()],
                },
            )?;
            parsers::manager_observation(&output)
        }
        Provider::Macops => {
            crate::catalog::cache::check_owned(ctx)?;
            Ok(Observation::Present {
                evidence: None,
                version: env!("CARGO_PKG_VERSION").into(),
            })
        }
        _ => crate::operations::providers::observe_native(ctx, p),
    }
}

pub use crate::policy::operation::parsers::parse_service;

pub fn discover(ctx: &Context, manager: &str) -> Result<Value> {
    if manager == "brew" {
        return crate::software::brew::discover(ctx);
    }
    if manager == "uv" {
        let (resources, coverage) = crate::software::uv_local::inventory(ctx)?;
        return Ok(
            json!({"manager":"uv","packages":resources,"coverage":coverage,"adopted":false}),
        );
    }
    if manager == "npm" {
        let (packages, coverage, namespaces) = crate::software::npm_local::inventory(ctx)?;
        let incomplete = !matches!(
            coverage.state,
            crate::domain::resource::CoverageState::Complete
                | crate::domain::resource::CoverageState::Absent
        );
        return Ok(
            json!({"manager":"npm","namespaces":namespaces,"packages":packages,"coverage":coverage,"incomplete":incomplete}),
        );
    }
    let command = crate::operations::providers::resolve(commands::discovery_query(manager)?)?;
    let out = crate::operations::command::query(&ctx.scope, &command)?;
    if manager == "bun" && crate::operations::providers::empty_bun_scope(&ctx.scope, &out)? {
        return Ok(json!({"manager":manager,"status":"empty","data":[],"adopted":false}));
    }
    let data = parsers::discovery(manager, &out)?;
    Ok(json!({"manager":manager,"command":command,"data":data,"adopted":false}))
}

fn import_apps(
    ctx: &Context,
    authority: &crate::operations::governance::Governance,
    catalog: &mut Catalog,
) -> Result<()> {
    for row in app_rows_with(authority)? {
        let Some(path) = crate::policy::paths::legacy_app_path(
            &row["path"],
            row.get("scope").map(String::as_str),
            &ctx.home,
        )?
        else {
            continue;
        };
        paths::read_path(&path)?;
        if let Some(package) = catalogue::app_row(&row, &path)?
            && !catalog.packages.iter().any(|p| p.id == package.id)
        {
            catalog.packages.push(package);
        }
    }
    Ok(())
}

use crate::policy::catalog::catalogue::{add_ai, add_managers};
