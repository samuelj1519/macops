use super::*;
use std::os::unix::fs::PermissionsExt;

// Construct the legacy LocalV1 wire state; production no longer offers this writer.
pub(super) fn apply(ctx: &Context, key: &str, dry: bool, _yes: bool) -> Result<Value> {
    let package = config_local::resolve_package(ctx, key)?
        .ok_or_else(|| Error::attention("not_found", "legacy fixture target"))?;
    if !matches!(
        package.provider,
        Provider::ConfigObserved {
            authority: ConfigAuthority::ChezmoiCandidate,
            source: Some(_),
            ..
        }
    ) {
        return Err(Error::attention(
            "unsupported_config_apply",
            "legacy wire fixture requires a bound source",
        ));
    }
    let query = config_local::capture(&package)?;
    let before = config_local::observe(&query, &mut (2 * FILE_LIMIT))?;
    let Observation::ConfigFile {
        target: ref target_stamp,
        source: Some(ref source_stamp),
        ..
    } = before
    else {
        return Err(Error::attention(
            "config_source_missing",
            "legacy fixture source missing",
        ));
    };
    let mapping = config_inventory::discover(ctx);
    let receipts = crate::catalog::target::receipts(ctx)?;
    let warnings = policy::validate_static_apply_eligibility(
        &mapping.coverage.state,
        &mapping.coverage.issues,
        receipts.installations.get(&package.id),
        target_stamp.as_ref(),
    )?;
    let source = query
        .source
        .as_ref()
        .ok_or_else(|| Error::new("fixture", "source"))?;
    let dir = Directory::open(&source.canonical_parent)?;
    let bytes = dir
        .read_regular(OsStr::new(&source.name), FILE_LIMIT)?
        .ok_or_else(|| Error::new("fixture", "source missing"))?;
    let mode = if query
        .expected_mode
        .ok_or_else(|| Error::new("fixture", "mode"))?
        .private
    {
        0o600
    } else {
        0o644
    };
    if target_stamp
        .as_ref()
        .is_some_and(|t| t.content_digest == source_stamp.content_digest)
        && receipts
            .installations
            .get(&package.id)
            .is_some_and(|r| r.observation == before)
    {
        return Ok(json!({"mode":"no-op","persistentWrites":false}));
    }
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: ctx.timeout_secs,
        recipe_sha256: fingerprint(&package)?,
        package,
        action: Action::Apply,
        before: before.clone(),
        target: Some(source_stamp.content_digest.clone()),
        command: None,
        query: Some(QuerySpec::LocalConfig(query.clone())),
        warnings,
        synchronous: true,
        delegated: false,
    };
    if dry {
        return Ok(
            json!({"mode":"dry-run","persistentWrites":false,"plan":plan,"desired_mode":mode}),
        );
    }
    seed(ctx, plan, &query, &bytes, mode)
}

fn seed(
    ctx: &Context,
    plan: Plan,
    query: &LocalConfigQuery,
    bytes: &[u8],
    mode: u32,
) -> Result<Value> {
    let Observation::ConfigFile {
        target: target_stamp,
        source: Some(source_stamp),
        ..
    } = plan.before.clone()
    else {
        return Err(Error::new("fixture", "before"));
    };
    let store = Store::lock(ctx)?;
    store.check_pending()?;
    let mut journal = operations::new_journal(ctx, plan)?;
    let before_bytes = target_stamp
        .as_ref()
        .map(|_| paths::regular_bytes(&query.target.requested))
        .transpose()?;
    let backup = ConfigBackupRecord {
        schema_version: 1,
        operation_id: journal.id.clone(),
        config_id: journal.plan.package.id.clone(),
        target: query.target.requested.clone(),
        parent_canonical: query.target.canonical_parent.clone(),
        parent_identity: query.target.parent_identity.clone(),
        before_stamp: target_stamp.clone(),
        expected_after_digest: source_stamp.content_digest.clone(),
        expected_after_mode: mode,
        expected_absent: false,
        reconcile: None,
    };
    write_backup(&store, &backup, before_bytes.as_deref())?;
    let temporary = query
        .target
        .canonical_parent
        .join(format!(".legacy-fixture-{}", journal.id));
    std::fs::write(&temporary, bytes)?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&temporary, &query.target.requested)?;
    let observation = config_local::observe(query, &mut (2 * FILE_LIMIT))?;
    let Observation::ConfigFile {
        target: Some(ref candidate),
        ..
    } = observation
    else {
        return Err(Error::new("fixture", "candidate"));
    };
    let directory = Directory::open(&backup_dir(&store.path, &journal.id))?;
    Store::atomic(directory.file(), "candidate.json", candidate)?;
    store.commit(&journal, observation)?;
    journal.phase = Phase::Succeeded;
    journal.message =
        "Legacy LocalV1 fixture matching the checked pre-migration wire format".into();
    store.save(&journal)?;
    Ok(json!(journal))
}
