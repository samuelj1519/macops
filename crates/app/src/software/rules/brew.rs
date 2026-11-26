use crate::{
    domain::{
        Action, BrewAbsenceEvidence, BrewCompatibility, BrewConsent, BrewContextBinding,
        BrewContextRecord, BrewContextRegistry, BrewEffectivePolicies, BrewEntryKind,
        BrewEvidenceStamp, BrewExecution, BrewFormulaKey, BrewGoal, BrewInstalledEvidence,
        BrewInvocationProjection, BrewNativeInfo, BrewNativeReport, BrewOwnerSnapshot,
        BrewPlanData, BrewQuerySpec, BrewSelection, Catalog, CommandSpec, Observation, Package,
        Plan, Provider, QuerySpec, Receipts, fingerprint,
    },
    error::{Error, Result},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const SOURCE_COMMIT: &str = "3e47189a707c739ee1278a28a3b845f5834574e6";
pub const QUERY_SCRIPT: &[u8] = include_bytes!("../brew/query-v1.rb");
pub const EFFECT_CONTRACT: &str = "brew-native-formula-v1";
const REPORT_LIMIT: usize = 4 * 1024 * 1024;

fn require(ok: bool, code: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::attention(code, "Brew 冻结身份、范围或证据不匹配"))
    }
}
fn path(path: &Path) -> Result<()> {
    require(
        crate::policy::paths::lexical(path).ok().as_deref() == Some(path),
        "brew_invalid_path",
    )
}
fn token(value: &str, extra: &[u8]) -> bool {
    value
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
}
pub fn full_name(selection: &BrewSelection) -> Result<String> {
    let parts: Vec<_> = selection.tap.split('/').collect();
    require(
        parts.len() == 2
            && parts.iter().all(|p| token(p, b"_-"))
            && selection.tap == selection.tap.to_ascii_lowercase()
            && token(&selection.name, b"+_.@-")
            && selection.name == selection.name.to_ascii_lowercase(),
        "brew_invalid_identity",
    )?;
    Ok(if selection.tap == "homebrew/core" {
        selection.name.clone()
    } else {
        format!("{}/{}", selection.tap, selection.name)
    })
}
pub fn logical_id(key: &BrewFormulaKey) -> Result<String> {
    path(&key.canonical_prefix)?;
    let names: Vec<_> = key.full_name.split('/').collect();
    let valid = match names.as_slice() {
        [name] => token(name, b"+_.@-") && *name == name.to_ascii_lowercase(),
        [user, tap, name] => {
            full_name(&BrewSelection {
                tap: format!("{user}/{tap}"),
                name: (*name).into(),
            })
            .ok()
            .as_deref()
                == Some(key.full_name.as_str())
        }
        _ => false,
    };
    require(valid, "brew_invalid_identity")?;
    crate::domain::resource::id(
        "application",
        &("brew", &key.canonical_prefix, &key.full_name),
    )
}
pub fn context_id(prefix: &Path) -> Result<String> {
    path(prefix)?;
    crate::domain::resource::id("brew-context", &("brew", prefix))
}
pub fn validate_binding(binding: &BrewContextBinding) -> Result<()> {
    for p in [
        &binding.invocation,
        &binding.canonical_program,
        &binding.prefix,
        &binding.cellar,
        &binding.metadata_scope.home,
        &binding.metadata_scope.actual_user_config_home,
    ] {
        path(p)?;
    }
    for p in [
        &binding.metadata_scope.xdg_config_home,
        &binding.metadata_scope.homebrew_xdg_config_home,
    ]
    .into_iter()
    .flatten()
    {
        path(p)?;
    }
    require(
        binding.context_id == context_id(&binding.prefix)? && binding.prefix != binding.cellar,
        "brew_invalid_binding",
    )?;
    require(
        crate::policy::catalog::cache::valid_hash(&binding.program_stamp.sha256)
            && binding.program_stamp.node.mode & 0o170_000 == 0o100_000
            && binding.program_stamp.node.mode & 0o111 != 0,
        "brew_invalid_program",
    )?;
    let BrewCompatibility::Homebrew7_0_15 {
        source_commit,
        acceptance_sha256,
        ruby_version,
    } = &binding.compatibility;
    require(
        source_commit == SOURCE_COMMIT
            && crate::policy::catalog::cache::valid_hash(acceptance_sha256)
            && ruby_version.starts_with("4.0.")
            && token(ruby_version, b"."),
        "brew_compatibility_unsupported",
    )?;
    require(
        binding.metadata_scope.config_files.len() == 3,
        "brew_env_scope_unknown",
    )?;
    Ok(())
}
pub fn validate_query(query: &BrewQuerySpec) -> Result<()> {
    validate_binding(&query.binding)?;
    require(
        query.schema_version == 1
            && query.key.canonical_prefix == query.binding.prefix
            && query.key.full_name == full_name(&query.selection)?
            && token(&query.env_capture_id, b"-_"),
        "brew_invalid_query",
    )?;
    let script = &query.script;
    path(&script.absolute_path)?;
    path(&script.canonical_path)?;
    require(
        script.absolute_path == script.canonical_path
            && script.size == QUERY_SCRIPT.len() as u64
            && script.sha256 == crate::policy::catalog::cache::digest(QUERY_SCRIPT)
            && crate::policy::catalog::cache::valid_hash(&script.product_build_id),
        "brew_query_script_changed",
    )
}
pub fn validate_package(package: &Package) -> Result<()> {
    if let Provider::BrewFormula { query, .. } = &package.provider {
        let owner = logical_id(&query.key)?;
        validate_query(query)?;
        require(package.id == owner, "brew_owner_mismatch")?;
    }
    Ok(())
}
pub fn resolved_package(input: &Package, query: &BrewQuerySpec) -> Result<Package> {
    let id = logical_id(&query.key)?;
    validate_query(query)?;
    let recipe_ref = match &input.provider {
        Provider::BrewFormula { recipe_ref, .. } => recipe_ref.clone(),
        Provider::Brew { cask: false, .. } => Some(input.id.clone()),
        _ => {
            return Err(Error::attention(
                "brew_provider_mismatch",
                "目标不是Brew formula",
            ));
        }
    };
    let mut package = input.clone();
    package.id = id;
    package.provider = Provider::BrewFormula {
        query: Box::new(query.clone()),
        recipe_ref,
    };
    Ok(package)
}
pub fn parse_report(bytes: &[u8]) -> Result<BrewNativeReport> {
    require(bytes.len() <= REPORT_LIMIT, "brew_query_incomplete")?;
    serde_json::from_value::<crate::policy::json::Object<BrewNativeReport>>(
        crate::policy::json::parse(bytes)?,
    )
    .map(|v| v.0)
    .map_err(Into::into)
}
fn policies(value: &BrewEffectivePolicies) -> Result<()> {
    require(
        value.no_auto_update
            && value.no_install_cleanup
            && value.no_autoremove
            && value.runtime.no_bootsnap
            && value.runtime.installed_dependents_check,
        "brew_env_policy_conflict",
    )
}
pub fn validate_stamp(query: &BrewQuerySpec, stamp: &BrewEvidenceStamp) -> Result<()> {
    require(
        stamp.context_id == query.binding.context_id
            && stamp.context_epoch == query.context_epoch
            && stamp.binding_fingerprint == fingerprint(&query.binding)?
            && stamp.env_capture_id == query.env_capture_id
            && token(&stamp.query_id, b"-_"),
        "brew_query_stale",
    )
}
fn selected<'a>(
    query: &BrewQuerySpec,
    report: &'a BrewNativeReport,
) -> Result<Vec<&'a crate::domain::BrewKegIdentity>> {
    let rack = &report.rack;
    require(
        rack.path == query.binding.cellar.join(&query.selection.name)
            && rack.cellar_before.mode & 0o170_000 == 0o040_000
            && rack.cellar_before == rack.cellar_after
            && rack.cellar_before.identity == query.binding.cellar_id
            && rack
                .rack_before
                .as_ref()
                .is_none_or(|s| s.mode & 0o170_000 == 0o040_000)
            && rack.rack_before == rack.rack_after
            && rack.entries.len() <= 4096
            && rack.kegs.len() <= 4096,
        "brew_rack_incomplete",
    )?;
    require(
        rack.rack_before.is_some() || (rack.entries.is_empty() && rack.kegs.is_empty()),
        "brew_rack_incomplete",
    )?;
    let mut leaves = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for entry in &rack.entries {
        require(
            !entry.name.is_empty()
                && !matches!(entry.name.as_str(), "." | "..")
                && !entry.name.contains(['/', '\0'])
                && leaves.insert(entry.name.clone()),
            "brew_rack_incomplete",
        )?;
        require(
            entry.stamp.mode & 0o170_000
                == if entry.kind == BrewEntryKind::Directory {
                    0o040_000
                } else {
                    0o100_000
                },
            "brew_rack_incomplete",
        )?;
        if entry.kind == BrewEntryKind::Directory {
            directories.insert(rack.path.join(&entry.name));
        }
    }
    let mut kegs = BTreeSet::new();
    let mut matching = vec![];
    for keg in &rack.kegs {
        path(&keg.path)?;
        require(
            keg.path.parent() == Some(rack.path.as_path())
                && kegs.insert(keg.path.clone())
                && keg.name == query.selection.name
                && keg.full_name
                    == full_name(&BrewSelection {
                        tap: keg.tap.clone(),
                        name: keg.name.clone(),
                    })?
                && matches!(keg.spec.as_str(), "stable" | "head")
                && !keg.version.is_empty()
                && keg.receipt.node.mode & 0o170_000 == 0o100_000
                && crate::policy::catalog::cache::valid_hash(&keg.receipt.sha256),
            "brew_keg_identity_unknown",
        )?;
        if keg.tap == query.selection.tap {
            matching.push(keg);
        }
    }
    let native: BTreeSet<_> = rack.native_selected.iter().cloned().collect();
    let expected: BTreeSet<_> = matching.iter().map(|k| k.path.clone()).collect();
    require(
        kegs == directories && native.len() == rack.native_selected.len() && native == expected,
        "brew_native_selection_mismatch",
    )?;
    require(
        match &report.selected_version {
            Some(version) => matching.iter().any(|k| k.version == *version),
            None => matching.is_empty(),
        },
        "brew_native_selection_mismatch",
    )?;
    Ok(matching)
}
pub fn observation(
    query: &BrewQuerySpec,
    stamp: BrewEvidenceStamp,
    report: BrewNativeReport,
) -> Result<Observation> {
    validate_query(query)?;
    validate_stamp(query, &stamp)?;
    policies(&report.policies)?;
    require(
        report.schema_version == 1
            && report.query_id == stamp.query_id
            && report.homebrew_version == "7.0.15"
            && report.prefix == query.binding.prefix
            && report.cellar == query.binding.cellar
            && report.actual_user_config_home
                == query.binding.metadata_scope.actual_user_config_home,
        "brew_native_scope_changed",
    )?;
    selected(query, &report)?;
    match report.selected_version.clone() {
        Some(version) => Ok(Observation::BrewInstalled {
            version,
            evidence: Box::new(BrewInstalledEvidence { stamp, report }),
        }),
        None => Ok(Observation::BrewMissing {
            evidence: Box::new(BrewAbsenceEvidence { stamp, report }),
        }),
    }
}

fn checked_observation<'a>(
    query: &BrewQuerySpec,
    observed: &'a Observation,
) -> Result<(&'a BrewEvidenceStamp, &'a BrewNativeReport)> {
    let (stamp, report) = match observed {
        Observation::BrewInstalled { evidence, .. } => (&evidence.stamp, &evidence.report),
        Observation::BrewMissing { evidence } => (&evidence.stamp, &evidence.report),
        _ => {
            return Err(Error::attention(
                "brew_query_incomplete",
                "需要完整的 Brew typed evidence",
            ));
        }
    };
    require(
        observation(query, stamp.clone(), report.clone())? == *observed,
        "brew_evidence_mismatch",
    )?;
    Ok((stamp, report))
}
#[derive(Debug)]
pub struct BrewPlan<'a> {
    pub plan: &'a Plan,
    pub data: &'a BrewPlanData,
    pub query: &'a BrewQuerySpec,
    before_report: &'a BrewNativeReport,
}
pub fn checked_plan(plan: &Plan) -> Result<Option<BrewPlan<'_>>> {
    let (data, query, recipe) = match (&plan.brew, &plan.query, &plan.package.provider) {
        (
            Some(data),
            Some(QuerySpec::BrewFormula(query)),
            Provider::BrewFormula {
                query: package_query,
                recipe_ref,
            },
        ) if query == package_query => (data.as_ref(), query.as_ref(), recipe_ref),
        (None, query, provider)
            if !matches!(query, Some(QuerySpec::BrewFormula(_)))
                && !matches!(provider, Provider::BrewFormula { .. }) =>
        {
            return Ok(None);
        }
        _ => return Err(Error::attention("invalid_brew_plan", "Brew 计划投影不完整")),
    };
    validate_package(&plan.package)?;
    let (before_stamp, before_report) = checked_observation(query, &plan.before)?;
    let owner = &plan.package.id;
    require(
        data.schema_version == 1
            && data.owner.logical_owner_id == *owner
            && data.owner.selected_package_id == *owner
            && data.owner.entries.keys().all(|id| id == owner)
            && data.context.context_id == query.binding.context_id
            && data.context.epoch == query.context_epoch
            && data.context.binding_fingerprint == before_stamp.binding_fingerprint
            && data.env_capture_id == query.env_capture_id
            && plan.uv_owner.is_none()
            && plan.synchronous
            && plan.delegated,
        "brew_plan_scope_mismatch",
    )?;
    let consent = &data.confirmation;
    require(
        consent.context_id == data.context.context_id
            && consent.epoch == data.context.epoch
            && consent.logical_owner_id == *owner
            && consent.action == plan.action
            && consent.effect_contract == EFFECT_CONTRACT
            && !(plan.action == Action::Uninstall
                && consent.consent == BrewConsent::ExistingContext),
        "brew_confirmation_mismatch",
    )?;
    let (action, target) = match &data.goal {
        BrewGoal::AdoptBaseline => (Action::Adopt, None),
        BrewGoal::UninstallAllInstalledVersions => (Action::Uninstall, None),
        BrewGoal::UpgradeAvailable { pkg_version } => (Action::Upgrade, Some(pkg_version)),
        BrewGoal::InstallAvailable { pkg_version } => {
            require(recipe.is_some(), "brew_install_recipe_required")?;
            (Action::Install, Some(pkg_version))
        }
    };
    require(
        plan.action == action
            && plan.target.as_ref() == target
            && (1..=3600).contains(&plan.timeout_secs),
        "brew_goal_mismatch",
    )?;
    let view = BrewPlan {
        plan,
        data,
        query,
        before_report,
    };
    require(plan.command == command(&view)?, "brew_command_mismatch")?;
    validate_execution(&view)?;
    Ok(Some(view))
}
pub fn formula_identity<'a>(
    query: &BrewQuerySpec,
    report: &'a BrewNativeReport,
) -> Result<&'a BrewNativeInfo> {
    let info = report
        .formula
        .as_ref()
        .ok_or_else(|| Error::attention("brew_formula_unavailable", "Native formula无法解析"))?;
    let native_name = full_name(&query.selection)?;
    require(
        info.name == query.selection.name
            && info.tap == query.selection.tap
            && info.full_name == native_name
            && query.key.full_name == native_name
            && info.active_spec == "stable"
            && !info.resolution.alias_changed
            && !info.resolution.migration_needed
            && info.latest_full_name == native_name,
        "brew_formula_identity_mismatch",
    )?;
    Ok(info)
}
pub fn upgrade_is_current(query: &BrewQuerySpec, observed: &Observation) -> Result<bool> {
    let (_, report) = checked_observation(query, observed)?;
    let info = formula_identity(query, report)?;
    let matching: Vec<_> = report
        .rack
        .kegs
        .iter()
        .filter(|k| k.tap == query.selection.tap)
        .collect();
    require(
        !info.pinned
            && matching.iter().all(|k| k.spec == "stable")
            && info
                .installed_paths
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                == matching.iter().map(|k| k.path.clone()).collect(),
        "brew_upgrade_scope_unsupported",
    )?;
    Ok(!info.outdated && matching.iter().any(|k| k.version == info.pkg_version))
}
fn validate_execution(view: &BrewPlan<'_>) -> Result<()> {
    if view.plan.action == Action::Upgrade {
        let current = upgrade_is_current(view.query, &view.plan.before)?;
        require(
            match view.data.execution {
                BrewExecution::VerifiedNoop => current,
                BrewExecution::NativeMutator => !current,
                BrewExecution::BaselineOnly => false,
            },
            "brew_execution_mismatch",
        )?;
    }
    match view.data.execution {
        BrewExecution::BaselineOnly => require(
            view.plan.action == Action::Adopt
                && matches!(view.plan.before, Observation::BrewInstalled { .. }),
            "brew_execution_mismatch",
        )?,
        BrewExecution::VerifiedNoop => {
            require(expected(view, &view.plan.before), "brew_execution_mismatch")?;
        }
        BrewExecution::NativeMutator => {
            require(
                view.plan.action != Action::Adopt
                    && !(view.plan.action == Action::Uninstall
                        && matches!(view.plan.before, Observation::BrewMissing { .. })),
                "brew_execution_mismatch",
            )?;
            let report = view.before_report;
            let matching: Vec<_> = report
                .rack
                .kegs
                .iter()
                .filter(|k| k.tap == view.query.selection.tap)
                .collect();
            require(
                matching.iter().all(|k| k.spec == "stable"),
                "brew_head_unsupported",
            )?;
        }
    }
    Ok(())
}
pub fn command(view: &BrewPlan<'_>) -> Result<Option<CommandSpec>> {
    if view.data.execution != BrewExecution::NativeMutator {
        return Ok(None);
    }
    native_command(view.query, view.plan.action).map(Some)
}
pub fn resolve_goal(
    query: &BrewQuerySpec,
    action: Action,
    observed: &Observation,
) -> Result<(BrewGoal, Option<String>)> {
    let report = match observed {
        Observation::BrewInstalled { evidence, .. } => &evidence.report,
        Observation::BrewMissing { evidence } => &evidence.report,
        _ => {
            return Err(Error::attention(
                "brew_observation_required",
                "Brew 查询不完整，未准备变更",
            ));
        }
    };
    let result = match action {
        Action::Adopt => (BrewGoal::AdoptBaseline, None),
        Action::Uninstall => (BrewGoal::UninstallAllInstalledVersions, None),
        Action::Install | Action::Upgrade => {
            let version = formula_identity(query, report)?.pkg_version.clone();
            let goal = if action == Action::Install {
                BrewGoal::InstallAvailable {
                    pkg_version: version.clone(),
                }
            } else {
                BrewGoal::UpgradeAvailable {
                    pkg_version: version.clone(),
                }
            };
            (goal, Some(version))
        }
        _ => {
            return Err(Error::attention(
                "brew_action_unsupported",
                "Brew 不支持此动作",
            ));
        }
    };
    Ok(result)
}
pub fn native_command(query: &BrewQuerySpec, action: Action) -> Result<CommandSpec> {
    let verb = match action {
        Action::Install => "install",
        Action::Upgrade => "upgrade",
        Action::Uninstall => "uninstall",
        _ => {
            return Err(Error::attention(
                "brew_action_unsupported",
                "Brew 不支持该写动作",
            ));
        }
    };
    let mut args = vec![verb.into(), "--formula".into()];
    if action == Action::Uninstall {
        args.push("--force".into());
    }
    args.push(format!("{}/{}", query.selection.tap, query.selection.name));
    Ok(CommandSpec {
        program: query.binding.invocation.clone(),
        args,
    })
}
pub fn expected(view: &BrewPlan<'_>, observed: &Observation) -> bool {
    let Ok((_, report)) = checked_observation(view.query, observed) else {
        return false;
    };
    match &view.data.goal {
        BrewGoal::AdoptBaseline => {
            let mut previous = view.before_report.clone();
            previous.query_id.clone_from(&report.query_id);
            previous == *report
        }
        BrewGoal::UninstallAllInstalledVersions => {
            matches!(observed, Observation::BrewMissing { .. })
        }
        BrewGoal::InstallAvailable { pkg_version } | BrewGoal::UpgradeAvailable { pkg_version } => {
            report
                .rack
                .kegs
                .iter()
                .any(|k| k.tap == view.query.selection.tap && k.version == *pkg_version)
                && formula_identity(view.query, report).is_ok_and(|info| {
                    info.pkg_version == *pkg_version && !info.outdated && !info.pinned
                })
                && (view.plan.action != Action::Upgrade
                    || upgrade_is_current(view.query, observed).unwrap_or(false))
        }
    }
}
pub fn invocation_projection(view: &BrewPlan<'_>) -> Result<BrewInvocationProjection> {
    Ok(BrewInvocationProjection {
        command: command(view)?
            .ok_or_else(|| Error::attention("brew_no_native_command", "计划没有 Native mutator"))?,
        cwd: view.query.binding.metadata_scope.home.clone(),
        timeout_secs: view.plan.timeout_secs,
    })
}
pub fn owner_snapshot(
    key: &BrewFormulaKey,
    catalog: &Catalog,
    receipts: &Receipts,
    registry: &BrewContextRegistry,
) -> Result<BrewOwnerSnapshot> {
    validate_registry(registry)?;
    let owner = logical_id(key)?;
    let mut entries: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut declarations = BTreeSet::new();
    let context = registry
        .contexts
        .values()
        .find(|record| record.binding.prefix == key.canonical_prefix);
    for p in &catalog.packages {
        require(
            p.id != owner
                || matches!(&p.provider,Provider::BrewFormula{query,..} if query.key==*key),
            "brew_owner_conflict",
        )?;
        let belongs = match &p.provider {
            Provider::BrewFormula { query, .. } => {
                if query.key == *key {
                    require(p.id == owner, "brew_owner_conflict")?;
                }
                query.key == *key
            }
            Provider::Brew {
                package,
                cask: false,
            } if package == &key.full_name
                || key.full_name.rsplit('/').next() == Some(package.as_str()) =>
            {
                require(
                    context.and_then(|c| c.declaration_keys.get(&p.id)) == Some(key),
                    "brew_owner_scope_unknown",
                )?;
                true
            }
            _ => false,
        };
        if belongs {
            declarations.insert(p.id.clone());
            entries.entry(owner.clone()).or_default().insert(format!(
                "declaration:{}:{}",
                p.id,
                fingerprint(p)?
            ));
        }
    }
    for (receipt_key, receipt) in &receipts.installations {
        let belongs = if let Some(QuerySpec::BrewFormula(q)) = &receipt.query {
            q.key == *key
        } else {
            require(
                !matches!(&receipt.package.provider,Provider::Brew{package,cask:false} if package==&key.full_name || key.full_name.rsplit('/').next()==Some(package.as_str())),
                "brew_owner_scope_unknown",
            )?;
            false
        };
        require(
            (receipt_key != &owner && receipt.package.id != owner) || belongs,
            "brew_owner_conflict",
        )?;
        if belongs {
            require(
                receipt.package.id == owner
                    && matches!((&receipt.package.provider,&receipt.query), (Provider::BrewFormula{query,..},Some(QuerySpec::BrewFormula(frozen))) if query==frozen)
                    && matches!((&receipt.scope,&receipt.query),(Some(scope),Some(QuerySpec::BrewFormula(frozen))) if scope.home==frozen.binding.metadata_scope.home),
                "brew_owner_conflict",
            )?;
            entries
                .entry(owner.clone())
                .or_default()
                .insert(format!("receipt:{}", fingerprint(receipt)?));
        }
    }
    require(declarations.len() <= 1, "brew_owner_conflict")?;
    Ok(BrewOwnerSnapshot {
        logical_owner_id: owner.clone(),
        selected_package_id: owner,
        entries,
    })
}
pub fn validate_registry(registry: &BrewContextRegistry) -> Result<()> {
    require(registry.schema_version == 1, "brew_registry_version")?;
    let mut cellars = BTreeMap::new();
    for (id, record) in &registry.contexts {
        validate_binding(&record.binding)?;
        require(
            *id == record.binding.context_id
                && record
                    .declaration_keys
                    .values()
                    .all(|k| k.canonical_prefix == record.binding.prefix),
            "brew_registry_scope_mismatch",
        )?;
        require(
            cellars
                .insert(&record.binding.cellar, &record.binding.prefix)
                .is_none(),
            "brew_cellar_overlap",
        )?;
    }
    Ok(())
}
pub fn rebind(
    record: &BrewContextRecord,
    binding: BrewContextBinding,
    continuity_verified: bool,
    pending_contexts: &BTreeSet<String>,
) -> Result<BrewContextRecord> {
    validate_binding(&binding)?;
    require(
        record.binding.context_id == binding.context_id
            && record.binding.prefix == binding.prefix
            && !pending_contexts.contains(&binding.context_id),
        "brew_rebind_blocked",
    )?;
    require(
        record.binding.cellar == binding.cellar || continuity_verified,
        "brew_cellar_continuity_unknown",
    )?;
    require(record.binding != binding, "brew_rebind_unchanged")?;
    Ok(BrewContextRecord {
        epoch: record
            .epoch
            .checked_add(1)
            .ok_or_else(|| Error::attention("brew_epoch_exhausted", "context epoch 已耗尽"))?,
        binding,
        observation_valid: false,
        writer_scope_confirmed: false,
        declaration_keys: record.declaration_keys.clone(),
    })
}
pub fn accept_observation(
    record: &BrewContextRecord,
    query: &BrewQuerySpec,
    observed: &Observation,
) -> Result<BrewContextRecord> {
    require(
        record.epoch == query.context_epoch && record.binding == query.binding,
        "brew_query_stale",
    )?;
    checked_observation(query, observed)?;
    let mut accepted = record.clone();
    accepted.observation_valid = true;
    Ok(accepted)
}

pub fn public_value(value: &serde_json::Value) -> serde_json::Value {
    use serde_json::{Value, json};
    match value {
        Value::Array(values) => Value::Array(values.iter().map(public_value).collect()),
        Value::Object(fields) => {
            if value.get("type").and_then(Value::as_str) == Some("brew-formula") {
                return json!({"type":"brew-formula","query":public_query(&value["query"]),"recipe_ref":value.get("recipe_ref")});
            }
            if value.get("kind").and_then(Value::as_str) == Some("brew-formula-native-v1") {
                return public_query(value);
            }
            let brew_plan =
                value.get("brew").is_some_and(Value::is_object) && fields.contains_key("package");
            let brew_journal = value
                .get("plan")
                .and_then(|p| p.get("brew"))
                .is_some_and(Value::is_object);
            let brew_observation = matches!(
                value.get("status").and_then(Value::as_str),
                Some("brew-installed" | "brew-missing")
            );
            let mut result = serde_json::Map::new();
            for (key, child) in fields {
                if (brew_observation && key == "evidence")
                    || ((brew_plan || brew_journal)
                        && matches!(
                            key.as_str(),
                            "recipe_sha256" | "plan_sha256" | "scope_sha256"
                        ))
                {
                    continue;
                }
                let projected = if brew_plan && key == "brew" {
                    json!({"schema_version":child["schema_version"],"goal":child["goal"],"execution":child["execution"],"context_id":child["context"]["context_id"],"epoch":child["context"]["epoch"],"effect_contract":child["confirmation"]["effect_contract"],"consent":child["confirmation"]["consent"]})
                } else {
                    public_value(child)
                };
                result.insert(key.clone(), projected);
            }
            Value::Object(result)
        }
        _ => value.clone(),
    }
}
fn public_query(query: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({"kind":"brew-formula-native-v1","schema_version":query["schema_version"],"prefix":query["key"]["canonical_prefix"],"full_name":query["key"]["full_name"],"program":query["binding"]["invocation"],"cellar":query["binding"]["cellar"],"context_id":query["binding"]["context_id"],"context_epoch":query["context_epoch"]})
}

#[cfg(test)]
#[path = "../../../tests/unit/brew_policy.rs"]
mod tests;
