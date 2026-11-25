use super::*;
use crate::{cli::Cli, operations::new_journal, test_support::TestResult};
use clap::Parser as _;

#[test]
fn recovery_approval_cannot_follow_a_replaced_active_journal() -> TestResult {
    let directory = tempfile::tempdir_in("/private/tmp")?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        directory.path().to_str().ok_or("home")?,
        "app",
    ])?;
    let ctx = Context::from_cli(&cli, &mut Vec::new())?;
    let first = new_journal(&ctx, crate::test_support::journal()?.plan)?;
    let replacement = new_journal(&ctx, first.plan.clone())?;
    let store = Store::lock(&ctx)?;
    store.save(&first)?;
    drop(store);
    let operation = recover_before_write_with(
        &ctx,
        None,
        |snapshot| {
            assert_eq!(snapshot.id, first.id);
            let changed = Store::lock(&ctx)?;
            changed.save(&replacement)
        },
        |_| Ok(true),
    );
    assert_eq!(
        operation
            .err()
            .ok_or("a replacement needs its own approval")?
            .code,
        "plan_stale"
    );
    let current = crate::operations::store::active(&ctx)?.ok_or("active")?;
    assert_eq!(current.id, replacement.id);
    assert_eq!(current.phase, Phase::Prepared);
    assert_eq!(crate::operations::store::history(&ctx)?.len(), 2);
    Ok(())
}
