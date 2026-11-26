use super::*;
#[test]
fn aliases_and_separate_roots() -> Result<()> {
    assert_eq!(normalize_name("Demo__TOOL..x")?, "demo-tool-x");
    for bad in ["", "-name", "name.", "../name", "a/b", "λ"] {
        assert!(normalize_name(bad).is_err());
    }
    assert_eq!(
        tool_id(Path::new("/A"), "Demo_Tool")?,
        tool_id(Path::new("/A"), "demo-tool")?
    );
    assert_ne!(
        tool_id(Path::new("/A"), "demo-tool")?,
        tool_id(Path::new("/B"), "demo-tool")?
    );
    Ok(())
}

fn uv_plan() -> Result<Plan> {
    use crate::domain::*;
    let mut plan = crate::test_support::journal()?.plan;
    plan.package.provider = Provider::UvObserved {
        package: "Demo_Tool".into(),
        binding: UvToolBinding {
            tool_dir: "/tools".into(),
            bin_dir: "/bin".into(),
        },
    };
    let anchor = |path: &str| UvDirectoryAnchor {
        path: path.into(),
        identity: FileId {
            device: 1,
            inode: 2,
        },
    };
    plan.query = Some(QuerySpec::LocalUvTool(LocalUvToolQuery {
        kind: UvQueryKind::LocalV1,
        name: "demo-tool".into(),
        namespace: ExistingUvNamespace {
            tool_dir: anchor("/tools"),
            bin_dir: anchor("/bin"),
        },
    }));
    plan.uv_owner = Some(UvOwnerSnapshot {
        selected_owner: plan.package.id.clone(),
        entries: std::collections::BTreeMap::new(),
    });
    plan.action = Action::Adopt;
    plan.command = None;
    plan.target = None;
    plan.before = Observation::UvTool {
        version: "1.0".into(),
        evidence: UvToolEvidence {
            namespace_identity: "a".repeat(64),
            payload_identity: FileId {
                device: 1,
                inode: 3,
            },
            receipt_sha256: "b".repeat(64),
            metadata_sha256: "c".repeat(64),
            pyvenv_sha256: "d".repeat(64),
            entrypoints: vec![],
            fingerprint: "e".repeat(64),
        },
    };
    Ok(plan)
}
#[test]
fn only_bound_adopt_is_permitted_and_cannot_lend_authority() -> Result<()> {
    use crate::domain::*;
    let plan = uv_plan()?;
    validate_plan(&plan)?;
    for action in [
        Action::Install,
        Action::Upgrade,
        Action::Uninstall,
        Action::Update,
        Action::Cleanup,
        Action::Maintain,
        Action::Start,
        Action::Stop,
        Action::Apply,
        Action::Restore,
        Action::Reconcile,
    ] {
        assert_eq!(
            super::action(&plan.package.provider, action)
                .err()
                .map(|e| e.code),
            Some("uv_write_blocked_review".into())
        );
    }
    for mutation in 0..5 {
        let mut bad = plan.clone();
        match mutation {
            0 => bad.command = Some(crate::test_support::command("write")),
            1 => bad.delegated = true,
            2 => bad.synchronous = false,
            3 => bad.target = Some("2".into()),
            _ => bad.query = None,
        }
        assert!(validate_plan(&bad).is_err());
    }
    let mut missing = plan.clone();
    missing.before = Observation::Missing { evidence: None };
    assert!(validate_plan(&missing).is_err());
    let mut legacy = plan.clone();
    legacy.package.provider = Provider::Uv {
        package: "demo-tool".into(),
        binding: None,
    };
    assert!(validate_plan(&legacy).is_err());
    let mut declared = plan.clone();
    declared.package.provider = Provider::Uv {
        package: "demo-tool".into(),
        binding: Some(UvToolBinding {
            tool_dir: "/tools".into(),
            bin_dir: "/bin".into(),
        }),
    };
    validate_plan(&declared)?;
    let generic = crate::test_support::journal()?.plan;
    validate_plan(&generic)?;
    let mut lent = plan.clone();
    lent.package = generic.package.clone();
    assert!(validate_plan(&lent).is_err());
    lent.query = generic.query.clone();
    assert!(validate_plan(&lent).is_err());
    let mut wrong = plan.clone();
    wrong.query = generic.query.clone();
    assert!(validate_plan(&wrong).is_err());
    let mut wrong = plan.clone();
    if let Some(QuerySpec::LocalUvTool(q)) = &mut wrong.query {
        q.name = "different".into();
    }
    assert!(validate_plan(&wrong).is_err());
    let mut wrong = plan.clone();
    if let Some(QuerySpec::LocalUvTool(q)) = &mut wrong.query {
        q.name = "Demo_Tool".into();
    }
    assert!(validate_plan(&wrong).is_err());
    let mut wrong = plan.clone();
    if let Some(QuerySpec::LocalUvTool(q)) = &mut wrong.query {
        q.namespace.bin_dir.path = "/other".into();
    }
    assert!(validate_plan(&wrong).is_err());
    let mut wrong = plan.clone();
    if let Provider::UvObserved { binding, .. } = &mut wrong.package.provider {
        binding.tool_dir = "relative".into();
    }
    assert!(validate_package(&wrong.package).is_err());
    if let Some(QuerySpec::LocalUvTool(q)) = plan.query {
        assert_eq!(query_id(&q)?, tool_id(Path::new("/tools"), "demo-tool")?);
    }
    assert!(normalize_name(&"x".repeat(215)).is_err());
    Ok(())
}
#[test]
fn old_uv_wire_shape_and_undefined_authority_are_strict() -> crate::test_support::TestResult {
    use crate::domain::*;
    let old = r#"{"type":"uv","package":"demo-tool"}"#;
    let p: Provider = serde_json::from_str(old)?;
    assert_eq!(serde_json::to_string(&p)?, old);
    assert!(
        serde_json::from_str::<Provider>(
            r#"{"type":"uv","package":"demo-tool","authority":{"allow_remove":true}}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<Provider>(r#"{"type":"uv","package":"demo-tool","binding":null}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<UvToolBinding>(
            r#"{"tool_dir":"/tools","bin_dir":"/bin","allow_remove":true}"#
        )
        .is_err()
    );
    let plan = uv_plan()?;
    let query = plan.query.ok_or("query")?;
    let bytes = serde_json::to_vec(&query)?;
    assert_eq!(serde_json::from_slice::<QuerySpec>(&bytes)?, query);
    Ok(())
}

#[test]
fn uv_persisted_inputs_and_receipt_cas_reject_invalid_or_changed_authority()
-> crate::test_support::TestResult {
    use crate::domain::*;
    use std::collections::{BTreeMap, BTreeSet};
    let plan = uv_plan()?;
    let mut journal = crate::test_support::journal()?;
    journal.plan = plan.clone();
    journal.phase = Phase::Prepared;
    journal.process_group = None;
    validate_journal(&journal)?;
    let mut running = journal.clone();
    running.phase = Phase::Running;
    assert!(validate_journal(&running).is_err());
    running.phase = Phase::Prepared;
    running.process_group = Some(1);
    assert!(validate_journal(&running).is_err());
    for which in 0..4 {
        let mut wrong = plan.clone();
        if which == 0 {
            wrong.uv_owner = None;
        } else if let Some(owner) = &mut wrong.uv_owner {
            match which {
                1 => owner.selected_owner = "other".into(),
                2 => {
                    owner.entries = BTreeMap::from([
                        ("a".into(), BTreeSet::new()),
                        ("b".into(), BTreeSet::new()),
                    ]);
                }
                _ => owner.entries = BTreeMap::from([("other".into(), BTreeSet::new())]),
            }
        }
        assert!(validate_plan(&wrong).is_err());
    }
    let empty = Receipts {
        schema_version: 1,
        installations: BTreeMap::new(),
    };
    validate_commit(&journal, &empty, &plan.before)?;
    let mut wrong = journal.clone();
    wrong.plan.uv_owner = None;
    assert!(validate_commit(&wrong, &empty, &plan.before).is_err());
    let mut wrong = journal.clone();
    wrong.plan.query = None;
    assert!(validate_commit(&wrong, &empty, &plan.before).is_err());
    assert!(validate_commit(&journal, &empty, &Observation::Missing { evidence: None }).is_err());
    let record = Receipt {
        scope: Some(journal.scope.clone()),
        query: plan.query.clone(),
        package: plan.package.clone(),
        observation: plan.before.clone(),
        operation_id: "OP-PREVIOUS".into(),
        updated_at: 0,
    };
    let mut existing = Receipts {
        schema_version: 1,
        installations: BTreeMap::from([(plan.package.id.clone(), record.clone())]),
    };
    assert!(validate_commit(&journal, &existing, &plan.before).is_err());
    if let Some(snapshot) = &mut journal.plan.uv_owner {
        snapshot.entries.insert(
            plan.package.id.clone(),
            BTreeSet::from([
                format!("receipt:{}", fingerprint(&record)?),
                "declaration:original".into(),
            ]),
        );
    }
    validate_commit(&journal, &existing, &plan.before)?;
    if let Some(record) = existing.installations.get_mut(&plan.package.id) {
        record.operation_id = journal.id.clone();
    }
    validate_commit(&journal, &existing, &plan.before)?;
    if let Some(record) = existing.installations.get_mut(&plan.package.id) {
        record.query = None;
        record.operation_id = "OP-DIFFERENT".into();
    }
    assert!(validate_commit(&journal, &existing, &plan.before).is_err());
    existing.installations.clear();
    let mut unrelated = record.clone();
    unrelated.package.id = "unrelated".into();
    unrelated.query = Some(crate::test_support::command("query").into());
    existing
        .installations
        .insert(unrelated.package.id.clone(), unrelated);
    let mut new = journal.clone();
    new.plan.uv_owner = plan.uv_owner.clone();
    validate_commit(&new, &existing, &plan.before)?;
    Ok(())
}
#[test]
fn uv_package_binding_and_state_expectations_are_explicit() -> crate::test_support::TestResult {
    use crate::domain::*;
    let plan = uv_plan()?;
    let mut journal = crate::test_support::journal()?;
    journal.plan = plan.clone();
    journal.phase = Phase::Prepared;
    journal.process_group = None;
    let mut wrong = plan.package.clone();
    if let Provider::UvObserved { binding, .. } = &mut wrong.provider {
        binding.bin_dir = "relative".into();
    }
    assert!(validate_package(&wrong).is_err());
    let mut bad_catalog = Catalog {
        schema_version: 1,
        packages: vec![plan.package.clone()],
    };
    if let Provider::UvObserved { package, .. } = &mut bad_catalog.packages[0].provider {
        *package = "bad/".into();
    }
    assert!(bad_catalog.validate().is_err());
    assert!(tool_id(Path::new("relative"), "demo").is_err());
    assert!(tool_id(Path::new("/root"), "bad/").is_err());
    let mut invalid = plan.package.clone();
    if let Provider::UvObserved { package, .. } = &mut invalid.provider {
        *package = "bad/".into();
    }
    assert!(binding(&invalid, plan.query.as_ref().ok_or("query")?).is_err());
    assert!(
        crate::policy::operation::commands::available_query(Path::new("/home"), &plan.package)
            .is_err()
    );
    assert!(
        crate::policy::operation::state::plan_inputs(
            &plan.package,
            Action::Adopt,
            &plan.before,
            Some("1")
        )
        .is_err()
    );
    assert!(
        crate::policy::operation::state::plan_inputs(
            &crate::test_support::package(),
            Action::Adopt,
            &plan.before,
            None
        )
        .is_err()
    );
    assert!(crate::policy::operation::state::expected(
        &journal,
        &plan.before
    ));
    journal.plan.action = Action::Upgrade;
    assert!(!crate::policy::operation::state::expected(
        &journal,
        &plan.before
    ));
    journal.plan.action = Action::Adopt;
    journal.plan.before = Observation::Missing { evidence: None };
    assert!(!crate::policy::operation::state::expected(
        &journal,
        &plan.before
    ));
    journal.plan.package = crate::test_support::package();
    assert!(!crate::policy::operation::state::expected(
        &journal,
        &plan.before
    ));
    Ok(())
}

#[test]
fn uv_invalid_nested_inputs_propagate_through_public_policy_boundaries()
-> crate::test_support::TestResult {
    use crate::domain::*;
    use std::{collections::BTreeMap, os::unix::ffi::OsStringExt};
    let plan = uv_plan()?;
    let mut journal = crate::test_support::journal()?;
    journal.plan = plan.clone();
    journal.phase = Phase::Prepared;
    journal.process_group = None;
    let mut wrong = plan.clone();
    wrong.action = Action::Upgrade;
    assert!(validate_plan(&wrong).is_err());
    assert!(
        crate::policy::operation::state::plan_inputs(
            &plan.package,
            Action::Upgrade,
            &plan.before,
            None
        )
        .is_err()
    );
    let mut wrong = journal.clone();
    wrong.plan.command = Some(crate::test_support::command("illegal"));
    assert!(validate_journal(&wrong).is_err());
    wrong.plan_sha256 = fingerprint(&wrong.plan)?;
    wrong.plan.recipe_sha256 = fingerprint(&wrong.plan.package)?;
    wrong.plan_sha256 = fingerprint(&wrong.plan)?;
    assert!(crate::policy::operation::state::validate_journal(&wrong).is_err());
    let empty = Receipts {
        schema_version: 1,
        installations: BTreeMap::new(),
    };
    assert!(validate_commit(&wrong, &empty, &plan.before).is_err());
    let mut generic = wrong.clone();
    generic.plan.package = crate::test_support::package();
    assert!(validate_commit(&generic, &empty, &plan.before).is_err());
    let mut package = plan.package.clone();
    let mut query = plan.query.clone().ok_or("query")?;
    if let Provider::UvObserved { binding, .. } = &mut package.provider {
        binding.bin_dir = "relative".into();
    }
    if let QuerySpec::LocalUvTool(q) = &mut query {
        q.namespace.bin_dir.path = "relative".into();
    }
    assert!(binding(&package, &query).is_err());
    let mut wrong = journal.clone();
    if let Some(QuerySpec::LocalUvTool(q)) = &mut wrong.plan.query {
        q.namespace.tool_dir.path = "relative".into();
    }
    assert!(validate_commit(&wrong, &empty, &plan.before).is_err());
    let record = Receipt {
        scope: Some(journal.scope.clone()),
        query: plan.query.clone(),
        package: plan.package.clone(),
        observation: plan.before.clone(),
        operation_id: "OP-PREVIOUS".into(),
        updated_at: 0,
    };
    let mut receipts = Receipts {
        schema_version: 1,
        installations: BTreeMap::from([("other".into(), record.clone())]),
    };
    if let Some(record) = receipts.installations.get_mut("other")
        && let Some(QuerySpec::LocalUvTool(q)) = &mut record.query
    {
        q.name = "bad/".into();
    }
    assert!(validate_commit(&journal, &receipts, &plan.before).is_err());
    let mut invalid = record;
    invalid.package.paths.push(OwnedPath {
        path: std::ffi::OsString::from_vec(vec![0xff]).into(),
        kind: PathKind::Payload,
    });
    receipts.installations.insert("other".into(), invalid);
    assert!(validate_commit(&journal, &receipts, &plan.before).is_err());
    Ok(())
}
