use crate::{
    context::Context,
    domain::{Scope, ServiceIdentity},
    error::{Error, Result},
    paths,
    policy::service,
};
use std::{
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn current(home: &Path) -> Result<ServiceIdentity> {
    let uid = nix::unistd::geteuid();
    let user = nix::unistd::User::from_uid(uid)
        .map_err(|_| Error::attention("service_identity_unobserved", "无法查询服务用户"))?
        .ok_or_else(|| Error::attention("service_identity_unobserved", "服务用户不存在"))?;
    let identity = ServiceIdentity {
        name: user.name,
        uid: uid.as_raw(),
    };
    service::validate_identity(&identity)?;
    service::home_owner(&identity, std::fs::metadata(paths::read_path(home)?)?.uid())?;
    Ok(identity)
}

pub fn validate(scope: &Scope) -> Result<()> {
    if let Some(expected) = &scope.service_identity {
        service::same_identity(expected, &current(&scope.home)?)?;
    }
    Ok(())
}

pub fn service_context(ctx: &Context) -> Result<Context> {
    if ctx.scope.service_identity.is_some() {
        return Ok(ctx.clone());
    }
    let mut scoped = ctx.clone();
    scoped.scope.service_identity = Some(current(&ctx.home)?);
    let path = if ctx.isolated {
        None
    } else {
        std::env::var_os("HOMEBREW_USER_CONFIG_HOME").map(PathBuf::from)
    };
    if let Some(path) = &path {
        paths::read_path(path)?;
    }
    scoped
        .scope
        .paths
        .insert("HOMEBREW_USER_CONFIG_HOME".into(), path);
    Ok(scoped)
}
