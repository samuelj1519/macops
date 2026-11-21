use super::*;

type PreparedWorker = (tempfile::TempDir, Context, Store, FrozenInvocation, Journal);

fn prepared_worker(node_probe: &str, cli_probe: &str) -> ResultFixture<PreparedWorker> {
    let (temp, ctx, mut recipe) = fixture()?;
    fs::write(
        &recipe.runner.node,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then {node_probe}; fi\nif [ \"$2\" = --version ]; then {cli_probe}; fi\nprintf unexpected > \"$HOME/unexpected-native\"\nexit 97\n"
        ),
    )?;
    recipe.runner.sha256 = Some(disk::runner_stamp(&recipe)?);
    adopt(&ctx, &recipe)?;
    let store = Store::lock(&ctx)?;
    store.check_pending()?;
    let (frozen, plan) = plan(&ctx, recipe, Action::Upgrade, InputFence::default())?;
    let mut journal = crate::operations::new_journal(&ctx, plan)?;
    disk::write_new(&frozen_path(&journal), &frozen)?;
    persist_recipe(&ctx, &frozen)?;
    journal.phase = Phase::Running;
    journal.process_group = Some(std::process::id());
    store.save(&journal)?;
    let invocation = FrozenInvocation {
        command: journal.plan.command.clone().ok_or("command")?,
        env: disk::frozen_environment(&ctx)?,
        cwd: ctx.home.clone(),
        timeout_secs: ctx.timeout_secs,
    };
    Ok((temp, ctx, store, invocation, journal))
}

type ResultFixture<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const NODE_VERSION: &str = "printf 'v26.13.1\\n'; exit 0";
const SKILLS_VERSION: &str = "printf '1.9.0\\n'; exit 0";

#[test]
fn worker_rechecks_the_original_metadata_and_complete_receipt_before_launch() -> TestResult {
    let (_temp, ctx, _store, invocation, journal) = prepared_worker(NODE_VERSION, SKILLS_VERSION)?;
    let native_registry = fs::read(disk::lock_path(&ctx))?;
    let existing = fs::read(disk::payload(&ctx, "alpha")?.join("SKILL.md"))?;
    worker_guard(&journal, &invocation)?;

    let path = ctx.state.join("receipts.json");
    let original = fs::read(&path)?;
    let mut changed = original.clone();
    changed.push(b'\n');
    fs::write(&path, &changed)?;
    assert!(worker_guard(&journal, &invocation).is_err());
    assert_eq!(fs::read(path)?, changed);
    assert_eq!(fs::read(disk::lock_path(&ctx))?, native_registry);
    assert_eq!(
        fs::read(disk::payload(&ctx, "alpha")?.join("SKILL.md"))?,
        existing
    );
    assert!(!ctx.home.join("unexpected-native").exists());
    assert!(!ctx.state.join(after_name(&journal)).exists());
    Ok(())
}

#[test]
fn version_probe_failures_are_refused_before_native_skills_mutation() -> TestResult {
    for (node, cli, expected) in [
        ("exit 7", SKILLS_VERSION, None),
        (
            NODE_VERSION,
            "printf '1.8.0\\n'; exit 0",
            Some("native_skills_cli_version_unsupported"),
        ),
    ] {
        let (_temp, ctx, _store, invocation, journal) = prepared_worker(node, cli)?;
        let failure = worker_guard(&journal, &invocation)
            .err()
            .ok_or("unsupported runner")?;
        if let Some(code) = expected {
            assert_eq!(failure.code, code);
        }
        assert!(!ctx.home.join("unexpected-native").exists());
        assert!(!ctx.state.join(after_name(&journal)).exists());
    }
    Ok(())
}
