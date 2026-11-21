use super::*;

fn selection() -> crate::cli::SkillSelection {
    crate::cli::SkillSelection {
        name: Some("alpha".into()),
        only: vec![],
        all: false,
        revision: None,
        change_id: None,
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
