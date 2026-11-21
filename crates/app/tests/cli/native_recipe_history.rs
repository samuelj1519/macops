use super::*;

#[test]
fn original_native_skill_terminal_record_can_finish_publishing_active_after_a_restart() -> TestResult
{
    let native = NativeFixture::new()?;
    let (out, installed) = native.install()?;
    assert_code(&out, 0);
    assert_eq!(installed["data"]["deliveryStage"], "verified");
    let operation = installed["data"]["journal"]["id"]
        .as_str()
        .ok_or("original operation")?;
    let active = native.f.file("state/active.json");
    let history = native.f.file(&format!("state/operations/{operation}.json"));
    let receipts = native.f.file("state/receipts.json");
    let lock = native.f.home.join(".agents/.skill-lock.json");
    let applied = native.f.home.join(".agents/skills/alpha/SKILL.md");
    let history_before = fs::read(&history)?;
    let receipts_before = fs::read(&receipts)?;
    let lock_before = fs::read(&lock)?;
    let applied_before = fs::read(&applied)?;
    let mut pending = native.f.read_json("state/active.json")?;
    pending["phase"] = json!("unknown");
    pending["message"] = json!("fixture before terminal publication reached active");
    write_json(&active, &pending)?;

    let (out, _) = native.f.run(&["recover", operation])?;
    assert_code(&out, 0);
    assert_eq!(
        native.f.read_json("state/active.json")?["phase"],
        "succeeded"
    );
    assert_eq!(fs::read(history)?, history_before);
    assert_eq!(fs::read(receipts)?, receipts_before);
    assert_eq!(fs::read(lock)?, lock_before);
    assert_eq!(fs::read(applied)?, applied_before);
    assert_eq!(native.calls()?.len(), 1);
    Ok(())
}
