use crate::{
    domain::*,
    error::Result,
    software::rules::brew::{
        EFFECT_CONTRACT, QUERY_SCRIPT, SOURCE_COMMIT, context_id, logical_id, observation,
    },
};
use std::{collections::BTreeMap, path::PathBuf};
pub(crate) fn node(directory: bool, inode: u64) -> BrewNodeStamp {
    BrewNodeStamp {
        identity: FileId { device: 7, inode },
        mode: if directory { 0o040_700 } else { 0o100_400 },
        size: 32,
        modified_ns: 42,
    }
}
pub(crate) fn query() -> TestResultQuery {
    let prefix = PathBuf::from("/fixture/brew");
    let home = PathBuf::from("/fixture/home");
    let config_files = [
        PathBuf::from("/etc/homebrew/brew.env"),
        prefix.join("etc/homebrew/brew.env"),
        home.join(".homebrew/brew.env"),
    ]
    .into_iter()
    .map(|path| BrewConfigStamp::Absent {
        path,
        anchor: "/".into(),
        parent: node(true, 2),
    })
    .collect();
    Ok(BrewQuerySpec {
        kind: BrewQueryKind::BrewFormulaNativeV1,
        schema_version: 1,
        key: BrewFormulaKey {
            canonical_prefix: prefix.clone(),
            full_name: "foo".into(),
        },
        selection: BrewSelection {
            tap: "homebrew/core".into(),
            name: "foo".into(),
        },
        context_epoch: 0,
        binding: BrewContextBinding {
            context_id: context_id(&prefix)?,
            invocation: "/fixture/bin/brew".into(),
            canonical_program: "/fixture/repository/bin/brew".into(),
            prefix,
            cellar: "/fixture/cellar".into(),
            program_stamp: BrewFileStamp {
                node: BrewNodeStamp {
                    mode: 0o100_700,
                    ..node(false, 3)
                },
                sha256: "a".repeat(64),
            },
            prefix_id: node(true, 4).identity,
            cellar_id: node(true, 5).identity,
            metadata_scope: BrewMetadataScope {
                home: home.clone(),
                xdg_config_home: None,
                homebrew_xdg_config_home: None,
                actual_user_config_home: home.join(".homebrew"),
                config_files,
            },
            compatibility: BrewCompatibility::Homebrew7_0_15 {
                source_commit: SOURCE_COMMIT.into(),
                acceptance_sha256: "b".repeat(64),
                ruby_version: "4.0.9".into(),
            },
        },
        script: BrewQueryScript {
            origin: BrewScriptOrigin::MacopsBuiltinV1,
            absolute_path: "/fixture/state/query.rb".into(),
            canonical_path: "/fixture/state/query.rb".into(),
            file_id: node(false, 6).identity,
            sha256: crate::policy::catalog::cache::digest(QUERY_SCRIPT),
            size: QUERY_SCRIPT.len() as u64,
            product_build_id: "c".repeat(64),
        },
        env_capture_id: "ENV-1".into(),
    })
}
type TestResultQuery = std::result::Result<BrewQuerySpec, Box<dyn std::error::Error>>;
pub(crate) fn report(q: &BrewQuerySpec, versions: &[(&str, &str, &str)]) -> BrewNativeReport {
    let path = q.binding.cellar.join(&q.selection.name);
    let kegs: Vec<_> = versions
        .iter()
        .map(|(tap, spec, version)| BrewKegIdentity {
            path: path.join(version),
            name: q.selection.name.clone(),
            tap: (*tap).into(),
            full_name: if *tap == "homebrew/core" {
                q.selection.name.clone()
            } else {
                format!("{tap}/{}", q.selection.name)
            },
            spec: (*spec).into(),
            version: (*version).into(),
            receipt: BrewFileStamp {
                node: node(false, 20),
                sha256: "e".repeat(64),
            },
        })
        .collect();
    let native_selected: Vec<_> = kegs
        .iter()
        .filter(|k| k.tap == q.selection.tap)
        .map(|k| k.path.clone())
        .collect();
    BrewNativeReport {
        schema_version: 1,
        query_id: "QUERY-1".into(),
        homebrew_version: "7.0.15".into(),
        selected_version: kegs
            .iter()
            .find(|k| k.tap == q.selection.tap)
            .map(|k| k.version.clone()),
        prefix: q.binding.prefix.clone(),
        cellar: q.binding.cellar.clone(),
        actual_user_config_home: q.binding.metadata_scope.actual_user_config_home.clone(),
        policies: BrewEffectivePolicies {
            no_auto_update: true,
            no_install_cleanup: true,
            no_autoremove: true,
            runtime: BrewRuntimePolicies {
                no_bootsnap: true,
                installed_dependents_check: true,
            },
        },
        rack: BrewRackProof {
            path,
            cellar_before: node(true, 5),
            cellar_after: node(true, 5),
            rack_before: Some(node(true, 8)),
            rack_after: Some(node(true, 8)),
            entries: kegs
                .iter()
                .map(|k| BrewRackEntry {
                    name: k.version.clone(),
                    kind: BrewEntryKind::Directory,
                    stamp: node(true, 10),
                })
                .collect(),
            kegs,
            native_selected,
        },
        formula: Some(BrewNativeInfo {
            name: q.selection.name.clone(),
            tap: q.selection.tap.clone(),
            full_name: q.key.full_name.clone(),
            active_spec: "stable".into(),
            pkg_version: "2.0.0".into(),
            pinned: false,
            outdated: true,
            resolution: BrewResolution {
                alias_changed: false,
                migration_needed: false,
            },
            latest_full_name: q.key.full_name.clone(),
            installed_paths: versions
                .iter()
                .map(|(_, _, v)| q.binding.cellar.join(&q.selection.name).join(v))
                .collect(),
        }),
    }
}
pub(crate) fn stamp(q: &BrewQuerySpec) -> Result<BrewEvidenceStamp> {
    Ok(BrewEvidenceStamp {
        context_id: q.binding.context_id.clone(),
        context_epoch: q.context_epoch,
        binding_fingerprint: fingerprint(&q.binding)?,
        env_capture_id: q.env_capture_id.clone(),
        query_id: "QUERY-1".into(),
    })
}
pub(crate) fn package(q: &BrewQuerySpec) -> Result<Package> {
    let mut p = crate::test_support::package();
    p.id = logical_id(&q.key)?;
    p.provider = Provider::BrewFormula {
        query: Box::new(q.clone()),
        recipe_ref: Some("declared:foo".into()),
    };
    Ok(p)
}
pub(crate) fn plan() -> std::result::Result<Plan, Box<dyn std::error::Error>> {
    let q = query()?;
    let p = package(&q)?;
    Ok(Plan {
        brew: Some(Box::new(BrewPlanData {
            schema_version: 1,
            owner: BrewOwnerSnapshot {
                logical_owner_id: p.id.clone(),
                selected_package_id: p.id.clone(),
                entries: BTreeMap::from([(
                    p.id.clone(),
                    std::collections::BTreeSet::from(["declared:fixture".into()]),
                )]),
            },
            context: BrewContextAuthority {
                context_id: q.binding.context_id.clone(),
                epoch: 0,
                binding_fingerprint: fingerprint(&q.binding)?,
            },
            goal: BrewGoal::UpgradeAvailable {
                pkg_version: "2.0.0".into(),
            },
            execution: BrewExecution::NativeMutator,
            confirmation: BrewScopeConfirmation {
                context_id: q.binding.context_id.clone(),
                epoch: 0,
                logical_owner_id: p.id.clone(),
                action: Action::Upgrade,
                effect_contract: EFFECT_CONTRACT.into(),
                consent: BrewConsent::ExistingContext,
            },
            env_capture_id: q.env_capture_id.clone(),
        })),
        npm_binding: None,
        uv_owner: None,
        timeout_secs: 5,
        package: p.clone(),
        action: Action::Upgrade,
        before: observation(
            &q,
            stamp(&q)?,
            report(&q, &[("homebrew/core", "stable", "1.0.0")]),
        )?,
        target: Some("2.0.0".into()),
        command: Some(CommandSpec {
            program: q.binding.invocation.clone(),
            args: vec![
                "upgrade".into(),
                "--formula".into(),
                "homebrew/core/foo".into(),
            ],
        }),
        query: Some(QuerySpec::BrewFormula(Box::new(q))),
        recipe_sha256: fingerprint(&p)?,
        warnings: vec![],
        synchronous: true,
        delegated: true,
    })
}
