#![cfg(test)]
use super::*;
use crate::test_support::TestResult;
use std::os::unix::fs::PermissionsExt;

fn recipe() -> Result<Recipe> {
    crate::skills::rules::native_skills::decode(&serde_json::to_vec(&serde_json::json!({
        "schemaVersion":1,"id":"alpha","owner":"skills-cli","cliVersion":"1.9.0",
        "source":{"repository":"owner/repo","commit":"a".repeat(40),"skillDirectory":"skills/alpha","memberName":"alpha","trackingRef":null},
        "installName":"alpha","scope":"global","clientScope":["codex"],"copy":true,
        "manualOnly":true,"licenseFiles":["LICENSE"],"derived":null,
        "runner":{"node":"/fixture/node","entry":"/fixture/skills/bin/cli.mjs","sha256":"f".repeat(64)}
    }))?)
}
fn fixture()
-> std::result::Result<(tempfile::TempDir, std::path::PathBuf, Object), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir_in("/private/tmp")?;
    let root = temp.path().join("alpha");
    std::fs::create_dir(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;
    std::fs::write(
        root.join("SKILL.md"),
        b"---\nname: alpha\ndescription: test\n---\nbody\n",
    )?;
    std::fs::write(root.join("foreign.txt"), b"preserve me")?;
    let before = disk::capture(&root)?;
    Ok((temp, root, before))
}
#[test]
fn customization_replays_only_fixed_manual_metadata_and_missing_license_and_preserves_foreign_files()
-> TestResult {
    let (_temp, root, native) = fixture()?;
    let recipe = recipe()?;
    let licenses = BTreeMap::from([("LICENSE".into(), b"fixture license".to_vec())]);
    let mut steps = prepare(&root, &recipe, &licenses, false)?;
    steps.validate(&native, &recipe, &licenses, false)?;
    assert!(!steps.complete()?);
    for index in 0..steps.patches.len() {
        steps.apply(index, |updated| {
            updated.validate(&native, &recipe, &licenses, false)
        })?;
        let saved = serde_json::to_vec(&steps)?;
        steps = crate::skills::rules::native_skills::decode(&saved)?;
        steps.validate(&native, &recipe, &licenses, false)?;
    }
    assert!(steps.complete()?);
    assert!(crate::capability::rules::invocation::manual_frontmatter(
        &std::fs::read(root.join("SKILL.md"))?
    )?);
    let codex = std::fs::read(root.join("agents/openai.yaml"))?;
    assert_eq!(
        crate::capability::rules::invocation::transform(MetadataKind::CodexPolicy, Some(&codex))?,
        codex
    );
    assert_eq!(std::fs::read(root.join("LICENSE"))?, b"fixture license");
    assert_eq!(std::fs::read(root.join("foreign.txt"))?, b"preserve me");
    for index in 0..steps.patches.len() {
        steps.apply(index, |_| Ok(()))?;
    }
    assert!(steps.complete()?);
    std::fs::write(root.join("foreign.txt"), b"foreign change")?;
    assert!(steps.verify().is_err());
    Ok(())
}
#[test]
fn customization_rejects_forged_target_patch_set_before_bytes_and_candidate_evidence() -> TestResult
{
    let (_temp, root, native) = fixture()?;
    let recipe = recipe()?;
    let licenses = BTreeMap::from([("LICENSE".into(), b"fixture license".to_vec())]);
    let original = prepare(&root, &recipe, &licenses, false)?;
    for variant in 0..9 {
        let mut changed = original.clone();
        match variant {
            0 => {
                changed.patches.pop();
            }
            1 => changed.patches[0].after.push(b'x'),
            2 => changed.patches[0].relative = "foreign.txt".into(),
            3 => changed.patches[0].before.path = root.join("foreign.txt"),
            4 => changed.patches[0].before_bytes = Some(b"forged original".to_vec()),
            5 => changed.patches[0].mode = 0o600,
            6 => changed.slot.name = "other".into(),
            7 => changed.native_tree.mode = 0o700,
            _ => changed.patches[1].parent.as_mut().ok_or("parent")?.target = root.join("foreign"),
        }
        assert!(
            changed
                .validate(&native, &recipe, &licenses, false)
                .is_err(),
            "variant {variant}"
        );
    }
    let mut staged = original;
    let mut saved = None;
    assert!(
        staged
            .apply(0, |updated| {
                saved = Some(updated.clone());
                Err(invalid())
            })
            .is_err()
    );
    let mut saved = saved.ok_or("checkpoint")?;
    saved.validate(&native, &recipe, &licenses, false)?;
    saved.patches[0]
        .candidate
        .as_mut()
        .ok_or("candidate")?
        .target = root.join("foreign.txt");
    assert!(saved.validate(&native, &recipe, &licenses, false).is_err());
    Ok(())
}
#[test]
fn license_customization_is_missing_create_same_noop_and_conflict_or_link_refusal() -> TestResult {
    let (_temp, root, _native) = fixture()?;
    let mut recipe = recipe()?;
    recipe.manual_only = false;
    let licenses = BTreeMap::from([("LICENSE".into(), b"license".to_vec())]);
    std::fs::write(root.join("LICENSE"), b"license")?;
    let native = disk::capture(&root)?;
    let mut same = prepare(&root, &recipe, &licenses, false)?;
    same.validate(&native, &recipe, &licenses, false)?;
    same.apply(0, |_| Ok(()))?;
    assert!(same.complete()?);
    std::fs::write(root.join("LICENSE"), b"foreign license")?;
    assert!(prepare(&root, &recipe, &licenses, false).is_err());
    std::fs::remove_file(root.join("LICENSE"))?;
    std::os::unix::fs::symlink("foreign.txt", root.join("LICENSE"))?;
    assert!(prepare(&root, &recipe, &licenses, false).is_err());
    Ok(())
}
