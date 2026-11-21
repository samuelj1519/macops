use crate::{
    context::Context,
    domain::{Kind, NpmContract, Observation, Package, Provider, Receipts},
    error::{Error, Result},
    inventory, paths,
    policy::targets,
};
use std::{collections::BTreeMap, path::Path};

pub fn resolve(ctx: &Context, key: &str) -> Result<Package> {
    let catalog = inventory::load(ctx)?;
    let has_legacy_skill = catalog.packages.iter().any(|p| {
        p.id == key
            && matches!(
                &p.provider,
                Provider::Legacy { category, .. } if category == "skills"
            )
    });
    if key.starts_with("capability:") || key.starts_with("plugin:") {
        return crate::capability_local::resolve_package(ctx, key, false)?
            .ok_or_else(|| Error::attention("not_found", "没有发现对应对象"));
    }
    if key.starts_with("skill:")
        && let Some(pkg) = crate::capability_local::resolve_package(ctx, key, has_legacy_skill)?
    {
        return Ok(pkg);
    }
    if let Some(p) = catalog.packages.iter().find(|p| {
        p.id == key
            && !matches!(
                p.provider,
                Provider::ConfigObserved { .. } | Provider::CapabilityObserved { .. }
            )
    }) {
        if crate::policy::uv_local::is_uv(&p.provider) {
            return crate::uv_ownership::declared(ctx, p);
        }
        return Ok(p.clone());
    }
    if key.starts_with("config:") {
        return crate::chezmoi_native::public_package(ctx, key)?
            .ok_or_else(|| Error::attention("not_found", "没有发现对应对象"));
    }
    let raw_path = Path::new(key);
    if raw_path.is_absolute()
        && !raw_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
    {
        match crate::chezmoi_native::public_package(ctx, key) {
            Ok(Some(pkg)) => return Ok(pkg),
            Ok(None) => (),
            Err(error) if error.code == "not_found" => (),
            Err(error) => return Err(error),
        }
    }
    let snapshot = crate::resource_inventory::snapshot(ctx, Some("applications"), None)?;
    let resource = snapshot.select(key)?;
    let crate::resource::Data::Application {
        manager,
        native_id,
        scope,
        recipe_ref,
        issues,
        ..
    } = &resource.data
    else {
        return Err(Error::new("unsupported_target", "目标不是软件实例"));
    };
    if manager == "uv" {
        return crate::uv_ownership::resource(ctx, resource);
    }
    if manager == "npm"
        && resource.status == crate::resource::State::Unknown
        && recipe_ref.is_none()
    {
        return Err(if issues.is_empty() {
            Error::attention("owner_conflict", "目标声明存在冲突，需要明确审查owner")
        } else {
            Error::attention("observation_required", "npm 安装记录无法确认")
        });
    }
    if let Some(recipe) = recipe_ref {
        return Ok(catalog.select(recipe)?.clone());
    }
    if manager != "npm" {
        return Err(Error::new(
            "unsupported_target",
            "该发现来源的变更适配尚未完成",
        ));
    }
    observed_npm(ctx, scope, native_id)
}

fn observed_npm(ctx: &Context, scope: &str, native_id: &str) -> Result<Package> {
    let prefix = paths::read_path(Path::new(scope))?;
    if !crate::policy::parsers::registry_name(native_id) {
        return Err(Error::new("invalid_native_package", "npm名称无效"));
    }
    let identity = targets::npm_id(&prefix, native_id)?;
    if let Some(owner) = owner(ctx, &identity)? {
        return Ok(owner);
    }
    Ok(Package {
        id: identity,
        name: native_id.into(),
        kind: Kind::Cli,
        official_url: "https://docs.npmjs.com/cli/v11/configuring-npm/folders".into(),
        provider: Provider::NpmObserved {
            prefix,
            name: native_id.into(),
        },
        paths: vec![],
        exact_version_required: false,
    })
}

pub fn refresh(ctx: &Context, planned: &Package) -> Result<Package> {
    if crate::policy::uv_local::is_uv(&planned.provider) {
        return crate::uv_ownership::refresh(ctx, planned);
    }
    if matches!(planned.provider, Provider::ConfigObserved { .. }) {
        return crate::config_local::refresh_package(ctx, planned);
    }
    if matches!(planned.provider, Provider::CapabilityObserved { .. }) {
        return crate::capability_local::refresh_package(ctx, planned);
    }
    let catalog = inventory::load(ctx)?;
    if let Some(identity) = canonical(planned)? {
        let current: Vec<_> = catalog
            .packages
            .iter()
            .filter_map(|p| match canonical(p) {
                Ok(Some(id)) if id == identity => Some(Ok(p)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<_>>()?;
        match current.as_slice() {
            [one] if one.id == planned.id => return Ok((*one).clone()),
            [] if targets::observer(planned)? => return Ok(planned.clone()),
            _ => {
                return Err(Error::attention(
                    "plan_stale",
                    "目标的管理意图在计划后发生变化，未执行",
                ));
            }
        }
    }
    if let Some(p) = catalog.packages.iter().find(|p| p.id == planned.id) {
        return Ok(p.clone());
    }
    Err(Error::attention("plan_stale", "目标声明已消失，未执行"))
}

pub fn canonical(p: &Package) -> Result<Option<String>> {
    if let Provider::BrewFormula { query, .. } = &p.provider {
        return crate::policy::brew::logical_id(&query.key).map(Some);
    }
    if crate::policy::uv_local::is_uv(&p.provider) {
        return crate::uv_ownership::identity(p);
    }
    if matches!(p.provider, Provider::CapabilityObserved { .. }) {
        return Ok(Some(crate::capability_local::canonical_identity(p)?));
    }
    if let Provider::ConfigObserved { target, .. } = &p.provider {
        return Ok(Some(crate::config_local::canonical_identity(target)?));
    }
    if let Provider::Npm {
        package, prefix, ..
    } = &p.provider
    {
        return Ok(Some(targets::npm_id(&paths::read_path(prefix)?, package)?));
    }
    if let Provider::NpmObserved { prefix, name } = &p.provider {
        return Ok(Some(targets::npm_id(&paths::read_path(prefix)?, name)?));
    }
    if let Some(target) = targets::descriptor(p)? {
        return Ok(Some(targets::npm_id(
            &paths::read_path(&target.prefix)?,
            &target.name,
        )?));
    }
    Ok(None)
}

pub fn receipts(ctx: &Context) -> Result<Receipts> {
    let path = ctx.state.join("receipts.json");
    if !paths::read_path(&path)?.exists() {
        return Ok(Receipts {
            schema_version: 1,
            installations: BTreeMap::new(),
        });
    }
    let receipts: Receipts = paths::regular_json(&path)?;
    crate::policy::state::receipt_schema(&receipts)?;
    Ok(receipts)
}

fn receipt_identity(receipt: &crate::domain::Receipt) -> Result<Option<String>> {
    if let Some(query @ crate::domain::QuerySpec::LocalUvTool(local)) = &receipt.query {
        crate::policy::uv_local::binding(&receipt.package, query)?;
        return crate::policy::uv_local::query_id(local).map(Some);
    }
    if let Some(query @ crate::domain::QuerySpec::LocalCapability(local)) = &receipt.query {
        crate::policy::capability_local::binding(&receipt.package, query)?;
        return Ok(Some(crate::capability_local::query_identity(
            &receipt.package,
            local,
        )?));
    }
    if let Some(query @ crate::domain::QuerySpec::LocalConfig(local)) = &receipt.query {
        crate::policy::config_local::binding(&receipt.package, query)?;
        return Ok(Some(crate::policy::config_local::config_id(
            &local.target.canonical_parent.join(&local.target.name),
        )?));
    }
    if let Some(query @ crate::domain::QuerySpec::LocalNpm(local)) = &receipt.query {
        crate::policy::npm_local::binding(&receipt.package, query)?;
        return Ok(Some(targets::npm_id(
            &local.namespace.canonical_prefix,
            &local.name,
        )?));
    }
    canonical(&receipt.package)
}

fn owner(ctx: &Context, identity: &str) -> Result<Option<Package>> {
    let owners: Vec<_> = receipts(ctx)?
        .installations
        .into_values()
        .filter_map(|r| match receipt_identity(&r) {
            Ok(Some(id)) if id == identity => Some(Ok(r.package)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    match owners.as_slice() {
        [one] => Ok(Some(one.clone())),
        [] => Ok(None),
        _ => Err(Error::attention(
            "owner_conflict",
            "同一安装实例已有多个管理记录，需要先审查",
        )),
    }
}

pub fn check_owner(ctx: &Context, p: &Package) -> Result<()> {
    if crate::policy::uv_local::is_uv(&p.provider) {
        return crate::uv_ownership::snapshot(ctx, p).map(|_| ());
    }
    if let Some(receipt) = receipts(ctx)?.installations.get(&p.id)
        && matches!(
            receipt.query,
            Some(
                crate::domain::QuerySpec::LocalNpm(_)
                    | crate::domain::QuerySpec::LocalConfig(_)
                    | crate::domain::QuerySpec::LocalCapability(_)
            )
        )
        && receipt_identity(receipt)? != canonical(p)?
    {
        return Err(Error::attention(
            "manager_scope_changed",
            "已登记的对象根或父目录身份已改变",
        ));
    }
    if let Some(identity) = canonical(p)?
        && let Some(owner) = owner(ctx, &identity)?
        && owner.id != p.id
    {
        return Err(Error::attention(
            "owner_conflict",
            "同一安装实例已有另一管理记录，未建立第二owner",
        ));
    }
    Ok(())
}

pub fn probe(npm: &Path, prefix: &Path, name: &str) -> Result<Observation> {
    crate::policy::paths::lexical(npm)?;
    let query = crate::npm_local::capture(prefix, name, NpmContract::NativeV1, false)?;
    crate::npm_local::observe(&query, false, &mut (1024 * 1024))
}

pub fn diagnose(ctx: &Context, packages: &[&Package]) -> Result<serde_json::Value> {
    let receipts = receipts(ctx)?;
    let mut rows = vec![];
    for package in packages {
        let current = inventory::observe(ctx, package);
        let mut row = serde_json::json!({"id":package.id,"observation":current});
        if targets::observer(package)?
            && let Some(receipt) = receipts.installations.get(&package.id)
        {
            row["baseline"] =
                serde_json::to_value(targets::baseline(&receipt.observation, current))?;
        }
        rows.push(row);
    }
    Ok(serde_json::Value::Array(rows))
}
