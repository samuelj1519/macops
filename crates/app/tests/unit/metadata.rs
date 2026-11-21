#![cfg(test)]
use super::*;
use crate::test_support::{TestResult, output};

#[test]
fn profile_and_alias_are_validated_at_the_boundary() -> TestResult {
    assert_eq!(PROFILES.map(Profile::name), ["glm", "kimi", "mi", "zen"]);
    assert_eq!(alias("abc-DEF_09.fixture")?, "abc-DEF_09.fixture");
    for value in ["", "two words", "a/b", "$(echo x)", "a@b", "中文"] {
        assert!(alias(value).is_err());
    }
    let helper = Path::new("/fixture/helper");
    assert_eq!(
        active_account(
            &json!({"apiKeyHelper":"/fixture/helper claude-mi@alias"}),
            helper,
            Profile::Mi
        )
        .as_deref(),
        Some("claude-mi@alias")
    );
    for value in [
        json!({}),
        json!({"apiKeyHelper":false}),
        json!({"apiKeyHelper":"/other claude-mi@alias"}),
        json!({"apiKeyHelper":"/fixture/helper claude-glm@alias"}),
        json!({"apiKeyHelper":"/fixture/helper claude-mi@bad;command"}),
    ] {
        assert!(active_account(&value, helper, Profile::Mi).is_none());
    }
    assert_eq!(keychain_status(Some(0), false), "present");
    assert_eq!(keychain_status(Some(0), true), "unknown");
    assert_eq!(keychain_status(Some(44), false), "unknown");
    assert_eq!(keychain_status(None, false), "unknown");
    Ok(())
}

#[test]
fn configuration_limits_preserve_counts_and_errors() -> TestResult {
    let summary = limited_lines("one\n\ntwo\n");
    assert_eq!(summary["count"], 2);
    assert_eq!(summary["truncated"], false);
    let summary = limited_lines(&"entry\n".repeat(81));
    assert_eq!(summary["count"], 81);
    assert_eq!(summary["shown"].as_array().ok_or("shown")?.len(), 80);
    assert_eq!(summary["truncated"], true);
    let mut out = output("text");
    assert_eq!(query_text(&out, false)?, "text");
    out.code = Some(1);
    assert!(query_text(&out, false).is_err());
    assert_eq!(query_text(&out, true)?, "text");
    out.code = Some(2);
    assert!(query_text(&out, true).is_err());
    out.uncertain = true;
    assert!(query_text(&out, true).is_err());
    Ok(())
}
