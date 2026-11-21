#![cfg(test)]
use super::*;
use crate::{
    domain::{
        ConfigAnchor, ConfigModePolicy, ConfigQueryKind, Kind, LocalConfigQuery, Package,
        QuerySpec, Scope,
    },
    test_support::TestResult,
};
use std::{collections::BTreeMap, path::Path};

fn sample_stamp(mode: u32, tag: char) -> ConfigFileStamp {
    ConfigFileStamp {
        identity: FileId {
            device: 1,
            inode: 10,
        },
        mode,
        content_digest: tag.to_string().repeat(64),
        fingerprint: 'f'.to_string().repeat(64),
    }
}

fn sample_backup(target: &str) -> Result<ConfigBackupRecord> {
    let p = Path::new(target);
    Ok(ConfigBackupRecord {
        schema_version: 1,
        operation_id: "OP-100-1".into(),
        config_id: crate::policy::config_local::config_id(p)?,
        target: p.to_path_buf(),
        parent_canonical: p.parent().unwrap_or(Path::new("/")).to_path_buf(),
        parent_identity: FileId {
            device: 1,
            inode: 2,
        },
        before_stamp: Some(sample_stamp(0o600, 'a')),
        expected_after_digest: "b".repeat(64),
        expected_after_mode: 0o600,
        expected_absent: false,
        reconcile: None,
    })
}

#[test]
fn backup_validation_and_mode_computation_satisfy_chezmoi_policies() -> TestResult {
    let rec = sample_backup("/fixture/home/.gitconfig")?;
    validate_backup_record(&rec)?;
    let mut bad_ver = rec.clone();
    bad_ver.schema_version = 2;
    assert!(validate_backup_record(&bad_ver).is_err());
    let mut bad_op = rec.clone();
    bad_op.operation_id = "bad".into();
    assert!(validate_backup_record(&bad_op).is_err());
    let mut bad_before = rec;
    bad_before.before_stamp = Some(ConfigFileStamp {
        identity: FileId {
            device: 1,
            inode: 1,
        },
        mode: 0o100_000,
        content_digest: "a".repeat(64),
        fingerprint: "f".repeat(64),
    });
    assert!(validate_backup_record(&bad_before).is_err());

    Ok(())
}

#[test]
fn static_apply_eligibility_blocks_all_unevaluated_controls_unless_adopted_in_sync() -> TestResult {
    let tgt = sample_stamp(0o600, 'a');
    assert!(
        validate_static_apply_eligibility(&CoverageState::Unavailable, &[], None, None).is_err()
    );
    assert_eq!(
        validate_static_apply_eligibility(&CoverageState::Complete, &[], None, Some(&tgt))?,
        Vec::<String>::new()
    );
    let ctrl = vec!["source_control_not_evaluated:/src/.chezmoiroot".to_owned()];
    assert_eq!(
        validate_static_apply_eligibility(&CoverageState::Unsupported, &ctrl, None, Some(&tgt))
            .err()
            .ok_or("unadopted blocked")?
            .code,
        "dynamic_chezmoi_control_present"
    );
    let pkg = Package {
        id: crate::policy::config_local::config_id(Path::new("/h/.rc"))?,
        name: ".rc".into(),
        kind: Kind::Config,
        official_url: "https://www.chezmoi.io/".into(),
        provider: Provider::ConfigObserved {
            target: "/h/.rc".into(),
            source: Some("/s/dot_rc".into()),
            authority: ConfigAuthority::ChezmoiCandidate,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let receipt = Receipt {
        scope: Some(Scope {
            home: "/h".into(),
            paths: BTreeMap::new(),
            service_identity: None,
        }),
        query: None,
        package: pkg,
        observation: Observation::ConfigFile {
            authority: ConfigAuthority::ChezmoiCandidate,
            target: Some(tgt.clone()),
            source: Some(sample_stamp(0o600, 'a')),
            mode_matches_source: Some(true),
        },
        operation_id: "OP-1".into(),
        updated_at: 1,
    };
    let warnings = validate_static_apply_eligibility(
        &CoverageState::Unsupported,
        &ctrl,
        Some(&receipt),
        Some(&tgt),
    )?;
    assert_eq!(warnings.len(), 1);
    let drifted = sample_stamp(0o600, 'c');
    assert!(
        validate_static_apply_eligibility(
            &CoverageState::Unsupported,
            &ctrl,
            Some(&receipt),
            Some(&drifted)
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn tx_plan_restore_and_recovery_decisions_cover_all_cas_branches() -> TestResult {
    let target = PathBuf::from("/h/.rc");
    let id = crate::policy::config_local::config_id(&target)?;
    let anchor = ConfigAnchor {
        requested: target.clone(),
        canonical_parent: "/h".into(),
        parent_identity: FileId {
            device: 1,
            inode: 2,
        },
        name: ".rc".into(),
    };
    let src_anchor = ConfigAnchor {
        requested: "/s/dot_rc".into(),
        canonical_parent: "/s".into(),
        parent_identity: FileId {
            device: 1,
            inode: 3,
        },
        name: "dot_rc".into(),
    };
    let mut plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: 30,
        package: Package {
            id,
            name: ".rc".into(),
            kind: Kind::Config,
            official_url: "https://www.chezmoi.io/".into(),
            provider: Provider::ConfigObserved {
                target,
                source: Some("/s/dot_rc".into()),
                authority: ConfigAuthority::ChezmoiCandidate,
            },
            paths: vec![],
            exact_version_required: false,
        },
        action: Action::Apply,
        before: Observation::ConfigFile {
            authority: ConfigAuthority::ChezmoiCandidate,
            target: Some(sample_stamp(0o644, 'a')),
            source: Some(sample_stamp(0o644, 'b')),
            mode_matches_source: Some(true),
        },
        target: Some("b".repeat(64)),
        command: None,
        query: Some(QuerySpec::LocalConfig(LocalConfigQuery {
            kind: ConfigQueryKind::LocalV1,
            authority: ConfigAuthority::ChezmoiCandidate,
            target: anchor,
            source: Some(src_anchor),
            expected_mode: Some(ConfigModePolicy {
                private: false,
                readonly: false,
                executable: false,
            }),
        })),
        recipe_sha256: "r".repeat(64),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    crate::policy::config_local::validate_plan(&plan)?;
    plan.action = Action::Reconcile;
    validate_config_tx_plan(&plan)?;
    plan.action = Action::Restore;
    validate_config_tx_plan(&plan)?;
    plan.action = Action::Uninstall;
    assert!(validate_config_tx_plan(&plan).is_err());
    plan.action = Action::Apply;
    plan.delegated = true;
    assert!(validate_config_tx_plan(&plan).is_err());
    plan.delegated = false;
    plan.target = None;
    assert!(validate_config_tx_plan(&plan).is_err());
    Ok(())
}

#[test]
fn restore_and_recovery_decisions_cover_all_cas_branches() -> TestResult {
    let b = sample_backup("/h/.rc")?;
    let before = sample_stamp(0o600, 'a');
    let after = sample_stamp(0o600, 'b');
    let ext = sample_stamp(0o600, 'e');
    assert_eq!(
        decide_restore(&b, Some(&before)),
        RestoreDecision::AlreadyAtBefore
    );
    assert_eq!(
        decide_restore(&b, Some(&after)),
        RestoreDecision::ReadyToRestore
    );
    assert_eq!(
        decide_restore(&b, Some(&ext)),
        RestoreDecision::ExternalConflict
    );

    assert_eq!(
        decide_tx_recovery(false, true, &b, Some(&after), None),
        RecoveryDecision::AlreadyCommitted
    );
    assert_eq!(
        decide_tx_recovery(false, false, &b, Some(&after), None),
        RecoveryDecision::CommitVerifiedWrite
    );
    assert_eq!(
        decide_tx_recovery(true, false, &b, Some(&before), None),
        RecoveryDecision::PreparedUntouched
    );
    assert_eq!(
        decide_tx_recovery(false, false, &b, Some(&ext), Some(&ext.content_digest)),
        RecoveryDecision::RollbackFailedWrite
    );
    assert_eq!(
        decide_tx_recovery(false, false, &b, Some(&ext), None),
        RecoveryDecision::UnknownExternalConflict
    );
    Ok(())
}

#[test]
fn mise_toml_reconcile_handles_specs_redaction_inline_tables_and_drift() -> TestResult {
    for (tool, spec) in [
        ("codex", "aqua:openai/codex"),
        ("claude", "aqua:anthropics/claude-code"),
        ("cursor", "http:cursor-agent"),
        ("kimi", "npm:@moonshot-ai/kimi-code"),
        ("dsh", "npm:@deepseek-ai/dsh"),
        ("codegraph", "npm:@colbymchenry/codegraph"),
    ] {
        assert_eq!(ai_mise_spec(tool)?, spec);
    }
    assert_eq!(
        ai_mise_spec("mimo").err().ok_or("mimo")?.code,
        "vendor_tool_no_mise_pin"
    );
    assert_eq!(
        ai_mise_spec("unknown").err().ok_or("unk")?.code,
        "unsupported_reconcile_tool"
    );
    assert!(has_mise_env_override("MISE_ENV") && has_mise_env_override("mise_config_file"));
    assert!(!has_mise_env_override("MISE_SHELL") && !has_mise_env_override("PATH"));

    let clean = "[tools]\n\"aqua:openai/codex\" = \"0.10.0\" # comment\n\"http:cursor-agent\" = { version = \"1.2.0\" }\n[settings]\nexperimental = true\n[env]\nAPI_KEY = \"SYNTHETIC_SECRET_ENV\"\ntop = 1\n";
    assert_eq!(
        evaluate_mise_reconcile(clean, clean, None)?,
        MiseReconcilePlan::InSync {
            spec: None,
            pin: None
        }
    );
    assert_eq!(
        evaluate_mise_reconcile(clean, clean, Some("codex"))?,
        MiseReconcilePlan::InSync {
            spec: Some("aqua:openai/codex".into()),
            pin: Some("0.10.0".into())
        }
    );
    let spaced = format!("{clean}\n");
    assert!(matches!(
        evaluate_mise_reconcile(&spaced, clean, None)?,
        MiseReconcilePlan::Drift { reason, .. } if reason == "formatting_or_comment_drift"
    ));
    assert!(matches!(
        evaluate_mise_reconcile(&spaced, clean, Some("codex"))?,
        MiseReconcilePlan::Drift { reason, .. } if reason == "formatting_or_comment_drift"
    ));

    let live_upgraded = clean
        .replace("\"0.10.0\"", "\"0.11.0\"")
        .replace("\"1.2.0\"", "\"1.3.0\"");
    assert!(matches!(
        evaluate_mise_reconcile(&live_upgraded, clean, Some("codex"))?,
        MiseReconcilePlan::Drift { reason, .. } if reason == "unrelated_keys_drifted"
    ));

    let live_codex = clean.replace("\"0.10.0\"", "\"0.11.0\"");
    let MiseReconcilePlan::Rewrite { rewritten, to, .. } =
        evaluate_mise_reconcile(&live_codex, clean, Some("codex"))?
    else {
        return Err("expected rewrite".into());
    };
    assert_eq!(to, "0.11.0");
    assert_eq!(rewritten, live_codex);

    let live_cursor = clean.replace("\"1.2.0\"", "\"1.4.0\"");
    let MiseReconcilePlan::Rewrite {
        rewritten: cur_rw, ..
    } = evaluate_mise_reconcile(&live_cursor, clean, Some("cursor"))?
    else {
        return Err("expected inline rewrite".into());
    };
    assert_eq!(cur_rw, live_cursor);
    Ok(())
}

#[test]
fn mise_toml_reconcile_rejects_malformed_inputs_arrays_and_redacts_secrets() -> TestResult {
    assert!(evaluate_mise_reconcile("[bad", "[tools]\n", None).is_err());
    assert!(evaluate_mise_reconcile("[tools]\n", "[bad", None).is_err());

    let live = "[tools]\n\"aqua:openai/codex\" = \"1.0.0\"\nonly_live = \"1.0.0\"\n[env]\nSECRET_A = \"SECRET_VAL_1\"\nONLY_LIVE_ENV = \"SECRET_VAL_2\"\n";
    let src = "[tools]\nonly_src = \"2.0.0\"\n[env]\nSECRET_A = \"SECRET_VAL_OLD\"\nONLY_SRC_ENV = \"SECRET_VAL_3\"\n";
    let MiseReconcilePlan::Drift { rows, .. } = evaluate_mise_reconcile(live, src, None)? else {
        return Err("expected drift".into());
    };
    let serialized = serde_json::to_string(&rows)?;
    assert!(!serialized.contains("SECRET_VAL"));
    assert!(serialized.contains("<redacted>"));

    assert!(matches!(
        evaluate_mise_reconcile("[tools]\n\"aqua:openai/codex\" = \"1.0.0\"\n", "[tools]\n", Some("codex"))?,
        MiseReconcilePlan::Drift { reason, .. } if reason == "missing_in_source"
    ));
    assert!(matches!(
        evaluate_mise_reconcile("[tools]\n", "[tools]\n\"aqua:openai/codex\" = \"1.0.0\"\n", Some("codex"))?,
        MiseReconcilePlan::Drift { reason, .. } if reason == "missing_in_live"
    ));
    for bad_shape in [
        "[tools]\n\"aqua:openai/codex\" = [\"1.0.0\", \"2.0.0\"]\n",
        "[tools]\n\"aqua:openai/codex\" = { version = \"1.0.0\", os = \"macos\" }\n",
        "[tools.\"aqua:openai/codex\"]\nversion = \"1.0.0\"\n",
    ] {
        assert_eq!(
            evaluate_mise_reconcile(bad_shape, bad_shape, Some("codex"))
                .err()
                .ok_or("shape")?
                .code,
            "unsupported_mise_pin_shape"
        );
    }
    Ok(())
}

#[test]
fn backup_rejects_bad_paths_ids_digests_and_modes() -> TestResult {
    let base = sample_backup("/h/.rc")?;
    for mutate in 0..7 {
        let mut b = base.clone();
        match mutate {
            0 => b.target = "relative".into(),
            1 => b.parent_canonical = "relative".into(),
            2 => b.config_id = "config:incorrect".into(),
            3 => b.expected_after_digest = "x".into(),
            4 => b.expected_after_mode = 0o10000,
            5 => b.before_stamp.as_mut().ok_or("stamp")?.content_digest = "bad".into(),
            _ => b.before_stamp.as_mut().ok_or("stamp")?.fingerprint = "bad".into(),
        }
        assert!(validate_backup_record(&b).is_err());
    }
    let mut absent = base;
    absent.before_stamp = None;
    absent.expected_absent = true;
    validate_backup_record(&absent)?;
    assert_eq!(
        decide_restore(&absent, None),
        RestoreDecision::AlreadyAtBefore
    );
    assert_eq!(
        decide_restore(&absent, Some(&sample_stamp(0o644, 'b'))),
        RestoreDecision::ExternalConflict
    );
    assert_eq!(
        decide_tx_recovery(false, false, &absent, None, None),
        RecoveryDecision::CommitVerifiedWrite
    );
    assert!(!restored_stamp_matches(
        None,
        Some(&sample_stamp(0o600, 'a'))
    ));
    assert!(!restored_stamp_matches(
        Some(&sample_stamp(0o600, 'a')),
        None
    ));
    Ok(())
}

#[test]
fn toml_values_are_unambiguous_and_unrecognized_shapes_do_not_hide_drift() -> TestResult {
    let sample = "top = 1\nfloat = 1.5\nflag = true\ndate = 1979-05-27T07:32:00Z\narr = [1, 2]\ninline = { a = 1, b = { x = false } }\n[[nested]]\nname = 'value'\n";
    let doc = Document::parse(sample)?;
    for (_, item) in doc.iter() {
        assert_ne!(canonical_item(item), "");
    }
    let tools_scalar = Document::parse("tools = false\nenv = 1\n")?;
    let (_, others) = collect_entries(&tools_scalar);
    assert_eq!(others.len(), 2);
    let source = "[tools]\n\"aqua:openai/codex\"='1.0.0'\n[settings]\n'a=integer:1,b' = 2\n";
    let live = "[tools]\n\"aqua:openai/codex\"='2.0.0'\n[settings]\na = 1\nb = 2\n";
    assert!(matches!(
        evaluate_mise_reconcile(live, source, Some("codex"))?,
        MiseReconcilePlan::Drift { .. }
    ));
    assert!(rewrite_tool_pin_spanned(sample, &doc, "missing", "1.0.0").is_err());
    for text in ["[tools]\nx = [1]\n", "[tools]\nx = { os = 'macos' }\n"] {
        let parsed = Document::parse(text)?;
        assert!(rewrite_tool_pin_spanned(text, &parsed, "x", "1.0.0").is_err());
    }
    for (live, src) in [("latest", "1.0.0"), ("2.0.0", "latest")] {
        assert!(
            evaluate_mise_reconcile(
                &format!("[tools]\n\"aqua:openai/codex\"='{live}'\n"),
                &format!("[tools]\n\"aqua:openai/codex\"='{src}'\n"),
                Some("codex")
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn unsupported_tool_drift_and_mode_changes_are_visible_without_values() -> TestResult {
    let source = "[tools]\nx={version='1', token='SYNTHETIC_PRIVATE'}\n[settings]\na=1\n";
    let live = "[tools]\nx=['1','2']\n[settings]\na=2\n";
    let result = evaluate_mise_reconcile(live, source, None)?;
    let MiseReconcilePlan::Drift { rows, .. } = result else {
        return Err("drift".into());
    };
    assert!(!serde_json::to_string(&rows)?.contains("SYNTHETIC_PRIVATE"));
    assert!(rows.iter().any(|r| r.live == "<unsupported>"));
    let mut record = sample_backup("/h/.rc")?;
    record.before_stamp = None;
    assert_eq!(
        decide_tx_recovery(true, false, &record, None, None),
        RecoveryDecision::PreparedUntouched
    );
    assert_eq!(
        decide_tx_recovery(
            true,
            false,
            &record,
            Some(&sample_stamp(0o644, 'e')),
            Some("e")
        ),
        RecoveryDecision::UnknownExternalConflict
    );
    assert!(!restored_stamp_matches(
        Some(&sample_stamp(0o600, 'a')),
        Some(&sample_stamp(0o644, 'a'))
    ));
    assert!(!restored_stamp_matches(
        Some(&sample_stamp(0o600, 'a')),
        Some(&sample_stamp(0o600, 'b'))
    ));
    Ok(())
}

#[test]
fn invalid_reconcile_tool_and_unencodable_backup_path_are_rejected() -> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let mut backup = sample_backup("/h/.rc")?;
    backup.target = std::ffi::OsString::from_vec(b"/h/\xff".to_vec()).into();
    assert!(validate_backup_record(&backup).is_err());
    assert!(evaluate_mise_reconcile("[tools]\n", "[tools]\n", Some("unknown")).is_err());
    Ok(())
}

#[test]
fn different_pin_with_comment_or_quote_drift_is_rejected_before_preview() -> TestResult {
    for (live, source) in [
        (
            "# local\n[tools]\n\"aqua:openai/codex\"=\"2.0.0\"\n",
            "# source\n[tools]\n\"aqua:openai/codex\"=\"1.0.0\"\n",
        ),
        (
            "[tools]\n\"aqua:openai/codex\" = '2.0.0'\n",
            "[tools]\n\"aqua:openai/codex\" = '1.0.0'\n",
        ),
    ] {
        assert!(
            matches!(evaluate_mise_reconcile(live, source, Some("codex"))?,
            MiseReconcilePlan::Drift { reason, rows } if reason == "formatting_or_comment_drift" && rows.is_empty())
        );
    }
    Ok(())
}
