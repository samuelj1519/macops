use super::*;
use crate::{domain::*, test_support::TestResult};
use serde_json::{Value, json};

use crate::test_support::brew::*;

#[test]
fn stable_owner_excludes_every_binding_field_and_keeps_prefix_and_native_name() -> TestResult {
    let q = query()?;
    let id = logical_id(&q.key)?;
    for name in ["foo", "vendor/tap/foo", "foo@1"] {
        let mut k = q.key.clone();
        k.full_name = name.into();
        assert_eq!(
            logical_id(&k)?,
            crate::domain::resource::id("application", &("brew", &k.canonical_prefix, name))?
        );
    }
    let mut changed = q.clone();
    changed.binding.invocation = "/other/bin/brew".into();
    changed.binding.canonical_program = "/other/repo/bin/brew".into();
    changed.binding.cellar = "/new/cellar".into();
    changed.binding.program_stamp.sha256 = "f".repeat(64);
    changed.context_epoch = 1;
    assert_eq!(logical_id(&changed.key)?, id);
    assert_eq!(
        context_id(&changed.key.canonical_prefix)?,
        q.binding.context_id
    );
    let mut k = q.key.clone();
    k.canonical_prefix = "/other/prefix".into();
    assert_ne!(logical_id(&k)?, id);
    for bad in ["", "homebrew/core/foo", "x/y/z/w", "../foo", "Foo", "-foo"] {
        let mut k = q.key.clone();
        k.full_name = bad.into();
        assert!(logical_id(&k).is_err(), "{bad}");
    }
    let mut k = q.key.clone();
    k.canonical_prefix = "relative".into();
    assert!(logical_id(&k).is_err());
    for (tap, name) in [
        ("one", "foo"),
        ("third/Tap", "foo"),
        ("third/tap", "../foo"),
        ("third/tap", "Foo"),
    ] {
        assert!(
            full_name(&BrewSelection {
                tap: tap.into(),
                name: name.into()
            })
            .is_err()
        );
    }
    Ok(())
}
#[test]
fn strict_native_info_and_every_nested_record_reject_positional_shapes_and_duplicates() -> TestResult
{
    let q = query()?;
    let r = report(&q, &[("homebrew/core", "stable", "1.0.0")]);
    let raw = serde_json::to_vec(&r)?;
    assert_eq!(parse_report(&raw)?, r);
    for pointer in [
        "",
        "/policies",
        "/rack",
        "/rack/cellar_before",
        "/rack/cellar_before/identity",
        "/rack/rack_before",
        "/rack/entries/0",
        "/rack/entries/0/stamp",
        "/rack/kegs/0",
        "/rack/kegs/0/receipt",
        "/rack/kegs/0/receipt/node",
        "/formula",
    ] {
        let mut v = serde_json::to_value(&r)?;
        let target = if pointer.is_empty() {
            &mut v
        } else {
            v.pointer_mut(pointer).ok_or("pointer")?
        };
        let values = target
            .as_object()
            .ok_or("object")?
            .values()
            .cloned()
            .collect();
        *target = Value::Array(values);
        assert!(parse_report(&serde_json::to_vec(&v)?).is_err(), "{pointer}");
    }
    let mut v = serde_json::to_value(&r)?;
    v["formula"]["unknown"] = json!(true);
    assert!(parse_report(&serde_json::to_vec(&v)?).is_err());
    let text = String::from_utf8(raw)?;
    let duplicate = text.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert!(parse_report(duplicate.as_bytes()).is_err());
    assert!(parse_report(&vec![b' '; REPORT_LIMIT + 1]).is_err());
    assert!(parse_report(b"{").is_err());
    Ok(())
}
#[test]
fn typed_missing_requires_complete_same_scope_native_selection() -> TestResult {
    let q = query()?;
    let s = stamp(&q)?;
    let present = observation(
        &q,
        s.clone(),
        report(
            &q,
            &[
                ("homebrew/core", "stable", "1.0.0"),
                ("vendor/tap", "stable", "2.0.0"),
            ],
        ),
    )?;
    assert_eq!(present.version(), Some("1.0.0"));
    assert!(matches!(present, Observation::BrewInstalled { .. }));
    let missing = observation(
        &q,
        s.clone(),
        report(&q, &[("vendor/tap", "stable", "2.0.0")]),
    )?;
    assert!(matches!(missing, Observation::BrewMissing { .. }));
    assert_eq!(missing.version(), None);
    let mut r = report(&q, &[]);
    r.rack.rack_before = None;
    r.rack.rack_after = None;
    assert!(matches!(
        observation(&q, s.clone(), r)?,
        Observation::BrewMissing { .. }
    ));
    Ok(())
}
#[test]
fn every_incomplete_native_proof_is_rejected() -> TestResult {
    let q = query()?;
    let s = stamp(&q)?;
    let base = report(&q, &[("homebrew/core", "stable", "1.0.0")]);
    for key in [
        "schema",
        "query",
        "version",
        "prefix",
        "cellar",
        "home",
        "policies",
        "rack",
        "cellar-stamp",
        "identity",
        "rack-stamp",
        "absent",
        "entries",
        "duplicate-entry",
        "name",
        "native-selected",
        "duplicate-selected",
        "keg-path",
        "tap",
        "spec",
        "receipt",
        "selected-version",
        "no-version",
    ] {
        let mut r = base.clone();
        match key {
            "schema" => r.schema_version = 2,
            "query" => r.query_id = "OTHER".into(),
            "version" => r.homebrew_version = "4.4.1".into(),
            "prefix" => r.prefix = "/other".into(),
            "cellar" => r.cellar = "/other".into(),
            "home" => r.actual_user_config_home = "/other".into(),
            "policies" => r.policies.no_autoremove = false,
            "rack" => r.rack.path = "/other".into(),
            "cellar-stamp" => r.rack.cellar_after.modified_ns += 1,
            "identity" => {
                r.rack.cellar_before.identity.inode += 1;
                r.rack.cellar_after = r.rack.cellar_before.clone();
            }
            "rack-stamp" => r.rack.rack_after = None,
            "absent" => {
                r.rack.rack_before = None;
                r.rack.rack_after = None;
            }
            "entries" => r.rack.entries.clear(),
            "duplicate-entry" => r.rack.entries.push(r.rack.entries[0].clone()),
            "name" => r.rack.entries[0].name = "../escape".into(),
            "native-selected" => r.rack.native_selected.clear(),
            "duplicate-selected" => r
                .rack
                .native_selected
                .push(r.rack.native_selected[0].clone()),
            "keg-path" => r.rack.kegs[0].path = "/outside/1".into(),
            "tap" => r.rack.kegs[0].tap.clear(),
            "spec" => r.rack.kegs[0].spec = "unknown".into(),
            "receipt" => r.rack.kegs[0].receipt.sha256.clear(),
            "selected-version" => r.selected_version = Some("unknown".into()),
            "no-version" => r.selected_version = None,
            _ => return Err("case".into()),
        }
        assert!(observation(&q, s.clone(), r).is_err(), "{key}");
    }
    let mut r = base;
    r.rack.entries.push(BrewRackEntry {
        name: ".DS_Store".into(),
        kind: BrewEntryKind::File,
        stamp: node(false, 44),
    });
    assert!(observation(&q, s, r).is_ok());
    Ok(())
}

#[test]
fn rebind_never_blesses_old_missing_even_after_cellar_continuity() -> TestResult {
    let q = query()?;
    let old = observation(&q, stamp(&q)?, report(&q, &[]))?;
    let record = BrewContextRecord {
        epoch: 0,
        binding: q.binding.clone(),
        observation_valid: true,
        writer_scope_confirmed: false,
        declaration_keys: BTreeMap::new(),
    };
    for cellar in [false, true] {
        let mut binding = q.binding.clone();
        if cellar {
            binding.cellar = "/new/cellar".into();
        } else {
            binding.invocation = "/new/bin/brew".into();
        }
        if cellar {
            assert!(rebind(&record, binding.clone(), false, &BTreeSet::new()).is_err());
        }
        let next = rebind(&record, binding.clone(), true, &BTreeSet::new())?;
        assert_eq!(next.epoch, 1);
        assert!(!next.observation_valid);
        assert_eq!(next.binding.prefix, q.key.canonical_prefix);
        assert!(accept_observation(&next, &q, &old).is_err());
        let mut new = q.clone();
        new.binding = binding;
        new.context_epoch = 1;
        new.env_capture_id = "ENV-2".into();
        let fresh = observation(&new, stamp(&new)?, report(&new, &[]))?;
        assert!(accept_observation(&next, &new, &fresh)?.observation_valid);
        assert!(observation(&new, stamp(&q)?, report(&new, &[])).is_err());
    }
    assert!(rebind(&record, q.binding.clone(), true, &BTreeSet::new()).is_err());
    let mut next = q.binding.clone();
    next.invocation = "/new/bin/brew".into();
    assert!(
        rebind(
            &record,
            next.clone(),
            true,
            &BTreeSet::from([q.binding.context_id.clone()])
        )
        .is_err()
    );
    let mut exhausted = record.clone();
    exhausted.epoch = u64::MAX;
    assert!(rebind(&exhausted, next, true, &BTreeSet::new()).is_err());
    assert!(accept_observation(&record, &q, &Observation::Missing { evidence: None }).is_err());
    Ok(())
}
#[test]
fn bound_plan_is_exact_projection_and_never_accepts_generic_presence() -> TestResult {
    let p = plan()?;
    let view = checked_plan(&p)?.ok_or("view")?;
    assert_eq!(
        invocation_projection(&view)?.command,
        p.command.clone().ok_or("command")?
    );
    assert!(!expected(
        &view,
        &Observation::Present {
            version: "2.0.0".into(),
            evidence: None
        }
    ));
    let mut current = report(view.query, &[("homebrew/core", "stable", "2.0.0")]);
    current.formula.as_mut().ok_or("info")?.outdated = false;
    let observed = observation(view.query, stamp(view.query)?, current)?;
    assert!(expected(&view, &observed));
    let mut missing_contract = p.clone();
    missing_contract.brew = None;
    assert!(checked_plan(&missing_contract).is_err());
    let mut invalid = p.clone();
    invalid.before = Observation::Present {
        version: "1.0.0".into(),
        evidence: None,
    };
    assert!(checked_plan(&invalid).is_err());
    for key in [
        "schema",
        "owner",
        "epoch",
        "fingerprint",
        "env",
        "sync",
        "delegated",
        "confirmation",
        "goal",
        "target",
        "command",
        "head",
        "pin",
        "alias",
        "migration",
        "latest",
        "paths",
        "info",
    ] {
        let mut bad = p.clone();
        let data = bad.brew.as_mut().ok_or("brew")?;
        match key {
            "schema" => data.schema_version = 2,
            "owner" => data.owner.selected_package_id = "other".into(),
            "epoch" => data.context.epoch = 1,
            "fingerprint" => data.context.binding_fingerprint.clear(),
            "env" => data.env_capture_id.clear(),
            "sync" => bad.synchronous = false,
            "delegated" => bad.delegated = false,
            "confirmation" => data.confirmation.effect_contract.clear(),
            "goal" => bad.action = Action::Install,
            "target" => bad.target = Some("3.0.0".into()),
            "command" => bad
                .command
                .as_mut()
                .ok_or("command")?
                .args
                .push("--ignore-dependencies".into()),
            _ => {
                if let Observation::BrewInstalled { evidence, .. } = &mut bad.before {
                    match key {
                        "head" => evidence.report.rack.kegs[0].spec = "head".into(),
                        "info" => evidence.report.formula = None,
                        _ => {
                            let info = evidence.report.formula.as_mut().ok_or("info")?;
                            match key {
                                "pin" => info.pinned = true,
                                "alias" => info.resolution.alias_changed = true,
                                "migration" => info.resolution.migration_needed = true,
                                "latest" => info.latest_full_name = "other".into(),
                                "paths" => info.installed_paths.push("/other/2".into()),
                                _ => return Err("case".into()),
                            }
                        }
                    }
                }
            }
        }
        assert!(checked_plan(&bad).is_err(), "{key}");
    }
    let ordinary = crate::test_support::journal()?.plan;
    assert!(checked_plan(&ordinary)?.is_none());
    let old = serde_json::to_value(&ordinary)?;
    assert!(old.get("brew").is_none());
    let round: Plan = serde_json::from_value(old.clone())?;
    assert_eq!(serde_json::to_value(round)?, old);
    Ok(())
}

#[test]
fn registry_and_owner_keep_one_logical_installation_and_reject_unknown_old_scope() -> TestResult {
    let q = query()?;
    let record = BrewContextRecord {
        epoch: 0,
        binding: q.binding.clone(),
        observation_valid: true,
        writer_scope_confirmed: false,
        declaration_keys: BTreeMap::new(),
    };
    let registry = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::from([(q.binding.context_id.clone(), record.clone())]),
    };
    validate_registry(&registry)?;
    let p = package(&q)?;
    let mut catalog = Catalog {
        schema_version: 1,
        packages: vec![p.clone()],
    };
    let mut receipts = Receipts::default();
    let snapshot = owner_snapshot(&q.key, &catalog, &receipts, &registry)?;
    assert_eq!(snapshot.logical_owner_id, logical_id(&q.key)?);
    assert_eq!(snapshot.entries.len(), 1);
    let observed = observation(
        &q,
        stamp(&q)?,
        report(&q, &[("homebrew/core", "stable", "1.0.0")]),
    )?;
    let receipt = Receipt {
        scope: Some(Scope {
            home: q.binding.metadata_scope.home.clone(),
            paths: BTreeMap::new(),
            service_identity: None,
        }),
        query: Some(QuerySpec::BrewFormula(Box::new(q.clone()))),
        package: p.clone(),
        observation: observed,
        operation_id: "OP-OLD".into(),
        updated_at: 1,
    };
    receipts.installations.insert(p.id.clone(), receipt.clone());
    assert_eq!(
        owner_snapshot(&q.key, &catalog, &receipts, &registry)?
            .entries
            .values()
            .next()
            .ok_or("entries")?
            .len(),
        2
    );
    let mut other = p.clone();
    other.id = "another-owner".into();
    catalog.packages.push(other);
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &registry).is_err());
    catalog.packages.pop();
    let mut old = p.clone();
    old.id = "declared:foo".into();
    old.provider = Provider::Brew {
        package: "foo".into(),
        cask: false,
    };
    catalog.packages = vec![old.clone()];
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &registry).is_err());
    let mut bound = registry.clone();
    bound
        .contexts
        .get_mut(&q.binding.context_id)
        .ok_or("context")?
        .declaration_keys
        .insert(old.id.clone(), q.key.clone());
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &bound).is_ok());
    let mut legacy = receipt.clone();
    legacy.package = old;
    legacy.query = None;
    receipts.installations.insert("old".into(), legacy);
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &bound).is_err());
    for bad in [0, 2] {
        let mut x = registry.clone();
        x.schema_version = bad;
        assert!(validate_registry(&x).is_err());
    }
    let mut overlap = registry.clone();
    let mut second = record;
    second.binding.prefix = "/other/prefix".into();
    second.binding.context_id = context_id(&second.binding.prefix)?;
    overlap
        .contexts
        .insert(second.binding.context_id.clone(), second);
    assert!(validate_registry(&overlap).is_err());
    let mut wrong = registry;
    let current = wrong
        .contexts
        .remove(&q.binding.context_id)
        .ok_or("remove")?;
    wrong.contexts.insert("wrong".into(), current);
    assert!(validate_registry(&wrong).is_err());
    Ok(())
}

#[test]
fn public_projection_preserves_other_channels_and_hides_private_brew_contracts() -> TestResult {
    let p = plan()?;
    let mut journal = serde_json::to_value(crate::test_support::journal()?)?;
    journal["plan"] = serde_json::to_value(&p)?;
    journal["plan"]["package"]["provider"]["query"]["binding"]["private-extra"] =
        json!("SYNTHETIC_ENV_SECRET");
    let raw = json!([journal,{"untouched":{"number":1,"null":null,"array":[true,"text"]}}]);
    let public = public_value(&raw);
    assert_eq!(public[1], raw[1]);
    assert_eq!(
        public[0]["plan"]["package"]["provider"]["query"]["full_name"],
        "foo"
    );
    let text = serde_json::to_string(&public)?;
    for hidden in [
        "SYNTHETIC_ENV_SECRET",
        "binding_fingerprint",
        "config_files",
        "program_stamp",
        "env_capture_id",
        "plan_sha256",
        "recipe_sha256",
        "query_id",
    ] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    assert_eq!(public[0]["plan"]["before"]["status"], "brew-installed");
    assert!(public[0]["plan"]["before"].get("evidence").is_none());
    let q = query()?;
    let missing = observation(&q, stamp(&q)?, report(&q, &[]))?;
    assert!(
        public_value(&serde_json::to_value(missing)?)
            .get("evidence")
            .is_none()
    );
    Ok(())
}

#[test]
fn binding_and_evidence_stamp_refuse_each_independent_scope_change() -> TestResult {
    let q = query()?;
    validate_query(&q)?;
    for key in [
        "invocation",
        "canonical-program",
        "home",
        "selector",
        "context",
        "same-cellar",
        "program-hash",
        "program-kind",
        "program-exec",
        "source",
        "acceptance",
        "ruby",
        "configs",
        "schema",
        "prefix",
        "full-name",
        "capture",
        "script-path",
        "script-canonical",
        "script-size",
        "script-hash",
        "build",
    ] {
        let mut bad = q.clone();
        match key {
            "invocation" => bad.binding.invocation = "relative".into(),
            "canonical-program" => bad.binding.canonical_program = "relative".into(),
            "home" => bad.binding.metadata_scope.home = "relative".into(),
            "selector" => bad.binding.metadata_scope.xdg_config_home = Some("relative".into()),
            "context" => bad.binding.context_id = "other".into(),
            "same-cellar" => bad.binding.cellar = bad.binding.prefix.clone(),
            "program-hash" => bad.binding.program_stamp.sha256.clear(),
            "program-kind" => bad.binding.program_stamp.node.mode = 0o040_700,
            "program-exec" => bad.binding.program_stamp.node.mode = 0o100_400,
            "source" | "acceptance" | "ruby" => {
                let BrewCompatibility::Homebrew7_0_15 {
                    source_commit,
                    acceptance_sha256,
                    ruby_version,
                } = &mut bad.binding.compatibility;
                match key {
                    "source" => source_commit.clear(),
                    "acceptance" => acceptance_sha256.clear(),
                    _ => *ruby_version = "3.4.0".into(),
                }
            }
            "configs" => bad.binding.metadata_scope.config_files.clear(),
            "schema" => bad.schema_version = 2,
            "prefix" => bad.key.canonical_prefix = "/other".into(),
            "full-name" => bad.key.full_name = "other".into(),
            "capture" => bad.env_capture_id.clear(),
            "script-path" => bad.script.absolute_path = "relative".into(),
            "script-canonical" => bad.script.canonical_path = "/other/query.rb".into(),
            "script-size" => bad.script.size += 1,
            "script-hash" => bad.script.sha256.clear(),
            "build" => bad.script.product_build_id.clear(),
            _ => return Err("case".into()),
        }
        assert!(validate_query(&bad).is_err(), "{key}");
    }
    let mut selected = q.clone();
    selected.binding.metadata_scope.xdg_config_home = Some("/fixture/xdg".into());
    selected.binding.metadata_scope.homebrew_xdg_config_home = Some("/fixture/other-xdg".into());
    validate_query(&selected)?;
    for key in ["context", "epoch", "binding", "env", "query"] {
        let mut bad = stamp(&q)?;
        match key {
            "context" => bad.context_id.clear(),
            "epoch" => bad.context_epoch += 1,
            "binding" => bad.binding_fingerprint.clear(),
            "env" => bad.env_capture_id.clear(),
            _ => bad.query_id.clear(),
        }
        assert!(validate_stamp(&q, &bad).is_err(), "{key}");
    }
    assert!(context_id(Path::new("relative")).is_err());
    Ok(())
}

fn goal_plan(action: Action) -> std::result::Result<Plan, Box<dyn std::error::Error>> {
    let mut p = plan()?;
    let data = p.brew.as_mut().ok_or("brew")?;
    data.confirmation.action = action;
    data.confirmation.consent = BrewConsent::Yes;
    data.goal = match action {
        Action::Adopt => BrewGoal::AdoptBaseline,
        Action::Install => BrewGoal::InstallAvailable {
            pkg_version: "2.0.0".into(),
        },
        Action::Uninstall => BrewGoal::UninstallAllInstalledVersions,
        _ => return Err("goal".into()),
    };
    p.action = action;
    if action == Action::Adopt {
        data.execution = BrewExecution::BaselineOnly;
        p.command = None;
        p.target = None;
    } else if action == Action::Uninstall {
        p.target = None;
        p.command.as_mut().ok_or("command")?.args = vec![
            "uninstall".into(),
            "--formula".into(),
            "--force".into(),
            "homebrew/core/foo".into(),
        ];
    } else {
        p.command.as_mut().ok_or("command")?.args[0] = "install".into();
    }
    Ok(p)
}
#[test]
fn all_goals_preserve_scoped_force_baseline_and_current_missing_noop() -> TestResult {
    let p = goal_plan(Action::Uninstall)?;
    let view = checked_plan(&p)?.ok_or("uninstall")?;
    assert_eq!(
        command(&view)?.ok_or("command")?.args,
        ["uninstall", "--formula", "--force", "homebrew/core/foo"]
    );
    let missing = observation(view.query, stamp(view.query)?, report(view.query, &[]))?;
    assert!(expected(&view, &missing));
    assert!(!expected(&view, &p.before));
    let mut noop = p.clone();
    noop.before = missing;
    noop.brew.as_mut().ok_or("brew")?.execution = BrewExecution::VerifiedNoop;
    noop.command = None;
    let view = checked_plan(&noop)?.ok_or("noop")?;
    assert!(command(&view)?.is_none());
    assert!(invocation_projection(&view).is_err());
    let p = goal_plan(Action::Adopt)?;
    let view = checked_plan(&p)?.ok_or("adopt")?;
    assert!(expected(&view, &p.before));
    let mut fresh = report(view.query, &[("homebrew/core", "stable", "1.0.0")]);
    fresh.query_id = "QUERY-2".into();
    let mut s = stamp(view.query)?;
    s.query_id = fresh.query_id.clone();
    let observed = observation(view.query, s, fresh)?;
    assert!(expected(&view, &observed));
    let p = goal_plan(Action::Install)?;
    checked_plan(&p)?.ok_or("install")?;
    let mut missing_recipe = p.clone();
    if let Provider::BrewFormula { recipe_ref, .. } = &mut missing_recipe.package.provider {
        *recipe_ref = None;
    }
    assert!(checked_plan(&missing_recipe).is_err());
    Ok(())
}

#[test]
fn bound_catalog_validation_and_foreign_owner_collisions_are_independent_gates() -> TestResult {
    let q = query()?;
    let p = package(&q)?;
    let mut catalog = Catalog {
        schema_version: 1,
        packages: vec![p.clone()],
    };
    catalog.validate()?;
    catalog.packages[0].id = "wrong".into();
    assert!(catalog.validate().is_err());
    let registry = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::new(),
    };
    let mut foreign = crate::test_support::package();
    foreign.id = p.id.clone();
    catalog.packages = vec![foreign.clone()];
    assert!(owner_snapshot(&q.key, &catalog, &Receipts::default(), &registry).is_err());
    catalog.packages.clear();
    let receipt = Receipt {
        scope: None,
        query: None,
        package: foreign,
        observation: Observation::Missing { evidence: None },
        operation_id: "OP-FOREIGN".into(),
        updated_at: 1,
    };
    let receipts = Receipts {
        schema_version: 1,
        installations: BTreeMap::from([(p.id, receipt)]),
    };
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &registry).is_err());
    Ok(())
}

fn non_utf8_path() -> std::path::PathBuf {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(b"/fixture/non-utf8-\xff".to_vec()).into()
}
#[test]
fn unsupported_path_encoding_and_partial_projection_fail_before_authority() -> TestResult {
    let q = query()?;
    let mut binding = q.binding.clone();
    binding.prefix = non_utf8_path();
    assert!(validate_binding(&binding).is_err());
    let mut key = q.key.clone();
    key.canonical_prefix = non_utf8_path();
    assert!(logical_id(&key).is_err());
    let mut invalid = q.clone();
    invalid.selection.tap.clear();
    assert!(validate_query(&invalid).is_err());
    invalid = q.clone();
    invalid.script.canonical_path = "relative".into();
    assert!(validate_query(&invalid).is_err());
    invalid = q.clone();
    invalid.schema_version = 2;
    let mut p = package(&q)?;
    if let Provider::BrewFormula { query, .. } = &mut p.provider {
        **query = invalid.clone();
    }
    assert!(validate_package(&p).is_err());
    assert!(observation(&invalid, stamp(&q)?, report(&q, &[])).is_err());
    invalid = q.clone();
    if let BrewConfigStamp::Absent { path, .. } =
        &mut invalid.binding.metadata_scope.config_files[0]
    {
        *path = non_utf8_path();
    }
    assert!(validate_stamp(&invalid, &stamp(&q)?).is_err());
    let mut bad = plan()?;
    if let Provider::BrewFormula { query, .. } = &mut bad.package.provider {
        query.key.full_name.clear();
    }
    assert!(checked_plan(&bad).is_err());
    let mut bad = plan()?;
    if let Observation::BrewInstalled { version, .. } = &mut bad.before {
        *version = "different-from-evidence".into();
    }
    assert!(checked_plan(&bad).is_err());
    let mut bad = plan()?;
    bad.brew
        .as_mut()
        .ok_or("brew")?
        .owner
        .entries
        .insert("another-owner".into(), BTreeSet::new());
    assert!(checked_plan(&bad).is_err());
    Ok(())
}
#[test]
fn directory_modes_and_relative_kegs_cannot_form_complete_native_proof() -> TestResult {
    let q = query()?;
    let base = report(&q, &[("homebrew/core", "stable", "1.0.0")]);
    for kind in [
        "cellar-file",
        "rack-file",
        "entry-kind",
        "receipt-kind",
        "relative-keg",
        "too-many-entries",
        "too-many-kegs",
    ] {
        let mut r = base.clone();
        match kind {
            "cellar-file" => {
                r.rack.cellar_before.mode = 0o100_600;
                r.rack.cellar_after = r.rack.cellar_before.clone();
            }
            "rack-file" => {
                r.rack.rack_before.as_mut().ok_or("rack")?.mode = 0o100_600;
                r.rack.rack_after = r.rack.rack_before.clone();
            }
            "entry-kind" => r.rack.entries[0].stamp.mode = 0o100_600,
            "receipt-kind" => r.rack.kegs[0].receipt.node.mode = 0o040_700,
            "relative-keg" => r.rack.kegs[0].path = "relative".into(),
            "too-many-entries" => r.rack.entries = vec![r.rack.entries[0].clone(); 4097],
            _ => r.rack.kegs = vec![r.rack.kegs[0].clone(); 4097],
        }
        assert!(observation(&q, stamp(&q)?, r).is_err(), "{kind}");
    }
    Ok(())
}
#[test]
fn execution_validation_refuses_adoption_mutation_and_missing_uninstall() -> TestResult {
    let mut p = goal_plan(Action::Adopt)?;
    p.brew.as_mut().ok_or("brew")?.execution = BrewExecution::NativeMutator;
    assert!(checked_plan(&p).is_err());
    let valid = goal_plan(Action::Adopt)?;
    let base = checked_plan(&valid)?.ok_or("view")?;
    let data = p.brew.as_ref().ok_or("brew")?;
    let view = BrewPlan {
        plan: &p,
        data,
        query: base.query,
        before_report: base.before_report,
    };
    assert!(validate_execution(&view).is_err());
    assert!(command(&view).is_err());
    let mut p = goal_plan(Action::Uninstall)?;
    let q = base.query;
    p.before = observation(q, stamp(q)?, report(q, &[]))?;
    assert!(checked_plan(&p).is_err());
    let mut p = goal_plan(Action::Adopt)?;
    p.before = observation(q, stamp(q)?, report(q, &[]))?;
    assert!(checked_plan(&p).is_err());
    let mut p = goal_plan(Action::Uninstall)?;
    p.brew.as_mut().ok_or("brew")?.execution = BrewExecution::VerifiedNoop;
    p.command = None;
    assert!(checked_plan(&p).is_err());
    Ok(())
}

#[test]
fn ownership_ignores_other_scopes_but_rejects_ambiguous_or_unserializable_owners() -> TestResult {
    let q = query()?;
    let p = package(&q)?;
    let empty = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::new(),
    };
    let no_receipts = Receipts::default();
    let mut other = q.clone();
    other.key.canonical_prefix = "/other/prefix".into();
    other.binding.prefix = other.key.canonical_prefix.clone();
    other.binding.cellar = "/other/cellar".into();
    other.binding.context_id = context_id(&other.binding.prefix)?;
    let record = BrewContextRecord {
        epoch: 0,
        binding: other.binding.clone(),
        observation_valid: false,
        writer_scope_confirmed: false,
        declaration_keys: BTreeMap::new(),
    };
    let mut registry = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::from([(other.binding.context_id.clone(), record)]),
    };
    let mut catalog = Catalog {
        schema_version: 1,
        packages: vec![package(&other)?, crate::test_support::package()],
    };
    assert!(
        owner_snapshot(&q.key, &catalog, &no_receipts, &registry)?
            .entries
            .is_empty()
    );
    registry.schema_version = 2;
    assert!(owner_snapshot(&q.key, &catalog, &no_receipts, &registry).is_err());
    let mut bad = q.key.clone();
    bad.full_name.clear();
    assert!(owner_snapshot(&bad, &catalog, &no_receipts, &empty).is_err());
    catalog.packages = vec![p.clone()];
    catalog.packages[0].paths.push(OwnedPath {
        path: non_utf8_path(),
        kind: PathKind::Cache,
    });
    assert!(owner_snapshot(&q.key, &catalog, &no_receipts, &empty).is_err());
    let mut legacy = crate::test_support::package();
    legacy.id = "declared:one".into();
    legacy.provider = Provider::Brew {
        package: "foo".into(),
        cask: false,
    };
    let mut second = legacy.clone();
    second.id = "declared:two".into();
    let record = BrewContextRecord {
        epoch: 0,
        binding: q.binding.clone(),
        observation_valid: true,
        writer_scope_confirmed: false,
        declaration_keys: BTreeMap::from([
            (legacy.id.clone(), q.key.clone()),
            (second.id.clone(), q.key.clone()),
        ]),
    };
    let registry = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::from([(q.binding.context_id.clone(), record)]),
    };
    catalog.packages = vec![legacy, second];
    assert!(owner_snapshot(&q.key, &catalog, &no_receipts, &registry).is_err());
    let mut bad = registry.clone();
    bad.contexts
        .get_mut(&q.binding.context_id)
        .ok_or("context")?
        .binding
        .invocation = "relative".into();
    assert!(validate_registry(&bad).is_err());
    let record = registry
        .contexts
        .get(&q.binding.context_id)
        .ok_or("record")?;
    let mut bad = q.binding.clone();
    bad.invocation = "relative".into();
    assert!(rebind(record, bad, true, &BTreeSet::new()).is_err());
    Ok(())
}
#[test]
fn third_party_short_recipe_names_and_receipt_scope_are_explicit() -> TestResult {
    let mut q = query()?;
    q.selection.tap = "vendor/tap".into();
    q.key.full_name = "vendor/tap/foo".into();
    let p = package(&q)?;
    let mut legacy = crate::test_support::package();
    legacy.id = "declared:short".into();
    legacy.provider = Provider::Brew {
        package: "foo".into(),
        cask: false,
    };
    let record = BrewContextRecord {
        epoch: 0,
        binding: q.binding.clone(),
        observation_valid: true,
        writer_scope_confirmed: false,
        declaration_keys: BTreeMap::from([(legacy.id.clone(), q.key.clone())]),
    };
    let registry = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::from([(q.binding.context_id.clone(), record)]),
    };
    let catalog = Catalog {
        schema_version: 1,
        packages: vec![legacy],
    };
    assert!(owner_snapshot(&q.key, &catalog, &Receipts::default(), &registry).is_ok());
    let observed = observation(&q, stamp(&q)?, report(&q, &[]))?;
    let mut receipt = Receipt {
        scope: None,
        query: Some(QuerySpec::BrewFormula(Box::new(q.clone()))),
        package: p.clone(),
        observation: observed,
        operation_id: "OP-1".into(),
        updated_at: 1,
    };
    let mut receipts = Receipts {
        schema_version: 1,
        installations: BTreeMap::from([(p.id.clone(), receipt.clone())]),
    };
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &registry).is_err());
    receipt.scope = Some(Scope {
        home: q.binding.metadata_scope.home.clone(),
        paths: BTreeMap::from([("UNRELATED".into(), Some(non_utf8_path()))]),
        service_identity: None,
    });
    receipts.installations.insert(p.id, receipt);
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &registry).is_err());
    Ok(())
}

#[test]
fn malformed_bound_packages_and_receipt_index_aliases_fail_closed() -> TestResult {
    let q = query()?;
    let mut p = package(&q)?;
    if let Provider::BrewFormula { query, .. } = &mut p.provider {
        query.key.full_name.clear();
    }
    assert!(validate_package(&p).is_err());
    let mut p = plan()?;
    p.package.id = "wrong".into();
    assert!(checked_plan(&p).is_err());
    let mut p = plan()?;
    if let Observation::BrewInstalled { evidence, .. } = &mut p.before {
        evidence.report.schema_version = 2;
    }
    assert!(checked_plan(&p).is_err());
    let mut p = goal_plan(Action::Adopt)?;
    p.brew.as_mut().ok_or("brew")?.execution = BrewExecution::NativeMutator;
    let before = goal_plan(Action::Adopt)?;
    let validated = checked_plan(&before)?.ok_or("view")?;
    let view = BrewPlan {
        plan: &p,
        data: p.brew.as_ref().ok_or("brew")?,
        query: validated.query,
        before_report: validated.before_report,
    };
    assert!(invocation_projection(&view).is_err());
    let catalog = Catalog {
        schema_version: 1,
        packages: vec![],
    };
    let registry = BrewContextRegistry {
        schema_version: 1,
        contexts: BTreeMap::new(),
    };
    let foreign = crate::test_support::package();
    let mut receipt = Receipt {
        scope: None,
        query: None,
        package: foreign,
        observation: Observation::Missing { evidence: None },
        operation_id: "OP-OTHER".into(),
        updated_at: 1,
    };
    let mut receipts = Receipts {
        schema_version: 1,
        installations: BTreeMap::from([("unrelated-index".into(), receipt.clone())]),
    };
    assert!(
        owner_snapshot(&q.key, &catalog, &receipts, &registry)?
            .entries
            .is_empty()
    );
    receipt.package.id = logical_id(&q.key)?;
    receipts
        .installations
        .insert("unrelated-index".into(), receipt.clone());
    assert!(owner_snapshot(&q.key, &catalog, &receipts, &registry).is_err());
    let mut third = q;
    third.selection.tap = "vendor/tap".into();
    third.key.full_name = "vendor/tap/foo".into();
    receipt.package.id = "old-short-owner".into();
    receipt.package.provider = Provider::Brew {
        package: "foo".into(),
        cask: false,
    };
    receipts
        .installations
        .insert("unrelated-index".into(), receipt);
    assert!(owner_snapshot(&third.key, &catalog, &receipts, &registry).is_err());
    Ok(())
}

#[test]
fn prepared_goal_and_resolved_package_use_native_identity_without_changing_owner() -> TestResult {
    let q = query()?;
    let p = package(&q)?;
    let observed = observation(
        &q,
        stamp(&q)?,
        report(&q, &[("homebrew/core", "stable", "1.0.0")]),
    )?;
    for action in [
        Action::Adopt,
        Action::Uninstall,
        Action::Install,
        Action::Upgrade,
    ] {
        let (_, version) = resolve_goal(&q, action, &observed)?;
        assert_eq!(
            version.is_some(),
            matches!(action, Action::Install | Action::Upgrade)
        );
    }
    assert!(resolve_goal(&q, Action::Cleanup, &observed).is_err());
    assert!(
        resolve_goal(
            &q,
            Action::Adopt,
            &Observation::Unknown {
                reason: "synthetic".into()
            }
        )
        .is_err()
    );
    let mut absent = report(&q, &[]);
    absent.formula = None;
    let missing = observation(&q, stamp(&q)?, absent)?;
    assert!(resolve_goal(&q, Action::Upgrade, &missing).is_err());
    assert_eq!(resolved_package(&p, &q)?.id, p.id);
    let mut legacy = crate::test_support::package();
    legacy.provider = Provider::Brew {
        package: "foo".into(),
        cask: false,
    };
    let resolved = resolved_package(&legacy, &q)?;
    assert_eq!(resolved.id, p.id);
    assert!(matches!(
        resolved.provider,
        Provider::BrewFormula {
            recipe_ref: Some(_),
            ..
        }
    ));
    assert!(resolved_package(&crate::test_support::package(), &q).is_err());
    let mut invalid = q;
    invalid.schema_version = 2;
    assert!(resolved_package(&p, &invalid).is_err());
    Ok(())
}

#[test]
fn journal_and_existing_owner_preserve_logical_brew_identity_across_capture_refresh() -> TestResult
{
    let p = plan()?;
    let q = match &p.query {
        Some(QuerySpec::BrewFormula(q)) => q.as_ref(),
        _ => return Err("query".into()),
    };
    let scope = Scope {
        home: q.binding.metadata_scope.home.clone(),
        paths: BTreeMap::new(),
        service_identity: None,
    };
    let old = Receipt {
        scope: Some(scope.clone()),
        query: p.query.clone(),
        package: p.package.clone(),
        observation: p.before.clone(),
        operation_id: "OP-OLD".into(),
        updated_at: 1,
    };
    let mut changed = p.package.clone();
    if let Provider::BrewFormula { query, .. } = &mut changed.provider {
        query.env_capture_id = "ENV-REFRESH".into();
    }
    crate::policy::operation::state::existing_owner(&scope, &changed, Some(&old))?;
    let mut selected = scope.clone();
    selected
        .paths
        .insert("XDG_CONFIG_HOME".into(), Some("/new/xdg".into()));
    crate::policy::operation::state::existing_owner(&selected, &changed, Some(&old))?;
    let mut other = scope.clone();
    other.home = "/different/home".into();
    assert!(crate::policy::operation::state::existing_owner(&other, &changed, Some(&old)).is_err());
    if let Provider::BrewFormula { query, .. } = &mut changed.provider {
        query.key.full_name = "other".into();
    }
    assert!(crate::policy::operation::state::existing_owner(&scope, &changed, Some(&old)).is_err());
    let mut j = crate::test_support::journal()?;
    j.plan = p.clone();
    j.plan_sha256 = fingerprint(&j.plan)?;
    j.scope = scope.clone();
    j.home = scope.home;
    j.scope_sha256 = fingerprint(&j.scope)?;
    assert!(!crate::policy::operation::state::expected(
        &j,
        &Observation::Present {
            version: "2.0.0".into(),
            evidence: None
        }
    ));
    let mut current = report(q, &[("homebrew/core", "stable", "2.0.0")]);
    current.formula.as_mut().ok_or("formula")?.outdated = false;
    let current = observation(q, stamp(q)?, current)?;
    assert!(crate::policy::operation::state::expected(&j, &current));
    j.plan.brew.as_mut().ok_or("brew")?.context.epoch += 1;
    assert!(!crate::policy::operation::state::expected(&j, &current));
    j.plan_sha256 = fingerprint(&j.plan)?;
    assert!(crate::policy::operation::state::validate_journal(&j).is_err());
    let mut invalid = q.clone();
    invalid.key.canonical_prefix = non_utf8_path();
    assert!(resolved_package(&p.package, &invalid).is_err());
    Ok(())
}

#[test]
fn target_version_does_not_authorize_a_different_native_formula() -> TestResult {
    let plan = plan()?;
    let view = checked_plan(&plan)?.ok_or("plan")?;
    let mut after = report(view.query, &[("homebrew/core", "stable", "2.0.0")]);
    let info = after.formula.as_mut().ok_or("formula")?;
    info.outdated = false;
    info.name = "other".into();
    info.tap = "vendor/tap".into();
    info.full_name = "vendor/tap/other".into();
    info.latest_full_name = info.full_name.clone();
    let observed = observation(view.query, stamp(view.query)?, after)?;
    assert!(!expected(&view, &observed));
    Ok(())
}

#[test]
fn exact_formula_identity_and_current_upgrade_keep_all_scope_checks() -> TestResult {
    validate_package(&crate::test_support::package())?;
    let q = query()?;
    let good = report(&q, &[("homebrew/core", "stable", "2.0.0")]);
    formula_identity(&q, &good)?;
    for variant in 0..8 {
        let mut changed = good.clone();
        let info = changed.formula.as_mut().ok_or("formula")?;
        match variant {
            0 => info.name = "other".into(),
            1 => info.tap = "vendor/tap".into(),
            2 => info.full_name = "homebrew/core/foo".into(),
            3 => info.active_spec = "head".into(),
            4 => info.resolution.alias_changed = true,
            5 => info.resolution.migration_needed = true,
            6 => info.latest_full_name = "other".into(),
            _ => changed.formula = None,
        }
        assert!(formula_identity(&q, &changed).is_err());
    }
    let mut wrong = q.clone();
    wrong.selection.tap = "invalid".into();
    assert!(formula_identity(&wrong, &good).is_err());
    wrong = q.clone();
    wrong.key.full_name = "other".into();
    assert!(formula_identity(&wrong, &good).is_err());
    let mut third = q.clone();
    third.selection.tap = "vendor/tap".into();
    third.key.full_name = "vendor/tap/foo".into();
    formula_identity(
        &third,
        &report(&third, &[("vendor/tap", "stable", "2.0.0")]),
    )?;
    let mut current = good;
    current.formula.as_mut().ok_or("formula")?.outdated = false;
    let observed = observation(&q, stamp(&q)?, current.clone())?;
    assert!(upgrade_is_current(&q, &observed)?);
    for variant in 0..3 {
        let mut changed = current.clone();
        match variant {
            0 => changed.formula.as_mut().ok_or("formula")?.pinned = true,
            1 => changed
                .formula
                .as_mut()
                .ok_or("formula")?
                .installed_paths
                .clear(),
            _ => changed.rack.kegs[0].spec = "head".into(),
        }
        let observed = observation(&q, stamp(&q)?, changed)?;
        assert!(upgrade_is_current(&q, &observed).is_err());
    }
    assert!(
        upgrade_is_current(
            &q,
            &Observation::Unknown {
                reason: "not observed".into()
            }
        )
        .is_err()
    );
    let mut head = plan()?;
    head.before = observation(
        &q,
        stamp(&q)?,
        report(&q, &[("homebrew/core", "head", "1.0.0")]),
    )?;
    head.action = Action::Uninstall;
    head.target = None;
    head.command = Some(native_command(&q, Action::Uninstall)?);
    let data = head.brew.as_mut().ok_or("brew")?;
    data.goal = BrewGoal::UninstallAllInstalledVersions;
    data.confirmation.action = Action::Uninstall;
    data.confirmation.consent = BrewConsent::Yes;
    assert!(checked_plan(&head).is_err());
    let mut ready = plan()?;
    ready.before = observed;
    assert!(checked_plan(&ready).is_err());
    ready.brew.as_mut().ok_or("brew")?.execution = BrewExecution::VerifiedNoop;
    ready.command = None;
    assert!(checked_plan(&ready)?.is_some());
    ready.brew.as_mut().ok_or("brew")?.execution = BrewExecution::BaselineOnly;
    assert!(checked_plan(&ready).is_err());
    Ok(())
}

#[test]
fn writer_scope_confirmation_is_independent_and_rebind_clears_it() -> TestResult {
    let q = query()?;
    let record = BrewContextRecord {
        epoch: 0,
        binding: q.binding.clone(),
        observation_valid: false,
        writer_scope_confirmed: false,
        declaration_keys: BTreeMap::new(),
    };
    let mut legacy = serde_json::to_value(&record)?;
    legacy
        .as_object_mut()
        .ok_or("record")?
        .remove("writer_scope_confirmed");
    let old: BrewContextRecord = serde_json::from_value(legacy)?;
    assert!(!old.writer_scope_confirmed);
    let observed = observation(&q, stamp(&q)?, report(&q, &[]))?;
    let mut accepted = accept_observation(&old, &q, &observed)?;
    assert!(accepted.observation_valid);
    assert!(!accepted.writer_scope_confirmed);
    accepted.writer_scope_confirmed = true;
    assert!(accept_observation(&accepted, &q, &observed)?.writer_scope_confirmed);
    let mut changed = q.binding.clone();
    changed.invocation = "/fixture/other-brew".into();
    let next = rebind(&accepted, changed, false, &BTreeSet::new())?;
    assert!(!next.observation_valid);
    assert!(!next.writer_scope_confirmed);
    Ok(())
}
