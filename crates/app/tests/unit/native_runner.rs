#![cfg(test)]
use super::*;
use crate::{cli::Cli, context::Context, test_support::TestResult};
use clap::Parser;
use std::os::unix::fs::PermissionsExt;

fn fixture()
-> std::result::Result<(tempfile::TempDir, Context, Journal), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let mut journal = crate::test_support::journal()?;
    journal.home = ctx.home.clone();
    journal.scope = ctx.scope.clone();
    journal.scope_sha256 = fingerprint(&ctx.scope)?;
    journal.state = ctx.state.clone();
    journal.settings = ctx.settings.clone();
    journal.cache = ctx.cache.clone();
    journal.phase = Phase::Prepared;
    journal.process_group = None;
    journal.plan.command = Some(CommandSpec {
        program: "/bin/true".into(),
        args: vec![],
    });
    journal.plan_sha256 = fingerprint(&journal.plan)?;
    Ok((temp, ctx, journal))
}
fn invocation(journal: &Journal) -> Result<FrozenInvocation> {
    Ok(FrozenInvocation {
        command: journal.plan.command.clone().ok_or_else(invalid)?,
        env: BTreeMap::from([("HOME".into(), journal.home.display().to_string())]),
        cwd: journal.home.clone(),
        timeout_secs: journal.plan.timeout_secs,
    })
}
#[test]
fn keeper_preserves_the_original_home_lock_until_its_last_reference_drops() -> TestResult {
    let (_temp, ctx, _journal) = fixture()?;
    let store = Store::lock(&ctx)?;
    let (keeper, home) = store.native_home_keeper()?;
    assert_eq!(home.requested, ctx.home);
    assert_eq!(crate::domain::FileId::from_file(&keeper)?, home.identity);
    assert!(rustix::io::fcntl_getfd(&keeper)?.contains(rustix::io::FdFlags::CLOEXEC));
    drop(store);
    assert!(Store::lock(&ctx).is_err());
    drop(keeper);
    let released = Store::lock(&ctx)?;
    released.verify_namespace()?;
    Ok(())
}
#[test]
fn prepare_requires_private_state_valid_scope_and_never_replaces_an_existing_intent() -> TestResult
{
    let (_temp, ctx, mut journal) = fixture()?;
    let store = Store::lock(&ctx)?;
    let call = invocation(&journal)?;
    let frozen = prepare(&store, &journal, &call)?;
    assert_eq!(binding(&store, &journal)?, frozen);
    assert!(prepare(&store, &journal, &call).is_err());
    assert!(!ctx.home.join("effect").exists());
    journal.phase = Phase::Running;
    journal.process_group = Some(1_000_000);
    assert!(prepare(&store, &journal, &call).is_err());
    assert!(launch(&store, &mut journal, &frozen).is_err());
    for variant in 0..16 {
        let mut changed = frozen.clone();
        match variant {
            0 => changed.operation_id.push('x'),
            1 => changed.plan_sha256.push('x'),
            2 => changed.scope_sha256.push('x'),
            3 => changed.schema_version = 3,
            4 => changed.nonce = "../".into(),
            5 => changed.completion_name = "other".into(),
            6 => changed.state.requested = ctx.home.clone(),
            7 => changed.invocation_sha256.push('x'),
            8 => changed.invocation.command.program = "relative".into(),
            9 => changed
                .invocation
                .env
                .insert("bad=key".into(), "x".into())
                .map_or((), |_| ()),
            10 => changed
                .invocation
                .env
                .insert("key".into(), "bad\0value".into())
                .map_or((), |_| ()),
            11 => changed.invocation.timeout_secs = 0,
            12 => changed.home.requested = ctx.state.clone(),
            13 => changed.controller_pid = 0,
            14 => changed.gate_name = "foreign.json".into(),
            _ => changed.home.mode = 0o777,
        }
        assert!(validate(&journal, &changed).is_err());
    }
    std::fs::set_permissions(&ctx.state, std::fs::Permissions::from_mode(0o755))?;
    assert!(private_state(&ctx.state).is_err());
    Ok(())
}
#[test]
fn observe_needs_original_nonce_worker_and_exit_proof_and_cannot_infer_from_payload() -> TestResult
{
    let (_temp, ctx, mut journal) = fixture()?;
    let store = Store::lock(&ctx)?;
    let frozen = prepare(&store, &journal, &invocation(&journal)?)?;
    journal.phase = Phase::Running;
    journal.process_group = Some(1_000_000);
    store.save(&journal)?;
    std::fs::write(ctx.home.join("effect"), "payload already exists")?;
    assert!(matches!(
        observe(&store, &journal, &frozen)?,
        NativeOutcome::Unknown { .. }
    ));
    let worker = WorkerBinding {
        binding_sha256: fingerprint(&frozen)?,
        worker_pid: 1_000_000,
        process_group: 1_000_000,
    };
    Store::atomic_new(&store.root, &name(&journal, "worker"), &worker)?;
    assert_eq!(
        observe(&store, &journal, &frozen)?,
        unknown("native_completion_missing")
    );
    let completion = Completion {
        binding_sha256: fingerprint(&frozen)?,
        worker: worker.clone(),
        child_pid: 999_999,
        code: Some(0),
        signal: None,
        uncertain: false,
    };
    Store::atomic_new(&store.root, &frozen.completion_name, &completion)?;
    assert_eq!(
        observe(&store, &journal, &frozen)?,
        NativeOutcome::Exited {
            code: Some(0),
            signal: None
        }
    );
    for variant in 0..7 {
        let mut changed = completion.clone();
        match variant {
            0 => changed.binding_sha256.push('x'),
            1 => changed.worker.worker_pid += 1,
            2 => changed.child_pid = 0,
            3 => changed.signal = Some(9),
            4 => changed.code = None,
            5 => changed.uncertain = true,
            _ => {
                changed.code = None;
                changed.signal = Some(15);
            }
        }
        Store::atomic(&store.root, &frozen.completion_name, &changed)?;
        let result = observe(&store, &journal, &frozen)?;
        if variant == 6 {
            assert_eq!(
                result,
                NativeOutcome::Exited {
                    code: None,
                    signal: Some(15)
                }
            );
        } else {
            assert!(matches!(result, NativeOutcome::Unknown { .. }));
        }
    }
    std::fs::set_permissions(
        ctx.state.join(&frozen.completion_name),
        std::fs::Permissions::from_mode(0o644),
    )?;
    assert!(matches!(
        observe(&store, &journal, &frozen)?,
        NativeOutcome::Unknown { .. }
    ));
    Ok(())
}

#[test]
fn owned_native_wait_retains_timeout_and_excess_output_as_uncertain() -> TestResult {
    use std::process::Command;

    let temp = tempfile::tempdir_in("/private/tmp")?;
    let captured = tempfile::tempfile_in(temp.path())?;
    let diagnostics = tempfile::tempfile_in(temp.path())?;
    let mut child = Command::new("/bin/sleep")
        .arg("10")
        .process_group(0)
        .spawn()?;
    let deadline = Instant::now()
        .checked_sub(Duration::from_secs(2))
        .ok_or("timeout fixture")?;
    verify_group_observer(child.id())?;
    let (exit, uncertain) = wait_native(&mut child, &captured, &diagnostics, deadline, 1)?;
    assert_eq!(exit.signal(), Some(nix::libc::SIGKILL));
    assert!(uncertain);
    assert!(child.try_wait()?.is_some());

    let mut child = Command::new("/usr/bin/true").process_group(0).spawn()?;
    assert!(child.wait()?.success());
    captured.set_len(4 * 1024 * 1024)?;
    let (exit, uncertain) = wait_native(&mut child, &captured, &diagnostics, Instant::now(), 1)?;
    assert!(exit.success());
    assert!(!uncertain);
    diagnostics.set_len(1)?;
    let (exit, uncertain) = wait_native(&mut child, &captured, &diagnostics, Instant::now(), 1)?;
    assert!(exit.success());
    assert!(uncertain);
    Ok(())
}
