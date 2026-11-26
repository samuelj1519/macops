#![cfg(test)]
use super::*;
use crate::{
    domain::{GovernanceCategory, Package},
    test_support::TestResult,
};
use std::os::unix::ffi::OsStringExt;
fn governance_plan() -> Result<Plan> {
    let package = Package {
        id: "governance:policy".into(),
        name: "policy".into(),
        kind: Kind::Skill,
        official_url: String::new(),
        provider: Provider::Governance {
            category: GovernanceCategory::Policy,
            manifest: "/h/.agents/backups/CASE-1/capability-plan.json".into(),
            manifest_sha256: "a".repeat(64),
        },
        paths: vec![],
        exact_version_required: false,
    };
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: 60,
        recipe_sha256: crate::domain::fingerprint(&package)?,
        package,
        action: Action::Maintain,
        before: Observation::Governance {
            fingerprint: "b".repeat(64),
        },
        target: Some("a".repeat(64)),
        command: None,
        query: None,
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    Ok(plan)
}

#[test]
fn capability_transactions_require_the_closed_non_command_protocol() -> TestResult {
    let mut plan = governance_plan()?;
    validate_plan(&plan)?;
    plan.action = Action::Upgrade;
    assert!(validate_plan(&plan).is_err());
    Ok(())
}

fn file_id(value: u64) -> crate::domain::FileId {
    crate::domain::FileId {
        device: 1,
        inode: value,
    }
}
fn directory(path: &str, inode: u64) -> crate::policy::governance::DirectoryBinding {
    crate::policy::governance::DirectoryBinding {
        requested: path.into(),
        canonical: path.into(),
        identity: file_id(inode),
        lineage: vec![file_id(1), file_id(inode)],
        uid: 501,
        mode: 0o755,
    }
}
fn ns(path: &str, name: &str) -> Namespace {
    Namespace {
        ancestor: directory(path, 2),
        remaining: vec![name.into()],
    }
}
fn raw_blob() -> Blob {
    Blob {
        sha256: "a".repeat(64),
        size: 3,
    }
}
fn present(inode: u64, mode: u32) -> State {
    State::File {
        identity: file_id(inode),
        sha256: "a".repeat(64),
        mode,
    }
}
fn example() -> Manifest {
    let base = PathBuf::from("/h/.agents");
    let ledgers = [
        LedgerSlot::Managed,
        LedgerSlot::Overlays,
        LedgerSlot::Receipts,
    ]
    .into_iter()
    .map(|slot| {
        let (root, name) = match slot {
            LedgerSlot::Managed => (base.clone(), "managed-assets.json"),
            LedgerSlot::Overlays => (base.clone(), "invocation-overlays.json"),
            LedgerSlot::Receipts | LedgerSlot::Native => (PathBuf::from("/state"), "receipts.json"),
        };
        LedgerChange {
            slot,
            path: root.join(name),
            namespace: ns(root.to_str().unwrap_or_default(), name),
            before: State::Missing,
            before_blob: None,
            after_blob: if slot == LedgerSlot::Overlays {
                Some(raw_blob())
            } else {
                None
            },
        }
    })
    .collect();
    Manifest {
        schema_version: 2,
        layout: None,
        links: None,
        configuration: None,
        operation_id: "OP-1".into(),
        change_id: "CHANGE-1".into(),
        category: GovernanceCategory::Policy,
        home: "/h".into(),
        state: "/state".into(),
        settings: "/settings".into(),
        pending: base.join("invocation-policy.pending.json"),
        inputs: vec![Witness {
            path: "/settings/rules.json".into(),
            namespace: ns("/settings", "rules.json"),
            state: State::Missing,
        }],
        directories: vec![DirectoryWitness {
            binding: directory("/h/.agents/skills/demo", 2),
            entries: BTreeMap::from([(
                "SKILL.md".into(),
                DirectoryEntry {
                    kind: "file".into(),
                    identity: file_id(3),
                    mode: 0o644,
                },
            )]),
        }],
        files: vec![FileChange {
            path: "/h/.agents/skills/demo/SKILL.md".into(),
            kind: crate::capability::rules::invocation::MetadataKind::Frontmatter.into(),
            namespace: ns("/h/.agents/skills/demo", "SKILL.md"),
            before: present(3, 0o644),
            before_blob: Some(raw_blob()),
            after_blob: Some(raw_blob()),
            after_mode: Some(0o644),
        }],
        ledgers,
    }
}
fn pending_for(manifest: &Manifest) -> Pending {
    Pending {
        schema_version: 2,
        operation_id: manifest.operation_id.clone(),
        manifest_sha256: "b".repeat(64),
        parents: vec![],
        files: vec![Candidate {
            target: manifest.files[0].path.clone(),
            staged: Some("/h/.agents/skills/demo/.OP-2.cap.tmp".into()),
            after: present(4, 0o644),
        }],
        ledgers: vec![Candidate {
            target: manifest.ledgers[1].path.clone(),
            staged: Some("/h/.agents/.OP-3.cap.tmp".into()),
            after: present(5, 0o600),
        }],
        rollbacks: vec![],
        retired_parents: vec![],
    }
}
fn manifest_rejected(change: impl FnOnce(&mut Manifest)) {
    let mut m = example();
    change(&mut m);
    assert!(validate_manifest(&m).is_err());
}
fn pending_rejected(change: impl FnOnce(&mut Pending)) {
    let m = example();
    let mut p = pending_for(&m);
    change(&mut p);
    assert!(validate_pending(&m, &p, &"b".repeat(64)).is_err());
}
#[test]
fn manifest_scopes_slots_and_resource_budgets_are_closed() -> TestResult {
    validate_manifest(&example())?;
    manifest_rejected(|m| m.schema_version = 1);
    manifest_rejected(|m| m.operation_id = "BAD".into());
    manifest_rejected(|m| m.files = vec![m.files[0].clone(); 5001]);
    manifest_rejected(|m| m.inputs = vec![m.inputs[0].clone(); 10001]);
    manifest_rejected(|m| m.directories = vec![m.directories[0].clone(); 513]);
    manifest_rejected(|m| m.ledgers = vec![m.ledgers[0].clone(); 5]);
    manifest_rejected(|m| m.change_id = "..".into());
    manifest_rejected(|m| m.home = "relative".into());
    manifest_rejected(|m| m.state = "relative".into());
    manifest_rejected(|m| m.settings = "relative".into());
    manifest_rejected(|m| m.pending = "relative".into());
    manifest_rejected(|m| m.category = GovernanceCategory::Links);
    manifest_rejected(|m| m.pending = "/h/foreign.json".into());
    manifest_rejected(|m| {
        m.ledgers.pop();
    });
    manifest_rejected(|m| m.ledgers.swap(0, 1));
    manifest_rejected(|m| m.ledgers[0].path = "/h/.agents/unrelated.json".into());
    let mut migrated = example();
    migrated.pending = "/state/invocation-policy.pending.json".into();
    for ledger in &mut migrated.ledgers[..2] {
        ledger.path = PathBuf::from("/state").join(ledger.path.file_name().ok_or("name")?);
        ledger.namespace = ns(
            "/state",
            ledger
                .path
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or("name")?,
        );
    }
    validate_manifest(&migrated)?;
    Ok(())
}
#[test]
fn namespace_and_witness_validation_rejects_ambiguous_or_unbound_paths() {
    manifest_rejected(|m| m.inputs[0].path = "relative".into());
    manifest_rejected(|m| m.inputs[0].namespace.ancestor.requested = "relative".into());
    manifest_rejected(|m| m.inputs[0].namespace.ancestor.canonical = "relative".into());
    manifest_rejected(|m| m.inputs[0].namespace.ancestor.lineage.clear());
    manifest_rejected(|m| m.inputs[0].namespace.ancestor.lineage = vec![file_id(2); 65]);
    manifest_rejected(|m| m.inputs[0].namespace.ancestor.identity.device = 0);
    manifest_rejected(|m| m.inputs[0].namespace.ancestor.identity.inode = 0);
    manifest_rejected(|m| m.inputs[0].namespace.remaining.clear());
    manifest_rejected(|m| m.inputs[0].namespace.remaining = vec!["a".into(); 65]);
    manifest_rejected(|m| m.inputs[0].namespace.remaining[0] = "../rules.json".into());
    manifest_rejected(|m| m.inputs[0].namespace.remaining[0] = "other.json".into());
    manifest_rejected(|m| m.inputs[0].state = present(0, 0o600));
    manifest_rejected(|m| m.directories.push(m.directories[0].clone()));
    manifest_rejected(|m| {
        m.directories[0].entries = BTreeMap::from([(
            "../escape".into(),
            m.directories[0].entries["SKILL.md"].clone(),
        )]);
    });
    manifest_rejected(|m| {
        m.directories[0].entries.values_mut().for_each(|e| {
            e.kind = "unknown".into();
        });
    });
    manifest_rejected(|m| {
        m.directories[0].entries.values_mut().for_each(|e| {
            e.mode = 0o10000;
        });
    });
    manifest_rejected(|m| {
        m.directories[0].entries.values_mut().for_each(|e| {
            e.identity.device = 0;
        });
    });
    manifest_rejected(|m| {
        m.directories[0].entries.values_mut().for_each(|e| {
            e.identity.inode = 0;
        });
    });
}
#[test]
fn payload_and_ledger_backups_cannot_authorize_other_files_or_link_replacements() -> TestResult {
    manifest_rejected(|m| m.files[0].path = "/h/.cursor/skills/demo/SKILL.md".into());
    manifest_rejected(|m| m.files[0].path = "/h/.agents/skills/DevNorm/SKILL.md".into());
    manifest_rejected(|m| m.files[0].namespace.ancestor.canonical = "/foreign".into());
    manifest_rejected(|m| m.files.push(m.files[0].clone()));
    manifest_rejected(|m| m.files[0].before_blob = None);
    manifest_rejected(|m| {
        m.files[0].before_blob.iter_mut().for_each(|b| {
            b.sha256 = "b".repeat(64);
        });
    });
    manifest_rejected(|m| m.files[0].after_mode = None);
    manifest_rejected(|m| m.files[0].after_mode = Some(0o10000));
    manifest_rejected(|m| {
        m.files[0].after_blob.iter_mut().for_each(|b| {
            b.size = 64 * 1024 * 1024 + 1;
        });
    });
    manifest_rejected(|m| m.ledgers[0].namespace.ancestor.canonical = "/foreign".into());
    manifest_rejected(|m| m.ledgers[0].before = present(3, 0o600));
    manifest_rejected(|m| {
        m.ledgers[0].after_blob = Some(Blob {
            sha256: "bad".into(),
            size: 0,
        });
    });
    let text = crate::capability::rules::delivery::LinkText::from_bytes(b"../safe")?;
    assert!(state_valid(&State::Link {
        identity: file_id(2),
        text: text.clone(),
        mode: None
    }));
    for state in [
        State::Link {
            identity: file_id(0),
            text: text.clone(),
            mode: None,
        },
        State::Link {
            identity: file_id(2),
            text: text.clone(),
            mode: Some(0o10000),
        },
        State::File {
            identity: file_id(2),
            sha256: "bad".into(),
            mode: 0o600,
        },
        present(2, 0o10000),
    ] {
        assert!(!state_valid(&state));
    }
    // Invalid text is now rejected at construction/decoding rather than admitted into State.
    for text in ["", "bad\0path"] {
        let encoded = serde_json::to_vec(
            &serde_json::json!({"kind":"link","identity":{"device":1,"inode":2},"text":text}),
        )?;
        assert!(serde_json::from_slice::<State>(&encoded).is_err());
    }
    assert!(!before_valid(
        &State::Link {
            identity: file_id(2),
            text,
            mode: None
        },
        None
    ));
    assert!(!before_valid(
        &State::File {
            identity: file_id(0),
            sha256: "a".repeat(64),
            mode: 0o600
        },
        Some(&raw_blob())
    ));
    Ok(())
}

#[test]
fn pending_binds_each_candidate_to_its_manifest_hash_scope_content_mode_and_order() -> TestResult {
    let m = example();
    let p = pending_for(&m);
    validate_pending(&m, &p, &"b".repeat(64))?;
    assert!(validate_pending(&m, &p, "invalid").is_err());
    pending_rejected(|p| p.schema_version = 1);
    pending_rejected(|p| p.operation_id = "OP-2".into());
    pending_rejected(|p| p.manifest_sha256 = "c".repeat(64));
    pending_rejected(|p| p.files.push(p.files[0].clone()));
    pending_rejected(|p| p.ledgers = vec![p.ledgers[0].clone(); 4]);
    pending_rejected(|p| p.rollbacks = vec![p.files[0].clone(); 20001]);
    pending_rejected(|p| p.files[0].target = "/h/.agents/skills/other/SKILL.md".into());
    pending_rejected(|p| p.files[0].after = present(0, 0o644));
    pending_rejected(|p| p.files[0].after = State::Missing);
    pending_rejected(|p| {
        if let State::File { sha256, .. } = &mut p.files[0].after {
            *sha256 = "c".repeat(64);
        }
    });
    pending_rejected(|p| p.files[0].after = present(4, 0o600));
    pending_rejected(|p| p.files[0].staged = Some("relative".into()));
    pending_rejected(|p| p.files[0].staged = Some("/h/.agents/skills/demo/not-a-candidate".into()));
    pending_rejected(|p| {
        p.files[0].staged = Some("/h/.agents/skills/foreign/.OP-2.cap.tmp".into());
    });
    pending_rejected(|p| p.ledgers.push(p.ledgers[0].clone()));
    pending_rejected(|p| {
        let mut c = p.files[0].clone();
        c.target = "/h/arbitrary".into();
        p.rollbacks.push(c);
    });
    let mut delete = example();
    delete.files[0].after_blob = None;
    delete.files[0].after_mode = None;
    let mut pending = pending_for(&delete);
    pending.files[0].after = State::Missing;
    pending.files[0].staged = None;
    validate_pending(&delete, &pending, &"b".repeat(64))?;
    pending.rollbacks.push(Candidate {
        target: delete.files[0].path.clone(),
        staged: Some("/h/.agents/skills/demo/.OP-9.cap.tmp".into()),
        after: present(9, 0o644),
    });
    pending.rollbacks.push(Candidate {
        target: delete.ledgers[1].path.clone(),
        staged: None,
        after: State::Missing,
    });
    validate_pending(&delete, &pending, &"b".repeat(64))?;
    Ok(())
}
fn parent_case() -> (Manifest, Pending) {
    let mut m = example();
    let mut file = m.files[0].clone();
    file.path = "/h/.agents/skills/demo/agents/openai.yaml".into();
    file.kind = crate::capability::rules::invocation::MetadataKind::CodexPolicy.into();
    file.before = State::Missing;
    file.before_blob = None;
    file.namespace.remaining = vec!["agents".into(), "openai.yaml".into()];
    m.files.push(file);
    let mut p = pending_for(&m);
    let staged: PathBuf = "/h/.agents/skills/demo/.OP-4.cap-dir".into();
    let mut binding = directory(staged.to_str().unwrap_or_default(), 6);
    binding.mode = 0o700;
    binding.lineage = vec![file_id(1), file_id(2), file_id(6)];
    p.parents.push(ParentCandidate {
        target: "/h/.agents/skills/demo/agents".into(),
        staged: staged.clone(),
        binding,
    });
    p.files.push(Candidate {
        target: m.files[1].path.clone(),
        staged: Some(staged.join(".OP-5.cap.tmp")),
        after: present(7, 0o644),
    });
    (m, p)
}
fn parent_rejected(change: impl FnOnce(&mut Manifest, &mut Pending)) {
    let (mut m, mut p) = parent_case();
    change(&mut m, &mut p);
    assert!(validate_pending(&m, &p, &"b".repeat(64)).is_err());
}
#[test]
fn newly_created_metadata_parents_require_identity_lineage_and_exclusive_staging_proofs()
-> TestResult {
    let (m, mut p) = parent_case();
    validate_manifest(&m)?;
    validate_pending(&m, &p, &"b".repeat(64))?;
    p.retired_parents.push(p.parents[0].target.clone());
    validate_pending(&m, &p, &"b".repeat(64))?;
    parent_rejected(|_, p| p.parents = vec![p.parents[0].clone(); 3]);
    parent_rejected(|_, p| p.parents[0].target = "/h/foreign".into());
    parent_rejected(|m, _| m.files[1].namespace.remaining.push("extra".into()));
    parent_rejected(|m, _| m.files[1].before = present(1, 0o600));
    parent_rejected(|m, p| {
        p.parents[0].target = "/h/.agents/skills/demo/other".into();
        m.files[1].path = p.parents[0].target.join("openai.yaml");
    });
    parent_rejected(|_, p| p.parents[0].staged = "/h/elsewhere/.OP-4.cap-dir".into());
    parent_rejected(|_, p| p.parents[0].staged = "/h/.agents/skills/demo/arbitrary".into());
    parent_rejected(|_, p| p.parents[0].binding.requested = "/h/foreign".into());
    parent_rejected(|_, p| p.parents[0].binding.canonical = "/h/foreign".into());
    parent_rejected(|_, p| {
        p.parents[0].binding.lineage.truncate(2);
    });
    parent_rejected(|_, p| p.parents[0].binding.lineage = vec![file_id(6)]);
    parent_rejected(|_, p| p.parents[0].binding.mode = 0o755);
    parent_rejected(|_, p| p.parents[0].binding.uid = 502);
    parent_rejected(|_, p| p.parents.push(p.parents[0].clone()));
    parent_rejected(|_, p| p.retired_parents.push("/h/foreign".into()));
    parent_rejected(|_, p| p.retired_parents = vec![p.parents[0].target.clone(); 2]);
    Ok(())
}

fn plan_rejected(change: impl FnOnce(&mut Plan)) -> TestResult {
    let mut plan = governance_plan()?;
    change(&mut plan);
    assert!(validate_plan(&plan).is_err());
    Ok(())
}
#[test]
fn governance_plans_cannot_become_process_recipes_or_ordinary_receipt_writes() -> TestResult {
    use crate::test_support;
    assert_eq!(category_name(GovernanceCategory::Links), "links");
    assert_eq!(category_name(GovernanceCategory::Source), "source");
    let mut ordinary = test_support::journal()?;
    validate_plan(&ordinary.plan)?;
    validate_journal(&ordinary)?;
    ordinary.plan.action = Action::Maintain;
    assert!(validate_plan(&ordinary.plan).is_err());
    ordinary.plan.action = Action::Install;
    ordinary.plan.before = Observation::Governance {
        fingerprint: "b".repeat(64),
    };
    assert!(validate_plan(&ordinary.plan).is_err());
    plan_rejected(|p| p.before = Observation::Missing { evidence: None })?;
    plan_rejected(|p| p.package.kind = Kind::Cli)?;
    plan_rejected(|p| p.package.id = "governance:source".into())?;
    plan_rejected(|p| p.command = Some(test_support::command("install")))?;
    plan_rejected(|p| p.query = Some(test_support::command("query").into()))?;
    plan_rejected(|p| p.synchronous = false)?;
    plan_rejected(|p| p.delegated = true)?;
    plan_rejected(|p| {
        if let Provider::Governance {
            manifest_sha256, ..
        } = &mut p.package.provider
        {
            *manifest_sha256 = "bad".into();
        }
    })?;
    plan_rejected(|p| {
        p.before = Observation::Governance {
            fingerprint: "bad".into(),
        }
    })?;
    plan_rejected(|p| p.target = None)?;
    plan_rejected(|p| {
        if let Provider::Governance { manifest, .. } = &mut p.package.provider {
            *manifest = "relative/capability-plan.json".into();
        }
    })?;
    plan_rejected(|p| {
        if let Provider::Governance { manifest, .. } = &mut p.package.provider {
            *manifest = "/h/.agents/backups/CASE-1/other.json".into();
        }
    })?;
    let mut journal = test_support::journal()?;
    journal.home = "/h".into();
    journal.scope.home = journal.home.clone();
    journal.state = "/state".into();
    journal.plan = governance_plan()?;
    validate_journal(&journal)?;
    for path in [
        "/state/backups/CASE-1/capability-plan.json".into(),
        PathBuf::from("/h/.agents/backups/CASE-1/capability-plan.json"),
    ] {
        if let Provider::Governance { manifest, .. } = &mut journal.plan.package.provider {
            *manifest = path;
        }
        validate_journal(&journal)?;
    }
    for path in [
        "/capability-plan.json".into(),
        PathBuf::from(format!(
            "/state/backups/{}/capability-plan.json",
            "x".repeat(129)
        )),
        "/foreign/backups/CASE-1/capability-plan.json".into(),
    ] {
        if let Provider::Governance { manifest, .. } = &mut journal.plan.package.provider {
            *manifest = path;
        }
        assert!(validate_journal(&journal).is_err());
    }
    let mut receipts = crate::domain::Receipts {
        schema_version: 1,
        installations: BTreeMap::new(),
    };
    journal.plan = governance_plan()?;
    assert!(
        crate::policy::operation::state::commit_receipt(
            &mut receipts,
            &journal,
            Observation::Governance {
                fingerprint: "b".repeat(64)
            },
            1
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn journal_nested_namespaces_and_rollback_candidates_propagate_rejection() -> TestResult {
    manifest_rejected(|m| m.files[0].namespace.remaining[0] = "other".into());
    manifest_rejected(|m| m.ledgers[0].namespace.remaining[0] = "other".into());
    manifest_rejected(|m| m.directories[0].binding.lineage.clear());
    pending_rejected(|p| p.ledgers[0].target = "/h/foreign".into());
    pending_rejected(|p| {
        let mut candidate = p.files[0].clone();
        candidate.after = present(8, 0o600);
        p.rollbacks.push(candidate);
    });
    let mut journal = crate::test_support::journal()?;
    journal.plan = governance_plan()?;
    journal.plan.synchronous = false;
    assert!(validate_journal(&journal).is_err());
    journal.plan_sha256 = crate::domain::fingerprint(&journal.plan)?;
    assert!(crate::policy::operation::state::validate_journal(&journal).is_err());
    Ok(())
}

fn links_example() -> TestResultManifest {
    use crate::capability::rules::delivery::{ChangeKind, LinkText, Profile, Selection};
    let mut m = example();
    m.schema_version = 3;
    m.category = GovernanceCategory::Links;
    m.layout = Some(crate::policy::governance::Layout {
        config: "/config".into(),
        data: "/data".into(),
        state: m.state.clone(),
        cache: "/historical-cache".into(),
    });
    m.links = Some(LinkAuthority {
        requested: Selection::All,
        canonical: directory("/h/.agents/skills", 2),
        selected: BTreeMap::from([("alpha".into(), "unit:alpha".into())]),
        profiles: BTreeMap::from([(Profile::Mi, directory("/h/.claude-mi", 3))]),
        pending_config_policies: vec!["claude:mi:pstack_allow".into()],
        protected: BTreeMap::new(),
    });
    let text = LinkText::managed("alpha")?;
    m.files = vec![FileChange {
        path: "/h/.claude-mi/skills/alpha".into(),
        kind: ChangeKind::ManagedLink,
        namespace: ns("/h/.claude-mi/skills", "alpha"),
        before: State::Missing,
        before_blob: None,
        after_blob: Some(Blob {
            sha256: crate::capability::rules::invocation::hash(text.as_str().as_bytes()),
            size: text.as_str().len() as u64,
        }),
        after_mode: None,
    }];
    m.files[0].namespace.ancestor.identity = file_id(4);
    m.files[0].namespace.ancestor.lineage = vec![file_id(1), file_id(3), file_id(4)];
    Ok(m)
}
type TestResultManifest = std::result::Result<Manifest, Box<dyn std::error::Error>>;

fn configuration_example() -> TestResultManifest {
    use crate::{
        capability::rules::delivery::{ChangeKind, Profile},
        policy::profile::{
            profile_authority::{Authority, Client, Target},
            profile_fields::{ConfigProfile, Intent, Owner, Role},
        },
    };
    let mut manifest = links_example()?;
    manifest.schema_version = 4;
    let owner = Owner {
        profile: ConfigProfile::Claude(Profile::Mi),
        role: Role::Live,
        intent: Intent::Plugins,
    };
    let target = PathBuf::from("/h/.claude-mi/settings.json");
    let root = directory("/h/.claude-mi", 3);
    manifest.files.push(FileChange {
        path: target.clone(),
        kind: ChangeKind::ProfileJson,
        namespace: Namespace {
            ancestor: root.clone(),
            remaining: vec!["settings.json".into()],
        },
        before: present(8, 0o600),
        before_blob: Some(raw_blob()),
        after_blob: Some(raw_blob()),
        after_mode: Some(0o600),
    });
    manifest.configuration = Some(Authority {
        mapping_inputs: crate::config::rules::config_tx::ConfigRootInputs {
            source: None,
            destination: None,
            config: None,
            env: BTreeMap::new(),
        },
        isolated: true,
        codex_root: None,
        targets: BTreeMap::from([(
            target.clone(),
            Target {
                clients: BTreeMap::from([(
                    owner.id(),
                    Client {
                        owner,
                        live_root: root,
                        live: target,
                        source: None,
                    },
                )]),
            },
        )]),
    });
    Ok(manifest)
}
#[test]
fn v4_profile_file_authority_requires_existing_regular_bytes_exact_kind_and_preserved_mode()
-> TestResult {
    use crate::capability::rules::delivery::ChangeKind;
    let good = configuration_example()?;
    validate_manifest(&good)?;
    let mut bad_authority = good.clone();
    bad_authority.links.as_mut().ok_or("links")?.canonical.mode = 0o10000;
    assert!(validate_manifest(&bad_authority).is_err());
    let mut missing_authority = good.clone();
    missing_authority.configuration = None;
    assert!(validate_profile_file(&missing_authority, &good.files[1]).is_err());
    let mut cases = vec![];
    let mut m = good.clone();
    m.layout = None;
    cases.push(m);
    let mut m = good.clone();
    m.configuration = None;
    cases.push(m);
    let mut m = good.clone();
    m.configuration.as_mut().ok_or("authority")?.targets.clear();
    cases.push(m);
    let mut m = good.clone();
    m.configuration.as_mut().ok_or("authority")?.codex_root = Some("relative".into());
    cases.push(m);
    for change in 0..7 {
        let mut m = good.clone();
        let file = &mut m.files[1];
        match change {
            0 => file.kind = ChangeKind::ProfileToml,
            1 => {
                file.before = State::Missing;
                file.before_blob = None;
            }
            2 => file.after_mode = Some(0o644),
            3 => file.after_blob = None,
            4 => file.before_blob = None,
            5 => {
                file.after_blob = Some(Blob {
                    sha256: "bad".into(),
                    size: 1,
                });
            }
            _ => m.schema_version = 3,
        }
        cases.push(m);
    }
    for manifest in cases {
        assert!(validate_manifest(&manifest).is_err());
    }
    let mut no_links = good.clone();
    no_links.links = None;
    no_links.files.remove(0);
    validate_manifest(&no_links)?;
    Ok(())
}
#[test]
fn v3_link_manifest_is_distinct_from_the_preserved_v2_policy_protocol() -> TestResult {
    let linked = links_example()?;
    validate_manifest(&linked)?;
    let value = serde_json::to_value(example())?;
    assert_eq!(value["files"][0]["kind"], "frontmatter");
    assert!(value.get("links").is_none());
    assert!(value.get("layout").is_none());
    validate_manifest(&serde_json::from_value::<crate::policy::json::Object<Manifest>>(value)?.0)?;
    Ok(())
}

fn rejected_link(change: impl FnOnce(&mut Manifest)) -> TestResult {
    let mut manifest = links_example()?;
    change(&mut manifest);
    assert!(validate_manifest(&manifest).is_err());
    Ok(())
}
#[test]
fn link_layout_profile_bindings_selected_units_and_category_are_closed() -> TestResult {
    use crate::capability::rules::delivery::{ChangeKind, Profile, Selection};
    rejected_link(|m| m.layout = None)?;
    rejected_link(|m| m.links = None)?;
    rejected_link(|m| {
        if let Some(l) = &mut m.layout {
            l.config = "relative".into();
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.layout {
            l.state = "/elsewhere".into();
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.layout {
            l.data = l.config.join("nested");
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.canonical.canonical = "/foreign".into();
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.canonical.lineage.clear();
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.canonical.requested = "/foreign".into();
            l.canonical.canonical = l.canonical.requested.clone();
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.requested = Selection::Explicit(vec![]);
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.selected.insert("bad/name".into(), "unit:x".into());
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.selected.insert("beta".into(), "unit:alpha".into());
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.selected.insert("beta".into(), String::new());
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.profiles.insert(Profile::Mi, directory("/h/.claude", 3));
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.protected.insert(
                Profile::Zen,
                std::collections::BTreeSet::from(["alpha".into()]),
            );
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.protected.insert(
                Profile::Mi,
                std::collections::BTreeSet::from(["bad/name".into()]),
            );
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.pending_config_policies.push("unknown".into());
        }
    })?;
    rejected_link(|m| m.files[0].kind = ChangeKind::Frontmatter)?;
    rejected_link(|m| m.files[0].after_mode = Some(0o755))?;
    let mut policy = example();
    policy.files[0].kind = ChangeKind::ManagedLink;
    assert!(validate_manifest(&policy).is_err());
    let mut allowed = links_example()?;
    allowed.links.as_mut().ok_or("links")?.protected.insert(
        Profile::Mi,
        std::collections::BTreeSet::from(["other".into()]),
    );
    validate_manifest(&allowed)?;
    Ok(())
}
#[test]
fn link_targets_never_authorize_other_profiles_protected_names_foreign_text_or_unobserved_mode()
-> TestResult {
    use crate::capability::rules::delivery::{LinkText, Profile};
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.protected.insert(
                Profile::Mi,
                std::collections::BTreeSet::from(["alpha".into()]),
            );
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.profiles.clear();
        }
    })?;
    rejected_link(|m| {
        if let Some(l) = &mut m.links {
            l.selected.clear();
        }
    })?;
    rejected_link(|m| m.files[0].namespace.ancestor.uid = 999)?;
    rejected_link(|m| m.files[0].namespace.ancestor.lineage = vec![file_id(1), file_id(2)])?;
    rejected_link(|m| {
        m.files[0].before = present(4, 0o600);
        m.files[0].before_blob = Some(raw_blob());
    })?;
    rejected_link(|m| {
        m.files[0].after_blob = None;
    })?;
    rejected_link(|m| {
        if let Some(b) = &mut m.files[0].after_blob {
            b.sha256 = "b".repeat(64);
        }
    })?;
    let mut retired = links_example()?;
    let text = LinkText::managed("alpha")?;
    retired.files[0].before = State::Link {
        identity: file_id(9),
        text: text.clone(),
        mode: Some(0o755),
    };
    retired.files[0].before_blob = retired.files[0].after_blob.take();
    validate_manifest(&retired)?;
    let mut bad = retired.clone();
    if let State::Link { mode, .. } = &mut bad.files[0].before {
        *mode = None;
    }
    assert!(validate_manifest(&bad).is_err());
    let mut foreign = retired.clone();
    if let State::Link { text, .. } = &mut foreign.files[0].before {
        *text = LinkText::from_bytes(b"../../.agents//skills/alpha")?;
    }
    assert!(validate_manifest(&foreign).is_err());
    let mut missing = retired;
    missing.files[0].before_blob = None;
    assert!(validate_manifest(&missing).is_err());
    Ok(())
}

#[test]
fn untrusted_inner_link_authorization_inputs_fail_without_relying_on_outer_dispatch() -> TestResult
{
    use crate::capability::rules::delivery::{LinkText, Profile};
    let mut file = example().files.remove(0);
    file.path = "/foreign/SKILL.md".into();
    assert!(validate_metadata_file(&example(), &file).is_err());
    let (mut source, pending) = parent_case();
    source.category = GovernanceCategory::Source;
    assert!(validate_parents(&source, &pending).is_err());
    rejected_link(|m| {
        if let Some(authority) = &mut m.links
            && let Some(binding) = authority.profiles.get_mut(&Profile::Mi)
        {
            binding.lineage.clear();
        }
    })?;
    let mut no_authority = links_example()?;
    no_authority.links = None;
    assert!(link_location(&no_authority, &no_authority.files[0]).is_err());
    for path in [
        "/foreign/.claude-mi/skills/alpha",
        "/h/.claude-mi/skills/alpha/extra",
        "/h/claude-mi/skills/alpha",
        "/h/.claude-other/skills/alpha",
    ] {
        let mut manifest = links_example()?;
        manifest.files[0].path = path.into();
        assert!(link_location(&manifest, &manifest.files[0]).is_err());
    }
    let mut non_utf8 = links_example()?;
    non_utf8.files[0].path =
        std::ffi::OsString::from_vec(b"/h/.claude-mi/skills/\xff".to_vec()).into();
    assert!(link_location(&non_utf8, &non_utf8.files[0]).is_err());
    let mut wrong_parent = links_example()?;
    wrong_parent.files[0].namespace = Namespace {
        ancestor: directory("/h/.claude-mi", 999),
        remaining: vec!["skills".into(), "alpha".into()],
    };
    assert!(validate_manifest(&wrong_parent).is_err());
    let mut broad_ancestor = links_example()?;
    broad_ancestor.files[0].namespace = Namespace {
        ancestor: directory("/h", 1),
        remaining: vec![".claude-mi".into(), "skills".into(), "alpha".into()],
    };
    assert!(validate_manifest(&broad_ancestor).is_err());
    let mut long_name = links_example()?;
    let name = "x".repeat(256);
    long_name.links.as_mut().ok_or("links")?.selected =
        BTreeMap::from([(name.clone(), "unit:x".into())]);
    long_name.files[0].path = PathBuf::from("/h/.claude-mi/skills").join(name);
    assert!(validate_link_file(&long_name, &long_name.files[0]).is_err());
    assert!(LinkText::managed("safe")?.owned_by("safe"));
    Ok(())
}

#[test]
fn frozen_nonempty_link_selectors_are_revalidated_in_the_loaded_plan() -> TestResult {
    let mut m = links_example()?;
    m.links.as_mut().ok_or("links")?.requested =
        crate::capability::rules::delivery::Selection::Explicit(vec!["unit:alpha".into()]);
    validate_manifest(&m)?;
    m.links.as_mut().ok_or("links")?.requested =
        crate::capability::rules::delivery::Selection::Explicit(vec!["bad\nselector".into()]);
    assert!(validate_manifest(&m).is_err());
    Ok(())
}
