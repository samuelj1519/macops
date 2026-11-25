#![cfg(test)]
use super::*;
use crate::{
    domain::{
        CapabilityKind, CapabilityQueryKind, CommandSpec, ConfigAnchor, FileId, Journal, Kind,
        LocalCapabilityQuery, Observation, Package, Plan, Provider, QuerySpec, fingerprint,
    },
    test_support::{TestResult, journal, package},
};
use std::{os::unix::ffi::OsStringExt, path::PathBuf};

fn fid(inode: u64) -> FileId {
    FileId { device: 1, inode }
}

fn anchor(path: &str, inode: u64) -> ConfigAnchor {
    let p = Path::new(path);
    ConfigAnchor {
        requested: p.to_path_buf(),
        canonical_parent: p.parent().unwrap_or(Path::new("/")).to_path_buf(),
        parent_identity: fid(inode),
        name: p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .into(),
    }
}

fn skill_pkg() -> Result<Package> {
    let lock = PathBuf::from("/fixture/home/.agents/.skill-lock.json");
    let id = skill_capability_id(&lock, "demo", "github", "owner/repo")?;
    Ok(Package {
        id,
        name: "demo".into(),
        kind: Kind::Skill,
        official_url: "https://github.com/vercel-labs/skills".into(),
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: "demo".into(),
            manager: "skills-cli:github".into(),
            record: lock,
            payload: "/fixture/home/.agents/skills/demo".into(),
            source: Some("owner/repo".into()),
            profile_id: None,
            settings: None,
        },
        paths: vec![],
        exact_version_required: false,
    })
}

fn plugin_pkg() -> Package {
    let prof = "agent:1111";
    let id = plugin_capability_id(prof, "review@official");
    Package {
        id,
        name: "review@official".into(),
        kind: Kind::Plugin,
        official_url: "https://code.claude.com/docs/en/plugins/cli-reference".into(),
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::ClaudePlugin,
            native_id: "review@official".into(),
            manager: "claude-plugin".into(),
            record: "/fixture/home/.claude-mi/plugins/installed_plugins.json".into(),
            payload: "/fixture/home/.claude-mi/plugins/cache/official/review/1.0.0".into(),
            source: None,
            profile_id: Some(prof.into()),
            settings: Some("/fixture/home/.claude-mi/settings.json".into()),
        },
        paths: vec![],
        exact_version_required: false,
    }
}

fn skill_journal() -> Result<Journal> {
    let mut j = journal()?;
    let pkg = skill_pkg()?;
    let stamp = build_stamp(StampInput {
        id: &pkg.id,
        record_digest: "a".repeat(64),
        payload_digest: "b".repeat(64),
        payload_identity: fid(9),
        file_count: 2,
        revision: Some("rev1".into()),
        configured_enabled: None,
        lineage: &[fid(1), fid(2)],
    })?;
    j.plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: 60,
        recipe_sha256: fingerprint(&pkg)?,
        package: pkg,
        action: Action::Adopt,
        before: Observation::CapabilityPack {
            capability: CapabilityKind::SkillsCli,
            pack: Some(stamp),
        },
        target: None,
        command: None,
        query: Some(QuerySpec::LocalCapability(LocalCapabilityQuery {
            kind: CapabilityQueryKind::LocalV1,
            capability: CapabilityKind::SkillsCli,
            native_id: "demo".into(),
            manager: "skills-cli:github".into(),
            source: Some("owner/repo".into()),
            record: anchor("/fixture/home/.agents/.skill-lock.json", 3),
            payload: anchor("/fixture/home/.agents/skills/demo", 4),
            settings: None,
        })),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    j.plan_sha256 = fingerprint(&j.plan)?;
    Ok(j)
}

#[test]
fn capability_ids_and_package_validation_enforce_skill_and_plugin_boundaries() -> TestResult {
    assert!(valid_plugin_id("review@official"));
    assert!(!valid_plugin_id("no-market"));
    assert!(!valid_plugin_id("../bad@official"));
    assert_eq!(
        classify_payload_entry(".DS_Store", 0, true),
        PayloadMetadataDisposition::IgnoreRegularMarker
    );
    assert_eq!(
        classify_payload_entry(".DS_Store", 2, true),
        PayloadMetadataDisposition::IgnoreRegularMarker
    );
    assert_eq!(
        classify_payload_entry(".DS_Store", 0, false),
        PayloadMetadataDisposition::RejectReservedEntry
    );
    assert_eq!(
        classify_payload_entry(".in_use", 0, true),
        PayloadMetadataDisposition::IgnoreRegularMarker
    );
    assert_eq!(
        classify_payload_entry(".in_use", 1, true),
        PayloadMetadataDisposition::RejectReservedEntry
    );
    assert_eq!(
        classify_payload_entry(".in_use", 0, false),
        PayloadMetadataDisposition::RejectReservedEntry
    );
    assert_eq!(
        classify_payload_entry("SKILL.md", 0, true),
        PayloadMetadataDisposition::Include
    );
    assert!(skill_capability_id(Path::new("relative"), "demo", "github", "o/r").is_err());
    let non_utf8 = PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/\xff".to_vec()));
    assert!(skill_capability_id(&non_utf8, "demo", "github", "o/r").is_err());

    let s = skill_pkg()?;
    let p = plugin_pkg();
    validate_package(&s)?;
    validate_package(&p)?;
    validate_package(&package())?;

    let mut bad_payload = s.clone();
    if let Provider::CapabilityObserved { payload, .. } = &mut bad_payload.provider {
        *payload = "relative".into();
    }
    assert!(validate_package(&bad_payload).is_err());

    let mut exact = s.clone();
    exact.exact_version_required = true;
    assert!(validate_package(&exact).is_err());

    for idx in 0..5 {
        let mut bad = s.clone();
        match idx {
            0 => bad.kind = Kind::Plugin,
            1 => bad.id = "capability:wrong".into(),
            2 => {
                if let Provider::CapabilityObserved { manager, .. } = &mut bad.provider {
                    *manager = "other".into();
                }
            }
            3 => {
                if let Provider::CapabilityObserved { source, .. } = &mut bad.provider {
                    *source = None;
                }
            }
            _ => {
                if let Provider::CapabilityObserved { record, .. } = &mut bad.provider {
                    *record = "relative".into();
                }
            }
        }
        assert!(validate_package(&bad).is_err(), "skill {idx}");
    }
    for idx in 0..5 {
        let mut bad = p.clone();
        match idx {
            0 => bad.kind = Kind::Skill,
            1 => bad.id = "capability:wrong".into(),
            2 => {
                if let Provider::CapabilityObserved { record, .. } = &mut bad.provider {
                    *record = "relative".into();
                }
            }
            3 => {
                if let Provider::CapabilityObserved { profile_id, .. } = &mut bad.provider {
                    *profile_id = None;
                }
            }
            _ => {
                if let Provider::CapabilityObserved { settings, .. } = &mut bad.provider {
                    *settings = Some("relative".into());
                }
            }
        }
        assert!(validate_package(&bad).is_err(), "plugin {idx}");
    }
    Ok(())
}

#[test]
fn query_and_journal_bindings_reject_tampering_even_with_recomputed_hashes() -> TestResult {
    let s = skill_pkg()?;
    let p = plugin_pkg();
    let valid = skill_journal()?;
    crate::policy::state::validate_journal(&valid)?;
    let cmd = QuerySpec::Command(CommandSpec {
        program: "/bin/echo".into(),
        args: vec![],
    });
    assert!(binding(&package(), &cmd).is_ok());
    assert!(binding(&s, &cmd).is_err());
    if let Some(q) = &valid.plan.query {
        assert!(binding(&package(), q).is_err());
        let mut wrong_pkg = s.clone();
        wrong_pkg.id = "capability:bad".into();
        assert!(binding(&wrong_pkg, q).is_err());
        for idx in 0..4 {
            let mut bad_q = q.clone();
            if let QuerySpec::LocalCapability(inner) = &mut bad_q {
                match idx {
                    0 => inner.record.name = String::new(),
                    1 => inner.payload.name = String::new(),
                    2 => inner.native_id = "other".into(),
                    _ => inner.settings = Some(anchor("/fixture/home/.claude-mi/settings.json", 5)),
                }
            }
            assert!(binding(&s, &bad_q).is_err());
        }
    }
    let plug_q = QuerySpec::LocalCapability(LocalCapabilityQuery {
        kind: CapabilityQueryKind::LocalV1,
        capability: CapabilityKind::ClaudePlugin,
        native_id: "review@official".into(),
        manager: "claude-plugin".into(),
        source: None,
        record: anchor("/fixture/home/.claude-mi/plugins/installed_plugins.json", 3),
        payload: anchor(
            "/fixture/home/.claude-mi/plugins/cache/official/review/1.0.0",
            4,
        ),
        settings: Some(anchor("/fixture/home/.claude-mi/settings.json", 5)),
    });
    binding(&p, &plug_q)?;
    let mut bad_plug_q = plug_q.clone();
    if let QuerySpec::LocalCapability(inner) = &mut bad_plug_q
        && let Some(cfg) = &mut inner.settings
    {
        cfg.name = String::new();
    }
    assert!(binding(&p, &bad_plug_q).is_err());
    let mut mismatch_plug_q = plug_q;
    if let QuerySpec::LocalCapability(inner) = &mut mismatch_plug_q
        && let Some(cfg) = &mut inner.settings
    {
        cfg.requested = "/fixture/home/.claude-glm/settings.json".into();
    }
    assert!(binding(&p, &mismatch_plug_q).is_err());
    Ok(())
}

#[test]
fn plan_and_adoptable_observation_reject_missing_and_mutating_forms() -> TestResult {
    let valid = skill_journal()?;
    for idx in 0..8 {
        let mut j = valid.clone();
        match idx {
            0 => j.plan.action = Action::Uninstall,
            1 => {
                j.plan.command = Some(CommandSpec {
                    program: "/bin/rm".into(),
                    args: vec![],
                });
            }
            2 => j.plan.target = Some("1.0.0".into()),
            3 => j.plan.delegated = true,
            4 => j.plan.synchronous = false,
            5 => j.plan.query = None,
            6 => j.plan.package = package(),
            _ => {
                j.plan.before = Observation::CapabilityPack {
                    capability: CapabilityKind::SkillsCli,
                    pack: None,
                };
            }
        }
        j.plan.recipe_sha256 = fingerprint(&j.plan.package)?;
        j.plan_sha256 = fingerprint(&j.plan)?;
        assert!(crate::policy::state::validate_journal(&j).is_err(), "{idx}");
    }
    Ok(())
}

#[test]
fn adoptable_observation_rejects_absent_and_malformed_capability_stamps() -> TestResult {
    let s = skill_pkg()?;
    assert!(adoptable_observation(&package(), &Observation::Missing { evidence: None }).is_err());
    assert!(adoptable_observation(&s, &Observation::Missing { evidence: None }).is_err());
    assert!(
        adoptable_observation(
            &s,
            &Observation::CapabilityPack {
                capability: CapabilityKind::ClaudePlugin,
                pack: None,
            }
        )
        .is_err()
    );
    assert_eq!(
        adoptable_observation(
            &s,
            &Observation::CapabilityPack {
                capability: CapabilityKind::SkillsCli,
                pack: None,
            }
        )
        .err()
        .ok_or("missing")?
        .code,
        "not_installed"
    );
    let mut bad_stamp = build_stamp(StampInput {
        id: &s.id,
        record_digest: "a".repeat(64),
        payload_digest: "b".repeat(64),
        payload_identity: fid(1),
        file_count: 1,
        revision: None,
        configured_enabled: Some(true),
        lineage: &[],
    })?;
    assert!(
        adoptable_observation(
            &s,
            &Observation::CapabilityPack {
                capability: CapabilityKind::SkillsCli,
                pack: Some(bad_stamp.clone()),
            }
        )
        .is_err()
    );
    bad_stamp.file_count = 0;
    assert!(
        adoptable_observation(
            &s,
            &Observation::CapabilityPack {
                capability: CapabilityKind::SkillsCli,
                pack: Some(bad_stamp),
            }
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn skill_and_plugin_record_parsers_detect_drift_missing_and_duplicates() -> TestResult {
    let lock = br#"{"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github","ref":"abc","skillFolderHash":"tree1"}}}"#;
    let (d1, r1) =
        parse_skill_record("cap:1", lock, "demo", "github", "owner/repo")?.ok_or("present")?;
    assert_eq!(r1.as_deref(), Some("abc"));
    assert!(parse_skill_record("cap:1", lock, "other", "github", "owner/repo")?.is_none());
    assert_eq!(
        parse_skill_record("cap:1", lock, "demo", "github", "changed/repo")
            .err()
            .ok_or("source changed")?
            .code,
        "capability_source_changed"
    );
    assert!(parse_skill_record("cap:1", b"{bad", "demo", "github", "owner/repo").is_err());

    let lock2 = br#"{"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github","ref":"abc","skillFolderHash":"tree2"}}}"#;
    let (d2, _) =
        parse_skill_record("cap:1", lock2, "demo", "github", "owner/repo")?.ok_or("present")?;
    assert_ne!(d1, d2);

    let book = br#"{"version":2,"plugins":{"review@official":[{"scope":"user","installPath":"/p/1.0.0","version":"1.0.0"}]}}"#;
    let settings = br#"{"enabledPlugins":{"review@official":false},"env":{"ANTHROPIC_AUTH_TOKEN":"SYNTHETIC_SECRET"}}"#;
    let parsed_p = parse_plugin_record(
        "cap:p",
        book,
        "review@official",
        Path::new("/p/1.0.0"),
        Some(settings),
    )?
    .ok_or("plugin")?;
    assert_eq!(parsed_p.revision.as_deref(), Some("1.0.0"));
    assert_eq!(parsed_p.configured_enabled, Some(false));
    assert!(!parsed_p.digest.contains("SYNTHETIC_SECRET"));
    let parsed_none = parse_plugin_record(
        "cap:p",
        book,
        "review@official",
        Path::new("/p/1.0.0"),
        None,
    )?
    .ok_or("plugin no settings")?;
    assert_eq!(parsed_none.configured_enabled, None);
    assert!(
        parse_plugin_record("cap:p", book, "absent@official", Path::new("/p"), None)?.is_none()
    );
    assert_eq!(
        parse_plugin_record("cap:p", book, "review@official", Path::new("/other"), None)
            .err()
            .ok_or("rebound")?
            .code,
        "capability_payload_rebound"
    );
    assert!(
        parse_plugin_record(
            "cap:p",
            book,
            "review@official",
            Path::new("/p/1.0.0"),
            Some(b"{")
        )
        .is_err()
    );
    assert!(parse_plugin_record("cap:p", b"{", "review@official", Path::new("/p"), None).is_err());
    let dup = br#"{"version":2,"plugins":{"review@official":[{"scope":"user","installPath":"/p/1"},{"scope":"user","installPath":"/p/2"}]}}"#;
    assert_eq!(
        parse_plugin_record("cap:p", dup, "review@official", Path::new("/p/1"), None)
            .err()
            .ok_or("dup")?
            .code,
        "duplicate_plugin_scope"
    );
    let tree_a = payload_tree_digest("cap:1", &[("SKILL.md".into(), false, "h1".into())])?;
    let tree_b = payload_tree_digest("cap:1", &[("SKILL.md".into(), true, "h1".into())])?;
    assert_ne!(tree_a, tree_b);
    Ok(())
}

#[test]
fn native_recipe_delegation_is_closed_to_one_skill_owner_and_supported_action_shape() -> TestResult
{
    let mut j = skill_journal()?;
    j.plan.delegated = true;
    j.plan.warnings = vec![format!("native-skills-v1:{}", "a".repeat(64))];
    j.plan.query = None;
    j.plan.target = None;
    j.plan.action = Action::Upgrade;
    j.plan.command = Some(CommandSpec {
        program: "/fixture/node".into(),
        args: vec!["/fixture/entry".into()],
    });
    validate_plan(&j.plan)?;
    for variant in 0..6 {
        let mut changed = j.plan.clone();
        match variant {
            0 => changed.package = plugin_pkg(),
            1 => changed.query = skill_journal()?.plan.query,
            2 => changed.target = Some("1.0.0".into()),
            3 => changed.synchronous = false,
            4 => changed.action = Action::Start,
            _ => changed.command = None,
        }
        assert!(validate_plan(&changed).is_err());
    }
    j.plan.action = Action::Adopt;
    j.plan.command = None;
    validate_plan(&j.plan)?;
    let mut package = skill_pkg()?;
    package.id = "capability:invalid".into();
    assert!(validate_package(&package).is_err());
    let Provider::CapabilityObserved { payload, .. } = &mut package.provider else {
        return Err("provider".into());
    };
    *payload = std::ffi::OsString::from_vec(vec![b'/', 255]).into();
    assert!(validate_package(&package).is_err());
    Ok(())
}

#[test]
fn github_record_source_case_is_semantic_but_local_path_case_and_other_repositories_remain_exact()
-> TestResult {
    let raw = serde_json::to_vec(
        &serde_json::json!({"version":3,"skills":{"demo":{"source":"antvis/GPT-Vis","sourceType":"github","skillFolderHash":"a".repeat(40)}}}),
    )?;
    assert!(
        parse_skill_record(
            "capability:fixture",
            &raw,
            "demo",
            "github",
            "antvis/gpt-vis"
        )?
        .is_some()
    );
    assert!(
        parse_skill_record("capability:fixture", &raw, "demo", "github", "other/repo").is_err()
    );
    assert!(
        parse_skill_record(
            "capability:fixture",
            &raw,
            "demo",
            "local",
            "antvis/GPT-Vis"
        )
        .is_err()
    );
    let local = serde_json::to_vec(
        &serde_json::json!({"version":3,"skills":{"demo":{"source":"/Artifact/Path","sourceType":"local","skillFolderHash":"a".repeat(40)}}}),
    )?;
    assert!(
        parse_skill_record(
            "capability:fixture",
            &local,
            "demo",
            "local",
            "/artifact/path"
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn github_source_matching_does_not_accept_an_invalid_repository_or_another_source_type()
-> TestResult {
    let local = serde_json::to_vec(
        &serde_json::json!({"version":3,"skills":{"demo":{"source":"/local/path","sourceType":"local","skillFolderHash":"a".repeat(40)}}}),
    )?;
    assert!(
        parse_skill_record("capability:fixture", &local, "demo", "github", "owner/repo").is_err()
    );
    let invalid = serde_json::to_vec(
        &serde_json::json!({"version":3,"skills":{"demo":{"source":"../invalid","sourceType":"github","skillFolderHash":"a".repeat(40)}}}),
    )?;
    assert!(
        parse_skill_record(
            "capability:fixture",
            &invalid,
            "demo",
            "github",
            "owner/repo"
        )
        .is_err()
    );
    let valid = serde_json::to_vec(
        &serde_json::json!({"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github","skillFolderHash":"a".repeat(40)}}}),
    )?;
    assert!(
        parse_skill_record("capability:fixture", &valid, "demo", "github", "../invalid").is_err()
    );
    Ok(())
}

#[test]
fn delegated_native_recipe_cannot_bypass_the_package_identity_validator() -> TestResult {
    let mut j = skill_journal()?;
    j.plan.delegated = true;
    j.plan.warnings = vec![format!("native-skills-v1:{}", "a".repeat(64))];
    j.plan.query = None;
    j.plan.target = None;
    j.plan.command = Some(CommandSpec {
        program: "/fixture/node".into(),
        args: vec![],
    });
    j.plan.action = Action::Upgrade;
    j.plan.package.id = "capability:invalid".into();
    assert!(validate_plan(&j.plan).is_err());
    Ok(())
}

#[test]
fn codex_capability_receipts_bind_profile_cache_and_selected_registration() -> TestResult {
    let root = Path::new("/fixture/codex");
    let record = root.join("config.toml");
    let payload = root.join("plugins/cache/market/fixture/1.0.0");
    let profile = crate::domain::resource::id("agent", &("codex", root))?;
    let id = codex_capability_id(&profile, "fixture@market")?;
    let mut pkg = plugin_pkg();
    pkg.id = id.clone();
    pkg.provider = Provider::CapabilityObserved {
        capability: CapabilityKind::CodexPlugin,
        native_id: "fixture@market".into(),
        manager: "codex-config".into(),
        record: record.clone(),
        payload: payload.clone(),
        source: None,
        profile_id: Some(profile),
        settings: None,
    };
    validate_package(&pkg)?;
    let parsed = |raw| parse_codex_record(&id, &record, &payload, "fixture@market", raw);
    let enabled = b"[plugins.'fixture@market']\nenabled = false\n";
    let status = parsed(enabled)?.ok_or("configured plugin")?;
    assert_eq!(status.revision.as_deref(), Some("1.0.0"));
    assert_eq!(status.configured_enabled, Some(false));
    assert!(parsed(b"")?.is_none());
    assert!(parsed(b"[[broken").is_err());
    assert!(parsed(b"[plugins.'fixture@market']\nenabled = 'x'\n").is_err());
    let different = parsed(
        b"[marketplaces.market]\nsource = 'fixture'\n[plugins.'fixture@market']\nenabled = false\n",
    )?
    .ok_or("with market")?;
    assert_ne!(status.digest, different.digest);
    assert_eq!(
        status.digest,
        parsed(b"model = 'example'\n[plugins.'fixture@market']\nenabled = false\n")?
            .ok_or("with model")?
            .digest
    );

    for variant in 0..10 {
        let mut invalid = pkg.clone();
        let Provider::CapabilityObserved {
            manager,
            source,
            settings,
            profile_id,
            record,
            payload,
            native_id,
            ..
        } = &mut invalid.provider
        else {
            return Err("fixture provider".into());
        };
        match variant {
            0 => invalid.kind = Kind::Skill,
            1 => manager.clear(),
            2 => *source = Some("example/fixture".into()),
            3 => *settings = Some("/other/settings".into()),
            4 => *profile_id = Some("agent:other".into()),
            5 => invalid.id = "capability:other".into(),
            6 => *payload = "/fixture/codex/plugins/cache/other/fixture/1.0.0".into(),
            7 => *record = "/".into(),
            8 => *native_id = "invalid".into(),
            _ => *record = "/relative/../config.toml".into(),
        }
        assert!(validate_package(&invalid).is_err(), "{invalid:?}");
    }
    for (record, payload, native_id) in [
        (root.join("wrong.toml"), payload.clone(), "fixture@market"),
        (record.clone(), PathBuf::from("/"), "fixture@market"),
        (PathBuf::from("/"), payload.clone(), "fixture@market"),
        (
            root.join(std::ffi::OsString::from_vec(vec![0xff])),
            payload.clone(),
            "fixture@market",
        ),
        (record.clone(), payload.clone(), "..@market"),
        (record.clone(), payload.clone(), "invalid"),
        (
            record.clone(),
            payload.with_file_name("invalid version"),
            "fixture@market",
        ),
        (
            record.clone(),
            payload.with_file_name(std::ffi::OsString::from_vec(vec![0xff])),
            "fixture@market",
        ),
    ] {
        assert!(parse_codex_record(&id, &record, &payload, native_id, enabled).is_err());
    }
    Ok(())
}
