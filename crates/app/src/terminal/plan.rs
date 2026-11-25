use crate::{context::Context, domain::Provider, policy::public_source::domain_link};

pub(crate) fn source_label(value: &str) -> &str {
    let public = value.split(['?', '#']).next().unwrap_or_default();
    if domain_link(public) {
        public
    } else {
        "待核对"
    }
}

pub(crate) fn plan_scope(ctx: &Context, plan: &crate::domain::Plan) -> String {
    use crate::{domain::QuerySpec, output::sanitize};
    let safe = |s: &str| sanitize(s, false);
    let mut lines = vec![
        format!("实例 {}", safe(&plan.package.id)),
        format!("来源 {}", source_label(&plan.package.official_url)),
    ];
    if let Some(QuerySpec::LocalNpm(query)) = &plan.query {
        lines.push(format!(
            "安装范围 {}",
            safe(&query.namespace.canonical_prefix.display().to_string())
        ));
    }
    if let Provider::Mise { package } = &plan.package.provider {
        let scope = match &plan.query {
            Some(QuerySpec::Mise(query)) => query.namespace.data.clone(),
            _ => crate::software::resource_software::mise_scope(ctx),
        };
        lines.push(format!(
            "mise 工具 {}\n安装范围 {}",
            safe(package),
            safe(&scope.display().to_string())
        ));
        if matches!(
            plan.action,
            crate::domain::Action::Install | crate::domain::Action::Upgrade
        ) {
            lines.push("安装并更新用户级配置中的版本".into());
        }
    }
    if let Some(release) = crate::software::mise_owner::retirement::bound_query(plan) {
        lines.push(format!(
            "移除版本 {} · {}",
            safe(&release.version),
            safe(&release.payload.display().to_string())
        ));
        lines.push(format!(
            "查询目录 {} · 此处未启用，其他项目仍可能使用此版本",
            safe(&ctx.home.display().to_string())
        ));
    }
    if let Provider::Npm {
        package, authority, ..
    } = &plan.package.provider
    {
        lines.push(format!("npm 包 {}", safe(package)));
        if let Some(registry) = authority.as_ref().and_then(|auth| auth.registry.as_ref()) {
            lines.push(format!("registry {}", source_label(&registry.url)));
            if let Some(scope_url) = &registry.scope_url {
                lines.push(format!("作用域 registry {}", source_label(scope_url)));
            }
            lines.push(format!(
                "安装脚本 {}",
                if registry.allow_scripts {
                    "允许"
                } else {
                    "禁止"
                }
            ));
        }
    }
    lines.extend(plan.warnings.iter().map(|warning| safe(warning)));
    lines.join("\n")
}
