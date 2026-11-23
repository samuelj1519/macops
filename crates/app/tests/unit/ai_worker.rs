use super::*;
use crate::{ai_state::Mise, cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{fs, path::PathBuf};

struct Fixture {
    _root: tempfile::TempDir,
    context: Context,
    recipe: Upgrade,
    operation: Journal,
    store: Store,
    profile: PathBuf,
}

impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let root = tempfile::tempdir_in("/private/tmp")?;
        let home = root.path().join("home");
        let live = home.join(".config/mise/config.toml");
        let source = home.join(".local/share/chezmoi/dot_config/mise/private_config.toml");
        let profile = home.join(".claude-mi/settings.json");
        for path in [&live, &source, &profile] {
            fs::create_dir_all(path.parent().ok_or("fixture parent")?)?;
        }
        let key = crate::policy::config_tx::ai_mise_spec("claude")?;
        let old_pin = format!("[tools]\n\"{key}\" = \"1.0.0\"\n");
        fs::write(&live, &old_pin)?;
        fs::write(&source, &old_pin)?;
        fs::write(&profile, br#"{"model":"custom-mi"}"#)?;
        let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "app"])?;
        let context = Context::from_cli(&cli, &mut vec![])?;
        let target = old_pin.replace("1.0.0", "2.0.0");
        let recipe = Upgrade {
            isolated: context.isolated,
            tool: "claude".into(),
            version: Some("2.0.0".into()),
            force_live: false,
            mise: Some(Mise {
                live: Snapshot::capture(&live)?,
                source: Snapshot::capture(&source)?,
                prev_live_pin: "1.0.0".into(),
                expected_live: target.clone(),
                expected_source: target,
                root_digest: config_tx::root_digest(&context)?,
                roots: context.chezmoi.clone(),
            }),
            profiles: ai_state::profiles(&context)?,
        };
        let mut plan = crate::test_support::journal()?.plan;
        plan.package.id = "ai-native:claude".into();
        plan.recipe_sha256 = fingerprint(&plan.package)?;
        plan.action = crate::domain::Action::Upgrade;
        plan.command = Some(CommandSpec {
            program: home.join("fixture-mise"),
            args: vec!["use".into(), "--global".into(), format!("{key}@2.0.0")],
        });
        plan.warnings = vec![format!("ai-native-v1:{}", fingerprint(&recipe)?)];
        let operation = operations::new_journal(&context, plan)?;
        let store = Store::lock(&context)?;
        ai_state::save(&store, &operation, &recipe)?;
        Ok(Self {
            _root: root,
            context,
            recipe,
            operation,
            store,
            profile,
        })
    }
}

#[test]
fn worker_uses_only_the_frozen_command_for_the_allowed_phase_and_a_matching_profile() -> TestResult
{
    let fixture = Fixture::new()?;
    let mut unrelated = fixture.operation.clone();
    unrelated.plan.warnings.clear();
    assert!(worker_command(&unrelated)?.is_none());
    ai_state::step(&fixture.store, &fixture.operation, Step::Inspect)?;
    assert_eq!(
        worker_command(&fixture.operation)?,
        fixture.operation.plan.command
    );

    fs::write(&fixture.profile, br#"{"model":"independent change"}"#)?;
    let error = worker_command(&fixture.operation)
        .err()
        .ok_or("changed profile")?;
    assert_eq!(error.code, "claude_profile_changed");
    assert_eq!(
        fs::read(&fixture.profile)?,
        br#"{"model":"independent change"}"#
    );
    fs::write(&fixture.profile, br#"{"model":"custom-mi"}"#)?;
    let live = fixture
        .recipe
        .mise
        .as_ref()
        .ok_or("mise files")?
        .live
        .anchor
        .requested
        .clone();
    fs::write(&live, b"[tools]\nnode = \"22\"\n")?;
    let error = worker_command(&fixture.operation)
        .err()
        .ok_or("changed live pin")?;
    assert_eq!(error.code, "plan_stale");
    assert_eq!(fs::read(live)?, b"[tools]\nnode = \"22\"\n");
    Ok(())
}

#[test]
fn worker_reshim_is_bound_to_the_original_program_and_rejects_unapproved_steps() -> TestResult {
    let fixture = Fixture::new()?;
    for step in [Step::Rollback, Step::Reconcile] {
        ai_state::step(&fixture.store, &fixture.operation, step)?;
        let error = worker_command(&fixture.operation)
            .err()
            .ok_or("unapproved phase")?;
        assert_eq!(error.code, "ai_worker_step");
    }
    ai_state::step(&fixture.store, &fixture.operation, Step::Reshim)?;
    let command = worker_command(&fixture.operation)?.ok_or("reshim")?;
    assert_eq!(command.args, vec!["reshim"]);
    assert_eq!(command.program, fixture.context.home.join("fixture-mise"));

    let mut changed = fixture.recipe.clone();
    changed.version = Some("3.0.0".into());
    Store::atomic(
        &fixture.store.root,
        &format!("ai-{}.json", fixture.operation.id),
        &changed,
    )?;
    let error = worker_command(&fixture.operation)
        .err()
        .ok_or("changed plan")?;
    assert_eq!(error.code, "ai_plan_changed");
    assert_eq!(fs::read(&fixture.profile)?, br#"{"model":"custom-mi"}"#);
    Ok(())
}

#[test]
fn rollback_reuses_the_recorded_live_pin_without_overwriting_an_independent_model_change()
-> TestResult {
    let mut fixture = Fixture::new()?;
    let mise = fixture.recipe.mise.as_ref().ok_or("mise")?;
    let live = &mise.live.anchor.requested;
    let source = &mise.source.anchor.requested;
    fs::write(live, &mise.expected_live)?;
    let post = Snapshot::capture(live)?;
    Store::atomic(
        &fixture.store.root,
        &format!("ai-{}-post.json", fixture.operation.id),
        &post,
    )?;
    ai_state::step(&fixture.store, &fixture.operation, Step::Rollback)?;
    fs::write(&fixture.profile, br#"{"model":"independent new model"}"#)?;

    let error = rollback(&fixture.store, &mut fixture.operation, &fixture.recipe)
        .err()
        .ok_or("model change must block reshim")?;
    assert_eq!(error.code, "claude_profile_changed");
    let name = format!("ai-{}-rollback.json", fixture.operation.id);
    let recovered = ai_state::snapshot(&fixture.store, &name)?.ok_or("recorded rollback")?;
    assert_eq!(recovered, Snapshot::capture(live)?);
    assert_eq!(recovered.text, mise.live.text);
    assert_eq!(recovered.mode, mise.live.mode);
    assert_eq!(fs::read_to_string(source)?, mise.source.text);
    assert_eq!(
        fs::read(&fixture.profile)?,
        br#"{"model":"independent new model"}"#
    );
    assert_eq!(
        ai_state::read_step(&fixture.store, &fixture.operation)?,
        Step::Rollback
    );

    let error = rollback(&fixture.store, &mut fixture.operation, &fixture.recipe)
        .err()
        .ok_or("resumed model conflict")?;
    assert_eq!(error.code, "claude_profile_changed");
    assert_eq!(Snapshot::capture(live)?, recovered);
    Ok(())
}

#[test]
fn stale_or_incomplete_live_pin_evidence_cannot_be_written_back_to_the_mise_source() -> TestResult {
    let mut fixture = Fixture::new()?;
    let mise = fixture.recipe.mise.as_ref().ok_or("mise")?;
    let live = mise.live.anchor.requested.clone();
    let source = mise.source.anchor.requested.clone();
    let original = fs::read(&source)?;
    let error = reconcile(
        &fixture.context,
        &fixture.store,
        &mut fixture.operation,
        &fixture.recipe,
    )
    .err()
    .ok_or("live pin must reach the target before write-back")?;
    assert_eq!(error.code, "ai_live_changed");
    assert_eq!(fs::read(&source)?, original);
    fs::write(live, &mise.expected_live)?;
    fixture.recipe.version = None;
    let error = reconcile(
        &fixture.context,
        &fixture.store,
        &mut fixture.operation,
        &fixture.recipe,
    )
    .err()
    .ok_or("missing target")?;
    assert_eq!(error.code, "ai_plan_invalid");
    assert_eq!(fs::read(source)?, original);
    assert_eq!(fs::read(&fixture.profile)?, br#"{"model":"custom-mi"}"#);
    Ok(())
}
