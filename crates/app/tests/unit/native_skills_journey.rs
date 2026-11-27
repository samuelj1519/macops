#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::fs;

fn installed_recipe(ctx: &Context, fallback: &Recipe) -> Result<Recipe> {
    let (_, bytes) = disk::witness(&disk::lock_path(ctx))?;
    let rows = policy::registry(bytes.as_deref())?;
    public_skill::from_record(ctx, "alpha", &rows["alpha"], fallback.runner.clone())
}

fn another(recipe: &Recipe, name: &str) -> Recipe {
    let mut recipe = recipe.clone();
    recipe.id = name.into();
    recipe.install_name = name.into();
    recipe.source.member_name = name.into();
    recipe.source.skill_directory = format!("skills/{name}");
    recipe
}

#[test]
fn plan_keeps_compatible_installed_members_and_refuses_a_changed_snapshot() -> TestResult {
    let (_tmp, ctx, fallback) = super::super::tests::fixture()?;
    let installed = installed_recipe(&ctx, &fallback)?;
    let beta = another(&fallback, "beta");
    let recipes = vec![installed.clone(), beta];
    let plan = plan_recipes(&ctx, "owner/repo", recipes.clone())?;
    assert!(plan.verify_batch().is_ok());
    let preview = plan.run(&ctx, true, true, true)?;
    assert_eq!(preview["available"], json!(["beta"]));
    assert_eq!(preview["alreadyPresent"], json!(["alpha"]));
    assert_eq!(preview["persistentWrites"], false);
    assert!(!ctx.state.exists());
    let already_installed = plan_recipes(&ctx, "owner/repo", vec![installed.clone()])?;
    assert_eq!(
        already_installed.run(&ctx, false, true, true)?["status"],
        "current"
    );

    let stale = plan_recipes(&ctx, "owner/repo", recipes)?;
    fs::write(
        disk::payload(&ctx, "alpha")?.join("SKILL.md"),
        b"---\nname: alpha\n---\nchanged\n",
    )?;
    assert!(stale.verify_batch().is_err());
    let mut conflict = installed;
    conflict.source.repository = "another/source".into();
    let conflicting = plan_recipes(&ctx, "another/source", vec![conflict.clone()])?;
    assert_eq!(
        conflicting
            .run(&ctx, false, true, true)
            .err()
            .ok_or("conflict")?
            .code,
        "skill_install_conflict"
    );
    let normal = plan_recipes(&ctx, "another/source", vec![conflict])?;
    assert_eq!(
        normal
            .run(&ctx, false, true, false)
            .err()
            .ok_or("existing")?
            .code,
        "skills_already_present"
    );
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn released_registration_is_reused_only_for_same_repository_and_member() -> TestResult {
    let (_tmp, ctx, fallback) = super::super::tests::fixture()?;
    let prior = installed_recipe(&ctx, &fallback)?;
    crate::capability::scoped_files::private(&ctx.config)?;
    let book = policy::merge(policy::book(None)?, &prior)?;
    fs::write(super::super::book_path(&ctx), disk::encode(&book)?)?;
    fs::remove_dir_all(disk::payload(&ctx, "alpha")?)?;
    fs::remove_file(disk::lock_path(&ctx))?;

    let mut desired = prior.clone();
    desired.id = "candidate".into();
    desired.source.commit = "c".repeat(40);
    let planned = plan_recipes(&ctx, "owner/repo", vec![desired.clone()])?;
    assert_eq!(planned.available[0].id, prior.id);
    assert_eq!(planned.available[0].source.commit, desired.source.commit);
    assert_eq!(planned.existing, [] as [String; 0]);
    desired.source.member_name = "someone-else".into();
    let conflicting = plan_recipes(&ctx, "owner/repo", vec![desired])?;
    assert_eq!(conflicting.available, [] as [Recipe; 0]);
    assert_eq!(conflicting.existing, ["alpha"]);
    assert!(conflicting.verify_batch().is_err());
    assert!(!ctx.state.exists());
    Ok(())
}

#[test]
fn adopt_batch_keeps_completed_member_and_names_every_unattempted_member() -> TestResult {
    let (_tmp, ctx, first) = super::super::tests::fixture()?;
    let broken = another(&first, "unavailable");
    let deferred = another(&first, "later");
    assert!(
        run_members(
            &ctx,
            vec![broken.clone()],
            Action::Adopt,
            &InputFence::default()
        )
        .is_err()
    );
    let result = run_members(
        &ctx,
        vec![first, broken, deferred],
        Action::Adopt,
        &InputFence::default(),
    )?;
    assert_eq!(result["results"][0]["journal"]["phase"], "succeeded");
    assert_eq!(result["results"][1]["name"], "unavailable");
    assert_eq!(result["unattempted"], json!(["later"]));
    assert_eq!(result["incomplete"], true);
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}

#[test]
fn shared_install_uses_verified_runner_once_per_member_and_reports_remaining_work() -> TestResult {
    let (_tmp, ctx, fallback) = super::super::tests::fixture()?;
    let installed = installed_recipe(&ctx, &fallback)?;
    let mut incoming = vec![installed];
    for wanted in ["beta", "gamma", "last"] {
        let mut candidate = another(&fallback, wanted);
        candidate.runner.sha256 = None;
        incoming.push(candidate);
    }
    let plan = plan_recipes(&ctx, "owner/repo", incoming)?;
    let mut called = Vec::new();
    let report = plan.run_with(&ctx, false, true, true, |actual, recipe, action, fence| {
        assert_eq!(actual.home, ctx.home);
        assert_eq!(action, Action::Install);
        assert_eq!(recipe.runner.sha256, fallback.runner.sha256);
        fence.verify()?;
        called.push(recipe.install_name);
        Ok(if called.len() == 1 {
            json!({"journal":{"phase":"succeeded"}})
        } else {
            json!({"journal":{"phase":"unknown"}})
        })
    })?;
    assert_eq!(called, ["beta", "gamma"]);
    assert_eq!(report["skillAction"], "install");
    assert_eq!(report["source"], "owner/repo");
    assert_eq!(report["unattempted"], json!(["last"]));
    assert_eq!(report["incomplete"], true);
    assert!(disk::payload(&ctx, "alpha")?.join("SKILL.md").exists());
    Ok(())
}

#[test]
fn recommended_upgrade_preserves_fixed_version_if_user_cancels_new_target() -> TestResult {
    use crate::policy::source_git::ObjectId;
    let (_tmp, ctx, fallback) = super::super::tests::fixture()?;
    let recipe = installed_recipe(&ctx, &fallback)?;
    let adopted = execute(
        &ctx,
        recipe.clone(),
        Action::Adopt,
        false,
        true,
        InputFence::default(),
    )?;
    assert_eq!(adopted["journal"]["phase"], "succeeded");
    let state = fs::read(ctx.state.join("active.json"))?;
    let before = fs::read(disk::lock_path(&ctx))?;
    let source = "owner/repo";
    let current = upgrade_repository_resolving(
        &ctx,
        source,
        |request| {
            assert_eq!(request.repository.key(), source);
            ObjectId::parse(&recipe.source.commit)
        },
        |_| Err(Error::new("fixture", "unexpected confirmation")),
    )?;
    assert_eq!(current["status"], "up-to-date");
    let cancelled = upgrade_repository_resolving(
        &ctx,
        source,
        |_| ObjectId::parse(&"b".repeat(40)),
        |message| {
            assert!(message.contains("alpha") && message.contains("更新技能"));
            Err(Error::attention("cancelled", "fixture declined"))
        },
    );
    assert_eq!(cancelled.err().ok_or("record changed")?.code, "cancelled");
    assert_eq!(fs::read(ctx.state.join("active.json"))?, state);
    assert_eq!(fs::read(disk::lock_path(&ctx))?, before);
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}
