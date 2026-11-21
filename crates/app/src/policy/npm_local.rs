use crate::{
    domain::{
        Action, CommandSpec, Journal, NpmAnchor, NpmConfigBindings, NpmContract, NpmWriteAuthority,
        Package, Provider, QuerySpec,
    },
    error::{Error, Result},
    policy::config_local::valid_digest,
};
use std::path::{Component, Path};

pub fn name(value: &str) -> Result<()> {
    if value.len() > 214 || !crate::policy::parsers::registry_name(value) {
        return Err(Error::new(
            "invalid_npm_name",
            "npm名称不在支持的有限语法内",
        ));
    }
    Ok(())
}

pub fn version(value: &str) -> Result<()> {
    if value.len() > 128 {
        return Err(Error::new("invalid_npm_version", "npm版本超出长度上限"));
    }
    crate::policy::parsers::exact_version(value).map(|_| ())
}

pub fn package_scope(package_name: &str) -> Option<&str> {
    package_name
        .strip_prefix('@')
        .and_then(|_| package_name.split_once('/'))
        .map(|(scope, _)| scope)
}

pub fn sanitized_env_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("NODE_OPTIONS")
        || (key.len() >= 11 && key.as_bytes()[..11].eq_ignore_ascii_case(b"npm_config_"))
}

pub fn validate_registry_url(url: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(Error::new(
            "invalid_npm_registry_url",
            "npm registry 必须使用无凭据 https URL",
        ));
    };
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if url.len() > 512
        || authority.is_empty()
        || !authority
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-:".contains(&b))
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_/~".contains(&b))
    {
        return Err(Error::new(
            "invalid_npm_registry_url",
            "npm registry URL 格式不合法或含未授权查询与凭据",
        ));
    }
    Ok(())
}

pub fn validate_authority(package_name: &str, authority: &NpmWriteAuthority) -> Result<()> {
    self::name(package_name)?;
    if let Some(reg) = &authority.registry {
        validate_registry_url(&reg.url)?;
        if let Some(scope_url) = &reg.scope_url {
            if package_scope(package_name).is_none() {
                return Err(Error::new(
                    "invalid_npm_authority",
                    "非 scoped npm 包不能声明 scope_url",
                ));
            }
            validate_registry_url(scope_url)?;
        }
    }
    Ok(())
}

fn valid_config_bindings(bindings: &NpmConfigBindings) -> bool {
    let stamp_ok = |s: &crate::domain::ConfigFileStamp| {
        s.mode <= 0o7777 && valid_digest(&s.content_digest) && valid_digest(&s.fingerprint)
    };
    bindings.userconfig.as_ref().is_none_or(stamp_ok)
        && bindings.globalconfig.as_ref().is_none_or(stamp_ok)
}

pub fn expected_npm_view_args(
    home: &Path,
    prefix: &Path,
    package_name: &str,
    authority: Option<&NpmWriteAuthority>,
) -> Result<Vec<String>> {
    self::name(package_name)?;
    crate::policy::paths::lexical(prefix)?;
    let Some(auth) = authority else {
        return Ok(vec![
            "view".into(),
            package_name.into(),
            "version".into(),
            "--json".into(),
        ]);
    };
    crate::policy::paths::lexical(home)?;
    validate_authority(package_name, auth)?;
    let reg = auth.registry.as_ref().ok_or_else(|| {
        Error::attention(
            "registry_authority_required",
            "配置写权限边界的 npm 配方查询远端版本须显式声明 registry",
        )
    })?;
    let mut args = vec![
        "view".into(),
        "--userconfig".into(),
        home.join(".npmrc").display().to_string(),
        "--globalconfig".into(),
        prefix.join("etc/npmrc").display().to_string(),
        format!("--registry={}", reg.url),
    ];
    if let Some(scope) = package_scope(package_name) {
        let eff = reg.scope_url.as_deref().unwrap_or(&reg.url);
        args.push(format!("--{scope}:registry={eff}"));
    }
    args.extend([
        "--replace-registry-host=never".into(),
        package_name.into(),
        "version".into(),
        "--json".into(),
    ]);
    Ok(args)
}

pub fn expected_npm_write_args(
    home: &Path,
    prefix: &Path,
    package_name: &str,
    authority: Option<&NpmWriteAuthority>,
    action: Action,
    target: Option<&str>,
) -> Result<Vec<String>> {
    self::name(package_name)?;
    crate::policy::paths::lexical(prefix)?;
    let Some(auth) = authority else {
        let (verb, spec) = match action {
            Action::Install | Action::Upgrade => {
                let ver =
                    target.ok_or_else(|| Error::new("exact_version_required", package_name))?;
                version(ver)?;
                ("install", format!("{package_name}@{ver}"))
            }
            Action::Uninstall => ("uninstall", package_name.into()),
            _ => return Err(Error::new("operation_unsupported", package_name)),
        };
        return Ok(vec![
            verb.into(),
            "--global".into(),
            "--prefix".into(),
            prefix.display().to_string(),
            spec,
        ]);
    };
    crate::policy::paths::lexical(home)?;
    validate_authority(package_name, auth)?;
    let user_cfg = home.join(".npmrc").display().to_string();
    let global_cfg = prefix.join("etc/npmrc").display().to_string();
    match action {
        Action::Uninstall => {
            if !auth.allow_remove {
                return Err(Error::attention(
                    "remove_authority_required",
                    "该 npm 配方未授权 allow_remove 精确卸载",
                ));
            }
            Ok(vec![
                "uninstall".into(),
                "--global".into(),
                "--prefix".into(),
                prefix.display().to_string(),
                "--userconfig".into(),
                user_cfg,
                "--globalconfig".into(),
                global_cfg,
                package_name.into(),
            ])
        }
        Action::Install | Action::Upgrade => {
            let ver = target.ok_or_else(|| Error::new("exact_version_required", package_name))?;
            version(ver)?;
            let reg = auth.registry.as_ref().ok_or_else(|| {
                Error::attention(
                    "registry_authority_required",
                    "配置写权限边界的 npm 配方升级须显式声明 registry 权威",
                )
            })?;
            let mut args = vec![
                "install".into(),
                "--global".into(),
                "--prefix".into(),
                prefix.display().to_string(),
                "--userconfig".into(),
                user_cfg,
                "--globalconfig".into(),
                global_cfg,
                format!("--registry={}", reg.url),
            ];
            if let Some(scope) = package_scope(package_name) {
                let eff = reg.scope_url.as_deref().unwrap_or(&reg.url);
                args.push(format!("--{scope}:registry={eff}"));
            }
            args.push("--replace-registry-host=never".into());
            if !reg.allow_scripts {
                args.push("--ignore-scripts".into());
            }
            args.push(format!("{package_name}@{ver}"));
            Ok(args)
        }
        _ => Err(Error::new("operation_unsupported", package_name)),
    }
}

pub fn bind_canonical_command_paths(
    command: &mut CommandSpec,
    canonical_prefix: &Path,
) -> Result<()> {
    let prefix_str = canonical_prefix
        .to_str()
        .ok_or_else(|| Error::new("invalid_npm_prefix", "npm 路径须为 UTF-8"))?
        .to_owned();
    let slot = command
        .args
        .get_mut(3)
        .ok_or_else(|| Error::new("invalid_npm_command", "npm 写命令缺少目录"))?;
    slot.clone_from(&prefix_str);
    if command
        .args
        .get(6)
        .is_some_and(|flag| flag == "--globalconfig")
    {
        let global_slot = command
            .args
            .get_mut(7)
            .ok_or_else(|| Error::new("invalid_npm_command", "npm 写命令缺少全局配置路径"))?;
        *global_slot = format!("{prefix_str}/etc/npmrc");
    }
    Ok(())
}

pub fn write_warnings(authority: Option<&NpmWriteAuthority>, action: Action) -> Vec<String> {
    if !matches!(action, Action::Install | Action::Upgrade) {
        return vec![];
    }
    let Some(NpmWriteAuthority {
        registry: Some(reg),
        ..
    }) = authority
    else {
        return vec![];
    };
    let mut warnings = vec![
        "RegistryAuthority 仅约束顶层包来源选择与生命周期脚本策略，不锁定传递依赖子树或 shrinkwrap 远端地址"
            .into(),
    ];
    if reg.allow_scripts {
        warnings.push("配方已显式开启生命周期脚本（allow_scripts=true）".into());
    }
    warnings
}

pub fn binding(package: &Package, query: &QuerySpec) -> Result<()> {
    match query {
        QuerySpec::LocalNpm(query) => validate_local_npm_binding(package, query),
        QuerySpec::Command(command) => validate_legacy_npm_command(package, command),
        QuerySpec::NativeConfig(_)
        | QuerySpec::BrewFormula(_)
        | QuerySpec::LocalConfig(_)
        | QuerySpec::LocalCapability(_)
        | QuerySpec::LocalUvTool(_) => {
            if matches!(
                package.provider,
                Provider::Npm { .. } | Provider::NpmObserved { .. }
            ) {
                Err(invalid_plan())
            } else {
                Ok(())
            }
        }
    }
}

fn validate_local_npm_binding(
    package: &Package,
    query: &crate::domain::LocalNpmQuery,
) -> Result<()> {
    let authority = package_authority(&package.provider);
    let (prefix, expected, contract) = match &package.provider {
        Provider::Npm {
            prefix, package, ..
        } => (prefix, package, NpmContract::DeclaredV1),
        Provider::NpmObserved { prefix, name } => (prefix, name, NpmContract::NativeV1),
        _ => {
            return Err(Error::new(
                "invalid_npm_query_binding",
                "本地npm查询不匹配provider",
            ));
        }
    };
    name(&query.name)?;
    if let Some(auth) = authority {
        validate_authority(expected, auth)?;
    }
    crate::policy::paths::lexical(&query.prefix)?;
    crate::policy::paths::lexical(&query.namespace.canonical_prefix)?;
    if prefix != &query.prefix
        || expected != &query.name
        || query.contract != contract
        || authority.is_some() != query.config_bindings.is_some()
        || query
            .config_bindings
            .as_ref()
            .is_some_and(|b| !valid_config_bindings(b))
    {
        return Err(Error::new(
            "invalid_npm_query_binding",
            "本地npm查询与冻结对象不匹配",
        ));
    }
    match &query.namespace.anchor {
        NpmAnchor::ExistingPrefix { path, .. } => {
            if path != &query.namespace.canonical_prefix {
                return Err(Error::new("invalid_npm_anchor", "现存根锚点不匹配"));
            }
        }
        NpmAnchor::CreatePrefix {
            path, missing_tail, ..
        } => {
            crate::policy::paths::lexical(path)?;
            if query.contract != NpmContract::DeclaredV1 || missing_tail.is_empty() {
                return Err(Error::new("invalid_npm_anchor", "新建根意图不匹配"));
            }
            let mut full = path.clone();
            for value in missing_tail {
                let mut parts = Path::new(value).components();
                if !matches!(parts.next(),Some(Component::Normal(part)) if part==value.as_str())
                    || parts.next().is_some()
                {
                    return Err(Error::new("invalid_npm_anchor", "新建根需要安全路径组件"));
                }
                full.push(value);
            }
            if full != query.namespace.canonical_prefix {
                return Err(Error::new("invalid_npm_anchor", "新建根路径与锚点不匹配"));
            }
        }
    }
    Ok(())
}

fn validate_legacy_npm_command(package: &Package, command: &CommandSpec) -> Result<()> {
    if matches!(package.provider, Provider::NpmObserved { .. }) {
        return Err(invalid_plan());
    }
    if let Provider::Npm {
        prefix,
        package,
        authority,
    } = &package.provider
    {
        if authority.is_some() {
            return Err(Error::new(
                "invalid_npm_query_binding",
                "带写权限契约的 npm 配方不允许回退旧命令查询",
            ));
        }
        crate::policy::paths::lexical(&command.program)?;
        crate::policy::paths::lexical(prefix)?;
        name(package)?;
        let expected = vec![
            "ls".into(),
            "--global".into(),
            "--prefix".into(),
            prefix.display().to_string(),
            "--depth=0".into(),
            "--json".into(),
            package.clone(),
        ];
        if command.args != expected {
            return Err(Error::new(
                "invalid_npm_query_binding",
                "旧npm查询与冻结对象不匹配",
            ));
        }
    }
    Ok(())
}

pub fn manifest_version(bytes: &[u8], name: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Manifest {
        name: String,
        version: String,
    }
    let crate::policy::json::Object(manifest): crate::policy::json::Object<Manifest> =
        serde_json::from_slice(bytes).map_err(|_| {
            Error::attention("invalid_npm_manifest", "npm manifest 名称或版本记录无效")
        })?;
    self::name(&manifest.name)?;
    if manifest.name != name {
        return Err(Error::attention(
            "alias_requires_review",
            "npm manifest 与安装名称不一致",
        ));
    }
    version(&manifest.version)?;
    Ok(manifest.version)
}

fn package_authority(provider: &Provider) -> Option<&NpmWriteAuthority> {
    match provider {
        Provider::Npm { authority, .. } => authority.as_ref(),
        _ => None,
    }
}

fn invalid_plan() -> Error {
    Error::new("invalid_npm_plan", "npm 查询、目录身份或写命令不匹配")
}

pub fn validate_plan(plan: &crate::domain::Plan) -> Result<()> {
    use crate::domain::Observation;
    let evidence = match &plan.before {
        Observation::Present { evidence, .. } | Observation::Missing { evidence } => {
            evidence.as_ref()
        }
        _ => None,
    };
    if let Some(query) = &plan.query {
        binding(&plan.package, query)?;
    }
    let local_required = matches!(
        plan.package.provider,
        Provider::Npm { .. } | Provider::NpmObserved { .. }
    );
    if local_required && plan.query.is_none() {
        return Err(invalid_plan());
    }
    let Some(QuerySpec::LocalNpm(query)) = &plan.query else {
        return if evidence.is_some() {
            Err(invalid_plan())
        } else {
            Ok(())
        };
    };
    if query.contract == NpmContract::NativeV1 {
        if plan.package.kind != crate::domain::Kind::Cli
            || plan.package.id
                != crate::policy::targets::npm_id(&query.namespace.canonical_prefix, &query.name)?
            || query.prefix != query.namespace.canonical_prefix
        {
            return Err(invalid_plan());
        }
        return Ok(());
    }
    let authority = package_authority(&plan.package.provider);
    let evidence = evidence.ok_or_else(invalid_plan)?;
    validate_evidence(query, evidence)?;
    if matches!(query.namespace.anchor, NpmAnchor::CreatePrefix { .. })
        && (plan.action != Action::Install
            || evidence.prefix_identity.is_some()
            || !matches!(plan.before, Observation::Missing { .. }))
    {
        return Err(invalid_plan());
    }
    match plan.action {
        Action::Adopt
            if plan.command.is_none() && matches!(plan.before, Observation::Present { .. }) =>
        {
            Ok(())
        }
        Action::Install | Action::Upgrade | Action::Uninstall => {
            if matches!(plan.before, Observation::Missing { .. })
                != (plan.action == Action::Install)
            {
                return Err(invalid_plan());
            }
            let command = plan.command.as_ref().ok_or_else(invalid_plan)?;
            let derived_home = if authority.is_some() {
                command
                    .args
                    .get(5)
                    .map(Path::new)
                    .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(".npmrc"))
                    .and_then(Path::parent)
                    .ok_or_else(invalid_plan)?
            } else {
                Path::new("/unused")
            };
            let expected = expected_npm_write_args(
                derived_home,
                &query.namespace.canonical_prefix,
                &query.name,
                authority,
                plan.action,
                plan.target.as_deref(),
            )
            .map_err(|_| invalid_plan())?;
            if command.args != expected || plan.delegated || !plan.synchronous {
                return Err(invalid_plan());
            }
            Ok(())
        }
        _ => Err(invalid_plan()),
    }
}

pub fn validate_binding_journal(journal: &Journal) -> Result<()> {
    use crate::domain::{Observation, Phase};
    let plan = &journal.plan;
    let Some(proof) = &plan.npm_binding else {
        return Ok(());
    };
    if plan.action != Action::Adopt
        || journal.phase == Phase::Running
        || journal.process_group.is_some()
        || plan.delegated
        || !plan.synchronous
        || !matches!(
            plan.package.provider,
            Provider::Npm {
                authority: Some(_),
                ..
            }
        )
        || !matches!(plan.before, Observation::Present { .. })
        || plan.target.as_deref() != plan.before.version()
        || !valid_digest(&proof.old_receipt_sha256)
    {
        return Err(invalid_plan());
    }
    let Some(QuerySpec::LocalNpm(query)) = &plan.query else {
        return Err(invalid_plan());
    };
    if !matches!(query.namespace.anchor, NpmAnchor::ExistingPrefix { .. })
        || plan.package.id
            != crate::policy::targets::npm_id(&query.namespace.canonical_prefix, &query.name)?
    {
        return Err(invalid_plan());
    }
    Ok(())
}

pub fn validate_journal_home(journal: &Journal) -> Result<()> {
    if let (
        Provider::Npm {
            authority: Some(_), ..
        },
        Some(command),
    ) = (&journal.plan.package.provider, &journal.plan.command)
        && command.args.get(5) != Some(&journal.home.join(".npmrc").display().to_string())
    {
        return Err(invalid_plan());
    }
    Ok(())
}

fn validate_evidence(
    query: &crate::domain::LocalNpmQuery,
    evidence: &crate::domain::NpmEvidence,
) -> Result<()> {
    if evidence.canonical_prefix != query.namespace.canonical_prefix
        || evidence.namespace
            != crate::policy::targets::npm_id(&query.namespace.canonical_prefix, &query.name)?
        || evidence.fingerprint.len() != 64
        || !evidence
            .fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || matches!(&query.namespace.anchor,NpmAnchor::ExistingPrefix{identity,..} if evidence.prefix_identity.as_ref()!=Some(identity))
    {
        return Err(invalid_plan());
    }
    Ok(())
}

pub fn committed_query(
    plan: &crate::domain::Plan,
    observed: &crate::domain::Observation,
) -> Result<Option<QuerySpec>> {
    let Some(QuerySpec::LocalNpm(query)) = &plan.query else {
        return Ok(plan.query.clone());
    };
    if query.contract == NpmContract::NativeV1 {
        return Ok(plan.query.clone());
    }
    let crate::domain::Observation::Present {
        evidence: Some(evidence),
        ..
    } = observed
    else {
        return Err(invalid_plan());
    };
    validate_evidence(query, evidence)?;
    let identity = evidence.prefix_identity.clone().ok_or_else(invalid_plan)?;
    let mut committed = query.clone();
    committed.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity,
    };
    Ok(Some(QuerySpec::LocalNpm(committed)))
}

pub fn committed_install_query(
    journal: &crate::domain::Journal,
    receipt: &crate::domain::Receipt,
) -> Result<Option<QuerySpec>> {
    if receipt.operation_id != journal.id
        || !matches!(&journal.plan.query,Some(QuerySpec::LocalNpm(query)) if matches!(query.namespace.anchor,NpmAnchor::CreatePrefix{..}))
    {
        return Ok(None);
    }
    let query = committed_query(&journal.plan, &receipt.observation)?;
    if query != receipt.query
        || receipt.scope.as_ref() != Some(&journal.scope)
        || crate::domain::fingerprint(&receipt.package)? != journal.plan.recipe_sha256
        || !crate::policy::state::expected(journal, &receipt.observation)
    {
        return Err(Error::attention(
            "commit_conflict",
            "已提交安装基线与冻结计划不匹配",
        ));
    }
    Ok(query)
}

#[cfg(test)]
#[path = "../../tests/unit/npm_policy.rs"]
mod tests;
