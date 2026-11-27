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

#[test]
fn recovery_output_points_at_original_scope_and_keeps_unverified_result_distinct() {
    let journal = json!({
        "id":"OP-1","phase":"unknown","home":"/fixture/home","state":"/fixture/state",
        "plan":{"package":{"id":"ai-native:codex","name":"codex"},"target":"2.0.0"}
    });
    let preview = recovery_text(&json!({"journal":journal}), true);
    for expected in [
        "核对预演（只读）",
        "/fixture/home",
        "/fixture/state",
        "计划版本 2.0.0",
    ] {
        assert!(preview.contains(expected), "{preview}");
    }
    let unknown = recovery_text(&json!({"journal":journal}), false);
    assert!(unknown.contains("mo history OP-1") && !unknown.contains("--mark-failed"));
    for (blocker, next) in [
        ("claude_profile_changed", "保留上面列出的档案改动"),
        ("ai_source_changed", "mo config reconcile"),
        ("fixture_payload_changed", "mo agent tools"),
    ] {
        let text = recovery_text(&json!({"journal":journal,"recoveryBlocker":blocker}), false);
        assert!(text.contains(next), "{text}");
        assert!(text.contains("mo recover OP-1 --mark-failed"));
    }
    let prepared = recovery_text(
        &json!({"pending":[{"id":"OP-1","phase":"prepared","plan":{"package":{"name":"codex"}}}]}),
        false,
    );
    assert!(prepared.contains("已准备") && prepared.contains("mo recover OP-1 --dry-run"));
}
