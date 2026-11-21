#![cfg(test)]
use super::*;
use crate::{
    domain::{
        Action, CommandSpec, FileId, LocalNpmQuery, NpmEvidence, NpmNamespace, NpmQueryKind,
        Observation, Phase, Receipt, fingerprint,
    },
    test_support::{TestResult, journal, package},
};
use serde_json::json;

fn id() -> FileId {
    FileId {
        device: 1,
        inode: 2,
    }
}

fn typed_install() -> crate::error::Result<crate::domain::Journal> {
    let mut journal = journal()?;
    let prefix = Path::new("/fixture/new");
    journal.plan.package.provider = Provider::Npm {
        prefix: prefix.into(),
        package: "demo".into(),
        authority: None,
    };
    let query = LocalNpmQuery {
        kind: NpmQueryKind::LocalV1,
        prefix: prefix.into(),
        name: "demo".into(),
        contract: NpmContract::DeclaredV1,
        namespace: NpmNamespace {
            canonical_prefix: prefix.into(),
            anchor: NpmAnchor::CreatePrefix {
                path: "/fixture".into(),
                identity: id(),
                missing_tail: vec!["new".into()],
            },
        },
        config_bindings: None,
    };
    journal.plan.before = Observation::Missing {
        evidence: Some(evidence(&query, None)?),
    };
    journal.plan.query = Some(QuerySpec::LocalNpm(query));
    journal.plan.command = Some(CommandSpec {
        program: "/fixture/bin/npm".into(),
        args: [
            "install",
            "--global",
            "--prefix",
            "/fixture/new",
            "demo@1.0.0",
        ]
        .map(String::from)
        .into(),
    });
    journal.plan.recipe_sha256 = fingerprint(&journal.plan.package)?;
    journal.plan_sha256 = fingerprint(&journal.plan)?;
    Ok(journal)
}

fn evidence(
    query: &LocalNpmQuery,
    prefix_identity: Option<FileId>,
) -> crate::error::Result<NpmEvidence> {
    Ok(NpmEvidence {
        kind: NpmQueryKind::LocalV1,
        namespace: crate::policy::targets::npm_id(&query.namespace.canonical_prefix, &query.name)?,
        canonical_prefix: query.namespace.canonical_prefix.clone(),
        prefix_identity,
        fingerprint: "a".repeat(64),
    })
}

fn local(query: &mut Option<QuerySpec>) -> Result<&mut LocalNpmQuery> {
    match query {
        Some(QuerySpec::LocalNpm(query)) => Ok(query),
        _ => Err(invalid_plan()),
    }
}

#[test]
fn npm_manifest_has_strict_bounded_identity_and_never_exposes_raw_errors() -> TestResult {
    assert_eq!(
        manifest_version(
            br#"{"name":"@scope/demo","version":"1.0.0","extra":"SYNTHETIC_SECRET"}"#,
            "@scope/demo"
        )?,
        "1.0.0"
    );
    for body in [
        r"{}",
        r"[]",
        r#"{"name":true,"version":"2"}"#,
        r#"{"name":"demo","version":2}"#,
        r#"{"name":"demo","version":null}"#,
        r#"{"name":"demo","version":"SYNTHETIC_SECRET","version":"2"}"#,
        r#"{"name":"demo","name":"other","version":"2"}"#,
        r#"{"name":"demo","version":"latest"}"#,
        r#"{"name":"demo","version":""}"#,
        r#"{"name":"../demo","version":"1"}"#,
        r#"{"name":"alias","version":"1"}"#,
    ] {
        let error = manifest_version(body.as_bytes(), "demo")
            .err()
            .ok_or("expected rejection")?;
        assert!(!error.to_string().contains("SYNTHETIC_SECRET"));
    }
    assert!(name(&"a".repeat(215)).is_err());
    assert!(version(&"1".repeat(129)).is_err());
    Ok(())
}

#[test]
fn local_query_binding_rejects_mismatched_identity_even_with_recomputed_hash() -> TestResult {
    let initial = typed_install()?;
    crate::policy::state::validate_journal(&initial)?;
    for field in [
        "prefix",
        "name",
        "contract",
        "canonical",
        "relative",
        "tail",
        "anchor",
        "provider",
        "missing",
        "action",
        "command",
        "argv",
        "evidence",
        "delegated",
        "async",
        "target",
        "version",
        "kind",
    ] {
        let mut bad = initial.clone();
        let plan = &mut bad.plan;
        match field {
            "prefix" => local(&mut plan.query)?.prefix = "/else".into(),
            "name" => local(&mut plan.query)?.name = "other".into(),
            "contract" => local(&mut plan.query)?.contract = NpmContract::NativeV1,
            "canonical" => local(&mut plan.query)?.namespace.canonical_prefix = "/other".into(),
            "relative" => local(&mut plan.query)?.prefix = "relative".into(),
            "tail" => {
                local(&mut plan.query)?.namespace.anchor = NpmAnchor::CreatePrefix {
                    path: "/fixture".into(),
                    identity: id(),
                    missing_tail: vec!["./new".into()],
                }
            }
            "anchor" => {
                local(&mut plan.query)?.namespace.anchor = NpmAnchor::ExistingPrefix {
                    path: "/wrong".into(),
                    identity: id(),
                }
            }
            "provider" => plan.package = package(),
            "missing" => plan.query = None,
            "action" => plan.action = Action::Uninstall,
            "command" => plan.command = None,
            "argv" => plan.command.as_mut().ok_or("command")?.args[3] = "/other".into(),
            "evidence" => plan.before = Observation::Missing { evidence: None },
            "delegated" => plan.delegated = true,
            "async" => plan.synchronous = false,
            "target" => plan.target = None,
            "version" => plan.target = Some("invalid".into()),
            "kind" => {
                local(&mut plan.query)?.namespace.anchor = NpmAnchor::CreatePrefix {
                    path: "relative".into(),
                    identity: id(),
                    missing_tail: vec![],
                }
            }
            _ => return Err("bad fixture".into()),
        }
        bad.plan.recipe_sha256 = fingerprint(&bad.plan.package)?;
        bad.plan_sha256 = fingerprint(&bad.plan)?;
        assert!(
            crate::policy::state::validate_journal(&bad).is_err(),
            "{field}"
        );
    }
    Ok(())
}

#[test]
fn old_command_wire_is_stable_and_new_query_requires_exact_shape() -> TestResult {
    for raw in [
        r#"{"program":"/fixture/bin/npm","args":["ls","--global","--prefix","/fixture/new","--depth=0","--json","demo"]}"#,
        r#"{"program":"/fixture/bin/other","args":[]}"#,
    ] {
        let query: QuerySpec = serde_json::from_str(raw)?;
        assert_eq!(serde_json::to_string(&query)?, raw);
        assert_eq!(
            fingerprint(&query)?,
            fingerprint(query.command().ok_or("legacy command")?)?
        );
    }
    for raw in [
        r#"{"status":"missing"}"#,
        r#"{"status":"present","version":"1.0.0"}"#,
        r#"{"status":"native-package","version":"1.0.0","source_fingerprint":"source","linked":false}"#,
    ] {
        assert_eq!(
            serde_json::to_string(&serde_json::from_str::<Observation>(raw)?)?,
            raw
        );
    }
    let initial = typed_install()?;
    let mut old = initial.plan.clone();
    let command: QuerySpec = serde_json::from_value(
        json!({"program":"/removed/npm","args":["ls","--global","--prefix","/fixture/new","--depth=0","--json","demo"]}),
    )?;
    old.query = Some(command.clone());
    old.before = Observation::Missing { evidence: None };
    validate_plan(&old)?;
    assert!(
        binding(
            &old.package,
            &QuerySpec::Command(crate::test_support::command("wrong"))
        )
        .is_err()
    );
    let mut value = serde_json::to_value(initial.plan.query.ok_or("typed query")?)?;
    value["program"] = json!("/fixture/bin/npm");
    assert!(serde_json::from_value::<QuerySpec>(value.clone()).is_err());
    value.as_object_mut().ok_or("object")?.remove("program");
    value["kind"] = json!("npm-local-v2");
    assert!(serde_json::from_value::<QuerySpec>(value).is_err());
    Ok(())
}

#[test]
fn create_prefix_commit_derives_a_stable_query_and_recovery_cannot_rebind() -> TestResult {
    let mut journal = typed_install()?;
    let query = local(&mut journal.plan.query)?.clone();
    let observed = Observation::Present {
        version: "1.0.0".into(),
        evidence: Some(evidence(&query, Some(id()))?),
    };
    let committed = committed_query(&journal.plan, &observed)?;
    assert!(
        matches!(&committed,Some(QuerySpec::LocalNpm(query)) if matches!(query.namespace.anchor,NpmAnchor::ExistingPrefix{..}))
    );
    assert!(committed_query(&journal.plan, &journal.plan.before).is_err());
    let mut receipt = Receipt {
        query: committed.clone(),
        scope: Some(journal.scope.clone()),
        package: journal.plan.package.clone(),
        operation_id: journal.id.clone(),
        observation: observed,
        updated_at: 1,
    };
    journal.phase = Phase::Unknown;
    assert_eq!(committed_install_query(&journal, &receipt)?, committed);
    for field in ["query", "scope", "package", "version"] {
        let mut malformed = receipt.clone();
        match field {
            "query" => malformed.query = journal.plan.query.clone(),
            "scope" => malformed.scope = None,
            "package" => malformed.package.name = "other".into(),
            "version" => {
                if let Observation::Present { version, .. } = &mut malformed.observation {
                    *version = "2.0.0".into();
                }
            }
            _ => return Err("bad fixture".into()),
        }
        assert!(
            committed_install_query(&journal, &malformed).is_err(),
            "{field}"
        );
    }
    receipt.operation_id = "OP-other".into();
    assert_eq!(committed_install_query(&journal, &receipt)?, None);
    Ok(())
}

#[test]
fn npm_query_rejects_unsupported_anchor_and_malformed_evidence() -> TestResult {
    let initial = typed_install()?;
    let Some(QuerySpec::LocalNpm(query)) = &initial.plan.query else {
        return Err("local query".into());
    };
    assert!(
        initial
            .plan
            .query
            .as_ref()
            .and_then(QuerySpec::command)
            .is_none()
    );
    let mut observed = initial.plan.package.clone();
    observed.provider = Provider::NpmObserved {
        prefix: query.prefix.clone(),
        name: query.name.clone(),
    };
    assert!(binding(&observed, &crate::test_support::command("query").into()).is_err());
    let mut transformed = query.clone();
    transformed.contract = NpmContract::NativeV1;
    assert!(binding(&observed, &QuerySpec::LocalNpm(transformed.clone())).is_err());
    transformed.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity: id(),
    };
    let mut invalid_native = initial.plan.clone();
    invalid_native.package = observed;
    invalid_native.query = Some(QuerySpec::LocalNpm(transformed));
    assert!(validate_plan(&invalid_native).is_err());

    for (prefix, name, program) in [
        ("/fixture/new", "--bad", "/npm"),
        ("relative", "demo", "/npm"),
        ("/fixture/new", "demo", "relative"),
    ] {
        let mut declared = initial.plan.package.clone();
        declared.provider = Provider::Npm {
            prefix: prefix.into(),
            package: name.into(),
            authority: None,
        };
        assert!(
            binding(
                &declared,
                &QuerySpec::Command(CommandSpec {
                    program: program.into(),
                    args: vec![]
                })
            )
            .is_err()
        );
    }
    for field in ["name", "canonical", "empty"] {
        let mut malformed = query.clone();
        match field {
            "name" => malformed.name = "/".into(),
            "canonical" => malformed.namespace.canonical_prefix = "relative".into(),
            "empty" => {
                malformed.namespace.anchor = NpmAnchor::CreatePrefix {
                    path: "/fixture".into(),
                    identity: id(),
                    missing_tail: vec![],
                }
            }
            _ => return Err("fixture".into()),
        }
        assert!(binding(&initial.plan.package, &QuerySpec::LocalNpm(malformed)).is_err());
    }
    Ok(())
}

#[test]
fn npm_plan_requires_evidence_bound_to_the_declared_installation() -> TestResult {
    let initial = typed_install()?;
    let Some(QuerySpec::LocalNpm(query)) = &initial.plan.query else {
        return Err("local query".into());
    };
    for field in ["prefix", "namespace", "length", "hex", "identity"] {
        let mut existing = query.clone();
        existing.namespace.anchor = NpmAnchor::ExistingPrefix {
            path: query.namespace.canonical_prefix.clone(),
            identity: id(),
        };
        let mut bad = evidence(&existing, Some(id()))?;
        match field {
            "prefix" => bad.canonical_prefix = "/different".into(),
            "namespace" => bad.namespace = "different".into(),
            "length" => bad.fingerprint = "f".into(),
            "hex" => bad.fingerprint = "g".repeat(64),
            "identity" => bad.prefix_identity = None,
            _ => return Err("fixture".into()),
        }
        assert!(validate_evidence(&existing, &bad).is_err(), "{field}");
    }
    let mut bad = initial.plan.clone();
    bad.before = Observation::Missing {
        evidence: Some(evidence(query, Some(id()))?),
    };
    assert!(validate_plan(&bad).is_err());
    bad.before = Observation::Present {
        version: "1.0.0".into(),
        evidence: Some(evidence(query, None)?),
    };
    assert!(validate_plan(&bad).is_err());
    assert!(committed_query(&bad, &bad.before).is_err());
    let mut existing = initial.plan.clone();
    local(&mut existing.query)?.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity: id(),
    };
    existing.before = Observation::Missing {
        evidence: Some(evidence(query, Some(id()))?),
    };
    existing.action = Action::Upgrade;
    assert!(validate_plan(&existing).is_err());
    existing.action = Action::Cleanup;
    assert!(validate_plan(&existing).is_err());
    existing.query = Some(crate::test_support::command("query").into());
    existing.package = package();
    assert!(validate_plan(&existing).is_err());
    Ok(())
}

#[test]
fn committed_install_lookup_preserves_preexisting_noncreation_query() -> TestResult {
    let mut journal = typed_install()?;
    let record = Receipt {
        package: journal.plan.package.clone(),
        query: journal.plan.query.clone(),
        scope: Some(journal.scope.clone()),
        observation: journal.plan.before.clone(),
        operation_id: journal.id.clone(),
        updated_at: 1,
    };
    let query = local(&mut journal.plan.query)?;
    query.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity: id(),
    };
    assert!(committed_install_query(&journal, &record)?.is_none());
    Ok(())
}

#[test]
fn unrepresentable_prefixes_are_rejected_in_binding_and_commit() -> TestResult {
    use std::os::unix::ffi::OsStringExt;
    let invalid = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/\xff".to_vec()));
    let mut journal = typed_install()?;
    let local = local(&mut journal.plan.query)?;
    local.prefix = invalid.clone();
    local.namespace.canonical_prefix = invalid.clone();
    local.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: invalid.clone(),
        identity: id(),
    };
    let mut bad = NpmEvidence {
        kind: NpmQueryKind::LocalV1,
        namespace: "cannot-encode".into(),
        canonical_prefix: invalid.clone(),
        prefix_identity: Some(id()),
        fingerprint: "a".repeat(64),
    };
    assert!(validate_evidence(local, &bad).is_err());
    journal.plan.package.provider = Provider::NpmObserved {
        prefix: invalid,
        name: "demo".into(),
    };
    local.contract = NpmContract::NativeV1;
    assert!(validate_plan(&journal.plan).is_err());

    let initial = typed_install()?;
    let mut receipts = crate::domain::Receipts::default();
    let Some(QuerySpec::LocalNpm(query)) = &initial.plan.query else {
        return Err("query".into());
    };
    bad.canonical_prefix = query.namespace.canonical_prefix.clone();
    let receipt = Receipt {
        scope: Some(initial.scope.clone()),
        package: initial.plan.package.clone(),
        query: initial.plan.query.clone(),
        observation: Observation::Present {
            version: "1.0.0".into(),
            evidence: Some(bad),
        },
        operation_id: initial.id.clone(),
        updated_at: 1,
    };
    assert!(committed_install_query(&initial, &receipt).is_err());
    assert!(
        crate::policy::state::commit_receipt(&mut receipts, &initial, receipt.observation, 1)
            .is_err()
    );
    assert!(receipts.installations.is_empty());
    let mut receipt = Receipt {
        scope: Some(initial.scope.clone()),
        package: initial.plan.package.clone(),
        query: initial.plan.query.clone(),
        observation: Observation::Present {
            version: "1.0.0".into(),
            evidence: Some(evidence(query, Some(id()))?),
        },
        operation_id: initial.id.clone(),
        updated_at: 1,
    };
    receipt.query = committed_query(&initial.plan, &receipt.observation)?;
    receipt.package.paths.push(crate::domain::OwnedPath {
        path: std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![255])),
        kind: crate::domain::PathKind::Payload,
    });
    assert!(committed_install_query(&initial, &receipt).is_err());
    let observation = receipt.observation.clone();
    receipts
        .installations
        .insert(initial.plan.package.id.clone(), receipt);
    assert!(crate::policy::state::commit_receipt(&mut receipts, &initial, observation, 2).is_err());
    Ok(())
}

#[test]
fn frozen_npm_plans_preserve_manifest_bound_existing_installations() -> TestResult {
    let seed = typed_install()?;
    let mut plan = seed.plan;
    let query = local(&mut plan.query)?;
    query.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity: id(),
    };
    plan.before = Observation::Present {
        version: "1.0.0".into(),
        evidence: Some(evidence(query, Some(id()))?),
    };
    plan.action = Action::Adopt;
    plan.command = None;
    validate_plan(&plan)?;
    let mut absent_query = plan.clone();
    absent_query.query = None;
    absent_query.package = package();
    assert!(validate_plan(&absent_query).is_err());
    plan.action = Action::Uninstall;
    plan.target = None;
    plan.command = Some(CommandSpec {
        program: "/bin/npm".into(),
        args: ["uninstall", "--global", "--prefix", "/fixture/new", "demo"]
            .map(String::from)
            .into(),
    });
    validate_plan(&plan)?;
    if let Observation::Present {
        evidence: Some(evidence),
        ..
    } = &mut plan.before
    {
        evidence.fingerprint = "changed".into();
    }
    assert!(validate_plan(&plan).is_err());
    Ok(())
}

#[test]
fn native_observer_identity_and_alias_path_are_cross_checked() -> TestResult {
    let seed = typed_install()?;
    let mut plan = seed.plan;
    let query = local(&mut plan.query)?;
    query.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity: id(),
    };
    query.contract = NpmContract::NativeV1;
    plan.package.provider = Provider::NpmObserved {
        prefix: query.prefix.clone(),
        name: query.name.clone(),
    };
    plan.package.id =
        crate::policy::targets::npm_id(&query.namespace.canonical_prefix, &query.name)?;
    plan.action = Action::Adopt;
    plan.command = None;
    plan.before = Observation::NativePackage {
        version: "1.0.0".into(),
        source_fingerprint: "test".into(),
        linked: false,
    };
    validate_plan(&plan)?;
    assert_eq!(committed_query(&plan, &plan.before)?, plan.query);
    let mut bad_kind = plan.clone();
    bad_kind.package.kind = crate::domain::Kind::App;
    assert!(validate_plan(&bad_kind).is_err());
    let query = local(&mut plan.query)?;
    query.prefix = "/fixture/alias".into();
    plan.package.provider = Provider::NpmObserved {
        prefix: query.prefix.clone(),
        name: query.name.clone(),
    };
    assert!(validate_plan(&plan).is_err());
    Ok(())
}

#[test]
fn nested_npm_query_objects_reject_sequences_and_extra_identity_fields() -> TestResult {
    let journal = typed_install()?;
    let orig = serde_json::to_value(journal.plan.query)?;
    for field in ["array", "extra"] {
        let mut value = orig.clone();
        value["namespace"]["anchor"]["identity"] = if field == "array" {
            json!([1, 2])
        } else {
            json!({"device":1,"inode":2,"extra":true})
        };
        assert!(serde_json::from_value::<QuerySpec>(value).is_err());
    }
    assert!(serde_json::from_value::<QuerySpec>(json!(["/bin/demo", ["query"]])).is_err());
    assert!(
        serde_json::from_value::<Observation>(json!({"status":"missing","evidence":[]})).is_err()
    );
    assert!(
        serde_json::from_value::<Package>(json!({
            "id":"npm:d","name":"d","kind":"cli","official_url":"https://e.org",
            "provider":{"type":"npm","package":"d","prefix":"/p","authority":[]}
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<Package>(json!({
            "id":"npm:d","name":"d","kind":"cli","official_url":"https://e.org",
            "provider":{"type":"npm","package":"d","prefix":"/p","authority":{"allow_remove":true,"registry":[]}}
        }))
        .is_err()
    );
    let mut bad_cfg_bindings = orig.clone();
    bad_cfg_bindings["config_bindings"] = json!([]);
    assert!(serde_json::from_value::<QuerySpec>(bad_cfg_bindings).is_err());
    let roundtrip: QuerySpec = serde_json::from_value(orig.clone())?;
    assert_eq!(serde_json::to_value(roundtrip)?, orig);
    assert!(manifest_version(br#"["demo","1.2.3"]"#, "demo").is_err());
    let local_cfg = QuerySpec::LocalConfig(crate::domain::LocalConfigQuery {
        kind: crate::domain::ConfigQueryKind::LocalV1,
        authority: crate::domain::ConfigAuthority::Untracked,
        target: crate::domain::ConfigAnchor {
            requested: "/fixture/home/.zshrc".into(),
            canonical_parent: "/fixture/home".into(),
            parent_identity: id(),
            name: ".zshrc".into(),
        },
        source: None,
        expected_mode: None,
    });
    assert!(binding(&journal.plan.package, &local_cfg).is_err());
    assert!(binding(&package(), &local_cfg).is_ok());
    Ok(())
}

#[test]
fn npm_registry_urls_and_view_write_argument_templates_are_strict() -> TestResult {
    use crate::domain::{NpmRegistryAuthority, NpmWriteAuthority};
    assert_eq!(package_scope("@deepseek-ai/dsh"), Some("@deepseek-ai"));
    assert_eq!(package_scope("plain-pkg"), None);
    assert!(sanitized_env_key("NODE_OPTIONS") && sanitized_env_key("node_options"));
    assert!(sanitized_env_key("npm_config_registry") && sanitized_env_key("NPM_CONFIG_USERCONFIG"));
    assert!(!sanitized_env_key("PATH"));

    validate_registry_url("https://registry.npmjs.org")?;
    validate_registry_url("https://reg.example.test:8443/npm/~public")?;
    for bad in [
        "http://registry.npmjs.org",
        "https://",
        "https:///path",
        "https://user:secret@registry.npmjs.org",
        "https://registry.npmjs.org/?token=secret",
        "https://registry.npmjs.org/#frag",
        &format!("https://registry.npmjs.org/{}", "a".repeat(520)),
    ] {
        assert!(validate_registry_url(bad).is_err(), "{bad}");
    }

    let auth = NpmWriteAuthority {
        allow_remove: true,
        registry: Some(NpmRegistryAuthority {
            url: "https://registry.npmjs.org".into(),
            scope_url: Some("https://npm.pkg.example.test/org".into()),
            allow_scripts: false,
        }),
    };
    validate_authority("@scope/demo", &auth)?;
    assert!(validate_authority("--bad", &auth).is_err());
    let mut bad_scope_url = auth.clone();
    if let Some(r) = &mut bad_scope_url.registry {
        r.scope_url = Some("http://insecure.invalid".into());
    }
    assert!(validate_authority("@scope/demo", &bad_scope_url).is_err());
    assert_eq!(
        validate_authority("unscoped", &auth)
            .err()
            .ok_or("scope")?
            .code,
        "invalid_npm_authority"
    );

    let home = Path::new("/fixture/home");
    let prefix = Path::new("/fixture/npm");
    assert!(expected_npm_view_args(home, prefix, "plain", Some(&bad_scope_url)).is_err());
    assert!(expected_npm_view_args(home, prefix, "--bad", None).is_err());
    assert!(expected_npm_view_args(home, Path::new("rel"), "plain", None).is_err());
    assert!(expected_npm_view_args(Path::new("rel"), prefix, "@scope/demo", Some(&auth)).is_err());
    let mut plain_auth = auth.clone();
    if let Some(r) = &mut plain_auth.registry {
        r.scope_url = None;
    }
    assert_eq!(
        expected_npm_view_args(home, prefix, "plain", Some(&plain_auth))?.len(),
        10
    );
    assert_eq!(
        expected_npm_view_args(home, prefix, "@scope/demo", Some(&auth))?,
        vec![
            "view",
            "--userconfig",
            "/fixture/home/.npmrc",
            "--globalconfig",
            "/fixture/npm/etc/npmrc",
            "--registry=https://registry.npmjs.org",
            "--@scope:registry=https://npm.pkg.example.test/org",
            "--replace-registry-host=never",
            "@scope/demo",
            "version",
            "--json",
        ]
    );
    assert_eq!(
        expected_npm_write_args(
            home,
            prefix,
            "@scope/demo",
            Some(&auth),
            Action::Upgrade,
            Some("1.2.3")
        )?,
        vec![
            "install",
            "--global",
            "--prefix",
            "/fixture/npm",
            "--userconfig",
            "/fixture/home/.npmrc",
            "--globalconfig",
            "/fixture/npm/etc/npmrc",
            "--registry=https://registry.npmjs.org",
            "--@scope:registry=https://npm.pkg.example.test/org",
            "--replace-registry-host=never",
            "--ignore-scripts",
            "@scope/demo@1.2.3",
        ]
    );
    Ok(())
}

#[test]
fn npm_authority_warnings_and_scripts_flags_follow_contract() -> TestResult {
    use crate::domain::{NpmRegistryAuthority, NpmWriteAuthority};
    let home = Path::new("/fixture/home");
    let prefix = Path::new("/fixture/npm");
    let scripts_auth = NpmWriteAuthority {
        allow_remove: true,
        registry: Some(NpmRegistryAuthority {
            url: "https://registry.npmjs.org".into(),
            scope_url: None,
            allow_scripts: true,
        }),
    };
    assert_eq!(
        expected_npm_view_args(home, prefix, "@scope/demo", Some(&scripts_auth))?[6],
        "--@scope:registry=https://registry.npmjs.org"
    );
    let scripts_args = expected_npm_write_args(
        home,
        prefix,
        "plain",
        Some(&scripts_auth),
        Action::Install,
        Some("1.0.0"),
    )?;
    assert!(!scripts_args.iter().any(|a| a == "--ignore-scripts"));
    assert_eq!(write_warnings(None, Action::Upgrade).len(), 0);
    assert_eq!(
        write_warnings(Some(&scripts_auth), Action::Uninstall).len(),
        0
    );
    assert_eq!(
        write_warnings(Some(&scripts_auth), Action::Upgrade).len(),
        2
    );
    let mut no_scripts = scripts_auth;
    if let Some(r) = &mut no_scripts.registry {
        r.allow_scripts = false;
    }
    assert_eq!(write_warnings(Some(&no_scripts), Action::Upgrade).len(), 1);
    Ok(())
}

#[test]
fn npm_missing_permissions_and_canonical_path_binding_fail_closed() -> TestResult {
    use crate::domain::NpmWriteAuthority;
    let home = Path::new("/fixture/home");
    let prefix = Path::new("/fixture/npm");
    let remove_only = NpmWriteAuthority {
        allow_remove: true,
        registry: None,
    };
    assert_eq!(
        expected_npm_view_args(home, prefix, "plain", Some(&remove_only))
            .err()
            .ok_or("v")?
            .code,
        "registry_authority_required"
    );
    assert_eq!(
        expected_npm_write_args(
            home,
            prefix,
            "plain",
            Some(&remove_only),
            Action::Upgrade,
            Some("1.0.0")
        )
        .err()
        .ok_or("w")?
        .code,
        "registry_authority_required"
    );
    assert_eq!(
        expected_npm_write_args(
            home,
            prefix,
            "plain",
            Some(&remove_only),
            Action::Cleanup,
            None
        )
        .err()
        .ok_or("c")?
        .code,
        "operation_unsupported"
    );
    assert_eq!(
        expected_npm_write_args(home, prefix, "plain", None, Action::Cleanup, None)
            .err()
            .ok_or("nc")?
            .code,
        "operation_unsupported"
    );
    let no_remove = NpmWriteAuthority {
        allow_remove: false,
        registry: None,
    };
    assert_eq!(
        expected_npm_write_args(
            home,
            prefix,
            "plain",
            Some(&no_remove),
            Action::Uninstall,
            None
        )
        .err()
        .ok_or("r")?
        .code,
        "remove_authority_required"
    );
    Ok(())
}

#[test]
fn npm_canonical_path_binding_and_invalid_inputs_fail_closed() -> TestResult {
    use crate::domain::NpmWriteAuthority;
    let home = Path::new("/fixture/home");
    let prefix = Path::new("/fixture/npm");
    let remove_only = NpmWriteAuthority {
        allow_remove: true,
        registry: None,
    };
    let mut cmd = CommandSpec {
        program: "/bin/npm".into(),
        args: expected_npm_write_args(
            home,
            Path::new("/alias/npm"),
            "plain",
            Some(&remove_only),
            Action::Uninstall,
            None,
        )?,
    };
    bind_canonical_command_paths(&mut cmd, prefix)?;
    assert_eq!(cmd.args[3], "/fixture/npm");
    assert_eq!(cmd.args[7], "/fixture/npm/etc/npmrc");
    let mut legacy_cmd = CommandSpec {
        program: "/bin/npm".into(),
        args: expected_npm_write_args(
            home,
            Path::new("/alias/npm"),
            "plain",
            None,
            Action::Uninstall,
            None,
        )?,
    };
    bind_canonical_command_paths(&mut legacy_cmd, prefix)?;
    assert_eq!(legacy_cmd.args[3], "/fixture/npm");
    assert!(
        bind_canonical_command_paths(
            &mut CommandSpec {
                program: "/bin/npm".into(),
                args: vec!["x".into()]
            },
            prefix
        )
        .is_err()
    );
    assert!(
        bind_canonical_command_paths(
            &mut CommandSpec {
                program: "/bin/npm".into(),
                args: vec![
                    "u".into(),
                    "-g".into(),
                    "-p".into(),
                    "/p".into(),
                    "-u".into(),
                    "/u".into(),
                    "--globalconfig".into()
                ]
            },
            prefix
        )
        .is_err()
    );
    let non_utf8 = std::path::PathBuf::from(
        <std::ffi::OsString as std::os::unix::ffi::OsStringExt>::from_vec(
            b"/fixture/\xff".to_vec(),
        ),
    );
    assert!(bind_canonical_command_paths(&mut cmd, &non_utf8).is_err());
    Ok(())
}

#[test]
fn npm_write_args_reject_invalid_names_paths_and_moving_tags() {
    use crate::domain::NpmWriteAuthority;
    let home = Path::new("/fixture/home");
    let prefix = Path::new("/fixture/npm");
    let remove_only = NpmWriteAuthority {
        allow_remove: true,
        registry: None,
    };
    assert!(expected_npm_write_args(home, prefix, "--bad", None, Action::Uninstall, None).is_err());
    assert!(
        expected_npm_write_args(
            Path::new("rel"),
            prefix,
            "plain",
            Some(&remove_only),
            Action::Uninstall,
            None
        )
        .is_err()
    );
    let bad_reg_auth = crate::domain::NpmWriteAuthority {
        allow_remove: true,
        registry: Some(crate::domain::NpmRegistryAuthority {
            url: "http://bad.invalid".into(),
            scope_url: None,
            allow_scripts: false,
        }),
    };
    assert!(
        expected_npm_write_args(
            home,
            prefix,
            "plain",
            Some(&bad_reg_auth),
            Action::Uninstall,
            None
        )
        .is_err()
    );
    assert!(
        expected_npm_write_args(
            home,
            prefix,
            "plain",
            Some(&remove_only),
            Action::Upgrade,
            None
        )
        .is_err()
    );
    assert!(
        expected_npm_write_args(
            home,
            prefix,
            "plain",
            Some(&remove_only),
            Action::Upgrade,
            Some("latest")
        )
        .is_err()
    );
}

fn authorized_upgrade_journal() -> crate::error::Result<crate::domain::Journal> {
    use crate::domain::{
        ConfigFileStamp, NpmConfigBindings, NpmRegistryAuthority, NpmWriteAuthority,
    };
    let home = Path::new("/fixture/home");
    let prefix = Path::new("/fixture/npm");
    let auth = NpmWriteAuthority {
        allow_remove: true,
        registry: Some(NpmRegistryAuthority {
            url: "https://registry.npmjs.org".into(),
            scope_url: Some("https://npm.pkg.example.test/org".into()),
            allow_scripts: false,
        }),
    };
    let valid_stamp = ConfigFileStamp {
        identity: id(),
        mode: 0o600,
        content_digest: "a".repeat(64),
        fingerprint: "b".repeat(64),
    };
    let mut j = typed_install()?;
    j.home = home.into();
    j.scope.home = home.into();
    j.scope_sha256 = fingerprint(&j.scope)?;
    j.plan.package.provider = Provider::Npm {
        prefix: prefix.into(),
        package: "@scope/demo".into(),
        authority: Some(auth.clone()),
    };
    if let Some(QuerySpec::LocalNpm(q)) = &mut j.plan.query {
        q.prefix = prefix.into();
        q.name = "@scope/demo".into();
        q.namespace = NpmNamespace {
            canonical_prefix: prefix.into(),
            anchor: NpmAnchor::ExistingPrefix {
                path: prefix.into(),
                identity: id(),
            },
        };
        q.config_bindings = Some(NpmConfigBindings {
            userconfig: Some(valid_stamp),
            globalconfig: None,
        });
        j.plan.before = Observation::Present {
            version: "1.0.0".into(),
            evidence: Some(evidence(q, Some(id()))?),
        };
    }
    j.plan.action = Action::Upgrade;
    j.plan.target = Some("1.2.3".into());
    j.plan.command = Some(CommandSpec {
        program: "/fixture/bin/npm".into(),
        args: expected_npm_write_args(
            home,
            prefix,
            "@scope/demo",
            Some(&auth),
            Action::Upgrade,
            Some("1.2.3"),
        )?,
    });
    j.plan.recipe_sha256 = fingerprint(&j.plan.package)?;
    j.plan_sha256 = fingerprint(&j.plan)?;
    Ok(j)
}

#[test]
fn npm_authorized_journal_and_config_bindings_reject_tampering() -> TestResult {
    use crate::domain::{ConfigFileStamp, NpmConfigBindings};
    let j = authorized_upgrade_journal()?;
    validate_plan(&j.plan)?;
    validate_journal_home(&j)?;

    let mut wrong_home = j.clone();
    wrong_home.home = "/other/home".into();
    wrong_home.scope.home = "/other/home".into();
    wrong_home.scope_sha256 = fingerprint(&wrong_home.scope)?;
    assert!(crate::policy::state::validate_journal(&wrong_home).is_err());

    let mut short_auth_cmd = j.plan.clone();
    if let Some(c) = &mut short_auth_cmd.command {
        c.args = vec!["install".into()];
    }
    assert!(validate_plan(&short_auth_cmd).is_err());

    let mut bad_pkg_auth = j.plan.clone();
    if let Provider::Npm {
        authority: Some(a), ..
    } = &mut bad_pkg_auth.package.provider
        && let Some(r) = &mut a.registry
    {
        r.url = "http://bad.invalid".into();
    }
    assert!(
        binding(
            &bad_pkg_auth.package,
            bad_pkg_auth.query.as_ref().ok_or("q")?
        )
        .is_err()
    );

    let mut wrong_usercfg = j.plan.clone();
    if let Some(c) = &mut wrong_usercfg.command {
        c.args[5] = "/fixture/home/other.rc".into();
    }
    assert!(validate_plan(&wrong_usercfg).is_err());

    let mut missing_bindings = j.plan.clone();
    if let Some(QuerySpec::LocalNpm(q)) = &mut missing_bindings.query {
        q.config_bindings = None;
    }
    assert!(
        binding(
            &missing_bindings.package,
            missing_bindings.query.as_ref().ok_or("q")?
        )
        .is_err()
    );

    let mut bad_stamp_plan = j.plan.clone();
    if let Some(QuerySpec::LocalNpm(q)) = &mut bad_stamp_plan.query {
        q.config_bindings = Some(NpmConfigBindings {
            userconfig: Some(ConfigFileStamp {
                identity: id(),
                mode: 0o77777,
                content_digest: "a".repeat(64),
                fingerprint: "b".repeat(64),
            }),
            globalconfig: None,
        });
    }
    assert!(
        binding(
            &bad_stamp_plan.package,
            bad_stamp_plan.query.as_ref().ok_or("q")?
        )
        .is_err()
    );
    assert!(
        binding(
            &j.plan.package,
            &QuerySpec::Command(CommandSpec {
                program: "/bin/npm".into(),
                args: vec!["ls".into()]
            })
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn npm_recipe_binding_intent_requires_a_verified_local_adoption() -> TestResult {
    let mut base = typed_install()?;
    assert!(
        serde_json::to_value(&base.plan)?
            .get("npm_binding")
            .is_none()
    );
    assert!(validate_binding_journal(&base).is_ok());
    base.phase = Phase::Prepared;
    base.process_group = None;
    base.plan.action = Action::Adopt;
    base.plan.command = None;
    base.plan.npm_binding = Some(crate::domain::NpmBindingProof {
        old_receipt_sha256: "a".repeat(64),
    });
    let Provider::Npm { authority, .. } = &mut base.plan.package.provider else {
        return Err("expected declared npm package".into());
    };
    *authority = Some(crate::domain::NpmWriteAuthority {
        allow_remove: true,
        registry: None,
    });
    let query = local(&mut base.plan.query)?;
    query.namespace.anchor = NpmAnchor::ExistingPrefix {
        path: query.namespace.canonical_prefix.clone(),
        identity: id(),
    };
    query.config_bindings = Some(crate::domain::NpmConfigBindings {
        userconfig: None,
        globalconfig: None,
    });
    let before = evidence(query, Some(id()))?;
    base.plan.package.id = before.namespace.clone();
    base.plan.before = Observation::Present {
        version: "1.0.0".into(),
        evidence: Some(before),
    };
    validate_binding_journal(&base)?;
    validate_plan(&base.plan)?;

    for change in [
        "action",
        "phase",
        "process",
        "delegated",
        "async",
        "provider",
        "before",
        "target",
        "digest",
        "query",
        "anchor",
        "id",
        "invalid-namespace",
    ] {
        let mut bad = base.clone();
        match change {
            "action" => bad.plan.action = Action::Install,
            "phase" => bad.phase = Phase::Running,
            "process" => bad.process_group = Some(123),
            "delegated" => bad.plan.delegated = true,
            "async" => bad.plan.synchronous = false,
            "provider" => bad.plan.package.provider = package().provider,
            "before" => bad.plan.before = Observation::Missing { evidence: None },
            "target" => bad.plan.target = Some("2.0.0".into()),
            "digest" => {
                bad.plan
                    .npm_binding
                    .as_mut()
                    .ok_or("binding proof")?
                    .old_receipt_sha256 = "x".into();
            }
            "query" => bad.plan.query = None,
            "anchor" => {
                local(&mut bad.plan.query)?.namespace.anchor = NpmAnchor::CreatePrefix {
                    path: "/fixture".into(),
                    identity: id(),
                    missing_tail: vec!["new".into()],
                };
            }
            "id" => bad.plan.package.id = "npm:unrelated".into(),
            "invalid-namespace" => {
                use std::{ffi::OsString, os::unix::ffi::OsStringExt};
                local(&mut bad.plan.query)?.namespace.canonical_prefix =
                    OsString::from_vec(vec![b'/', 0xff]).into();
            }
            _ => return Err("unknown mutation".into()),
        }
        assert!(validate_binding_journal(&bad).is_err(), "{change}");
        if change == "digest" {
            bad.plan.recipe_sha256 = fingerprint(&bad.plan.package)?;
            bad.plan_sha256 = fingerprint(&bad.plan)?;
            let error = crate::policy::state::validate_journal(&bad)
                .err()
                .ok_or("journal accepted invalid binding proof")?;
            assert_eq!(error.code, "invalid_npm_plan");
        }
    }
    Ok(())
}
