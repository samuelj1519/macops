#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn native_versions_and_strict_payload_versions_preserve_selection_contracts() -> TestResult {
    for tool in TOOLS {
        assert_ne!(binary(tool)?, "");
        assert_eq!(version(tool, Some("1.2.3"))?.as_deref(), Some("1.2.3"));
    }
    assert_eq!(binary("cursor")?, "cursor-agent");
    assert!(binary("../x").is_err());
    for tool in ["mimo", "codex", "claude", "cursor", "kimi"] {
        assert_eq!(version(tool, None)?, None);
    }
    assert_eq!(
        version("dsh", Some("1.2.3-rc.1"))?.as_deref(),
        Some("1.2.3-rc.1")
    );
    for (tool, value) in [
        ("dsh", None),
        ("codegraph", None),
        ("dsh", Some("latest")),
        ("dsh", Some("1.2")),
        ("dsh", Some("1..3")),
        ("dsh", Some("1.x.3")),
        ("dsh", Some("1.2.3-")),
        ("dsh", Some("1.2.3-rc_1")),
        ("codegraph", Some("1.2.3-rc.1")),
        ("codegraph", Some("1.2.3+build")),
    ] {
        assert_eq!(
            version(tool, value).err().ok_or("invalid")?.code,
            "exact_version_required"
        );
    }
    assert!(version("unknown", None).is_err());
    assert_eq!(running_version("CodeGraph v1.2.3")?, "1.2.3");
    assert!(running_version("no version").is_err());
    assert!(running_version("1.2.3 2.3.4").is_err());
    Ok(())
}

#[test]
fn pin_spans_keep_all_non_target_bytes_and_preupgrade_drift_is_global() -> TestResult {
    let text = "# keep\n[tools]\n\"aqua:openai/codex\" = { version = '1.2.3' } # preserve\nnode = '24.0.0'\n";
    assert_eq!(pin(text, "codex")?, "1.2.3");
    let updated = rewrite(text, "codex", "2.0.0")?;
    assert_eq!(updated, text.replace("'1.2.3'", "\"2.0.0\""));
    assert_eq!(
        pin("[tools]\n\"aqua:openai/codex\" = '1.0.0'\n", "codex")?,
        "1.0.0"
    );
    require_sync(text, text, false)?;
    for drift in [
        updated.clone(),
        text.replace("24.0.0", "25.0.0"),
        format!("{text}# external\n"),
    ] {
        assert_eq!(
            require_sync(&drift, text, false).err().ok_or("drift")?.exit,
            3
        );
        require_sync(&drift, text, true)?;
    }
    assert!(require_sync("[", text, false).is_err());
    for text in [
        "[",
        "[tools]\n",
        "[tools]\n\"aqua:openai/codex\" = ['1.0.0']\n",
        "[tools]\n\"aqua:openai/codex\" = {version='1.0.0',os='macos'}\n",
        "[tools]\n\"aqua:openai/codex\" = {path='x'}\n",
        "[tools]\n\"aqua:openai/codex\" = 'latest'\n",
    ] {
        assert!(pin(text, "codex").is_err());
        assert!(rewrite(text, "codex", "2.0.0").is_err());
    }
    assert!(pin(text, "unknown").is_err());
    assert!(rewrite(text, "codex", "bad").is_err());
    assert!(rewrite(text, "unknown", "2.0.0").is_err());
    Ok(())
}

#[test]
fn cursor_inline_download_options_survive_upgrade_and_reconciliation() -> TestResult {
    let old = r#"# preserve
[tools]
"http:cursor-agent" = { version = "1.0.0", version_list_url = "https://example.test/releases", version_regex = '([^/]+)/', platforms = { macos-arm64 = { url = "https://example.test/{{ version }}.tgz" }, macos-x64 = { url = "https://example.test/x64/{{ version }}.tgz" } } } # keep
node = "24.0.0"
"#;
    let next = old.replace("1.0.0", "2.0.0");
    assert_eq!(pin(old, "cursor")?, "1.0.0");
    assert_eq!(rewrite(old, "cursor", "2.0.0")?, next);
    assert!(matches!(
        config_tx::evaluate_mise_reconcile(&next, old, Some("cursor"))?,
        config_tx::MiseReconcilePlan::Rewrite { rewritten, .. } if rewritten == next
    ));
    let drift = next.replace("x64/{{ version }}", "other/{{ version }}");
    assert!(matches!(
        config_tx::evaluate_mise_reconcile(&drift, old, Some("cursor"))?,
        config_tx::MiseReconcilePlan::Drift { reason, .. } if reason == "tool_options_drifted"
    ));
    assert!(matches!(
        config_tx::evaluate_mise_reconcile(&drift, &next, None)?,
        config_tx::MiseReconcilePlan::Drift { rows, .. }
            if rows.iter().any(|row| row.key == "http:cursor-agent" && row.live.contains("下载选项"))
    ));
    assert!(pin(&old.replace("version_regex", "postinstall"), "cursor").is_err());
    Ok(())
}

#[test]
fn cursor_http_pin_requires_a_complete_string_download_descriptor() -> TestResult {
    let parse = |value| {
        pin(
            &format!("[tools]\n\"http:cursor-agent\" = {value}\n"),
            "cursor",
        )
    };
    assert_eq!(
        parse("{ version = '1.0.0', url = 'https://example.test/cursor' }")?,
        "1.0.0"
    );
    for descriptor in [
        "{ version = '1.0.0', version_list_url = 'https://example.test/releases' }",
        "{ version = '1.0.0', url = 9 }",
        "{ version = '1.0.0', platforms = [] }",
        "{ version = '1.0.0', platforms = {} }",
        "{ version = '1.0.0', platforms = { macos-arm64 = false } }",
        "{ version = '1.0.0', platforms = { macos-arm64 = { url = 9 } } }",
        "{ version = '1.0.0', platforms = { macos-arm64 = { url = 'here', path = 'there' } } }",
    ] {
        assert_eq!(
            parse(descriptor).err().ok_or(descriptor)?.code,
            "unsupported_mise_pin_shape"
        );
    }
    Ok(())
}
