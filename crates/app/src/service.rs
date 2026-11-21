use crate::{
    context::Context,
    domain::{Action, Observation, Plan, fingerprint},
    error::{Error, Result},
    identity, inventory, operations, paths, policy, providers,
};
use serde_json::{Value, json};

pub fn control(
    ctx: &Context,
    name: &str,
    action: Action,
    dry_run: bool,
    yes: bool,
) -> Result<Value> {
    if name != "postgresql" {
        return Err(Error::new("undeclared_service", name));
    }
    let scoped = identity::service_context(ctx)?;
    let package = policy::service::package();
    let catalog = inventory::load(&scoped)?;
    if fingerprint(catalog.select(&package.id)?)? != fingerprint(&package)? {
        return Err(Error::attention(
            "service_source_changed",
            "声明的服务来源已被替换",
        ));
    }
    let before = inventory::observe(&scoped, &package);
    if let Observation::Unknown { .. } = &before {
        return Err(Error::attention(
            "service_unobserved",
            "当前服务状态或归属无法确认，未执行",
        ));
    }
    if policy::service::expected(action, &before) {
        return Ok(
            json!({"backend":"rust","mode":if dry_run {"dry-run"} else {"no-op"},"service":name,"observation":before,"changed":false,"persistentWrites":false}),
        );
    }
    let command = providers::mutation_spec(&scoped, &package, action, None)?;
    if scoped.isolated && !paths::read_path(&command.program)?.starts_with(&scoped.home) {
        return Err(Error::new(
            "global_write_in_fixture",
            "fixture 服务控制只能运行自己的假管理器",
        ));
    }
    let query = providers::query_spec(&scoped, &package)?;
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action,
        before,
        target: None,
        command: Some(command),
        query: Some(query),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    operations::execute(&scoped, plan, dry_run, yes)
}
