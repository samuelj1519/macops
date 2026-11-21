use crate::{
    context::Context,
    domain::{Action, CommandSpec, Observation, Package, Provider, QuerySpec},
    error::Result,
    paths, process,
};

pub fn observe_native(ctx: &Context, p: &Package) -> Result<Observation> {
    if matches!(
        p.provider,
        Provider::ConfigObserved {
            authority: crate::domain::ConfigAuthority::ChezmoiNative,
            ..
        }
    ) {
        return Ok(Observation::Unknown {
            reason: "native_metadata_cached_not_live".into(),
        });
    }
    if matches!(p.provider, Provider::Brew { cask: false, .. }) {
        return Ok(Observation::Unknown {
            reason: "brew_context_binding_required".into(),
        });
    }
    let scoped;
    let ctx = if matches!(p.provider, Provider::Service { .. }) {
        scoped = crate::identity::service_context(ctx)?;
        &scoped
    } else {
        ctx
    };
    if matches!(p.provider, Provider::Uv { binding: None, .. }) {
        return Ok(Observation::Unknown {
            reason: "legacy_uv_query_unbound".into(),
        });
    }
    if matches!(
        p.provider,
        Provider::Uv { .. }
            | Provider::UvObserved { .. }
            | Provider::Npm { .. }
            | Provider::NpmObserved { .. }
            | Provider::ConfigObserved { .. }
            | Provider::CapabilityObserved { .. }
    ) && let Some(record) = crate::target::receipts(ctx)?.installations.remove(&p.id)
        && let Some(
            query @ (QuerySpec::LocalUvTool(_)
            | QuerySpec::LocalNpm(_)
            | QuerySpec::LocalConfig(_)
            | QuerySpec::LocalCapability(_)),
        ) = record.query
    {
        return observe_query(p, &query, &ctx.scope, false);
    }
    let spec = query_spec(ctx, p)?;
    observe_query(p, &spec, &ctx.scope, false)
}

pub fn observe_query(
    p: &Package,
    query: &QuerySpec,
    scope: &crate::domain::Scope,
    post: bool,
) -> Result<Observation> {
    use crate::domain::NpmContract;
    if matches!(query, QuerySpec::NativeConfig(_)) {
        return Ok(Observation::Unknown {
            reason: "native_explicit_query_required".into(),
        });
    }
    if matches!(query, QuerySpec::BrewFormula(_)) {
        return Ok(Observation::Unknown {
            reason: "brew_native_runtime_not_ready".into(),
        });
    }
    if matches!(p.provider, Provider::Uv { binding: None, .. }) {
        return Ok(Observation::Unknown {
            reason: "legacy_uv_query_unbound".into(),
        });
    }
    crate::policy::uv_local::binding(p, query)?;
    if let QuerySpec::LocalUvTool(query) = query {
        return crate::uv_local::observe(query, &mut crate::uv_local::TOTAL_LIMIT.clone());
    }
    crate::policy::npm_local::binding(p, query)?;
    crate::policy::config_local::binding(p, query)?;
    crate::policy::capability_local::binding(p, query)?;
    if let QuerySpec::LocalNpm(query) = query {
        return crate::npm_local::observe_scoped(Some(scope), query, post, &mut (1024 * 1024));
    }
    if let QuerySpec::LocalConfig(query) = query {
        return crate::config_local::observe(query, &mut (2 * 1024 * 1024));
    }
    if let QuerySpec::LocalCapability(query) = query {
        return crate::capability_local::observe_default(&p.id, query);
    }
    if let Provider::Npm {
        prefix, package, ..
    } = &p.provider
    {
        let local = crate::npm_local::capture(prefix, package, NpmContract::DeclaredV1, false)?;
        if local.namespace.canonical_prefix != *prefix {
            return Err(crate::error::Error::attention(
                "legacy_npm_prefix_unbound",
                "旧记录无法确认链接目录的原身份",
            ));
        }
        let observation = crate::npm_local::observe(&local, post, &mut (1024 * 1024))?;
        return match observation {
            Observation::Present { version, .. } => Ok(Observation::Present {
                version,
                evidence: None,
            }),
            _ => Err(crate::error::Error::attention(
                "legacy_npm_absence_unverified",
                "旧记录无法证明该目录中的卸载结果",
            )),
        };
    }
    if let Some(target) = crate::policy::targets::descriptor(p)? {
        if !matches!((&p.provider,query), (Provider::Command {query:old,..}, QuerySpec::Command(current)) if old==current)
        {
            return Err(crate::error::Error::new(
                "invalid_observer_recipe",
                "旧观察查询不匹配",
            ));
        }
        return crate::target::probe(&target.npm, &target.prefix, &target.name);
    }
    if let QuerySpec::Mise(query) = query {
        if !matches!(p.provider, Provider::Mise { .. }) {
            return Err(crate::error::Error::new(
                "invalid_mise_query",
                "查询与 mise 配方不匹配",
            ));
        }
        let mut command = crate::mise_owner::query_command(scope, query)?;
        let output = process::capture_query(&mut command)?;
        if let Some(retirement) = &query.retirement {
            return crate::mise_owner::retirement::observe(&output, scope, query, retirement, post);
        }
        return observe_output(p, &output, scope);
    }
    let command = query
        .command()
        .ok_or_else(|| crate::error::Error::new("invalid_query", "缺少原生命令"))?;
    let output = process::query(scope, command)?;
    observe_output(p, &output, scope)
}

pub fn observe_output(
    p: &Package,
    output: &process::Output,
    scope: &crate::domain::Scope,
) -> Result<Observation> {
    if let Provider::Service { formula } = &p.provider {
        let identity = scope.service_identity.as_ref().ok_or_else(|| {
            crate::error::Error::new("invalid_service_scope", "服务查询缺少冻结身份")
        })?;
        return crate::policy::service::observation(formula, output, identity);
    }
    if matches!(p.provider, Provider::Bun { .. }) && empty_bun_scope(scope, output)? {
        return Ok(Observation::Missing { evidence: None });
    }
    parse_observation(p, output)
}

pub fn empty_bun_scope(scope: &crate::domain::Scope, output: &process::Output) -> Result<bool> {
    let Some(root) = crate::policy::paths::empty_bun_root(scope, output) else {
        return Ok(false);
    };
    let manifest = paths::read_path(&root.join("package.json"))?;
    let payload = paths::read_path(&root.join("node_modules"))?;
    Ok(!manifest.exists() && !payload.exists())
}

pub use crate::policy::parsers::{parse_observation, synchronous};

pub fn resolve(template: crate::policy::commands::Template) -> Result<CommandSpec> {
    match template {
        crate::policy::commands::Template::Exact(spec) => Ok(spec),
        crate::policy::commands::Template::Named { name, args } => Ok(CommandSpec {
            program: process::executable(&name)?,
            args,
        }),
    }
}
pub fn mutation_spec(
    ctx: &Context,
    p: &Package,
    action: Action,
    version: Option<&str>,
) -> Result<CommandSpec> {
    if let Provider::Npm { prefix, .. } = &p.provider {
        paths::read_path(prefix)?;
    }
    let mise_owned = if matches!(p.provider, Provider::Mise { .. })
        && matches!(action, Action::Install | Action::Upgrade)
    {
        crate::mise_owner::declared_owner(ctx)?
    } else {
        false
    };
    resolve(crate::policy::commands::mutation(
        &ctx.home, mise_owned, p, action, version,
    )?)
}
pub fn query_spec(ctx: &Context, p: &Package) -> Result<QuerySpec> {
    capture_query(ctx, p, None)
}

pub fn capture_query(ctx: &Context, p: &Package, action: Option<Action>) -> Result<QuerySpec> {
    use crate::domain::NpmContract;
    if crate::policy::uv_local::descriptor(&p.provider).is_some()
        && let Some(receipt) = crate::target::receipts(ctx)?.installations.remove(&p.id)
    {
        let Some(query @ QuerySpec::LocalUvTool(_)) = receipt.query else {
            return Err(crate::error::Error::attention(
                "owner_migration_required",
                "uv 旧收据没有冻结本地 namespace，不能重新绑定",
            ));
        };
        crate::policy::uv_local::binding(p, &query)?;
        return Ok(query);
    }
    let query = match &p.provider {
        Provider::Uv {
            package,
            binding: Some(binding),
        }
        | Provider::UvObserved { package, binding } => {
            QuerySpec::LocalUvTool(crate::uv_local::capture(binding, package)?)
        }
        Provider::Uv { binding: None, .. } => {
            return Err(crate::error::Error::attention(
                "legacy_uv_query_unbound",
                "旧 uv 声明没有明确绑定，只读展示，不调用原生查询",
            ));
        }
        Provider::Npm {
            prefix,
            package,
            authority,
        } => QuerySpec::LocalNpm(crate::npm_local::capture_with_configs(
            authority.as_ref().map(|_| ctx.home.as_path()),
            prefix,
            package,
            NpmContract::DeclaredV1,
            action == Some(Action::Install),
        )?),
        Provider::NpmObserved { prefix, name } => QuerySpec::LocalNpm(crate::npm_local::capture(
            prefix,
            name,
            NpmContract::NativeV1,
            false,
        )?),
        Provider::Mise { .. } => QuerySpec::Mise(crate::mise_owner::plan_query(
            ctx,
            p,
            resolve(crate::policy::commands::query(&ctx.home, p)?)?,
            action,
        )?),
        Provider::ConfigObserved { .. } => QuerySpec::LocalConfig(crate::config_local::capture(p)?),
        Provider::CapabilityObserved { .. } => {
            QuerySpec::LocalCapability(crate::capability_local::capture(p)?)
        }
        _ => resolve(crate::policy::commands::query(&ctx.home, p)?).map(QuerySpec::Command)?,
    };
    crate::policy::uv_local::binding(p, &query)?;
    crate::policy::npm_local::binding(p, &query)?;
    crate::policy::config_local::binding(p, &query)?;
    crate::policy::capability_local::binding(p, &query)?;
    Ok(query)
}
