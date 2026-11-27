use super::*;

fn selection() -> crate::cli::SkillSelection {
    crate::cli::SkillSelection {
        name: Some("alpha".into()),
        only: vec![],
        all: false,
        revision: None,
        change_id: None,
        agents: crate::cli::SkillAgents::default(),
        switch_source: false,
    }
}

#[test]
fn pinned_refresh_uses_one_candidate_and_records_current_content_without_installing() -> TestResult
{
    let (_temp, ctx, recipe) = fixture()?;
    adopt(&ctx, &recipe)?;
    let registry_path = disk::lock_path(&ctx);
    let registry = fs::read(&registry_path)?;
    let installed = fs::read(disk::payload(&ctx, "alpha")?.join("SKILL.md"))?;
    let mut observed = vec![];
    let preview = refresh_with_candidate(&ctx, &selection(), true, |_, candidate| {
        observed.push(candidate.source.tracking_ref.clone());
        Err(Error::attention(
            "fixture_unavailable",
            "unexpected preview I/O",
        ))
    })?;
    assert_eq!(observed, Vec::<Option<String>>::new());
    assert_eq!(preview["networkInvoked"], false);
    assert!(!ctx.cache.exists());
    assert!(!ctx.home.join("check-called").exists());

    let refreshed = refresh_with_candidate(&ctx, &selection(), false, |_, candidate| {
        observed.push(candidate.source.tracking_ref.clone());
        Ok((
            candidate.source.commit.clone(),
            "e".repeat(64),
            "b".repeat(40),
        ))
    })?;
    assert_eq!(observed, vec![None]);
    assert_eq!(refreshed["results"][0]["status"], "current");
    assert_eq!(
        refreshed["results"][0]["nativePinCheck"]["scope"],
        "registered-sha-pin"
    );
    assert_eq!(refreshed["installationWrites"], false);
    assert_eq!(fs::read(registry_path)?, registry);
    assert_eq!(
        fs::read(disk::payload(&ctx, "alpha")?.join("SKILL.md"))?,
        installed
    );
    assert!(ctx.home.join("check-called").exists());
    assert_eq!(
        outdated(&ctx, Some("alpha"), &[])?["cache"]["status"],
        "verified"
    );
    Ok(())
}

#[test]
fn failed_tracking_candidate_preserves_the_last_complete_candidate_cache() -> TestResult {
    let (_temp, ctx, mut recipe) = fixture()?;
    let branch = "refs/heads/main";
    recipe.source.tracking_ref = Some(branch.into());
    adopt(&ctx, &recipe)?;
    let mut observed = vec![];
    let refreshed = refresh_with_candidate(&ctx, &selection(), false, |_, candidate| {
        observed.push(candidate.source.tracking_ref.clone());
        if candidate.source.tracking_ref.is_some() {
            Ok(("f".repeat(40), "d".repeat(64), "c".repeat(40)))
        } else {
            Ok((
                candidate.source.commit.clone(),
                "e".repeat(64),
                "b".repeat(40),
            ))
        }
    })?;
    assert_eq!(observed, vec![None, Some(branch.into())]);
    assert_eq!(refreshed["results"][0]["status"], "outdated");
    assert_eq!(refreshed["results"][0]["repositoryAhead"], true);
    let cache_path = ctx.cache.join("native-skills-checks.json");
    let settled = fs::read(&cache_path)?;
    let failure = refresh_with_candidate(&ctx, &selection(), false, |_, candidate| {
        if candidate.source.tracking_ref.is_some() {
            Err(Error::attention(
                "fixture_unavailable",
                "candidate unavailable",
            ))
        } else {
            Ok((
                candidate.source.commit.clone(),
                "e".repeat(64),
                "b".repeat(40),
            ))
        }
    });
    assert_eq!(
        failure.err().ok_or("failed refresh accepted")?.code,
        "fixture_unavailable"
    );
    assert_eq!(fs::read(cache_path)?, settled);
    Ok(())
}

#[test]
fn refresh_labels_a_native_tracking_ref_and_preserves_cache_if_runner_changes() -> TestResult {
    let (_temp, ctx, mut recipe) = fixture()?;
    let recorded = disk::lock_path(&ctx);
    let mut register: Value = serde_json::from_slice(&fs::read(&recorded)?)?;
    register["skills"]["alpha"]["ref"] = json!("HEAD");
    fs::write(&recorded, serde_json::to_vec(&register)?)?;
    recipe.source.tracking_ref = Some("HEAD".into());
    adopt(&ctx, &recipe)?;
    let mock = |_: &Context, candidate: &Recipe| {
        Ok((
            candidate.source.commit.clone(),
            "e".repeat(64),
            "b".repeat(40),
        ))
    };
    let viewed = refresh_with_candidate(&ctx, &selection(), false, mock)?;
    assert_eq!(
        viewed["results"][0]["nativePinCheck"]["scope"],
        "registered-ref"
    );
    fs::remove_file(ctx.home.join("check-called"))?;
    let cache = ctx.cache.join("native-skills-checks.json");
    let original = fs::read(&cache)?;
    fs::write(&recipe.runner.entry, b"changed during maintenance")?;
    let next = refresh_with_candidate(&ctx, &selection(), false, mock);
    assert!(next.is_err());
    assert_eq!(fs::read(cache)?, original);
    assert!(!ctx.home.join("check-called").exists());
    Ok(())
}
