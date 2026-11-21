use super::*;
use crate::{cli::Cli, test_support::TestResult};
use clap::Parser;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn private_commit_recovers_only_its_own_configuration_file_and_retains_mode() -> TestResult {
    let temp = tempfile::Builder::new().tempdir_in("/private/tmp")?;
    let managed = temp.path().join("managed");
    private(&managed)?;
    let target = managed.join("recipes.json");
    fs::write(&target, b"before")?;
    fs::set_permissions(&target, fs::Permissions::from_mode(0o640))?;
    let (before, original) = witness(&target)?;
    assert_eq!(original.as_deref(), Some(b"before".as_slice()));
    let commit = Commit::prepare(before, b"published".to_vec())?;
    assert!(
        commit
            .verify(&managed.join("foreign.json"), b"published")
            .is_err()
    );
    assert!(commit.verify(&target, b"different").is_err());
    let veto = || {
        Err(Error::attention(
            "fixture_input_changed",
            "fixture scope changed",
        ))
    };
    assert_eq!(
        commit
            .publish(&target, b"published", veto)
            .err()
            .ok_or("veto ignored")?
            .code,
        "fixture_input_changed"
    );
    assert_eq!(fs::read(&target)?, b"before");
    for _ in 0..2 {
        commit.publish(&target, b"published", || Ok(()))?;
        assert_eq!(fs::read(&target)?, b"published");
        assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o7777, 0o640);
    }

    let unchanged = Commit::prepare(witness(&target)?.0, b"published".to_vec())?;
    assert!(unchanged.pair.is_none());
    unchanged.publish(&target, b"published", || Ok(()))?;
    let replacement = managed.join("replacement.json");
    fs::write(&replacement, b"published")?;
    fs::set_permissions(&replacement, fs::Permissions::from_mode(0o640))?;
    fs::rename(replacement, &target)?;
    assert!(commit.publish(&target, b"published", || Ok(())).is_err());
    assert!(unchanged.publish(&target, b"published", || Ok(())).is_err());
    assert!(Commit::prepare(unchanged.before, b"new".to_vec()).is_err());
    assert_eq!(fs::read(&target)?, b"published");
    Ok(())
}

#[test]
fn immutable_recipe_and_state_updates_respect_private_namespace_and_nonregular_entries()
-> TestResult {
    let temp = tempfile::Builder::new().tempdir_in("/private/tmp")?;
    let owner = temp.path().join("owner");
    let sibling = temp.path().join("untouched");
    fs::write(&sibling, b"independent file")?;
    let intent = owner.join("intent.json");
    let expected = serde_json::json!({"recipe":"fixture"});
    write_new(&intent, &expected)?;
    assert_eq!(fs::read(&intent)?, encode(&expected)?);
    assert_eq!(fs::metadata(&intent)?.permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::read_dir(&owner)?.count(), 1);
    assert_eq!(
        write_new(&intent, &serde_json::json!({"recipe":"foreign"}))
            .err()
            .ok_or("immutable intent overwritten")?
            .code,
        "already_exists"
    );

    let step = owner.join("steps.json");
    write_cas(&step, None, b"first")?;
    assert!(write_cas(&step, None, b"wrong").is_err());
    write_cas(&step, Some(b"first"), b"settled")?;
    assert_eq!(fs::read(&step)?, b"settled");
    fs::remove_file(&step)?;
    symlink(&sibling, &step)?;
    assert!(regular(&step).is_err());
    assert!(witness(&step).is_err());
    assert!(write_cas(&step, None, b"wrong").is_err());
    assert_eq!(fs::read(&sibling)?, b"independent file");

    fs::set_permissions(&owner, fs::Permissions::from_mode(0o755))?;
    assert!(write_new(&owner.join("extra.json"), &expected).is_err());
    assert!(!owner.join("extra.json").exists());
    Ok(())
}

#[test]
fn independent_claude_profile_placements_are_protected_during_central_maintenance() -> TestResult {
    let temp = tempfile::Builder::new().tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    let selected = home.join(".agents/skills/alpha");
    let profile = home.join(".claude-custom");
    let placement = profile.join("skills/alpha");
    let other = profile.join("skills/unrelated");
    fs::create_dir_all(&selected)?;
    fs::create_dir_all(placement.parent().ok_or("profile placement parent")?)?;
    fs::write(selected.join("SKILL.md"), b"fixture skill")?;
    fs::write(
        profile.join("settings.json"),
        br#"{"model":"opaque-custom"}"#,
    )?;
    fs::write(&other, b"unrelated")?;
    let cli = Cli::try_parse_from(["macops", "--home", home.to_str().ok_or("home")?, "list"])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    let recipe = policy::decode(&serde_json::to_vec(&serde_json::json!({
        "schemaVersion":1,"id":"alpha","owner":"skills-cli","cliVersion":"1.9.0",
        "source":{"repository":"owner/repo","commit":"a".repeat(40),"skillDirectory":"skills/alpha","memberName":"alpha","trackingRef":null},
        "installName":"alpha","scope":"global","clientScope":["codex"],"copy":true,"manualOnly":false,"licenseFiles":[],"derived":null,
        "runner":{"node":"/fixture/node","entry":"/fixture/skills/bin/cli.mjs"}
    }))?)?;

    symlink(&selected, &placement)?;
    let before = snapshot(&ctx, &recipe)?;
    let refused = before
        .remove_unshared(&ctx, &recipe)
        .err()
        .ok_or("shared removal")?;
    assert_eq!(refused.code, "native_skills_shared_installation");
    assert_eq!(fs::read_link(&placement)?, selected);
    fs::write(&other, b"profile owner change")?;
    assert!(before.protected(&ctx, &recipe).is_err());
    assert_eq!(fs::read(selected.join("SKILL.md"))?, b"fixture skill");
    assert_eq!(
        fs::read(profile.join("settings.json"))?,
        br#"{"model":"opaque-custom"}"#
    );

    fs::remove_file(&placement)?;
    let unshared = snapshot(&ctx, &recipe)?;
    unshared.remove_unshared(&ctx, &recipe)?;
    assert!(unshared.after(&ctx, &recipe, None, true).is_err());
    fs::remove_file(selected.join("SKILL.md"))?;
    fs::remove_dir(&selected)?;
    assert!(unshared.after(&ctx, &recipe, None, true)?.is_empty());
    assert_eq!(fs::read(&other)?, b"profile owner change");
    assert!(local_scope(&ctx, "alpha", &[profile.join("skills")]).is_err());
    local_scope(&ctx, "alpha", &[temp.path().join("unrelated-root")])?;
    Ok(())
}
