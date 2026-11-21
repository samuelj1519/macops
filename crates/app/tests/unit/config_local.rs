#![cfg(test)]
use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{fs, os::unix::fs::PermissionsExt};

fn test_context(home: &Path) -> Result<Context> {
    let mut warnings = Vec::new();
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(|| Error::new("utf8", "home"))?,
        "config",
        "diff",
    ])
    .map_err(|e| Error::new("parse", e.to_string()))?;
    Context::from_cli(&cli, &mut warnings)
}

#[test]
fn anchored_config_capture_and_observation_detect_parent_and_type_changes() -> TestResult {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let source = home.join(".local/share/chezmoi");
    fs::create_dir_all(&source)?;
    fs::write(source.join("private_dot_secret"), "SYNTHETIC_SECRET_1")?;
    let target = home.join(".secret");
    fs::write(&target, "SYNTHETIC_SECRET_1")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;

    let ctx = test_context(&home)?;
    let pkg = resolve_package(&ctx, target.to_str().ok_or("utf8")?)?.ok_or("resolved")?;
    let query = capture(&pkg)?;
    let observed = observe(&query, &mut (2 * 1024 * 1024))?;
    assert!(!serde_json::to_string(&observed)?.contains("SYNTHETIC_SECRET_1"));
    policy::adoptable_observation(&pkg, &observed)?;

    fs::remove_file(&target)?;
    let missing_obs = observe(&query, &mut (2 * 1024 * 1024))?;
    assert!(matches!(
        missing_obs,
        Observation::ConfigFile { target: None, .. }
    ));

    symlink(source.join("private_dot_secret"), &target)?;
    assert_eq!(
        observe(&query, &mut (2 * 1024 * 1024))
            .err()
            .ok_or("symlink")?
            .code,
        "unsupported_config_file"
    );
    fs::remove_file(&target)?;
    fs::write(&target, vec![b'x'; usize::try_from(FILE_LIMIT + 1)?])?;
    assert_eq!(
        observe(&query, &mut (2 * 1024 * 1024))
            .err()
            .ok_or("oversize")?
            .code,
        "config_file_limit"
    );
    assert_eq!(
        observe(&query, &mut 0).err().ok_or("zero budget")?.code,
        "config_file_limit"
    );

    let sub = home.join("subdir");
    fs::create_dir_all(&sub)?;
    let sub_cfg = sub.join("app.toml");
    fs::write(&sub_cfg, "key = 1\n")?;
    let sub_pkg = resolve_package(&ctx, sub_cfg.to_str().ok_or("utf8")?)?.ok_or("sub")?;
    let sub_query = capture(&sub_pkg)?;
    fs::remove_dir_all(&sub)?;
    fs::create_dir_all(&sub)?;
    fs::write(&sub_cfg, "key = 1\n")?;
    assert_eq!(
        observe(&sub_query, &mut (2 * 1024 * 1024))
            .err()
            .ok_or("rebound")?
            .code,
        "config_namespace_changed"
    );
    Ok(())
}

#[test]
fn resolve_and_refresh_block_conflicts_and_uncertain_authority() -> TestResult {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let source = home.join(".local/share/chezmoi");
    fs::create_dir_all(home.join(".config/demo"))?;
    fs::create_dir_all(&source)?;
    let untracked = home.join(".config/demo/settings.json");
    fs::write(&untracked, "{}")?;
    let ctx = test_context(&home)?;
    let id = policy::config_id(&untracked)?;
    let resolved = resolve_package(&ctx, &id)?.ok_or("resolved by id")?;
    assert_eq!(refresh_package(&ctx, &resolved)?.id, id);

    fs::write(source.join("modify_dot_zshrc"), "#!/bin/sh\n")?;
    assert_eq!(
        refresh_package(&ctx, &resolved)
            .err()
            .ok_or("stale on uncertain authority")?
            .code,
        "plan_stale"
    );
    assert_eq!(
        resolve_package(&ctx, &id).err().ok_or("uncertain")?.code,
        "config_authority_uncertain"
    );
    fs::remove_file(source.join("modify_dot_zshrc"))?;

    let conflict_target = home.join(".conflict");
    fs::write(source.join("dot_conflict"), "a")?;
    fs::write(source.join("private_dot_conflict"), "b")?;
    assert_eq!(
        resolve_package(&ctx, conflict_target.to_str().ok_or("utf8")?)
            .err()
            .ok_or("conflict")?
            .code,
        "owner_conflict"
    );
    fs::remove_file(source.join("dot_conflict"))?;
    fs::remove_file(source.join("private_dot_conflict"))?;

    Ok(())
}

#[test]
fn diff_and_receipt_views_cover_missing_destinations_budgets_and_rebound_parents() -> TestResult {
    let temp = tempfile::Builder::new()
        .prefix("macops-cfg-unit-")
        .tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let source = home.join("source");
    let missing_dest = home.join("missing-dest");
    fs::create_dir_all(&source)?;
    fs::write(source.join("dot_zshrc"), "export A=1\n")?;

    let mut warnings = Vec::new();
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or("home")?,
        "--chezmoi-source",
        source.to_str().ok_or("src")?,
        "--chezmoi-destination",
        missing_dest.to_str().ok_or("dst")?,
        "config",
        "diff",
    ])
    .map_err(|e| Error::new("parse", e.to_string()))?;
    let ctx = Context::from_cli(&cli, &mut warnings)?;
    assert!(resolve_package(&ctx, "short-name")?.is_none());
    assert!(capture(&crate::test_support::package()).is_err());
    assert!(refresh_package(&ctx, &crate::test_support::package()).is_err());

    fs::create_dir_all(&missing_dest)?;
    let target = missing_dest.join(".zshrc");
    fs::write(&target, "export A=1\n")?;
    let pkg = resolve_package(&ctx, target.to_str().ok_or("tgt")?)?.ok_or("pkg")?;
    let mut changed_pkg = pkg.clone();
    changed_pkg.provider = Provider::ConfigObserved {
        target: target.clone(),
        source: None,
        authority: ConfigAuthority::Untracked,
    };
    assert_eq!(
        refresh_package(&ctx, &changed_pkg)
            .err()
            .ok_or("stale")?
            .code,
        "plan_stale"
    );

    let plan = crate::operations::plan(&ctx, &pkg, crate::domain::Action::Adopt, None, false)?;
    crate::operations::execute(&ctx, plan, false, true)?;
    assert_eq!(
        resolve_package(&ctx, &pkg.id)?.ok_or("by receipt id")?.id,
        pkg.id
    );

    fs::remove_file(source.join("dot_zshrc"))?;
    fs::remove_dir_all(&missing_dest)?;
    fs::create_dir_all(&missing_dest)?;
    let (_, cov) = crate::resource_configs::collect(&ctx, &[], None)?;
    assert!(
        cov.iter()
            .any(|c| c.issues.iter().any(|i| i == "adopted_config_unobserved"))
    );

    Ok(())
}
