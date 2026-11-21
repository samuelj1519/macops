use super::*;

fn add_member(native: &NativeFixture, name: &str) -> TestResult {
    let mut recipe: Value = serde_json::from_slice(&fs::read(&native.recipe)?)?;
    recipe["id"] = json!(name);
    recipe["installName"] = json!(name);
    recipe["source"]["memberName"] = json!(name);
    recipe["source"]["skillDirectory"] = json!(format!("skills/{name}"));
    let path = native.f.file(&format!("{name}-recipe.json"));
    write_json(&path, &recipe)?;
    let (out, _) = native.f.run(&[
        "skills",
        "install",
        "--recipe",
        path.to_str().ok_or("path")?,
    ])?;
    assert_code(&out, 0);
    Ok(())
}

#[test]
fn batch_upgrade_reports_completed_and_unattempted_members_after_a_later_refusal() -> TestResult {
    let native = NativeFixture::new()?;
    let (out, _) = native.install()?;
    assert_code(&out, 0);
    for name in ["beta", "gamma"] {
        add_member(&native, name)?;
    }
    let (out, preview) = native.f.run(&["--dry-run", "skills", "upgrade", "--all"])?;
    assert_code(&out, 0);
    let outcomes = preview["data"]["results"]
        .as_array()
        .ok_or("preview results")?;
    assert_eq!(outcomes.len(), 3);
    let order = outcomes
        .iter()
        .map(|row| row["recipe"]["id"].as_str().ok_or("preview member"))
        .collect::<Result<Vec<_>, _>>()?;
    let changed = native
        .f
        .home
        .join(".agents/skills")
        .join(order[1])
        .join("SKILL.md");
    fs::write(&changed, b"independent edit before this upgrade")?;
    let (out, upgrade) = native.f.run(&["skills", "upgrade", "--all"])?;
    assert_code(&out, 3);
    assert_eq!(upgrade["data"]["incomplete"], true, "{upgrade}");
    assert_eq!(upgrade["data"]["partial"], true);
    let results = upgrade["data"]["results"]
        .as_array()
        .ok_or("partial results")?;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["journal"]["phase"], "succeeded");
    assert_eq!(results[1]["recipe"], order[1]);
    assert_eq!(results[1]["error"]["code"], "native_skills_scope_changed");
    assert_eq!(upgrade["data"]["unattempted"], json!([order[2]]));
    let calls = native.calls()?;
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[3]["name"], order[0]);
    assert_eq!(fs::read(changed)?, b"independent edit before this upgrade");
    Ok(())
}

#[test]
fn a_single_upgrade_with_unknown_native_outcome_returns_attention_and_original_evidence()
-> TestResult {
    let native = NativeFixture::new()?;
    let (out, _) = native.install()?;
    assert_code(&out, 0);
    native.mode("partial")?;
    let (out, value) = native.f.run(&["skills", "upgrade", "alpha"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["incomplete"], true);
    assert_eq!(value["data"]["partial"], false);
    assert_eq!(value["data"]["unattempted"], json!([]));
    let result = &value["data"]["results"][0];
    assert_eq!(result["nativeOutcome"]["code"], 1);
    assert_eq!(result["deliveryStage"], "native-unknown");
    assert_eq!(result["journal"]["phase"], "unknown");
    assert_eq!(
        native.f.read_json("state/active.json")?["id"],
        result["journal"]["id"]
    );
    assert_eq!(native.calls()?.len(), 2);
    Ok(())
}

#[test]
fn batch_does_not_attempt_another_member_after_an_unknown_native_outcome() -> TestResult {
    let native = NativeFixture::new()?;
    let (out, _) = native.install()?;
    assert_code(&out, 0);
    add_member(&native, "beta")?;
    let (out, preview) = native.f.run(&["--dry-run", "skills", "upgrade", "--all"])?;
    assert_code(&out, 0);
    let first = preview["data"]["results"][0]["recipe"]["id"]
        .as_str()
        .ok_or("first")?;
    let second = preview["data"]["results"][1]["recipe"]["id"]
        .as_str()
        .ok_or("second")?;
    native.mode("partial")?;
    let (out, value) = native.f.run(&["skills", "upgrade", "--all"])?;
    assert_code(&out, 3);
    assert_eq!(value["data"]["unattempted"], json!([second]));
    let results = value["data"]["results"].as_array().ok_or("results")?;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["journal"]["phase"], "unknown");
    let calls = native.calls()?;
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2]["name"], first);
    Ok(())
}
