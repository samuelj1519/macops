use crate::{
    context::Context,
    domain::{
        LocalUvToolQuery, Package, Plan, Provider, QuerySpec, UvOwnerSnapshot, UvToolBinding,
        fingerprint,
    },
    error::{Error, Result},
    policy::uv_local as policy,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

fn conflict() -> Error {
    Error::attention("owner_conflict", "同一 uv 安装实例存在多个 owner，未执行")
}
fn migration() -> Error {
    Error::attention(
        "owner_migration_required",
        "uv owner 或 provider 表示迁移需要单独审查，未自动移植",
    )
}
pub fn identity(package: &Package) -> Result<Option<String>> {
    policy::descriptor(&package.provider)
        .map(|(name, binding)| policy::tool_id(&binding.tool_dir, name))
        .transpose()
}
fn same_representation(left: &Provider, right: &Provider) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
        && matches!((policy::descriptor(left),policy::descriptor(right)),(Some((a,x)),Some((b,y))) if policy::normalize_name(a).ok()==policy::normalize_name(b).ok() && x==y)
}
type OwnerMap = BTreeMap<String, Package>;
type OwnerEntries = BTreeMap<String, BTreeSet<String>>;
fn owners(ctx: &Context, identity: &str, name: &str) -> Result<(OwnerMap, OwnerEntries)> {
    let catalog = crate::catalog::inventory::declared_catalog(ctx)?;
    let receipts = crate::catalog::target::receipts(ctx)?;
    let mut packages = BTreeMap::<String, Package>::new();
    let mut entries = BTreeMap::<String, BTreeSet<String>>::new();
    for package in &catalog.packages {
        if self::identity(package)?.as_deref() == Some(identity) {
            entries
                .entry(package.id.clone())
                .or_default()
                .insert(format!("declaration:{}", fingerprint(package)?));
            packages.insert(package.id.clone(), package.clone());
        }
    }
    for receipt in receipts.installations.values() {
        if matches!(&receipt.package.provider,Provider::Uv{package,binding:None} if policy::normalize_name(package).ok().as_deref()==Some(name))
        {
            return Err(migration());
        }
        let Some(QuerySpec::LocalUvTool(query)) = &receipt.query else {
            if self::identity(&receipt.package)?.as_deref() == Some(identity) {
                return Err(migration());
            }
            continue;
        };
        policy::binding(&receipt.package, &QuerySpec::LocalUvTool(query.clone()))?;
        if policy::query_id(query)? != identity {
            continue;
        }
        if let Some(current) = packages.get(&receipt.package.id)
            && (!same_representation(&current.provider, &receipt.package.provider)
                || receipt.scope.as_ref() != Some(&ctx.scope))
        {
            return Err(migration());
        }
        entries
            .entry(receipt.package.id.clone())
            .or_default()
            .insert(format!("receipt:{}", fingerprint(receipt)?));
        packages
            .entry(receipt.package.id.clone())
            .or_insert_with(|| receipt.package.clone());
    }
    if packages.len() > 1 {
        return Err(conflict());
    }
    Ok((packages, entries))
}
pub fn snapshot(ctx: &Context, package: &Package) -> Result<UvOwnerSnapshot> {
    let (name, binding) = policy::descriptor(&package.provider).ok_or_else(migration)?;
    let identity = policy::tool_id(&binding.tool_dir, name)?;
    let (owners, entries) = owners(ctx, &identity, &policy::normalize_name(name)?)?;
    if let Some(owner) = owners.values().next() {
        if owner.id != package.id {
            return Err(conflict());
        }
        if !same_representation(&owner.provider, &package.provider) {
            return Err(migration());
        }
    }
    if let Some(receipt) = crate::catalog::target::receipts(ctx)?
        .installations
        .get(&package.id)
        && (receipt.scope.as_ref() != Some(&ctx.scope)
            || self::identity(&receipt.package)?.as_deref() != Some(&identity))
    {
        return Err(migration());
    }
    Ok(UvOwnerSnapshot {
        selected_owner: package.id.clone(),
        entries,
    })
}
pub fn check(ctx: &Context, plan: &Plan, committed: Option<&str>) -> Result<()> {
    let Some(frozen) = &plan.uv_owner else {
        return Err(migration());
    };
    let mut current = snapshot(ctx, &plan.package)?;
    let mut expected = frozen.clone();
    if let Some(operation) = committed {
        let receipts = crate::catalog::target::receipts(ctx)?;
        let receipt = receipts
            .installations
            .get(&plan.package.id)
            .ok_or_else(migration)?;
        if receipt.operation_id != operation
            || receipt.query != plan.query
            || receipt.observation != plan.before
            || fingerprint(&receipt.package)? != plan.recipe_sha256
            || receipt.scope.as_ref() != Some(&ctx.scope)
        {
            return Err(migration());
        }
        for snapshot in [&mut expected, &mut current] {
            snapshot.entries.retain(|_, tokens| {
                tokens.retain(|s| !s.starts_with("receipt:"));
                !tokens.is_empty()
            });
        }
    }
    if expected != current {
        return Err(Error::attention(
            "uv_owner_changed",
            "uv owner 集合在计划后改变，未提交",
        ));
    }
    Ok(())
}
pub fn declared(ctx: &Context, package: &Package) -> Result<Package> {
    if policy::descriptor(&package.provider).is_some() {
        snapshot(ctx, package)?;
    }
    Ok(package.clone())
}
pub fn resource(ctx: &Context, item: &crate::domain::resource::Resource) -> Result<Package> {
    let crate::domain::resource::Data::Application {
        native_id, scope, ..
    } = &item.data
    else {
        return Err(conflict());
    };
    let identity = policy::tool_id(Path::new(scope), native_id)?;
    let (owners, _) = owners(ctx, &identity, native_id)?;
    if let Some(owner) = owners.into_values().next() {
        return declared(ctx, &owner);
    }
    if item.status != crate::domain::resource::State::Present {
        return Err(Error::attention(
            "uv_observation_required",
            "uv 需要稳定的完整本地观察",
        ));
    }
    let (bindings, _) = crate::software::uv_local::bindings(ctx)?;
    let choices: Vec<_> = bindings
        .into_iter()
        .filter(|b| b.tool_dir == Path::new(scope))
        .collect();
    let [binding] = choices.as_slice() else {
        return Err(Error::attention(
            "uv_namespace_conflict",
            "uv binDir 绑定不唯一",
        ));
    };
    let package = Package {
        id: identity,
        name: native_id.clone(),
        kind: crate::domain::Kind::Cli,
        official_url: String::new(),
        provider: Provider::UvObserved {
            package: native_id.clone(),
            binding: binding.clone(),
        },
        paths: vec![],
        exact_version_required: false,
    };
    snapshot(ctx, &package)?;
    Ok(package)
}
pub fn refresh(ctx: &Context, planned: &Package) -> Result<Package> {
    snapshot(ctx, planned)?;
    if let Some(package) = crate::catalog::inventory::declared_catalog(ctx)?
        .packages
        .into_iter()
        .find(|p| p.id == planned.id)
    {
        return Ok(package);
    }
    Ok(planned.clone())
}
pub fn query_binding(query: &LocalUvToolQuery) -> UvToolBinding {
    UvToolBinding {
        tool_dir: query.namespace.tool_dir.path.clone(),
        bin_dir: query.namespace.bin_dir.path.clone(),
    }
}

pub fn annotate(
    ctx: &Context,
    items: &mut [crate::domain::resource::Resource],
    reports: &mut Vec<crate::domain::resource::Coverage>,
) -> Result<()> {
    use crate::domain::resource::{Coverage, CoverageState, Data, Management, State};
    let receipts = crate::catalog::target::receipts(ctx)?;
    for item in items {
        let Data::Application { native_id, .. } = &item.data else {
            continue;
        };
        match owners(ctx, &item.id, native_id) {
            Ok((owners, _)) => {
                if let Some(owner) = owners.into_values().next() {
                    if let Data::Application { recipe_ref, .. } = &mut item.data {
                        *recipe_ref = Some(owner.id.clone());
                    }
                    if receipts.installations.contains_key(&owner.id) {
                        item.management = Management::Adopted;
                    }
                }
            }
            Err(error) => {
                item.status = State::Unknown;
                if let Data::Application { issues, .. } = &mut item.data {
                    issues.push(error.code.clone());
                }
                reports.push(Coverage {
                    source: "uv-owner-mapping".into(),
                    state: CoverageState::Invalid,
                    count: 0,
                    roots: vec![ctx.catalog.clone()],
                    issues: vec![error.code],
                });
            }
        }
    }
    Ok(())
}
