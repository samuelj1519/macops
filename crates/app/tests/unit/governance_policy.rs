#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

fn locations() -> Locations {
    Locations {
        home: "/fixture/home".into(),
        settings: "/fixture/home/.local/settings".into(),
        config: "/fixture/home/.config/macops".into(),
        data: "/fixture/home/.local/share/macops".into(),
        state: "/fixture/home/.local/state/macops".into(),
        cache: "/fixture/home/.cache/macops".into(),
    }
}
fn binding(path: &Path, inode: u64) -> DirectoryBinding {
    let identity = FileId { device: 1, inode };
    DirectoryBinding {
        requested: path.into(),
        canonical: path.into(),
        lineage: vec![identity.clone()],
        identity,
        uid: 501,
        mode: 0o700,
    }
}
fn ledger(p: &Locations) -> CommittedMigration {
    CommittedMigration {
        schema_version: 1,
        phase: MigrationPhase::Committed,
        migration_id: "OP-100-1".into(),
        home: p.home.clone(),
        legacy_settings: p.settings.clone(),
        layout: Layout {
            config: p.config.clone(),
            data: p.data.clone(),
            state: p.state.clone(),
            cache: p.cache.clone(),
        },
        root_bindings: DurableBindings {
            config: binding(&p.config, 10),
            data: binding(&p.data, 11),
            state: binding(&p.state, 12),
        },
        cache_audit: Some(binding(&p.cache, 13)),
        converted: DATASETS
            .into_iter()
            .map(|key| (key, Transfer::Missing))
            .collect(),
        legacy_links: vec![],
        archives: vec![],
        retained: vec![],
    }
}
#[test]
fn authority_switches_once_and_volatile_cache_does_not_change_durable_scope() -> TestResult {
    let mut p = locations();
    let l = ledger(&p);
    let old = select(&p, None)?;
    assert_eq!(old.authority, Authority::Legacy);
    assert_eq!(old.managed, p.home.join(".agents/managed-assets.json"));
    p.cache = "/fixture/rebuilt-cache".into();
    let new = select(&p, Some(&l))?;
    assert!(matches!(new.authority, Authority::Migrated { .. }));
    assert_eq!(new.managed, p.state.join("managed-assets.json"));
    assert_eq!(new.updates, p.cache.join("managed-updates.json"));
    assert_eq!(new.profiles, p.config.join("manifests/skill-profiles.json"));
    Ok(())
}

fn proof(source: &Path) -> Result<FileProof> {
    let parent = source
        .parent()
        .ok_or_else(|| Error::new("fixture", "parent"))?;
    let parent_id = FileId {
        device: 1,
        inode: 50,
    };
    Ok(FileProof {
        anchor: ConfigAnchor {
            requested: source.into(),
            canonical_parent: parent.into(),
            parent_identity: parent_id.clone(),
            name: source
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::new("fixture", "name"))?
                .into(),
        },
        identity: FileId {
            device: 1,
            inode: 60,
        },
        lineage: vec![parent_id],
        sha256: "a".repeat(64),
        mode: 0o644,
    })
}
fn copied(p: &Locations, l: &CommittedMigration, dataset: Dataset) -> Result<Transfer> {
    let source = dataset.source(p);
    Ok(Transfer::Copied {
        source: source.clone(),
        target: dataset.target(&l.layout),
        original: Box::new(proof(&source)?),
        target_sha256: "a".repeat(64),
        target_mode: 0o600,
    })
}
fn audit_fixture(p: &Locations, l: &mut CommittedMigration) {
    for kind in [
        ArchiveKind::Settings,
        ArchiveKind::Backups,
        ArchiveKind::RebuildState,
        ArchiveKind::UnifyState,
    ] {
        l.archives.push(Archive {
            kind,
            root: archive_root(p, kind),
            mode: 0o700,
            entries: BTreeMap::from([
                (
                    "file".into(),
                    IndexedEntry::File {
                        sha256: "a".repeat(64),
                        mode: 0o600,
                        size: 20,
                    },
                ),
                ("directory".into(), IndexedEntry::Directory { mode: 0o700 }),
                (
                    "link".into(),
                    IndexedEntry::Link {
                        text: "relative".into(),
                        mode: 0o777,
                    },
                ),
            ]),
        });
    }
    for (owner, relative) in [
        (RetainKind::NativeSkillsLock, ".agents/.skill-lock.json"),
        (RetainKind::NativeSkills, ".agents/skills"),
        (RetainKind::Translations, ".agents/translations"),
        (RetainKind::SkillAliases, ".agents/skill-aliases.json"),
        (RetainKind::SkillSources, ".agents/skill-sources.json"),
        (RetainKind::Releases, ".agents/releases"),
        (RetainKind::NativeShare, ".local/share"),
        (RetainKind::Launchers, ".local/bin"),
        (RetainKind::Harness, ".claude"),
        (RetainKind::Harness, ".claude-mi"),
        (RetainKind::Harness, ".codex"),
    ] {
        l.retained.push(Retained {
            owner,
            path: p.home.join(relative),
            entry: None,
        });
    }
}

#[test]
fn dataset_locations_match_the_explicit_conversion_contract_and_roundtrip_strictly() -> TestResult {
    let p = locations();
    let mut l = ledger(&p);
    let cases = [
        (
            Dataset::ManagedAssets,
            ".agents/managed-assets.json",
            ".local/state/macops/managed-assets.json",
        ),
        (
            Dataset::InvocationOverlays,
            ".agents/invocation-overlays.json",
            ".local/state/macops/invocation-overlays.json",
        ),
        (
            Dataset::ManagedUpdates,
            ".agents/cache/managed-updates.json",
            ".cache/macops/managed-updates.json",
        ),
        (
            Dataset::SkillProfiles,
            ".local/settings/manifests/skill-profiles.json",
            ".config/macops/manifests/skill-profiles.json",
        ),
        (
            Dataset::AssetDefinitions,
            ".local/settings/manifests/managed-agent-assets.json",
            ".config/macops/manifests/managed-agent-assets.json",
        ),
        (
            Dataset::InvocationRules,
            ".local/settings/manifests/agent-invocation-policy.json",
            ".config/macops/manifests/agent-invocation-policy.json",
        ),
        (
            Dataset::SourceChannels,
            ".local/settings/manifests/agent-source-channels.json",
            ".config/macops/manifests/agent-source-channels.json",
        ),
        (
            Dataset::MacApps,
            ".local/settings/manifests/mac-apps.tsv",
            ".config/macops/manifests/mac-apps.tsv",
        ),
    ];
    for (dataset, source, target) in cases {
        assert_eq!(dataset.source(&p), p.home.join(source));
        assert_eq!(dataset.target(&l.layout), p.home.join(target));
        l.converted.insert(dataset, copied(&p, &l, dataset)?);
    }
    l.legacy_links.push(LegacyLink {
        path: p.home.join(".agents/skill-profiles.json"),
        text: "../.local/settings/manifests/skill-profiles.json".into(),
        target: p.settings.join("manifests/skill-profiles.json"),
    });
    audit_fixture(&p, &mut l);
    let parsed = parse(&p, &serde_json::to_vec(&l)?)?;
    assert_eq!(parsed, l);
    let paths = select(&p, Some(&parsed))?;
    for dataset in DATASETS {
        assert_eq!(paths.dataset(dataset), dataset.target(&l.layout));
    }
    assert_eq!(paths.required.len(), 7);
    assert!(!paths.required.contains(&Dataset::ManagedUpdates));
    assert!(cache_allowed(&p, &p.cache));
    assert!(!cache_allowed(&p, &p.state));
    assert!(!cache_allowed(&p, p.home.as_path()));
    Ok(())
}

#[test]
fn malformed_ledger_bindings_or_scope_never_select_legacy_as_a_fallback() -> TestResult {
    let p = locations();
    let base = ledger(&p);
    for case in 0..21 {
        let mut l = base.clone();
        match case {
            0 => l.schema_version = 3,
            1 => l.migration_id = "wrong".into(),
            2 => l.migration_id = format!("OP-{}", "a".repeat(129)),
            3 => l.home = "/other".into(),
            4 => l.legacy_settings = "/other".into(),
            5 => l.layout.config = "/other".into(),
            6 => l.layout.data = "/other".into(),
            7 => l.layout.state = "/other".into(),
            8 => l.layout.cache = l.layout.state.clone(),
            9 => l.layout.cache = "relative".into(),
            10 => l.root_bindings.config.requested = "/other".into(),
            11 => l.root_bindings.config.canonical = "relative".into(),
            12 => l.root_bindings.config.identity.inode = 0,
            13 => l.root_bindings.config.identity.device = 0,
            14 => l.root_bindings.config.lineage.clear(),
            15 => {
                l.root_bindings.config.lineage = vec![
                    FileId {
                        device: 1,
                        inode: 10
                    };
                    65
                ];
            }
            16 => {
                l.root_bindings.config.lineage = vec![FileId {
                    device: 1,
                    inode: 999,
                }];
            }
            17 => l.root_bindings.config.lineage.insert(
                0,
                FileId {
                    device: 0,
                    inode: 3,
                },
            ),
            18 => l.root_bindings.config.mode = 0o755,
            19 => l.cache_audit.as_mut().ok_or("cache")?.mode = 0o10000,
            _ => {
                l.converted.remove(&Dataset::MacApps);
            }
        }
        assert!(select(&p, Some(&l)).is_err(), "case {case}");
        assert_eq!(
            parse(&p, &serde_json::to_vec(&l)?).err().ok_or("bad")?.code,
            "invalid_migration_ledger"
        );
    }
    for malformed in [
        b"{".as_slice(),
        b"[]",
        br#"{"schema_version":1,"schema_version":1}"#,
        br#"{"phase":"prepared"}"#,
    ] {
        assert_eq!(
            parse(&p, malformed).err().ok_or("invalid")?.code,
            "invalid_migration_ledger"
        );
    }
    let mut invalid_locations = p;
    invalid_locations.home = "relative".into();
    assert!(select(&invalid_locations, None).is_err());
    Ok(())
}

#[test]
fn copied_source_and_target_evidence_is_bound_and_cannot_be_retargeted() -> TestResult {
    let p = locations();
    let base = ledger(&p);
    for case in 0..15 {
        let mut l = base.clone();
        let mut transfer = copied(&p, &l, Dataset::ManagedAssets)?;
        let Transfer::Copied {
            source,
            target,
            original,
            target_sha256,
            target_mode,
        } = &mut transfer
        else {
            return Err("copied".into());
        };
        match case {
            0 => *source = "/other".into(),
            1 => *target = "/other".into(),
            2 => *target_sha256 = "bad".into(),
            3 => *target_sha256 = "b".repeat(64),
            4 => *target_mode = 0o644,
            5 => original.mode = 0o10000,
            6 => original.identity.device = 0,
            7 => original.identity.inode = 0,
            8 => original.sha256 = "z".repeat(64),
            9 => original.anchor.requested = "/other".into(),
            10 => original.anchor.canonical_parent = "relative".into(),
            11 => original.anchor.name = "..".into(),
            12 => original.lineage.clear(),
            13 => original.anchor.parent_identity.inode = 0,
            _ => original.anchor.requested = "relative".into(),
        }
        l.converted.insert(Dataset::ManagedAssets, transfer);
        assert!(validate(&p, &l).is_err(), "case {case}");
    }
    let mut p = locations();
    p.config = p.settings.clone();
    let l = ledger(&p);
    assert!(validate(&p, &l).is_err());
    Ok(())
}

#[test]
fn unsafe_links_archive_entries_and_retained_roots_are_rejected_without_fallback() {
    let p = locations();
    let base = ledger(&p);
    for case in 0..7 {
        let mut l = base.clone();
        l.legacy_links.push(LegacyLink {
            path: p.home.join(".agents/skill-profiles.json"),
            text: "../.local/settings/manifests/skill-profiles.json".into(),
            target: Dataset::SkillProfiles.source(&p),
        });
        match case {
            0 => l.legacy_links.push(l.legacy_links[0].clone()),
            1 => l.legacy_links[0].path = "/wrong".into(),
            2 => l.legacy_links[0].target = "/wrong".into(),
            3 => l.legacy_links[0].text = "/other".into(),
            4 => l.legacy_links[0].text = PathBuf::from("bad\0name"),
            5 => l.legacy_links[0].text = "/Users/example/job/forbidden".into(),
            _ => l.root_bindings.data.mode = 0o755,
        }
        assert!(validate(&p, &l).is_err());
    }
}

#[test]
fn unsafe_archive_entries_and_retained_roots_are_rejected_without_fallback() {
    let p = locations();
    let base = ledger(&p);
    for case in 0..10 {
        let mut l = base.clone();
        l.archives.push(Archive {
            kind: ArchiveKind::Backups,
            root: archive_root(&p, ArchiveKind::Backups),
            mode: 0o700,
            entries: BTreeMap::from([(
                "file".into(),
                IndexedEntry::File {
                    sha256: "a".repeat(64),
                    mode: 0o600,
                    size: 1,
                },
            )]),
        });
        match case {
            0 => l.archives.push(l.archives[0].clone()),
            1 => l.archives[0].root = "/wrong".into(),
            2 => l.archives[0].mode = 0o10000,
            3 => l.archives[0].mode = 0o755,
            4 => {
                l.archives[0]
                    .entries
                    .insert(PathBuf::new(), IndexedEntry::Directory { mode: 0o700 });
            }
            5 => {
                l.archives[0]
                    .entries
                    .insert("/absolute".into(), IndexedEntry::Directory { mode: 0o700 });
            }
            6 => {
                l.archives[0]
                    .entries
                    .insert("../outside".into(), IndexedEntry::Directory { mode: 0o700 });
            }
            7 => {
                l.archives[0].entries.insert(
                    "bad".into(),
                    IndexedEntry::File {
                        sha256: "bad".into(),
                        mode: 0o600,
                        size: 0,
                    },
                );
            }
            8 => {
                l.archives[0]
                    .entries
                    .insert("bad".into(), IndexedEntry::Directory { mode: 0o10000 });
            }
            _ => {
                l.archives[0].entries.insert(
                    "bad".into(),
                    IndexedEntry::Link {
                        text: "bad\0".into(),
                        mode: 0o777,
                    },
                );
            }
        }
        assert!(validate(&p, &l).is_err());
    }
}

#[test]
fn archive_budget_and_retention_bounds_are_enforced() -> TestResult {
    let p = locations();
    let base = ledger(&p);
    let mut l = base.clone();
    l.archives.push(Archive {
        kind: ArchiveKind::Settings,
        root: p.settings.clone(),
        mode: 0o700,
        entries: (0..40001)
            .map(|i| {
                (
                    format!("file-{i}").into(),
                    IndexedEntry::Directory { mode: 0o700 },
                )
            })
            .collect(),
    });
    assert!(validate(&p, &l).is_err());
    for path in [
        "/outside",
        "/fixture/home/.agents/unknown",
        "/fixture/home/.claude/nested",
    ] {
        assert!(!retained_path(&p, RetainKind::Harness, Path::new(path)));
    }
    let mut l = base.clone();
    l.retained = vec![
        Retained {
            path: p.home.join(".agents/skills"),
            owner: RetainKind::NativeSkills,
            entry: None,
        };
        10001
    ];
    assert!(validate(&p, &l).is_err());
    l.retained.truncate(2);
    assert!(validate(&p, &l).is_err());
    l.retained = vec![Retained {
        path: "relative".into(),
        owner: RetainKind::NativeSkills,
        entry: None,
    }];
    assert!(validate(&p, &l).is_err());
    l.retained = vec![Retained {
        path: p.home.join("unknown"),
        owner: RetainKind::NativeSkills,
        entry: None,
    }];
    assert!(validate(&p, &l).is_err());
    let mut l = base;
    l.cache_audit = None;
    validate(&p, &l)?;
    l.root_bindings.state.mode = 0o755;
    assert!(validate(&p, &l).is_err());
    Ok(())
}

#[test]
fn nul_locations_and_relative_binding_requests_are_rejected_at_the_boundary() {
    let mut p = locations();
    let mut l = ledger(&p);
    l.root_bindings.config.requested = "relative".into();
    assert!(validate(&p, &l).is_err());
    l = ledger(&p);
    p.home = "relative".into();
    assert!(validate(&p, &l).is_err());
    p = locations();
    p.cache = "/fixture/bad\0cache".into();
    assert!(select(&p, None).is_err());
}

#[test]
fn v2_exact_paths_roundtrip_and_v1_reads_only_historical_retained_kinds() -> TestResult {
    use RetainKind::*;
    use RetainedEntry::*;

    let p = locations();
    let mut version_one = ledger(&p);
    audit_fixture(&p, &mut version_one);
    let legacy = serde_json::to_vec(&version_one)?;
    assert_eq!(parse(&p, &legacy)?, version_one);
    let mut version_two = version_one.clone();
    version_two.schema_version = LEDGER_VERSION;
    for (path, owner, entry) in [
        (".agents/.git", AgentRepositoryMetadata, Directory),
        (".agents/.gitignore", AgentRepositoryMetadata, File),
        (".agents/README.zh-CN.md", AgentRepositoryMetadata, File),
        (
            ".agents/SKILL-LEARNING-GUIDE.zh-CN.md",
            AgentRepositoryMetadata,
            File,
        ),
        (
            ".agents/SKILL-USAGE-GUIDE.zh-CN.md",
            AgentRepositoryMetadata,
            File,
        ),
        (".agents/.DS_Store", SystemMetadata, File),
        (".local/state/.DS_Store", SystemMetadata, File),
        (
            ".agents/cache/upstream/cursor-plugins",
            LegacyGovernanceHistory,
            Directory,
        ),
        (
            ".agents/out/M29-20261020-001-REPORT.txt",
            LegacyGovernanceHistory,
            File,
        ),
        (
            ".agents/bin/agentflow-sync-skills",
            IndependentAgentEntry,
            File,
        ),
        (".agents/locks", EmptyLegacyLockContainer, EmptyDirectory),
        (".local/state/claude", NativeState, Directory),
        (
            ".local/state/gh",
            NativeState,
            Symlink {
                text: "other-owner".into(),
            },
        ),
    ] {
        let entry = Retained {
            path: p.home.join(path),
            owner,
            entry: Some(entry),
        };
        assert!(retained_entry_valid(&p, p.roots(), &entry), "{path}");
        let mut wrong = version_one.clone();
        wrong.retained.push(entry.clone());
        assert_eq!(
            parse(&p, &serde_json::to_vec(&wrong)?)
                .err()
                .ok_or("v1 new kind")?
                .code,
            "invalid_migration_ledger"
        );
        version_two.retained.push(entry);
    }
    assert_eq!(parse(&p, &serde_json::to_vec(&version_two)?)?, version_two);

    Ok(())
}

#[test]
fn v2_retained_paths_and_entry_shapes_cannot_be_forged() -> TestResult {
    use RetainKind::*;
    use RetainedEntry::*;
    use std::os::unix::ffi::OsStrExt;

    let p = locations();
    let mut version_two = ledger(&p);
    version_two.schema_version = LEDGER_VERSION;
    let last = Retained {
        path: p.home.join(".local/state/gh"),
        owner: NativeState,
        entry: Some(Symlink {
            text: "other-owner".into(),
        }),
    };
    let mut invalid = Vec::new();
    for path in [
        ".local/state/unknown",
        ".local/state/gh/nested",
        ".local/state/./gh",
        ".local/state/gh/",
        ".local/state/gh\0",
        ".local/state/macos-rebuild",
        ".local/state/mo-unify",
        ".local/state/macops",
        ".agents/bin/skillctl",
    ] {
        invalid.push(Retained {
            path: p.home.join(path),
            ..last.clone()
        });
    }
    for text in [
        "",
        "bad\0target",
        "macops",
        "../../.agents",
        "../../.local/settings",
        "../../../",
    ] {
        invalid.push(Retained {
            entry: Some(Symlink { text: text.into() }),
            ..last.clone()
        });
    }
    let non_utf8 = Retained {
        entry: Some(Symlink {
            text: std::ffi::OsStr::from_bytes(b"invalid-\xff").into(),
        }),
        ..last.clone()
    };
    assert!(!retained_entry_valid(&p, p.roots(), &non_utf8));
    assert!(retained_rule(&p, &p.home.join(std::ffi::OsStr::from_bytes(b"owner-\xff"))).is_none());
    invalid.extend([
        Retained {
            owner: SystemMetadata,
            ..last.clone()
        },
        Retained {
            entry: None,
            ..last.clone()
        },
        Retained {
            entry: Some(File),
            ..last.clone()
        },
        Retained {
            entry: Some(Directory),
            path: p.home.join(".agents/locks"),
            owner: EmptyLegacyLockContainer,
        },
        Retained {
            entry: Some(Directory),
            path: p.home.join(".agents/.DS_Store"),
            owner: SystemMetadata,
        },
    ]);
    for entry in invalid {
        assert!(!retained_entry_valid(&p, p.roots(), &entry), "{entry:?}");
        let mut wrong = version_two.clone();
        wrong.retained = vec![entry];
        assert!(parse(&p, &serde_json::to_vec(&wrong)?).is_err());
    }
    version_two.retained = vec![last.clone(), last];
    assert!(parse(&p, &serde_json::to_vec(&version_two)?).is_err());
    Ok(())
}

#[test]
fn forged_ledger_versions_and_kinds_fail_with_the_same_closed_error() -> TestResult {
    let p = locations();
    let mut wrong = ledger(&p);
    wrong.schema_version = LEDGER_VERSION;
    wrong.retained = vec![Retained {
        path: p.home.join(".agents/bin/agentflow-sync-skills"),
        owner: RetainKind::IndependentAgentEntry,
        entry: Some(RetainedEntry::File),
    }];
    let mut value = serde_json::to_value(&wrong)?;
    for version in [0, 1, 3] {
        value["schema_version"] = serde_json::json!(version);
        assert!(parse(&p, &serde_json::to_vec(&value)?).is_err());
    }
    value["schema_version"] = serde_json::json!(2);
    value["retained"][0]["owner"] = serde_json::json!("unknown_owner");
    assert!(parse(&p, &serde_json::to_vec(&value)?).is_err());
    value["retained"][0]["owner"] = serde_json::json!("independent_agent_entry");
    value["retained"][0]["entry"]["kind"] = serde_json::json!("unknown_entry");
    assert!(parse(&p, &serde_json::to_vec(&value)?).is_err());
    value["retained"][0]["entry"] = serde_json::Value::Null;
    assert!(parse(&p, &serde_json::to_vec(&value)?).is_err());

    value["schema_version"] = serde_json::json!(1);
    value["retained"][0]["path"] = serde_json::json!(p.home.join(".agents/skills"));
    value["retained"][0]["owner"] = serde_json::json!("native_skills");
    assert!(parse(&p, &serde_json::to_vec(&value)?).is_err());
    value["retained"][0]
        .as_object_mut()
        .ok_or("retained entry")?
        .remove("entry");
    assert!(parse(&p, &serde_json::to_vec(&value)?).is_ok());
    Ok(())
}

#[test]
fn active_and_default_root_scope_protects_all_exact_native_and_legacy_paths() {
    let p = locations();
    let safe = p.roots();
    assert!(root_scope_valid(&p, safe));
    let defaults = default_roots(&p);
    assert!(root_scope_valid(
        &p,
        defaults.each_ref().map(PathBuf::as_path)
    ));
    for history in legacy_history_roots(&p) {
        let mut conflicting = p.clone();
        conflicting.settings = history.join("selected-settings");
        assert!(!root_scope_valid(&conflicting, safe));
    }
    for name in [
        "agent-media",
        "billion-context",
        "claude",
        "claudectl",
        "cmux",
        "gh",
        "herdr",
        "herdr-core",
        "mimocode",
        "mise",
        "pnpm",
    ] {
        let leaf = p.home.join(".local/state").join(name);
        assert_eq!(
            retained_rule(&p, &leaf),
            Some((RetainKind::NativeState, RetainedShape::NativeState))
        );
        let record = Retained {
            path: leaf.clone(),
            owner: RetainKind::NativeState,
            entry: Some(RetainedEntry::Directory),
        };
        for index in 0..safe.len() {
            for collision in [
                leaf.clone(),
                p.home.join(".local/state"),
                leaf.join("missing"),
            ] {
                let mut roots = safe;
                roots[index] = &collision;
                assert!(!root_scope_valid(&p, roots), "{index}: {collision:?}");
                assert!(
                    !retained_entry_valid(&p, roots, &record),
                    "{index}: {collision:?}"
                );
            }
        }
    }
    for index in 0..safe.len() {
        for relative in [
            ".local/settings",
            ".local/settings/manifests",
            ".agents",
            ".agents/backups",
            ".agents/invocation-policy.pending.json",
            ".agents/cache/upstream",
            ".agents/out",
            ".agents/bin",
            ".local/state/macos-rebuild",
            ".local/state/mo-unify",
            ".config/macops",
            ".local/share/macops",
            ".local/state/macops",
            ".cache/macops",
        ] {
            let root = p.home.join(relative);
            for collision in [root.clone(), root.join("missing")] {
                let mut roots = safe;
                roots[index] = &collision;
                let same_default = defaults[index].as_path() == collision;
                assert_eq!(
                    root_scope_valid(&p, roots),
                    same_default,
                    "{index}: {collision:?}"
                );
            }
        }
    }
    let retained = Retained {
        path: p.home.join(".local/state/gh"),
        owner: RetainKind::NativeState,
        entry: Some(RetainedEntry::Directory),
    };
    let mut forged_scope = p.clone();
    forged_scope.settings.clone_from(&retained.path);
    assert!(!root_scope_valid(&forged_scope, p.roots()));
    assert!(!retained_entry_valid(&forged_scope, p.roots(), &retained));
}
