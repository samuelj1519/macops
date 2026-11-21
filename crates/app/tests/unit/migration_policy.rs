use super::*;
use crate::test_support::TestResult;
use serde_json::json;

fn locations() -> Locations {
    Locations {
        home: "/fixture/home".into(),
        settings: "/fixture/settings".into(),
        config: "/fixture/config".into(),
        data: "/fixture/data".into(),
        state: "/fixture/state".into(),
        cache: "/fixture/cache".into(),
    }
}

#[test]
fn closed_classification_and_known_legacy_manifest_names() {
    let p = locations();
    for d in DATASETS {
        assert_eq!(classify(&p, &d.source(&p)), Classification::Convert(d));
    }
    for kind in [
        ArchiveKind::Settings,
        ArchiveKind::Backups,
        ArchiveKind::RebuildState,
        ArchiveKind::UnifyState,
    ] {
        assert_eq!(
            classify(&p, &governance::archive_root(&p, kind)),
            Classification::ArchiveIndexed(kind)
        );
    }
    for (path, owner) in [
        (".agents/.skill-lock.json", RetainKind::NativeSkillsLock),
        (".agents/skills", RetainKind::NativeSkills),
        (".agents/translations", RetainKind::Translations),
        (".agents/skill-aliases.json", RetainKind::SkillAliases),
        (".agents/skill-sources.json", RetainKind::SkillSources),
        (".agents/releases", RetainKind::Releases),
        (".local/share", RetainKind::NativeShare),
        (".local/bin", RetainKind::Launchers),
        (".claude-mi", RetainKind::Harness),
    ] {
        assert_eq!(
            classify(&p, &p.home.join(path)),
            Classification::RetainInPlace(owner)
        );
    }
    assert_eq!(
        classify(&p, &p.home.join(".agents/skill-profiles.json")),
        Classification::ProfilesLink
    );
    assert_eq!(
        classify(&p, &p.home.join(".agents/cache")),
        Classification::Container
    );
    for root in [&p.config, &p.data, &p.state, &p.cache] {
        assert_eq!(classify(&p, root), Classification::Runtime);
    }
    assert_eq!(
        classify(&p, &p.home.join(".agents/unknown.json")),
        Classification::Block
    );
    for name in [
        "skill-profiles.json",
        "managed-agent-assets.json",
        "agent-invocation-policy.json",
        "agent-source-channels.json",
        "mac-apps.tsv",
        "agent-media.json",
        "agent-plugins.tsv",
        "agent-skills.tsv",
        "grok-bot-skills.tsv",
        "home-top.tsv",
        "maintenance.tsv",
    ] {
        assert!(known_manifest(name));
    }
    for name in [
        "unknown.json",
        "managed-assets.json",
        "invocation-overlays.json",
        "managed-updates.json",
    ] {
        assert!(!known_manifest(name));
    }
}

#[test]
fn copied_datasets_require_their_existing_native_schemas() -> TestResult {
    let home = Path::new("/fixture/home");
    for (dataset, value) in [
        (
            Dataset::ManagedAssets,
            json!({"unknown_vendor":{"version":"1"}}),
        ),
        (Dataset::ManagedUpdates, json!({})),
        (
            Dataset::InvocationOverlays,
            json!({"schema_version":1,"files":{}}),
        ),
        (Dataset::SkillProfiles, json!({})),
        (
            Dataset::AssetDefinitions,
            json!({"schema_version":1,"assets":{}}),
        ),
        (
            Dataset::InvocationRules,
            json!({"schema_version":1,"standalone_manual_only":[]}),
        ),
        (Dataset::SourceChannels, json!({"schema_version":1})),
        (
            Dataset::SourceChannels,
            json!({"schema_version":1,"source_ids":{"owner/repo":"known"}}),
        ),
    ] {
        validate_dataset(home, dataset, &serde_json::to_vec(&value)?, 501)?;
    }
    let invalid_namespace = validate_dataset(
        home,
        Dataset::ManagedAssets,
        br#"{"vendor":[false,3],"_macops_sources":{"schema_version":0,"roots":{}}}"#,
        501,
    );
    assert_eq!(
        invalid_namespace
            .err()
            .ok_or("private namespace schema")?
            .code,
        "unverified_source_baseline"
    );
    validate_dataset(
        home,
        Dataset::MacApps,
        b"path\tbundle_id\tversion\towner\tsource\n",
        501,
    )?;
    for d in DATASETS {
        assert!(validate_dataset(home, d, b"[]", 501).is_err());
        assert!(validate_dataset(home, d, b"{oops", 501).is_err());
    }
    assert!(validate_dataset(home, Dataset::MacApps, b"\xff", 501).is_err());
    for value in [
        json!({}),
        json!({"schema_version":2}),
        json!({"schema_version":1,"source_ids":[]}),
        json!({"schema_version":1,"source_ids":{"x":null}}),
        json!({"schema_version":1,"source_ids":{"x":"bad/name"}}),
        json!({"schema_version":1,"source_ids":{"x":"x".repeat(256)}}),
    ] {
        assert!(
            validate_dataset(
                home,
                Dataset::SourceChannels,
                &serde_json::to_vec(&value)?,
                501
            )
            .is_err()
        );
    }
    Ok(())
}

#[test]
fn placement_manifest_never_selects_arbitrary_paths_or_names() -> TestResult {
    let p = locations();
    let layout = governance::Layout {
        config: p.config.clone(),
        data: p.data.clone(),
        state: p.state.clone(),
        cache: p.cache.clone(),
    };
    validate_placements(&p, &[])?;
    for dataset in DATASETS {
        let item = Placement {
            dataset,
            target: dataset.target(&layout),
            slot: format!("{dataset:?}.candidate"),
            identity: crate::domain::FileId {
                device: 1,
                inode: 2,
            },
            sha256: "a".repeat(64),
            size: 0,
        };
        validate_placements(&p, std::slice::from_ref(&item))?;
        assert!(validate_placements(&p, &[item.clone(), item.clone()]).is_err());
        for field in 0..8 {
            let mut wrong = item.clone();
            match field {
                0 => wrong.target = "/arbitrary".into(),
                1 => wrong.slot = "../escape".into(),
                2 => wrong.identity.device = 0,
                3 => wrong.identity.inode = 0,
                4 => wrong.sha256 = "x".into(),
                5 => wrong.sha256 = "g".repeat(64),
                6 => wrong.sha256 = "1".repeat(63) + "A",
                _ => wrong.size = 16 * 1024 * 1024 + 1,
            }
            assert!(validate_placements(&p, &[wrong]).is_err());
        }
    }
    Ok(())
}
