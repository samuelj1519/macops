#![cfg(test)]
use super::*;
use crate::domain::fingerprint;
use crate::{
    domain::resource::{Data, Management, Resource, State},
    test_support::TestResult,
};

fn target() -> NpmTarget {
    NpmTarget {
        npm: "/fixture/bin/npm".into(),
        prefix: "/fixture/npm".into(),
        name: "@scope/demo".into(),
    }
}
fn observer() -> Result<Package> {
    use crate::domain::CommandSpec;
    let target = target();
    Ok(Package {
        id: npm_id(&target.prefix, &target.name)?,
        name: target.name.clone(),
        kind: Kind::Cli,
        official_url: "https://docs.npmjs.com/cli/v11/commands/npm-ls/".into(),
        provider: Provider::Command {
            query: CommandSpec {
                program: "/fixture/bin/macops".into(),
                args: vec![
                    "__npm_observe".into(),
                    "--npm".into(),
                    target.npm.display().to_string(),
                    "--prefix".into(),
                    target.prefix.display().to_string(),
                    "--name".into(),
                    target.name,
                ],
            },
            available: None,
            install: None,
            upgrade: None,
            uninstall: None,
            synchronous: true,
        },
        paths: vec![],
        exact_version_required: false,
    })
}

#[test]
fn observer_recipe_binds_identity_and_never_declares_software_writes() -> TestResult {
    let p = observer()?;
    assert_eq!(descriptor(&p)?, Some(target()));
    assert_ne!(npm_id(Path::new("/other"), "@scope/demo")?, p.id);
    assert_ne!(npm_id(&target().prefix, "demo")?, p.id);
    assert!(descriptor(&crate::test_support::package())?.is_none());
    let mut plain = p.clone();
    plain.provider = Provider::Npm {
        package: "demo".into(),
        prefix: "/fixture/npm".into(),
        authority: None,
    };
    assert!(descriptor(&plain)?.is_none());
    for field in ["available", "install", "upgrade", "uninstall"] {
        let mut value = serde_json::to_value(&p)?;
        value["provider"][field] = serde_json::to_value(crate::test_support::command("write"))?;
        assert!(descriptor(&serde_json::from_value(value)?).is_err());
    }
    let mut bad = p.clone();
    bad.kind = Kind::App;
    assert!(descriptor(&bad).is_err());
    for args in [
        vec!["__npm_observe".into()],
        vec![
            "__npm_observe",
            "--bad",
            "/npm",
            "--prefix",
            "/prefix",
            "--name",
            "demo",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "__npm_observe",
            "--npm",
            "/npm",
            "--bad",
            "/prefix",
            "--name",
            "demo",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "__npm_observe",
            "--npm",
            "/npm",
            "--prefix",
            "/prefix",
            "--bad",
            "demo",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    ] {
        let mut bad = p.clone();
        if let Provider::Command { query, .. } = &mut bad.provider {
            query.args = args;
        }
        assert!(descriptor(&bad).is_err());
    }
    for (index, value) in [(2, "relative"), (4, "relative"), (6, "--bad")] {
        let mut bad = p.clone();
        if let Provider::Command { query, .. } = &mut bad.provider {
            query.args[index] = value.into();
        }
        assert!(descriptor(&bad).is_err());
    }
    let mut bad = p.clone();
    bad.id = "another-instance".into();
    assert!(descriptor(&bad).is_err());
    let mut bad = p;
    if let Provider::Command { query, .. } = &mut bad.provider {
        query.program = target().npm;
    }
    assert!(descriptor(&bad).is_err());
    Ok(())
}

#[test]
fn observer_journal_cannot_be_repurposed_into_a_mutating_worker() -> TestResult {
    let mut j = crate::test_support::journal()?;
    validate_journal(&j)?;
    j.plan.package = observer()?;
    if let Provider::Command { query, .. } = &j.plan.package.provider {
        j.plan.query = Some(query.clone().into());
    }
    j.plan.action = Action::Adopt;
    j.plan.command = None;
    j.plan.before = Observation::NativePackage {
        version: "1.0.0".into(),
        source_fingerprint: "a".repeat(64),
        linked: false,
    };
    j.plan.recipe_sha256 = fingerprint(&j.plan.package)?;
    j.plan_sha256 = fingerprint(&j.plan)?;
    crate::policy::state::validate_journal(&j)?;
    let mut invalid_scope = j.clone();
    invalid_scope.scope.service_identity = Some(crate::domain::ServiceIdentity {
        name: "fixture".into(),
        uid: 501,
    });
    invalid_scope.scope_sha256 = fingerprint(&invalid_scope.scope)?;
    assert!(crate::policy::state::validate_journal(&invalid_scope).is_err());
    validate_journal(&j)?;
    assert!(crate::policy::state::expected(&j, &j.plan.before));
    let mut invalid = j.clone();
    invalid.plan.package.id = "another-instance".into();
    assert!(validate_journal(&invalid).is_err());
    invalid.plan.recipe_sha256 = fingerprint(&invalid.plan.package)?;
    invalid.plan_sha256 = fingerprint(&invalid.plan)?;
    assert!(crate::policy::state::validate_journal(&invalid).is_err());
    for action in [Action::Install, Action::Upgrade, Action::Uninstall] {
        let mut bad = j.clone();
        bad.plan.action = action;
        assert!(validate_journal(&bad).is_err());
    }
    let mut bad = j.clone();
    bad.plan.command = Some(crate::test_support::command("write"));
    assert!(validate_journal(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.query = None;
    assert!(validate_journal(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.delegated = true;
    assert!(validate_journal(&bad).is_err());
    let mut bad = j.clone();
    bad.plan.synchronous = false;
    assert!(validate_journal(&bad).is_err());
    let mut bad = j;
    bad.plan.before = Observation::Present {
        evidence: None,
        version: "1.0.0".into(),
    };
    assert!(validate_journal(&bad).is_err());
    Ok(())
}

fn view(name: &str, management: Management, recipe: Option<&str>) -> Resource {
    Resource {
        id: "same".into(),
        name: name.into(),
        aliases: vec!["alias".into()],
        management,
        status: State::Present,
        data: Data::Application {
            manager: "npm".into(),
            native_id: "demo".into(),
            scope: "/fixture/npm".into(),
            releases: vec![],
            baseline: None,
            issues: vec![],
            recipe_ref: recipe.map(String::from),
        },
    }
}
#[test]
fn unified_views_keep_native_facts_ownership_aliases_and_persistent_conflicts() -> TestResult {
    let native = view("native", Management::Native, None);
    let declared = view("declared", Management::Adopted, Some("recipe:a"));
    for items in [
        vec![declared.clone(), native.clone()],
        vec![native.clone(), declared.clone()],
    ] {
        let (items, conflict) = merge_views(items);
        assert!(!conflict);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "native");
        assert_eq!(items[0].management, Management::Adopted);
        assert!(items[0].aliases.contains(&"declared".into()));
        assert!(items[0].aliases.contains(&"native".into()));
        assert_eq!(serde_json::to_value(&items[0])?["recipe_ref"], "recipe:a");
    }
    let (items, conflict) = merge_views(vec![
        declared.clone(),
        view("other", Management::Declared, Some("recipe:b")),
        native,
    ]);
    assert!(conflict);
    assert_eq!(items[0].status, State::Unknown);
    assert!(serde_json::to_value(&items[0])?["recipe_ref"].is_null());
    let mut other = view("config", Management::Untracked, None);
    other.data = Data::Config {
        target: "/fixture/config".into(),
        source_candidates: vec![],
        owner: "unknown".into(),
        consumers: vec![],
        observed_type: "file".into(),
        mode: None,
        baseline: None,
    };
    let (items, conflict) = merge_views(vec![other.clone(), other]);
    assert!(!conflict);
    assert_eq!(items.len(), 1);
    let mut distinct = declared.clone();
    distinct.id = "other".into();
    let (items, conflict) = merge_views(vec![declared, distinct]);
    assert!(!conflict);
    assert_eq!(items.len(), 2);
    Ok(())
}

#[test]
fn non_utf8_paths_cannot_be_silently_lossily_fingerprinted() {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(
        b"/fixture/invalid-\xff".to_vec(),
    ));
    assert!(npm_id(&path, "demo").is_err());
}

#[test]
fn native_baseline_comparison_and_execution_contract_preserve_read_only_authority() -> TestResult {
    use crate::domain::resource::Comparison;
    let observed = Observation::NativePackage {
        version: "1.0.0".into(),
        source_fingerprint: "source".into(),
        linked: false,
    };
    assert_eq!(
        baseline(&observed, observed.clone()).comparison,
        Comparison::Same
    );
    assert_eq!(
        baseline(&observed, Observation::Missing { evidence: None }).comparison,
        Comparison::Different
    );
    assert_eq!(
        baseline(
            &observed,
            Observation::Unknown {
                reason: "query unknown".into()
            }
        )
        .comparison,
        Comparison::Unknown
    );
    assert_eq!(
        baseline(&Observation::Missing { evidence: None }, observed.clone()).comparison,
        Comparison::Unknown
    );
    let p = observer()?;
    crate::policy::state::plan_inputs(&p, Action::Adopt, &observed, None)?;
    for action in [Action::Install, Action::Upgrade, Action::Uninstall] {
        assert!(crate::policy::state::plan_inputs(&p, action, &observed, None).is_err());
    }
    assert!(
        crate::policy::state::plan_inputs(
            &crate::test_support::package(),
            Action::Adopt,
            &observed,
            None
        )
        .is_err()
    );
    let mut invalid = p;
    invalid.id = "wrong".into();
    assert!(crate::policy::state::plan_inputs(&invalid, Action::Adopt, &observed, None).is_err());
    let mut j = crate::test_support::journal()?;
    j.plan.before = observed.clone();
    assert!(validate_journal(&j).is_err());
    assert!(!crate::policy::state::expected(&j, &observed));
    j.plan.action = Action::Adopt;
    assert!(!crate::policy::state::expected(
        &j,
        &Observation::NativePackage {
            version: "1.0.0".into(),
            source_fingerprint: "changed".into(),
            linked: false
        }
    ));
    Ok(())
}

#[test]
fn reserved_observer_rejects_query_of_the_other_protocol() -> TestResult {
    let mut journal = crate::test_support::journal()?;
    journal.plan.package = observer()?;
    journal.plan.action = Action::Adopt;
    journal.plan.command = None;
    journal.plan.before = Observation::NativePackage {
        version: "1.0.0".into(),
        source_fingerprint: "source".into(),
        linked: false,
    };
    let target = target();
    let local = crate::domain::LocalNpmQuery {
        kind: crate::domain::NpmQueryKind::LocalV1,
        prefix: target.prefix.clone(),
        name: target.name,
        contract: crate::domain::NpmContract::NativeV1,
        namespace: crate::domain::NpmNamespace {
            canonical_prefix: target.prefix.clone(),
            anchor: crate::domain::NpmAnchor::ExistingPrefix {
                path: target.prefix,
                identity: crate::domain::FileId {
                    device: 1,
                    inode: 2,
                },
            },
        },
        config_bindings: None,
    };
    journal.plan.query = Some(crate::domain::QuerySpec::LocalNpm(local));
    assert!(validate_journal(&journal).is_err());
    Ok(())
}

#[test]
fn native_observer_journal_accepts_only_adopt_with_local_query() -> TestResult {
    let mut journal = crate::test_support::journal()?;
    journal.plan.action = Action::Adopt;
    journal.plan.command = None;
    journal.plan.before = Observation::NativePackage {
        version: "1.0.0".into(),
        source_fingerprint: "source".into(),
        linked: false,
    };
    let target = target();
    journal.plan.package = observer()?;
    journal.plan.package.provider = Provider::NpmObserved {
        prefix: target.prefix.clone(),
        name: target.name.clone(),
    };
    journal.plan.query = Some(crate::domain::QuerySpec::LocalNpm(
        crate::domain::LocalNpmQuery {
            kind: crate::domain::NpmQueryKind::LocalV1,
            prefix: target.prefix.clone(),
            name: target.name,
            contract: crate::domain::NpmContract::NativeV1,
            namespace: crate::domain::NpmNamespace {
                canonical_prefix: target.prefix.clone(),
                anchor: crate::domain::NpmAnchor::ExistingPrefix {
                    path: target.prefix,
                    identity: crate::domain::FileId {
                        device: 1,
                        inode: 2,
                    },
                },
            },
            config_bindings: None,
        },
    ));
    validate_journal(&journal)?;
    journal.plan.action = Action::Uninstall;
    assert!(validate_journal(&journal).is_err());
    Ok(())
}

#[test]
fn config_observer_baseline_and_inputs_stay_isolated_from_npm_observers() -> TestResult {
    use crate::domain::resource::Comparison;
    let npm_obs = Observation::NativePackage {
        version: "1.0.0".into(),
        source_fingerprint: "source".into(),
        linked: false,
    };
    let cfg_obs = Observation::ConfigFile {
        authority: crate::domain::ConfigAuthority::Untracked,
        target: None,
        source: None,
        mode_matches_source: None,
    };
    assert_eq!(
        baseline(&cfg_obs, cfg_obs.clone()).comparison,
        Comparison::Same
    );
    assert_eq!(
        baseline(&cfg_obs, Observation::Missing { evidence: None }).comparison,
        Comparison::Different
    );
    assert_eq!(
        baseline(
            &cfg_obs,
            Observation::Unknown {
                reason: "err".into()
            }
        )
        .comparison,
        Comparison::Unknown
    );
    assert_eq!(baseline(&npm_obs, cfg_obs).comparison, Comparison::Unknown);
    let cfg_pkg = Package {
        id: crate::policy::config_local::config_id(Path::new("/fixture/home/.zshrc"))?,
        name: "/fixture/home/.zshrc".into(),
        kind: Kind::Config,
        official_url: String::new(),
        provider: Provider::ConfigObserved {
            target: "/fixture/home/.zshrc".into(),
            source: None,
            authority: crate::domain::ConfigAuthority::Untracked,
        },
        paths: vec![],
        exact_version_required: false,
    };
    assert!(!npm_observer(&cfg_pkg)?);
    assert!(super::observer(&cfg_pkg)?);
    let mut bad_obs = observer()?;
    bad_obs.id = "broken".into();
    assert!(super::observer(&bad_obs).is_err());
    assert!(crate::policy::state::plan_inputs(&cfg_pkg, Action::Adopt, &npm_obs, None).is_err());
    let cap_obs = Observation::CapabilityPack {
        capability: crate::domain::CapabilityKind::SkillsCli,
        pack: None,
    };
    assert_eq!(
        baseline(&cap_obs, cap_obs.clone()).comparison,
        Comparison::Same
    );
    assert_eq!(
        baseline(&cap_obs, Observation::Missing { evidence: None }).comparison,
        Comparison::Different
    );
    assert_eq!(baseline(&npm_obs, cap_obs).comparison, Comparison::Unknown);
    Ok(())
}

#[test]
fn uv_baseline_is_comparable_only_to_bound_tool_observations() {
    let observed = Observation::UvTool {
        version: "1.0.0".into(),
        evidence: crate::domain::UvToolEvidence {
            namespace_identity: "namespace".into(),
            payload_identity: crate::domain::FileId {
                device: 1,
                inode: 2,
            },
            receipt_sha256: "a".repeat(64),
            metadata_sha256: "b".repeat(64),
            pyvenv_sha256: "c".repeat(64),
            entrypoints: vec![],
            fingerprint: "d".repeat(64),
        },
    };
    assert_eq!(
        baseline(&observed, observed.clone()).comparison,
        crate::domain::resource::Comparison::Same
    );
    assert_eq!(
        baseline(&observed, Observation::Missing { evidence: None }).comparison,
        crate::domain::resource::Comparison::Different
    );
    assert_eq!(
        baseline(
            &observed,
            Observation::Unknown {
                reason: "not_observed".into()
            }
        )
        .comparison,
        crate::domain::resource::Comparison::Unknown
    );
}
