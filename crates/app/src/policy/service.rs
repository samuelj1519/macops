use crate::{
    domain::{
        Action, Journal, Kind, Observation, Package, ProcessOutput, Provider, ServiceIdentity,
    },
    error::{Error, Result},
};
use serde::Deserialize;

pub fn package() -> Package {
    Package {
        id: "service:postgresql".into(),
        name: "postgresql".into(),
        kind: Kind::Service,
        official_url: "https://docs.brew.sh/Manpage#services-subcommand".into(),
        provider: Provider::Service {
            formula: "postgresql@16".into(),
        },
        paths: vec![],
        exact_version_required: false,
    }
}

pub fn is_control(action: Action) -> bool {
    matches!(action, Action::Start | Action::Stop)
}

pub fn validate_identity(identity: &ServiceIdentity) -> Result<()> {
    if identity.uid == 0 || identity.name.is_empty() || identity.name == "root" {
        return Err(Error::attention(
            "service_identity_unsupported",
            "仅支持可确认身份的普通用户服务",
        ));
    }
    Ok(())
}

pub fn home_owner(identity: &ServiceIdentity, owner: u32) -> Result<()> {
    if identity.uid != owner {
        return Err(Error::attention(
            "service_home_owner_mismatch",
            "HOME 不属于目标服务用户",
        ));
    }
    Ok(())
}

pub fn same_identity(expected: &ServiceIdentity, actual: &ServiceIdentity) -> Result<()> {
    if expected != actual {
        return Err(Error::attention(
            "service_identity_changed",
            "服务执行与恢复必须使用原用户身份",
        ));
    }
    Ok(())
}

pub fn expected(action: Action, observation: &Observation) -> bool {
    let wanted = match action {
        Action::Start => true,
        Action::Stop => false,
        _ => return false,
    };
    matches!(observation, Observation::Service {running,loaded,registered} if *running==wanted && *loaded==wanted && *registered==wanted)
}

#[derive(Deserialize)]
struct RawService {
    name: String,
    running: bool,
    loaded: bool,
    registered: bool,
    user: Option<String>,
}

pub fn observation(
    formula: &str,
    output: &ProcessOutput,
    identity: &ServiceIdentity,
) -> Result<Observation> {
    if output.uncertain || output.code != Some(0) {
        return Err(Error::attention("service_unobserved", "服务状态查询未完成"));
    }
    let mut rows: Vec<RawService> = serde_json::from_str(&output.stdout)
        .map_err(|_| Error::attention("service_unobserved", "服务状态格式或类型不完整"))?;
    if rows.len() != 1 || rows[0].name != formula {
        return Err(Error::attention(
            "service_identity_mismatch",
            "服务查询对象不匹配",
        ));
    }
    let row = rows.remove(0);
    if row.user.as_ref().is_some_and(|name| name != &identity.name)
        || ((row.running || row.loaded || row.registered)
            && row.user.as_ref() != Some(&identity.name))
    {
        return Err(Error::attention(
            "service_owner_unconfirmed",
            "无法确认目标为当前用户服务，未执行",
        ));
    }
    Ok(Observation::Service {
        running: row.running,
        loaded: row.loaded,
        registered: row.registered,
    })
}

pub fn validate_binding(journal: &Journal) -> Result<()> {
    let formula = match &journal.plan.package.provider {
        Provider::Service { formula } => Some(formula.as_str()),
        _ => None,
    };
    let service = formula.is_some();
    let control = is_control(journal.plan.action);
    if !service && !control && journal.scope.service_identity.is_none() {
        return Ok(());
    }
    if !service
        || !control
        || journal.plan.package.kind != Kind::Service
        || journal.plan.package.id != "service:postgresql"
        || !matches!(journal.plan.before, Observation::Service { .. })
        || journal.plan.target.is_some()
        || journal.plan.delegated
        || !journal.plan.synchronous
    {
        return Err(Error::new(
            "invalid_service_journal",
            "服务操作类型、来源或目标不匹配",
        ));
    }
    let identity = journal
        .scope
        .service_identity
        .as_ref()
        .ok_or_else(|| Error::new("invalid_service_journal", "服务操作缺少冻结身份"))?;
    validate_identity(identity)?;
    let verb = if journal.plan.action == Action::Start {
        "start"
    } else {
        "stop"
    };
    let valid = journal
        .plan
        .command
        .as_ref()
        .zip(
            journal
                .plan
                .query
                .as_ref()
                .and_then(crate::domain::QuerySpec::command),
        )
        .is_some_and(|(command, query)| {
            formula == Some("postgresql@16")
                && command.program == query.program
                && command.args == ["services", verb, "postgresql@16"]
                && query.args == ["services", "info", "postgresql@16", "--json"]
        });
    if !valid {
        return Err(Error::new(
            "invalid_service_journal",
            "服务 argv 与声明不匹配",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../tests/unit/service.rs"]
mod tests;
