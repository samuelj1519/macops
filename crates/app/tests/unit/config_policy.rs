#![cfg(test)]
use super::*;
use crate::{
    domain::{
        CommandSpec, ConfigAnchor, ConfigAuthority, ConfigFileStamp, ConfigModePolicy,
        ConfigQueryKind, FileId, Journal, Kind, LocalConfigQuery, Observation, Package, Plan,
        Provider, QuerySpec, fingerprint,
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

fn stamp(
    id: &str,
    parent: &str,
    name: &str,
    raw: &str,
    inode: u64,
    mode: u32,
) -> Result<ConfigFileStamp> {
    let file_id = fid(inode);
    Ok(ConfigFileStamp {
        content_digest: content_digest(id, raw)?,
        fingerprint: stamp_fingerprint(Path::new(parent), name, raw, &file_id, mode, &[fid(1)])?,
        identity: file_id,
        mode,
    })
}

fn static_pkg() -> Result<Package> {
    let target = PathBuf::from("/fixture/home/.netrc");
    Ok(Package {
        id: config_id(&target)?,
        name: target.display().to_string(),
        kind: Kind::Config,
        official_url: "https://www.chezmoi.io/reference/target-types/".into(),
        provider: Provider::ConfigObserved {
            target,
            source: Some("/fixture/source/private_dot_netrc".into()),
            authority: ConfigAuthority::ChezmoiCandidate,
        },
        paths: vec![],
        exact_version_required: false,
    })
}

fn untracked_pkg() -> Result<Package> {
    let target = PathBuf::from("/fixture/home/.mytoolrc");
    Ok(Package {
        id: config_id(&target)?,
        name: target.display().to_string(),
        kind: Kind::Config,
        official_url: String::new(),
        provider: Provider::ConfigObserved {
            target,
            source: None,
            authority: ConfigAuthority::Untracked,
        },
        paths: vec![],
        exact_version_required: false,
    })
}

fn config_journal() -> Result<Journal> {
    let mut j = journal()?;
    let pkg = static_pkg()?;
    let s = stamp(
        &pkg.id,
        "/fixture/source",
        "private_dot_netrc",
        "raw-1",
        11,
        0o644,
    )?;
    let t = stamp(&pkg.id, "/fixture/home", ".netrc", "raw-1", 12, 0o600)?;
    j.plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: 60,
        recipe_sha256: fingerprint(&pkg)?,
        package: pkg,
        action: Action::Adopt,
        before: Observation::ConfigFile {
            authority: ConfigAuthority::ChezmoiCandidate,
            target: Some(t),
            source: Some(s),
            mode_matches_source: Some(true),
        },
        target: None,
        command: None,
        query: Some(QuerySpec::LocalConfig(LocalConfigQuery {
            kind: ConfigQueryKind::LocalV1,
            authority: ConfigAuthority::ChezmoiCandidate,
            target: anchor("/fixture/home/.netrc", 2),
            source: Some(anchor("/fixture/source/private_dot_netrc", 3)),
            expected_mode: Some(ConfigModePolicy {
                private: true,
                readonly: false,
                executable: false,
            }),
        })),
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    j.plan_sha256 = fingerprint(&j.plan)?;
    Ok(j)
}

#[test]
fn source_policies_and_mode_contracts_match_finite_chezmoi_subset() -> TestResult {
    assert_eq!(
        source_file_policy("dot_zshrc")?,
        ConfigModePolicy {
            private: false,
            readonly: false,
            executable: false,
        }
    );
    assert_eq!(
        source_file_policy("private_readonly_executable_dot_tool")?,
        ConfigModePolicy {
            private: true,
            readonly: true,
            executable: true,
        }
    );
    for rejected in [
        ".chezmoiignore",
        "dot_zshrc.tmpl",
        "symlink_dot_zshrc",
        "encrypted_dot_secret.age",
        "readonly_private_dot_x",
    ] {
        assert!(source_file_policy(rejected).is_err(), "{rejected}");
    }
    let ordinary = ConfigModePolicy {
        private: false,
        readonly: false,
        executable: false,
    };
    assert!(mode_satisfies(0o644, ordinary));
    assert!(mode_satisfies(0o600, ordinary));
    assert!(!mode_satisfies(0o755, ordinary));
    assert!(!mode_satisfies(0o444, ordinary));
    let private_ro_exec = ConfigModePolicy {
        private: true,
        readonly: true,
        executable: true,
    };
    assert!(mode_satisfies(0o500, private_ro_exec));
    assert!(!mode_satisfies(0o550, private_ro_exec));
    assert!(!mode_satisfies(0o700, private_ro_exec));
    assert!(!mode_satisfies(0o400, private_ro_exec));
    Ok(())
}

#[test]
fn identity_digests_and_anchors_fail_closed_on_invalid_inputs() -> TestResult {
    let a = config_id(Path::new("/fixture/home/.zshrc"))?;
    let b = config_id(Path::new("/fixture/home/.bashrc"))?;
    assert_ne!(a, b);
    assert!(config_id(Path::new("relative")).is_err());
    let non_utf8 = PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/\xff".to_vec()));
    assert!(config_id(&non_utf8).is_err());
    assert!(stamp_fingerprint(&non_utf8, "a", "raw", &fid(1), 0o600, &[]).is_err());
    assert!(valid_digest(&"a".repeat(64)));
    assert!(!valid_digest("short"));
    assert!(!valid_digest(&"z".repeat(64)));
    let good = anchor("/fixture/home/.zshrc", 2);
    validate_anchor(&good)?;
    for mode in 0..4 {
        let mut bad = good.clone();
        match mode {
            0 => bad.requested = "relative".into(),
            1 => bad.canonical_parent = "relative".into(),
            2 => bad.name = String::new(),
            _ => bad.name = "other".into(),
        }
        assert!(validate_anchor(&bad).is_err());
    }
    Ok(())
}

#[test]
fn package_validation_rejects_mismatched_kinds_authorities_and_templates() -> TestResult {
    let pkg = static_pkg()?;
    validate_package(&pkg)?;
    validate_package(&untracked_pkg()?)?;
    validate_package(&package())?;
    let mut mismatch = package();
    mismatch.kind = Kind::Config;
    assert!(validate_package(&mismatch).is_err());
    let mut mismatch = pkg.clone();
    mismatch.kind = Kind::Cli;
    assert!(validate_package(&mismatch).is_err());
    let mut wrong_id = pkg.clone();
    wrong_id.id = "config:wrong".into();
    assert!(validate_package(&wrong_id).is_err());
    let mut exact = pkg.clone();
    exact.exact_version_required = true;
    assert!(validate_package(&exact).is_err());
    let mut root_target = pkg.clone();
    root_target.id = config_id(Path::new("/"))?;
    root_target.provider = Provider::ConfigObserved {
        target: "/".into(),
        source: None,
        authority: ConfigAuthority::Untracked,
    };
    assert!(validate_package(&root_target).is_err());
    for src in [
        Some(PathBuf::from("relative")),
        Some(PathBuf::from("/")),
        Some(PathBuf::from("/fixture/source/dot_netrc.tmpl")),
        None,
    ] {
        let mut bad = pkg.clone();
        bad.provider = Provider::ConfigObserved {
            target: "/fixture/home/.netrc".into(),
            source: src,
            authority: ConfigAuthority::ChezmoiCandidate,
        };
        assert!(validate_package(&bad).is_err());
    }
    let mut rel_target = pkg.clone();
    rel_target.provider = Provider::ConfigObserved {
        target: "relative".into(),
        source: None,
        authority: ConfigAuthority::Untracked,
    };
    assert!(validate_package(&rel_target).is_err());
    let mut bad_combo = untracked_pkg()?;
    bad_combo.provider = Provider::ConfigObserved {
        target: "/fixture/home/.mytoolrc".into(),
        source: Some("/fixture/source/dot_mytoolrc".into()),
        authority: ConfigAuthority::Untracked,
    };
    assert!(validate_package(&bad_combo).is_err());
    Ok(())
}

#[test]
fn query_and_journal_bindings_reject_tampering_even_with_recomputed_hashes() -> TestResult {
    let pkg = static_pkg()?;
    let valid = config_journal()?;
    crate::policy::state::validate_journal(&valid)?;
    let cmd_query = QuerySpec::Command(CommandSpec {
        program: "/bin/echo".into(),
        args: vec![],
    });
    assert!(binding(&package(), &cmd_query).is_ok());
    assert!(binding(&pkg, &cmd_query).is_err());
    if let Some(q) = &valid.plan.query {
        assert!(binding(&package(), q).is_err());
        let mut bad_pkg = pkg.clone();
        bad_pkg.id = "config:bad".into();
        assert!(binding(&bad_pkg, q).is_err());
        let mut bad_q = q.clone();
        if let QuerySpec::LocalConfig(inner) = &mut bad_q {
            inner.target.name = String::new();
        }
        assert!(binding(&pkg, &bad_q).is_err());
        let mut bad_src_anchor = q.clone();
        if let QuerySpec::LocalConfig(inner) = &mut bad_src_anchor
            && let Some(src) = &mut inner.source
        {
            src.name = String::new();
        }
        assert!(binding(&pkg, &bad_src_anchor).is_err());
        let mut bad_src_policy = q.clone();
        if let QuerySpec::LocalConfig(inner) = &mut bad_src_policy
            && let Some(src) = &mut inner.source
        {
            src.requested = "/fixture/source/dot_netrc.tmpl".into();
            src.name = "dot_netrc.tmpl".into();
        }
        assert!(binding(&pkg, &bad_src_policy).is_err());
    }
    Ok(())
}

#[test]
fn journal_plan_validation_rejects_tampering_even_with_recomputed_hashes() -> TestResult {
    let valid = config_journal()?;
    crate::policy::state::validate_journal(&valid)?;
    let mut bad_before_j = valid.clone();
    bad_before_j.plan.before = Observation::ConfigFile {
        authority: ConfigAuthority::ChezmoiCandidate,
        target: None,
        source: None,
        mode_matches_source: None,
    };
    bad_before_j.plan_sha256 = fingerprint(&bad_before_j.plan)?;
    assert!(crate::policy::state::validate_journal(&bad_before_j).is_err());
    for tamper in 0..10 {
        let mut j = valid.clone();
        match tamper {
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
            7 => {
                if let Some(QuerySpec::LocalConfig(q)) = &mut j.plan.query {
                    q.authority = ConfigAuthority::Untracked;
                }
            }
            8 => {
                if let Some(QuerySpec::LocalConfig(q)) = &mut j.plan.query {
                    q.expected_mode = Some(ConfigModePolicy {
                        private: false,
                        readonly: false,
                        executable: false,
                    });
                }
            }
            _ => {
                if let Some(QuerySpec::LocalConfig(q)) = &mut j.plan.query {
                    q.source = None;
                }
            }
        }
        j.plan.recipe_sha256 = fingerprint(&j.plan.package)?;
        j.plan_sha256 = fingerprint(&j.plan)?;
        assert!(
            crate::policy::state::validate_journal(&j).is_err(),
            "{tamper}"
        );
    }
    let mut untracked_j = valid;
    let u_pkg = untracked_pkg()?;
    let u_stamp = stamp(&u_pkg.id, "/fixture/home", ".mytoolrc", "raw", 20, 0o644)?;
    untracked_j.plan.package = u_pkg;
    untracked_j.plan.before = Observation::ConfigFile {
        authority: ConfigAuthority::Untracked,
        target: Some(u_stamp),
        source: None,
        mode_matches_source: None,
    };
    untracked_j.plan.query = Some(QuerySpec::LocalConfig(LocalConfigQuery {
        kind: ConfigQueryKind::LocalV1,
        authority: ConfigAuthority::Untracked,
        target: anchor("/fixture/home/.mytoolrc", 2),
        source: None,
        expected_mode: None,
    }));
    untracked_j.plan.recipe_sha256 = fingerprint(&untracked_j.plan.package)?;
    untracked_j.plan_sha256 = fingerprint(&untracked_j.plan)?;
    crate::policy::state::validate_journal(&untracked_j)?;
    Ok(())
}

#[test]
fn adoptable_observation_rejects_missing_drifted_and_invalid_stamps() -> TestResult {
    let pkg = static_pkg()?;
    let s = stamp(
        &pkg.id,
        "/fixture/source",
        "private_dot_netrc",
        "same",
        1,
        0o644,
    )?;
    let t = stamp(&pkg.id, "/fixture/home", ".netrc", "same", 2, 0o600)?;
    assert!(adoptable_observation(&package(), &Observation::Missing { evidence: None }).is_err());
    assert!(adoptable_observation(&pkg, &Observation::Missing { evidence: None }).is_err());
    assert!(
        adoptable_observation(
            &pkg,
            &Observation::ConfigFile {
                authority: ConfigAuthority::Untracked,
                target: Some(t.clone()),
                source: None,
                mode_matches_source: None,
            }
        )
        .is_err()
    );
    let mut bad_stamp = t.clone();
    bad_stamp.mode = 0o1_0000;
    let drifted = stamp(&pkg.id, "/fixture/home", ".netrc", "different", 2, 0o600)?;
    let obs_case = |tgt, src, mode_ok| Observation::ConfigFile {
        authority: ConfigAuthority::ChezmoiCandidate,
        target: tgt,
        source: src,
        mode_matches_source: mode_ok,
    };
    for (obs, expected_code) in [
        (obs_case(None, Some(s.clone()), None), Some("not_installed")),
        (obs_case(Some(bad_stamp), Some(s.clone()), Some(true)), None),
        (
            obs_case(Some(t.clone()), None, Some(true)),
            Some("config_source_missing"),
        ),
        (obs_case(Some(t.clone()), Some(s.clone()), None), None),
        (
            obs_case(Some(drifted), Some(s.clone()), Some(true)),
            Some("configuration_drift"),
        ),
        (
            obs_case(Some(t.clone()), Some(s.clone()), Some(false)),
            Some("configuration_drift"),
        ),
    ] {
        let err = adoptable_observation(&pkg, &obs).err().ok_or("err")?;
        if let Some(code) = expected_code {
            assert_eq!(err.code, code);
        }
    }
    assert!(
        adoptable_observation(
            &untracked_pkg()?,
            &Observation::ConfigFile {
                authority: ConfigAuthority::Untracked,
                target: Some(t),
                source: Some(s),
                mode_matches_source: None,
            }
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn query_spec_wire_encoding_preserves_legacy_commands_and_rejects_malformed_shapes() -> TestResult {
    let old_cmd = r#"{"program":"/bin/echo","args":["ok"]}"#;
    let parsed: QuerySpec = serde_json::from_str(old_cmd)?;
    assert_eq!(serde_json::to_string(&parsed)?, old_cmd);
    assert!(serde_json::from_str::<QuerySpec>(r#"["not","an","object"]"#).is_err());
    assert!(
        serde_json::from_str::<QuerySpec>(
            r#"{"kind":"config-local-v1","authority":"untracked","target":[],"extra":1}"#
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn authority_rules_reject_ambiguous_or_incomplete_scans() {
    assert!(!non_generating_control_issue("physical_candidates_only"));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:.chezmoiignore"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiignore.tmpl"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiremove"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiremove.txt"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiroot"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiversion"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoitemplates"
    ));
    assert!(non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoidata.toml"
    ));
    assert!(!non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiexternal.toml"
    ));
    assert!(!non_generating_control_issue(
        "source_control_not_evaluated:/s/.chezmoiexternals"
    ));
    assert!(authority_allows_untracked(&CoverageState::Complete, &[]));
    assert!(authority_allows_untracked(&CoverageState::Absent, &[]));
    assert!(authority_allows_untracked(
        &CoverageState::Unsupported,
        &[
            "physical_candidates_only".into(),
            "source_template_not_evaluated:/s/a.tmpl".into(),
            "source_link_expression_not_read:/s/symlink_b".into(),
            "source_control_not_evaluated:/s/.chezmoiignore".into(),
            "source_control_not_evaluated:.chezmoiignore.tmpl".into(),
            "source_control_not_evaluated:/s/.chezmoiremove".into(),
            "source_control_not_evaluated:/s/.chezmoiremove.tmpl".into(),
            "source_control_not_evaluated:/s/.chezmoiroot".into(),
            "source_control_not_evaluated:/s/.chezmoiversion".into(),
            "source_control_not_evaluated:/s/.chezmoitemplates".into(),
            "source_control_not_evaluated:/s/.chezmoidata.toml".into(),
        ]
    ));
    assert!(!non_generating_control_issue("other:issue"));
    for external in [
        "source_control_not_evaluated:/s/.chezmoiexternal.toml",
        "source_control_not_evaluated:/s/.chezmoiexternal.yaml.tmpl",
        "source_control_not_evaluated:/s/.chezmoiexternals",
        "source_control_not_evaluated:/s/.unknown_control",
    ] {
        assert!(!authority_allows_untracked(
            &CoverageState::Unsupported,
            &[external.into()]
        ));
    }
    assert!(!authority_allows_untracked(
        &CoverageState::Unsupported,
        &[
            "physical_candidates_only".into(),
            "source_control_not_evaluated:/s/.chezmoiexternal.toml".into(),
        ]
    ));
    assert!(!authority_allows_untracked(
        &CoverageState::Unsupported,
        &["unsupported_source_attribute:/s/modify_x".into()]
    ));
    for state in [
        CoverageState::Unavailable,
        CoverageState::Invalid,
        CoverageState::Truncated,
    ] {
        assert!(!authority_allows_untracked(&state, &[]));
    }
}
