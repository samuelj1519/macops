use super::*;
use crate::{
    cli::Cli,
    test_support::TestResult,
    test_support::brew::{query, report, stamp},
};
use clap::Parser;
use std::os::unix::fs::PermissionsExt;

fn fixture()
-> std::result::Result<(tempfile::TempDir, Context, BrewQuerySpec), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let mut q = query()?;
    q.binding.metadata_scope.home = home;
    Ok((temp, ctx, q))
}
#[test]
fn context_registry_is_private_and_rebind_requires_new_actual_observation() -> TestResult {
    let (_temp, ctx, mut q) = fixture()?;
    let store = Store::lock(&ctx)?;
    assert!(read_registry(&store)?.contexts.is_empty());
    let initial = register_context(&ctx, &store, q.binding.clone(), BTreeMap::new())?;
    assert!(!initial.observation_valid);
    assert_eq!(
        std::fs::metadata(ctx.state.join(REGISTRY))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(register_context(&ctx, &store, q.binding.clone(), BTreeMap::new()).is_err());
    let old = policy::observation(&q, stamp(&q)?, report(&q, &[]))?;
    record_observation(&ctx, &store, &q, &old)?;
    assert!(read_registry(&store)?.contexts[&q.binding.context_id].observation_valid);
    let mut new = q.binding.clone();
    new.invocation = "/new/absolute/brew".into();
    let rebound = rebind_context(&ctx, &store, new.clone(), false)?;
    assert_eq!(rebound.epoch, 1);
    assert!(!rebound.observation_valid);
    assert!(record_observation(&ctx, &store, &q, &old).is_err());
    q.binding = new;
    q.context_epoch = 1;
    q.env_capture_id = "ENV-2".into();
    let fresh = policy::observation(&q, stamp(&q)?, report(&q, &[]))?;
    record_observation(&ctx, &store, &q, &fresh)?;
    assert!(read_registry(&store)?.contexts[&q.binding.context_id].observation_valid);
    let mut wrong = ctx.clone();
    wrong.home = ctx.home.join("other");
    assert!(record_observation(&wrong, &store, &q, &fresh).is_err());
    Ok(())
}
#[test]
fn corrupted_registry_and_unknown_context_never_create_authority() -> TestResult {
    let (_temp, ctx, q) = fixture()?;
    let store = Store::lock(&ctx)?;
    assert!(rebind_context(&ctx, &store, q.binding.clone(), true).is_err());
    let missing = policy::observation(&q, stamp(&q)?, report(&q, &[]))?;
    assert!(record_observation(&ctx, &store, &q, &missing).is_err());
    for bytes in [
        b"{\"schema_version\":1,\"schema_version\":1,\"contexts\":{}}".as_slice(),
        b"{\"schema_version\":1,\"contexts\":{\"x\":{},\"x\":{}}}",
        b"[1,{}]",
    ] {
        std::fs::write(ctx.state.join(REGISTRY), bytes)?;
        std::fs::set_permissions(
            ctx.state.join(REGISTRY),
            std::fs::Permissions::from_mode(0o600),
        )?;
        assert_eq!(
            read_registry(&store).err().ok_or("invalid registry")?.code,
            "invalid_json"
        );
    }
    Ok(())
}

#[test]
fn registry_permission_change_is_not_silently_repaired() -> TestResult {
    let (_temp, ctx, q) = fixture()?;
    let store = Store::lock(&ctx)?;
    register_context(&ctx, &store, q.binding, BTreeMap::new())?;
    std::fs::set_permissions(
        ctx.state.join(REGISTRY),
        std::fs::Permissions::from_mode(0o644),
    )?;
    assert_eq!(
        read_registry(&store).err().ok_or("mode")?.code,
        "brew_registry_not_private"
    );
    assert_eq!(
        std::fs::metadata(ctx.state.join(REGISTRY))?
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    Ok(())
}
