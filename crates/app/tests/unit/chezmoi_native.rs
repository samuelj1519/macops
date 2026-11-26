#![cfg(test)]
use super::*;
use crate::{
    cli::Cli,
    config::rules::chezmoi_native::Stamp,
    domain::{FileId, Scope},
    test_support::TestResult,
};
use clap::Parser;
use std::{fs, os::unix::fs::PermissionsExt};

const INERT_PROGRAM: &[u8] = b"inert native tool fixture";

struct Fixture {
    _temp: tempfile::TempDir,
    ctx: Context,
    binding: Binding,
}
impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir_in("/private/tmp")?;
        let home = temp.path().join("home");
        for path in [
            home.clone(),
            home.join("source/home"),
            home.join("destination"),
            home.join("bin"),
        ] {
            std::fs::create_dir_all(path)?;
        }
        let cli = Cli::try_parse_from([
            "macops",
            "--home",
            home.to_str().ok_or("home")?,
            "config",
            "list",
        ])?;
        let ctx = Context::from_cli(&cli, &mut vec![])?;
        let mut binding = Binding {
            schema: "chezmoi-owner-v1".into(),
            scope: ctx.scope.clone(),
            program: home.join("bin/chezmoi"),
            lookup: home.join("bin/chezmoi"),
            version: "chezmoi version v2.74.1".into(),
            configured_source: home.join("source"),
            effective_source: home.join("source/home"),
            destination: home.join("destination"),
            config: home.join("chezmoi.toml"),
            cache: ctx.cache.join("chezmoi-native"),
            persistent_state: ctx.home.join("chezmoistate.boltdb"),
            record_state: ctx.state.clone(),
            anchors: vec![],
            inputs: vec![],
            effects: "configured-native-v1".into(),
            filtered: false,
        };
        let stamp = |path| Stamp {
            path,
            identity: Some(FileId {
                device: 1,
                inode: 2,
            }),
            mode: 0o700,
            digest: "a".repeat(64),
        };
        binding.anchors = [
            binding.program.clone(),
            binding.configured_source.clone(),
            binding.effective_source.clone(),
            binding.destination.clone(),
        ]
        .into_iter()
        .map(stamp)
        .collect();
        binding.inputs = [
            binding.config.clone(),
            binding.configured_source.join(".chezmoiroot"),
            binding.effective_source.join(".chezmoiignore"),
            binding.effective_source.join("dot_demo.tmpl"),
        ]
        .into_iter()
        .map(stamp)
        .collect();
        Ok(Self {
            _temp: temp,
            ctx,
            binding,
        })
    }
    fn journal(&self) -> Result<Journal> {
        let target = self.binding.destination.join(".demo");
        let package = package(
            &target,
            &self.binding.effective_source.join("dot_demo.tmpl"),
        )?;
        let query = Query {
            kind: QueryKind::NativeV1,
            source_adopt: None,
            source_forget: None,
            binding_sha256: fingerprint(&self.binding)?,
            targets: vec![target],
        };
        let plan = Plan {
            brew: None,
            npm_binding: None,
            uv_owner: None,
            timeout_secs: 1,
            recipe_sha256: fingerprint(&package)?,
            package,
            action: Action::Apply,
            before: Observation::Unknown {
                reason: "fixture-prepared".into(),
            },
            target: None,
            command: Some(policy::command(
                &self.binding,
                NativeCommand::Apply,
                &query.targets,
            )?),
            query: Some(QuerySpec::NativeConfig(query)),
            warnings: vec![],
            synchronous: true,
            delegated: false,
        };
        operations::new_journal(&self.ctx, plan)
    }
    fn operation(&self, journal: &Journal) -> Result<(Operation, FrozenInvocation)> {
        let invocation = FrozenInvocation {
            command: journal.plan.command.clone().ok_or_else(io::changed)?,
            env: io::environment(&self.ctx),
            cwd: self.ctx.home.clone(),
            timeout_secs: journal.plan.timeout_secs,
        };
        Ok((
            Operation {
                schema: "chezmoi-native-operation-v1".into(),
                plan_sha256: journal.plan_sha256.clone(),
                binding: self.binding.clone(),
                invocation_sha256: fingerprint(&invocation)?,
            },
            invocation,
        ))
    }

    fn bind_files(&mut self, program: &[u8]) -> TestResult {
        use crate::platform::fs::Directory;

        fs::write(&self.binding.program, program)?;
        fs::set_permissions(&self.binding.program, fs::Permissions::from_mode(0o700))?;
        fs::write(&self.binding.config, b"# fixture native configuration\n")?;
        let source = self.binding.effective_source.join("dot_demo.tmpl");
        fs::write(&source, b"fixture source")?;
        let mut anchors = vec![io::stamp(&self.binding.program, true)?];
        for path in [
            &self.binding.configured_source,
            &self.binding.effective_source,
            &self.binding.destination,
        ] {
            let directory = Directory::open(path)?;
            anchors.push(Stamp {
                path: path.clone(),
                identity: Some(directory.identity().clone()),
                mode: fs::metadata(path)?.permissions().mode() & 0o7777,
                digest: fingerprint(&directory.lineage()?)?,
            });
        }
        self.binding.anchors = anchors;
        self.binding.inputs = io::inputs(&self.binding, &[source])?;
        io::verify(&self.binding)?;
        Ok(())
    }
}

#[test]
fn private_operation_rejects_cross_plan_scope_command_and_source_claims() -> TestResult {
    let fixture = Fixture::new()?;
    let journal = fixture.journal()?;
    let (operation, _) = fixture.operation(&journal)?;
    validate_operation(&journal, &operation)?;
    for index in 0..8 {
        let mut changed = operation.clone();
        let mut current = journal.clone();
        match index {
            0 => changed.schema = "other".into(),
            1 => changed.plan_sha256 = "other".into(),
            2 => {
                changed.binding.scope.home = "/other".into();
                if let Some(QuerySpec::NativeConfig(query)) = &mut current.plan.query {
                    query.binding_sha256 = fingerprint(&changed.binding)?;
                }
            }
            3 => current.plan.command = None,
            4 => current.plan.query = None,
            5 => {
                changed.binding.inputs.pop();
                let Some(QuerySpec::NativeConfig(query)) = &mut current.plan.query else {
                    return Err("query".into());
                };
                query.binding_sha256 = fingerprint(&changed.binding)?;
            }
            6 => current.plan.package.provider = Provider::Macops,
            _ => {
                changed.binding.record_state = "/other".into();
                if let Some(QuerySpec::NativeConfig(query)) = &mut current.plan.query {
                    query.binding_sha256 = fingerprint(&changed.binding)?;
                }
            }
        }
        assert!(validate_operation(&current, &changed).is_err());
    }
    Ok(())
}

#[test]
fn worker_guard_requires_same_invocation_home_temp_and_scope_values() -> TestResult {
    let f = Fixture::new()?;
    let journal = f.journal()?;
    let (operation, invocation) = f.operation(&journal)?;
    let store = Store::lock(&f.ctx)?;
    for index in 0..5 {
        let mut op = operation.clone();
        let mut inv = invocation.clone();
        match index {
            0 => op.invocation_sha256 = "other".into(),
            1 => inv.cwd = "/other".into(),
            2 => {
                inv.env.remove("HOME");
            }
            3 => {
                inv.env.remove("TMPDIR");
            }
            _ => {
                inv.env.insert("XDG_CONFIG_HOME".into(), "/other".into());
            }
        }
        if index != 0 {
            op.invocation_sha256 = fingerprint(&inv)?;
        }
        Store::atomic(&store.root, &operation_name(&journal.id), &op)?;
        assert!(worker_guard(&journal, &inv).is_err());
    }
    Store::atomic(&store.root, &operation_name(&journal.id), &operation)?;
    assert!(worker_guard(&journal, &invocation).is_err());
    let legacy = crate::test_support::journal()?;
    assert_eq!(
        worker_guard(&legacy, &invocation)
            .err()
            .ok_or("guard")?
            .code,
        "native_adapter_mismatch"
    );
    Ok(())
}

#[test]
fn prepared_and_terminal_recovery_does_not_start_a_native_process() -> TestResult {
    let f = Fixture::new()?;
    let mut journal = f.journal()?;
    let store = Store::lock(&f.ctx)?;
    store.save(&journal)?;
    let value = recover(&f.ctx, &store, &mut journal, false, true)?;
    assert_eq!(value["nativeResult"], "not-started");
    assert_eq!(journal.phase, Phase::Failed);
    recover(&f.ctx, &store, &mut journal, false, true)?;
    journal.phase = Phase::Succeeded;
    recover(&f.ctx, &store, &mut journal, false, true)?;
    journal.phase = Phase::Unknown;
    let mut other = f.ctx.clone();
    other.scope = Scope {
        home: "/other".into(),
        paths: BTreeMap::new(),
        service_identity: None,
    };
    assert!(recover(&other, &store, &mut journal, false, true).is_err());
    Ok(())
}

#[test]
fn running_native_is_unknown_without_verification_or_rollback() -> TestResult {
    let f = Fixture::new()?;
    let mut journal = f.journal()?;
    let (operation, invocation) = f.operation(&journal)?;
    let store = Store::lock(&f.ctx)?;
    let value = finish(
        &store,
        &mut journal,
        &operation,
        &invocation,
        &NativeOutcome::Running,
    )?;
    assert_eq!(value["phase"], "unknown");
    assert_eq!(value["filesMatch"], Value::Null);
    assert_eq!(value["rollback"], "not-provided");
    Ok(())
}

#[test]
fn unknown_native_cache_is_metadata_only_and_bad_shapes_fail_closed() -> TestResult {
    let f = Fixture::new()?;
    assert_eq!(cached_list(&f.ctx)?["coverage"], "unresolved");
    let store = Store::lock(&f.ctx)?;
    let cache = Cache {
        source_publication: None,
        binding: f.binding.clone(),
        entries: BTreeMap::new(),
        observed_at: 1,
        complete: false,
        status: vec![],
    };
    Store::atomic(&store.root, "chezmoi-metadata.json", &cache)?;
    assert_eq!(cached_list(&f.ctx)?["coverage"], "cached");
    let mut other = cache;
    other.binding.scope.home = "/other".into();
    Store::atomic(&store.root, "chezmoi-metadata.json", &other)?;
    assert!(cached_list(&f.ctx).is_err());
    assert_eq!(
        cached_snapshot(&f.ctx).coverage.state,
        crate::domain::resource::CoverageState::Invalid
    );
    Ok(())
}

#[test]
fn after_evidence_requires_original_plan_scope_frame_and_worker_identity() -> TestResult {
    let f = Fixture::new()?;
    let mut journal = f.journal()?;
    journal.process_group = Some(42);
    let (operation, invocation) = f.operation(&journal)?;
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err("query".into());
    };
    let record = Verification {
        schema: "chezmoi-native-verification-v1".into(),
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        scope_sha256: journal.scope_sha256.clone(),
        binding_sha256: query.binding_sha256.clone(),
        invocation_sha256: operation.invocation_sha256.clone(),
        worker_pid: 42,
        files_match: Some(true),
        reason: "matched".into(),
    };
    validate_verification(&journal, &operation, &invocation, &record)?;
    for index in 0..8 {
        let mut bad = record.clone();
        match index {
            0 => bad.schema = "other".into(),
            1 => bad.operation_id = "OP-other".into(),
            2 => bad.plan_sha256 = "other".into(),
            3 => bad.scope_sha256 = "other".into(),
            4 => bad.binding_sha256 = "other".into(),
            5 => bad.invocation_sha256 = "other".into(),
            6 => bad.worker_pid = 43,
            _ => {
                bad.invocation_sha256 = fingerprint(&FrozenInvocation {
                    cwd: "/other".into(),
                    ..invocation.clone()
                })?;
            }
        }
        assert!(validate_verification(&journal, &operation, &invocation, &bad).is_err());
    }
    Ok(())
}

#[test]
fn malformed_after_cannot_upgrade_filtered_or_unknown_file_evidence() -> TestResult {
    let f = Fixture::new()?;
    let mut journal = f.journal()?;
    journal.process_group = Some(42);
    let (mut operation, invocation) = f.operation(&journal)?;
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err("query".into());
    };
    let mut record = Verification {
        schema: "chezmoi-native-verification-v1".into(),
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        scope_sha256: journal.scope_sha256.clone(),
        binding_sha256: query.binding_sha256.clone(),
        invocation_sha256: operation.invocation_sha256.clone(),
        worker_pid: 42,
        files_match: None,
        reason: "unconfirmed".into(),
    };
    validate_verification(&journal, &operation, &invocation, &record)?;
    for reason in ["", "SYNTHETIC_SECRET", "with space"] {
        record.reason = reason.into();
        assert!(validate_verification(&journal, &operation, &invocation, &record).is_err());
    }
    record.reason = "matched".into();
    assert!(validate_verification(&journal, &operation, &invocation, &record).is_err());
    record.files_match = Some(false);
    assert!(validate_verification(&journal, &operation, &invocation, &record).is_err());
    record.reason = "mismatch".into();
    validate_verification(&journal, &operation, &invocation, &record)?;
    record.files_match = Some(true);
    record.reason = "matched".into();
    operation.binding.filtered = true;
    assert!(validate_verification(&journal, &operation, &invocation, &record).is_err());
    Ok(())
}

#[test]
fn registered_cache_keeps_lookup_bound_to_the_selected_scope() -> TestResult {
    let mut f = Fixture::new()?;
    f.bind_files(INERT_PROGRAM)?;
    let target = f.binding.destination.join(".demo");
    let virtual_target = f.binding.destination.join(".native virtual");
    let entry = |absolute, relative: &str| Managed {
        absolute,
        source_absolute: f.binding.effective_source.join(relative),
        source_relative: relative.into(),
    };
    let cache = Cache {
        source_publication: None,
        binding: f.binding.clone(),
        entries: BTreeMap::from([
            (".demo".into(), entry(target.clone(), "dot_demo.tmpl")),
            (
                ".native virtual".into(),
                entry(virtual_target.clone(), "dot_virtual"),
            ),
        ]),
        observed_at: 7,
        complete: true,
        status: vec![],
    };
    let store = Store::lock(&f.ctx)?;
    io::save_owner(&store, &f.binding)?;
    Store::atomic(&store.root, "chezmoi-metadata.json", &cache)?;
    drop(store);
    let cached_path = f.ctx.state.join("chezmoi-metadata.json");
    let before = fs::read(&cached_path)?;

    let result = run(
        &f.ctx,
        &Config::Where {
            query: "virtual".into(),
        },
        false,
        false,
    )?
    .ok_or("cached search output")?;
    assert_eq!(result["nativeInvocations"], 0);
    assert_eq!(result["matches"]["shown"], json!([virtual_target]));
    assert_eq!(result["observedAt"], 7);
    assert_eq!(result["tool"]["program"], json!(f.binding.program));

    let id = crate::policy::config_local::config_id(&target)?;
    let package = public_package(&f.ctx, &id)?.ok_or("cached package")?;
    assert_eq!(package.id, id);
    assert_eq!(
        public_package(&f.ctx, target.to_str().ok_or("absolute target")?)?
            .ok_or("cached package by path")?
            .id,
        id
    );
    assert!(public_package(&f.ctx, "another-name")?.is_none());
    let absent = public_package(&f.ctx, "config:absent")
        .err()
        .ok_or("unregistered configuration ID")?;
    assert_eq!(absent.code, "config_native_explicit_entry_required");

    let snapshot = cached_snapshot(&f.ctx);
    assert_eq!(snapshot.coverage.count, 2);
    assert_eq!(snapshot.native_targets, [target, virtual_target]);
    assert_eq!(snapshot.candidates.len(), 1);
    assert_eq!(io::resolve(&f.ctx)?.destination, f.binding.destination);

    let mut altered = f.ctx.clone();
    altered.chezmoi.source = Some(f.ctx.home.join("other-source"));
    assert_eq!(
        io::resolve(&altered).err().ok_or("source override")?.code,
        "chezmoi_binding_changed"
    );
    assert_eq!(fs::read(cached_path)?, before);
    assert!(!f.ctx.state.join("active.json").exists());
    Ok(())
}

#[test]
fn native_after_records_filtered_or_changed_inputs_without_granting_a_receipt() -> TestResult {
    for filtered in [false, true] {
        let mut f = Fixture::new()?;
        f.binding.filtered = filtered;
        f.bind_files(INERT_PROGRAM)?;
        let mut journal = f.journal()?;
        journal.phase = Phase::Running;
        journal.process_group = Some(std::process::id());
        let (operation, invocation) = f.operation(&journal)?;
        let store = Store::lock(&f.ctx)?;
        Store::atomic(&store.root, &operation_name(&journal.id), &operation)?;
        worker_guard(&journal, &invocation)?;
        let proof_name = verification_name(&journal.id);
        assert!(worker_after(&journal, &invocation, &NativeOutcome::Running).is_err());
        assert!(!f.ctx.state.join(&proof_name).exists());
        if !filtered {
            fs::write(
                f.binding.effective_source.join("dot_demo.tmpl"),
                b"updated source",
            )?;
        }
        let outcome = NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        };
        worker_after(&journal, &invocation, &outcome)?;
        let proof: Verification = private_read(&journal.state, &proof_name)?;
        assert_eq!(proof.files_match, None);
        assert_eq!(
            proof.reason,
            if filtered {
                "partial-filtered"
            } else {
                "plan_stale"
            }
        );
        let proof_path = f.ctx.state.join(proof_name);
        let saved = fs::read(&proof_path)?;
        assert!(worker_after(&journal, &invocation, &outcome).is_err());
        assert_eq!(fs::read(proof_path)?, saved);

        let value = finish(&store, &mut journal, &operation, &invocation, &outcome)?;
        assert_eq!(value["phase"], "unknown");
        assert!(value["filesMatch"].is_null());
        assert!(store.receipts()?.installations.is_empty());
    }
    Ok(())
}

#[test]
fn matching_file_evidence_requires_original_success_to_publish_the_receipt() -> TestResult {
    let mut f = Fixture::new()?;
    f.bind_files(INERT_PROGRAM)?;
    let mut journal = f.journal()?;
    journal.phase = Phase::Running;
    let worker_pid = std::process::id();
    journal.process_group = Some(worker_pid);
    let (operation, invocation) = f.operation(&journal)?;
    let Some(QuerySpec::NativeConfig(query)) = &journal.plan.query else {
        return Err("query".into());
    };
    let proof = Verification {
        schema: "chezmoi-native-verification-v1".into(),
        operation_id: journal.id.clone(),
        plan_sha256: journal.plan_sha256.clone(),
        scope_sha256: journal.scope_sha256.clone(),
        binding_sha256: query.binding_sha256.clone(),
        invocation_sha256: operation.invocation_sha256.clone(),
        worker_pid,
        files_match: Some(true),
        reason: "matched".into(),
    };
    let store = Store::lock(&f.ctx)?;
    Store::atomic(&store.root, &verification_name(&journal.id), &proof)?;
    let mut unconfirmed = journal.clone();
    let result = finish(
        &store,
        &mut unconfirmed,
        &operation,
        &invocation,
        &NativeOutcome::Unknown {
            reason: "completion_unavailable".into(),
        },
    )?;
    assert_eq!(result["filesMatch"], true);
    assert_eq!(result["nativeResult"], "unknown");
    assert_eq!(result["phase"], "unknown");
    assert!(store.receipts()?.installations.is_empty());

    let result = finish(
        &store,
        &mut journal,
        &operation,
        &invocation,
        &NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        },
    )?;
    assert_eq!(result["phase"], "succeeded");
    assert_eq!(result["verification"], "matched");
    let receipts = store.receipts()?;
    let id = &journal.plan.package.id;
    let receipt = receipts
        .installations
        .get(id)
        .ok_or("native apply receipt")?;
    assert_eq!(receipt.operation_id, journal.id);
    assert!(matches!(
        receipt.observation,
        Observation::ConfigNative { matched: true, .. }
    ));
    assert_eq!(
        public_package(&f.ctx, id)?
            .ok_or("package from receipt")?
            .id,
        *id
    );
    let stored_receipt = fs::read(f.ctx.state.join("receipts.json"))?;
    let (resources, coverage) = crate::config::resource_configs::collect(&f.ctx, &[], None)?;
    let selected = resources
        .iter()
        .find(|item| &item.id == id)
        .ok_or("receipt inventory entry")?;
    let crate::domain::resource::Data::Config {
        owner,
        baseline: Some(baseline),
        ..
    } = &selected.data
    else {
        return Err("expected native configuration receipt baseline".into());
    };
    assert_eq!(owner, "chezmoi-native-cached");
    assert_eq!(selected.status, crate::domain::resource::State::Unknown);
    assert_eq!(
        baseline.comparison,
        crate::domain::resource::Comparison::Unknown
    );
    assert!(coverage.iter().any(|report| {
        report
            .issues
            .iter()
            .any(|code| code == "adopted_config_unobserved")
    }));
    assert_eq!(fs::read(f.ctx.state.join("receipts.json"))?, stored_receipt);
    Ok(())
}

#[test]
fn recovery_without_a_saved_native_exit_retains_the_same_operation_until_manual_resolution()
-> TestResult {
    let mut f = Fixture::new()?;
    f.bind_files(INERT_PROGRAM)?;
    let mut journal = f.journal()?;
    let (operation, invocation) = f.operation(&journal)?;
    let store = Store::lock(&f.ctx)?;
    native_runner::prepare(&store, &journal, &invocation)?;
    journal.phase = Phase::Unknown;
    store.save(&journal)?;
    let operation_file = operation_name(&journal.id);
    let intent = f.ctx.state.join(&operation_file);
    let original = fs::read(f.ctx.state.join("active.json"))?;
    assert!(recover(&f.ctx, &store, &mut journal, false, true).is_err());
    assert_eq!(fs::read(f.ctx.state.join("active.json"))?, original);

    Store::atomic(&store.root, &operation_file, &operation)?;
    let intent_bytes = fs::read(&intent)?;
    let result = recover(&f.ctx, &store, &mut journal, false, true)?;
    assert_eq!(result["phase"], "unknown");
    assert_eq!(result["nativeResult"], "unknown");
    assert_eq!(result["operation_id"], journal.id);
    assert!(store.receipts()?.installations.is_empty());

    let result = recover(&f.ctx, &store, &mut journal, true, true)?;
    assert_eq!(result["phase"], "failed");
    assert_eq!(result["operation_id"], journal.id);
    assert_eq!(fs::read(intent)?, intent_bytes);
    assert!(!f.ctx.state.join(verification_name(&journal.id)).exists());
    assert!(store.receipts()?.installations.is_empty());
    Ok(())
}

#[test]
fn native_after_distinguishes_drift_from_an_inconclusive_verification() -> TestResult {
    let synthetic = br#"#!/bin/sh
for argument do
    case "$argument" in
        verify)
            printf 'verify\n' >> "$HOME/query-calls"
            if [ -f "$HOME/verify-fails" ]; then
                printf 'fixture diagnostics\n' >&2
                exit 17
            fi
            exit 0
            ;;
        status)
            printf 'status\n' >> "$HOME/query-calls"
            printf ' M %s\n' "$HOME/destination/.demo"
            exit 0
            ;;
    esac
done
exit 90
"#;
    for verify_fails in [false, true] {
        let mut f = Fixture::new()?;
        f.bind_files(synthetic)?;
        if verify_fails {
            fs::write(f.ctx.home.join("verify-fails"), b"fixture flag")?;
        }
        let mut journal = f.journal()?;
        journal.plan.timeout_secs = 5;
        journal.plan_sha256 = fingerprint(&journal.plan)?;
        journal.phase = Phase::Running;
        journal.process_group = Some(std::process::id());
        let (operation, invocation) = f.operation(&journal)?;
        let store = Store::lock(&f.ctx)?;
        Store::atomic(&store.root, &operation_name(&journal.id), &operation)?;
        worker_guard(&journal, &invocation)?;
        let outcome = NativeOutcome::Exited {
            code: Some(0),
            signal: None,
        };
        worker_after(&journal, &invocation, &outcome)?;
        let result = finish(&store, &mut journal, &operation, &invocation, &outcome)?;
        let (verdict, reason, calls) = if verify_fails {
            (Value::Null, "native_verify_unconfirmed", "verify\n")
        } else {
            (Value::Bool(false), "mismatch", "verify\nstatus\n")
        };
        assert_eq!(result["filesMatch"], verdict);
        assert_eq!(result["verification"], reason);
        assert_eq!(result["phase"], "unknown");
        assert_eq!(result["nativeResult"], "succeeded");
        assert_eq!(fs::read_to_string(f.ctx.home.join("query-calls"))?, calls);
        assert!(store.receipts()?.installations.is_empty());
        assert!(!f.binding.destination.join(".demo").exists());
        assert!(!result.to_string().contains("fixture diagnostics"));
    }
    Ok(())
}
