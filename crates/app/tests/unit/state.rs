#![cfg(test)]
use super::*;
use crate::{domain::*, test_support::*};

#[test]
fn journal_integrity_checks_each_frozen_binding() -> TestResult {
    let valid = journal()?;
    validate_journal(&valid)?;
    for field in 0..8 {
        let mut bad = valid.clone();
        match field {
            0 => bad.plan.timeout_secs = 0,
            1 => bad.schema_version = 2,
            2 => bad.id = "not-an-operation".into(),
            3 => bad.plan.package.name = "tamper".into(),
            4 => bad.plan.target = Some("2".into()),
            5 => bad.scope.home = "/other/home".into(),
            6 => bad.scope_sha256 = "bad".into(),
            _ => bad.home = "/other/home".into(),
        }
        assert_eq!(
            validate_journal(&bad).err().ok_or("rejected")?.code,
            "invalid_journal"
        );
    }
    assert!(valid_id("OP-123-abc"));
    assert!(!valid_id("OTHER"));
    assert!(!valid_id("OP-../outside"));
    Ok(())
}

#[test]
fn recovery_policy_has_explicit_terminal_inflight_and_unknown_paths() -> TestResult {
    let base = journal()?;
    let state = base.state.clone();
    for phase in [Phase::Succeeded, Phase::Failed] {
        let mut j = base.clone();
        j.phase = phase;
        assert_eq!(
            recovery(&j, &state, true, false, false)?,
            Recovery::Terminal
        );
        assert!(!pending(phase));
    }
    for phase in [Phase::Prepared, Phase::Running, Phase::Unknown] {
        assert!(pending(phase));
    }
    assert!(recovery(&base, &state, true, true, false).is_err());
    assert_eq!(
        recovery(&base, &state, false, true, false)?,
        Recovery::ResolveFailed
    );
    assert_eq!(
        recovery(&base, &state, false, false, false)?,
        Recovery::Verify
    );
    let mut j = base.clone();
    j.phase = Phase::Prepared;
    j.process_group = None;
    assert_eq!(recovery(&j, &state, false, false, false)?, Recovery::NoGate);
    j.plan.action = Action::Adopt;
    assert_eq!(
        recovery(&j, &state, false, false, true)?,
        Recovery::CompleteAdopt
    );
    assert_eq!(recovery(&j, &state, false, false, false)?, Recovery::NoGate);
    for mode in 0..3 {
        let mut j = base.clone();
        match mode {
            0 => j.process_group = None,
            1 => j.plan.synchronous = false,
            _ => j.plan.delegated = true,
        }
        assert!(recovery(&j, &state, false, false, false).is_err());
    }
    assert!(
        recovery(
            &base,
            std::path::Path::new("/other/state"),
            false,
            false,
            false
        )
        .is_err()
    );
    assert!(completion(Some(0), false, false).is_none());
    for tuple in [
        (Some(0), true, false),
        (Some(0), false, true),
        (Some(9), false, false),
        (None, false, false),
    ] {
        assert!(completion(tuple.0, tuple.1, tuple.2).is_some());
    }
    Ok(())
}

#[test]
fn expected_results_and_owner_receipt_are_not_version_only() -> TestResult {
    let mut j = journal()?;
    assert!(expected(
        &j,
        &Observation::Present {
            evidence: None,
            version: "1.0.0".into()
        }
    ));
    assert!(!expected(
        &j,
        &Observation::Present {
            evidence: None,
            version: "2.0.0".into()
        }
    ));
    assert!(!expected(&j, &Observation::Missing { evidence: None }));
    j.plan.target = None;
    assert!(expected(
        &j,
        &Observation::Present {
            evidence: None,
            version: "anything".into()
        }
    ));
    j.plan.action = Action::Uninstall;
    assert!(expected(&j, &Observation::Missing { evidence: None }));
    assert!(!expected(
        &j,
        &Observation::Unknown {
            reason: "failed".into()
        }
    ));
    let p = package();
    let s = scope();
    let before = Observation::Present {
        evidence: None,
        version: "1.0.0".into(),
    };
    let q: QuerySpec = command("query").into();
    let receipt = Receipt {
        scope: Some(s.clone()),
        query: Some(q.clone()),
        package: p.clone(),
        observation: before.clone(),
        operation_id: "OP-1".into(),
        updated_at: 1,
    };
    existing_owner(&s, &p, None)?;
    existing_owner(&s, &p, Some(&receipt))?;
    receipt_matches(&s, &p, &before, &q, &receipt)?;
    for mode in 0..4 {
        let mut bad = receipt.clone();
        match mode {
            0 => bad.package.name = "different".into(),
            1 => bad.observation = Observation::Missing { evidence: None },
            2 => bad.query = None,
            _ => bad.scope = None,
        }
        assert!(receipt_matches(&s, &p, &before, &q, &bad).is_err());
    }
    let mut other = s.clone();
    other.home = "/different".into();
    assert!(existing_owner(&other, &p, Some(&receipt)).is_err());
    let mut changed = p.clone();
    changed.provider = Provider::Brew {
        package: "demo".into(),
        cask: false,
    };
    assert!(existing_owner(&s, &changed, Some(&receipt)).is_err());
    Ok(())
}

#[test]
fn journal_authority_repairs_only_matching_frozen_inputs() -> TestResult {
    let valid = journal()?;
    assert_eq!(
        recovery_journal(&valid.id, Some(valid.clone()), None)?.id,
        valid.id
    );
    let mut old = valid.clone();
    old.phase = Phase::Prepared;
    assert_eq!(
        recovery_journal(&valid.id, Some(valid.clone()), Some(old))?.phase,
        Phase::Running
    );
    assert!(recovery_journal(&valid.id, None, None).is_err());
    assert!(recovery_journal(&valid.id, None, Some(valid.clone())).is_err());
    let mut other = valid.clone();
    other.id = "OP-2".into();
    assert!(recovery_journal(&valid.id, Some(other.clone()), None).is_err());
    let mut terminal = valid.clone();
    terminal.phase = Phase::Succeeded;
    assert_eq!(
        recovery_journal(&valid.id, Some(other), Some(terminal))?.id,
        valid.id
    );
    for field in 0..5 {
        let mut old = valid.clone();
        match field {
            0 => {
                old.plan.target = Some("2.0.0".into());
                old.plan_sha256 = fingerprint(&old.plan)?;
            }
            1 => {
                old.scope.paths.insert("FIELD".into(), None);
                old.scope_sha256 = fingerprint(&old.scope)?;
            }
            2 => old.state = "/other-state".into(),
            3 => old.cache = "/other-cache".into(),
            _ => old.settings = "/other-settings".into(),
        }
        assert!(recovery_journal(&valid.id, Some(valid.clone()), Some(old)).is_err());
    }
    let mut bad = valid.clone();
    bad.schema_version = 9;
    assert!(recovery_journal(&valid.id, Some(valid.clone()), Some(bad.clone())).is_err());
    assert!(recovery_journal(&valid.id, Some(bad), None).is_err());
    Ok(())
}

#[test]
fn planning_requires_observation_exact_targets_and_owned_slot() -> TestResult {
    let mut p = package();
    let missing = Observation::Missing { evidence: None };
    plan_inputs(&p, Action::Install, &missing, None)?;
    p.exact_version_required = true;
    assert!(plan_inputs(&p, Action::Install, &missing, None).is_err());
    assert!(version_request(&p, Action::Upgrade, None).is_err());
    version_request(&p, Action::Install, Some("1.0.0"))?;
    version_request(&p, Action::Uninstall, None)?;
    p.provider = Provider::Brew {
        package: "demo".into(),
        cask: false,
    };
    assert!(plan_inputs(&p, Action::Install, &missing, Some("1.0.0")).is_err());
    p = package();
    assert!(
        plan_inputs(
            &p,
            Action::Install,
            &Observation::Unknown {
                reason: "unobserved".into()
            },
            None
        )
        .is_err()
    );
    require_install_slot(Action::Install, &missing)?;
    require_install_slot(
        Action::Upgrade,
        &Observation::Present {
            evidence: None,
            version: "1".into(),
        },
    )?;
    assert!(
        require_install_slot(
            Action::Install,
            &Observation::Present {
                evidence: None,
                version: "1".into()
            }
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn unsupported_path_encoding_is_an_error_not_a_valid_fingerprint() -> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let bad_path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![255]));
    let mut j = journal()?;
    j.scope.home = bad_path.clone();
    assert!(validate_journal(&j).is_err());
    let mut p = package();
    p.paths.push(OwnedPath {
        path: bad_path.clone(),
        kind: PathKind::Payload,
    });
    let mut j = journal()?;
    j.plan.package = p.clone();
    assert!(validate_journal(&j).is_err());
    let mut j = journal()?;
    j.plan.command.as_mut().ok_or("command")?.program = bad_path.clone();
    assert!(validate_journal(&j).is_err());
    let s = scope();
    assert!(cache_version(&p, &s, None).is_err());
    let mut bad_scope = s.clone();
    bad_scope.home = bad_path.clone();
    assert!(cache_version(&package(), &bad_scope, None).is_err());
    let good = package();
    let q: QuerySpec = command("query").into();
    let before = Observation::Present {
        evidence: None,
        version: "1".into(),
    };
    let mut receipt = Receipt {
        package: good.clone(),
        scope: Some(s.clone()),
        query: Some(q.clone()),
        observation: before.clone(),
        operation_id: "OP-1".into(),
        updated_at: 1,
    };
    receipt.package = p;
    assert!(receipt_matches(&s, &good, &before, &q, &receipt).is_err());
    let mut old = package();
    if let Provider::Command { query, .. } = &mut old.provider {
        query.program = bad_path;
    }
    receipt.package = old.clone();
    assert!(existing_owner(&s, &good, Some(&receipt)).is_err());
    receipt.package = good.clone();
    assert!(existing_owner(&s, &old, Some(&receipt)).is_err());
    let mut bad = good.clone();
    bad.paths.push(OwnedPath {
        path: std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![255])),
        kind: PathKind::Payload,
    });
    assert!(receipt_matches(&s, &bad, &before, &q, &receipt).is_err());
    let j = journal()?;
    assert!(require_fresh_plan(&j.plan, &bad, &j.plan.before).is_err());
    Ok(())
}

#[test]
fn workers_plans_and_resolution_need_exact_protocol_identity() -> TestResult {
    let j = journal()?;
    let path = j.state.join("operations").join(format!("{}.json", j.id));
    worker_gate(*b"RUN\n")?;
    assert!(worker_gate(*b"STOP").is_err());
    worker_identity(&j, &path, 999, 999)?;
    for field in 0..5 {
        let mut wrong = j.clone();
        let (pid, group, p) = match field {
            0 => {
                wrong.phase = Phase::Prepared;
                (999, 999, path.clone())
            }
            1 => {
                wrong.process_group = None;
                (999, 999, path.clone())
            }
            2 => (999, 1, path.clone()),
            3 => (u32::MAX, 1, path.clone()),
            _ => (999, 999, std::path::PathBuf::from("/other")),
        };
        assert!(worker_identity(&wrong, &p, pid, group).is_err());
    }
    require_fresh_plan(&j.plan, &j.plan.package, &j.plan.before)?;
    let mut other = j.plan.package.clone();
    other.name = "other".into();
    assert!(require_fresh_plan(&j.plan, &other, &j.plan.before).is_err());
    assert!(
        require_fresh_plan(
            &j.plan,
            &j.plan.package,
            &Observation::Unknown {
                reason: "changed".into()
            }
        )
        .is_err()
    );
    let mut r = Receipt {
        query: None,
        scope: None,
        package: package(),
        observation: j.plan.before.clone(),
        operation_id: j.id.clone(),
        updated_at: 1,
    };
    assert!(adopt_was_committed(&j, Some(&r)));
    assert!(!adopt_was_committed(&j, None));
    r.operation_id = "OP-other".into();
    assert!(!adopt_was_committed(&j, Some(&r)));
    r.operation_id = j.id.clone();
    r.observation = Observation::Present {
        evidence: None,
        version: "other".into(),
    };
    assert!(!adopt_was_committed(&j, Some(&r)));
    assert_eq!(
        version_resolution(Action::Uninstall, true, None, None)?,
        Resolution::Absent
    );
    assert_eq!(
        version_resolution(Action::Install, false, Some("1.0.0"), None)?,
        Resolution::Explicit("1.0.0".into())
    );
    assert_eq!(
        version_resolution(Action::Upgrade, true, None, Some("1.0.0"))?,
        Resolution::Cached("1.0.0".into())
    );
    assert_eq!(
        version_resolution(Action::Install, true, None, None)?,
        Resolution::Unresolved
    );
    assert_eq!(
        version_resolution(Action::Install, false, None, None)?,
        Resolution::Latest
    );
    assert!(version_resolution(Action::Install, false, Some("latest"), None).is_err());
    Ok(())
}

#[test]
fn receipt_commit_uses_installer_completion_not_receipt_presence() -> TestResult {
    let j = journal()?;
    let mut receipts = Receipts {
        schema_version: 1,
        installations: std::collections::BTreeMap::default(),
    };
    receipt_schema(&receipts)?;
    receipts.schema_version = 2;
    assert!(receipt_schema(&receipts).is_err());
    receipts.schema_version = 1;
    commit_receipt(
        &mut receipts,
        &j,
        Observation::Present {
            evidence: None,
            version: "1.0.0".into(),
        },
        12,
    )?;
    assert_eq!(
        receipts
            .installations
            .get(&j.plan.package.id)
            .ok_or("receipt")?
            .updated_at,
        12
    );
    let mut removal = j;
    removal.plan.action = Action::Uninstall;
    commit_receipt(
        &mut receipts,
        &removal,
        Observation::Missing { evidence: None },
        15,
    )?;
    assert!(receipts.installations.is_empty());
    Ok(())
}

#[test]
fn a_second_commit_cannot_modify_the_first_commit_for_that_operation() -> TestResult {
    let j = journal()?;
    let observed = Observation::Present {
        version: "1.0.0".into(),
        evidence: None,
    };
    let mut receipts = Receipts {
        schema_version: 1,
        installations: std::collections::BTreeMap::new(),
    };
    assert!(commit_receipt(&mut receipts, &j, observed.clone(), 1)?);
    let bytes = serde_json::to_vec(&receipts)?;
    assert!(!commit_receipt(&mut receipts, &j, observed.clone(), 2)?);
    assert!(
        commit_receipt(
            &mut receipts,
            &j,
            Observation::Missing { evidence: None },
            2
        )
        .is_err()
    );
    assert_eq!(serde_json::to_vec(&receipts)?, bytes);
    for field in ["query", "scope", "package"] {
        let mut altered = receipts.clone();
        let record = altered
            .installations
            .get_mut(&j.plan.package.id)
            .ok_or("receipt")?;
        match field {
            "query" => record.query = None,
            "scope" => record.scope = None,
            "package" => record.package.name = "other".into(),
            _ => return Err("fixture".into()),
        }
        assert!(commit_receipt(&mut altered, &j, observed.clone(), 3).is_err());
    }
    let cfg_target = std::path::PathBuf::from("/fixture/home/.zshrc");
    let cfg_id = crate::policy::config_local::config_id(&cfg_target)?;
    let cfg_pkg = Package {
        id: cfg_id.clone(),
        name: cfg_target.display().to_string(),
        kind: Kind::Config,
        official_url: String::new(),
        provider: Provider::ConfigObserved {
            target: cfg_target,
            source: None,
            authority: ConfigAuthority::Untracked,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let cfg_stamp = ConfigFileStamp {
        identity: FileId {
            device: 1,
            inode: 5,
        },
        mode: 0o600,
        content_digest: "a".repeat(64),
        fingerprint: "b".repeat(64),
    };
    let cfg_obs = Observation::ConfigFile {
        authority: ConfigAuthority::Untracked,
        target: Some(cfg_stamp),
        source: None,
        mode_matches_source: None,
    };
    plan_inputs(&cfg_pkg, Action::Adopt, &cfg_obs, None)?;
    assert!(plan_inputs(&cfg_pkg, Action::Adopt, &cfg_obs, Some("1.0.0")).is_err());
    assert!(plan_inputs(&cfg_pkg, Action::Upgrade, &cfg_obs, None).is_err());
    assert!(
        plan_inputs(
            &cfg_pkg,
            Action::Adopt,
            &Observation::Unknown {
                reason: "io".into()
            },
            None
        )
        .is_err()
    );
    assert!(plan_inputs(&package(), Action::Adopt, &cfg_obs, None).is_err());
    let mut cfg_j = journal()?;
    cfg_j.plan.action = Action::Adopt;
    assert!(!expected(&cfg_j, &cfg_obs));
    cfg_j.plan.package = cfg_pkg;
    cfg_j.plan.before = cfg_obs.clone();
    assert!(expected(&cfg_j, &cfg_obs));
    assert!(!expected(
        &cfg_j,
        &Observation::ConfigFile {
            authority: ConfigAuthority::Untracked,
            target: None,
            source: None,
            mode_matches_source: None,
        }
    ));
    Ok(())
}

#[test]
fn capability_observer_plan_inputs_and_expected_matching_fail_closed() -> TestResult {
    let lock = std::path::PathBuf::from("/fixture/home/.agents/.skill-lock.json");
    let cap_id =
        crate::policy::capability_local::skill_capability_id(&lock, "demo", "github", "o/r")?;
    let cap_pkg = Package {
        id: cap_id,
        name: "demo".into(),
        kind: Kind::Skill,
        official_url: "https://github.com/vercel-labs/skills".into(),
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: "demo".into(),
            manager: "skills-cli:github".into(),
            record: lock,
            payload: "/fixture/home/.agents/skills/demo".into(),
            source: Some("o/r".into()),
            profile_id: None,
            settings: None,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let cap_obs = Observation::CapabilityPack {
        capability: CapabilityKind::SkillsCli,
        pack: Some(CapabilityStamp {
            record_digest: "a".repeat(64),
            payload_digest: "b".repeat(64),
            payload_identity: FileId {
                device: 1,
                inode: 7,
            },
            file_count: 1,
            revision: None,
            configured_enabled: None,
            fingerprint: "c".repeat(64),
        }),
    };
    plan_inputs(&cap_pkg, Action::Adopt, &cap_obs, None)?;
    assert!(plan_inputs(&cap_pkg, Action::Adopt, &cap_obs, Some("1.0.0")).is_err());
    assert!(plan_inputs(&cap_pkg, Action::Uninstall, &cap_obs, None).is_err());
    assert!(plan_inputs(&package(), Action::Adopt, &cap_obs, None).is_err());
    let mut cap_j = journal()?;
    cap_j.plan.action = Action::Adopt;
    assert!(!expected(&cap_j, &cap_obs));
    cap_j.plan.package = cap_pkg;
    cap_j.plan.before = cap_obs.clone();
    assert!(expected(&cap_j, &cap_obs));
    assert!(!expected(
        &cap_j,
        &Observation::CapabilityPack {
            capability: CapabilityKind::SkillsCli,
            pack: None,
        }
    ));
    Ok(())
}

#[test]
fn npm_authority_change_requires_readopt_while_prefix_change_blocks_owner() -> TestResult {
    let cap_j = journal()?;
    let npm_old = Package {
        id: "npm:demo".into(),
        name: "demo".into(),
        kind: Kind::Cli,
        official_url: "https://docs.npmjs.com/cli/v11".into(),
        provider: Provider::Npm {
            package: "demo".into(),
            prefix: "/fixture/npm".into(),
            authority: None,
        },
        paths: vec![],
        exact_version_required: false,
    };
    let mut npm_new = npm_old.clone();
    npm_new.provider = Provider::Npm {
        package: "demo".into(),
        prefix: "/fixture/npm".into(),
        authority: Some(crate::domain::NpmWriteAuthority {
            allow_remove: true,
            registry: None,
        }),
    };
    let obs = Observation::Present {
        version: "1.0.0".into(),
        evidence: None,
    };
    let q = crate::domain::QuerySpec::Command(crate::test_support::command("query"));
    let rec = crate::domain::Receipt {
        scope: Some(cap_j.scope.clone()),
        query: Some(q.clone()),
        package: npm_old,
        observation: obs.clone(),
        operation_id: "OP-1".into(),
        updated_at: 1,
    };
    let err = receipt_matches(&cap_j.scope, &npm_new, &obs, &q, &rec)
        .err()
        .ok_or("authority drift")?;
    assert_eq!(err.code, "installation_drift");
    assert!(err.message.contains("写权限"));
    existing_owner(&cap_j.scope, &npm_new, Some(&rec))?;
    let mut different_prefix = npm_new;
    if let Provider::Npm { prefix, .. } = &mut different_prefix.provider {
        *prefix = "/other/npm".into();
    }
    assert_eq!(
        existing_owner(&cap_j.scope, &different_prefix, Some(&rec))
            .err()
            .ok_or("prefix changed")?
            .code,
        "manager_scope_changed"
    );
    Ok(())
}
