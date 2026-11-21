#![cfg(test)]
use super::*;
use crate::{
    domain::{ConfigAuthority, Kind, Observation, Package, Provider},
    test_support::{TestResult, journal},
};
use serde_json::{Value, json};
use std::os::unix::ffi::OsStringExt;

fn stamp(path: PathBuf) -> Stamp {
    Stamp {
        path,
        identity: Some(FileId {
            device: 1,
            inode: 2,
        }),
        mode: 0o700,
        digest: "a".repeat(64),
    }
}
fn binding() -> Binding {
    let source = PathBuf::from("/fixture/home/source");
    let effective = source.join("home");
    let mut value = Binding {
        schema: "chezmoi-owner-v1".into(),
        scope: Scope {
            home: "/fixture/home".into(),
            paths: BTreeMap::new(),
            service_identity: None,
        },
        program: "/fixture/home/bin/chezmoi".into(),
        lookup: "/fixture/home/bin/chezmoi".into(),
        version: "chezmoi version v2.74.1".into(),
        configured_source: source,
        effective_source: effective,
        destination: "/fixture/home/dest".into(),
        config: "/fixture/home/chezmoi.toml".into(),
        cache: "/fixture/home/cache".into(),
        persistent_state: "/fixture/home/state.db".into(),
        record_state: "/fixture/home/macops-state".into(),
        anchors: vec![],
        inputs: vec![],
        effects: "configured-native-v1".into(),
        filtered: false,
    };
    value.anchors = [
        &value.program,
        &value.configured_source,
        &value.effective_source,
        &value.destination,
    ]
    .iter()
    .map(|p| stamp((*p).clone()))
    .collect();
    value.inputs = [
        value.config.clone(),
        value.configured_source.join(".chezmoiroot"),
        value.effective_source.join(".chezmoiignore"),
    ]
    .into_iter()
    .map(stamp)
    .collect();
    value
}
fn row(binding: &Binding) -> Value {
    json!({"absolute":binding.destination.join(".file"),"sourceAbsolute":binding.effective_source.join("dot_file.tmpl"),"sourceRelative":"dot_file.tmpl"})
}

#[test]
fn binding_requires_owner_namespace_and_selected_context_stamps() -> TestResult {
    let good = binding();
    validate(&good)?;
    for mutate in 0..12 {
        let mut bad = good.clone();
        match mutate {
            0 => bad.schema = "other".into(),
            1 => bad.effects = "zero-effects".into(),
            2 => bad.version = "unknown".into(),
            3 => bad.anchors.clear(),
            4 => bad.inputs.clear(),
            5 => bad.program = "relative".into(),
            6 => bad.effective_source = "/outside".into(),
            7 => {
                bad.anchors.remove(0);
            }
            8 => bad.anchors[0].path = "relative".into(),
            9 => bad.anchors[0].mode = 0o10000,
            10 => bad.anchors[0].digest = "bad".into(),
            _ => bad.anchors.push(bad.anchors[0].clone()),
        }
        assert!(validate(&bad).is_err(), "case {mutate}");
    }
    let mut extra = good.clone();
    extra.inputs.push(stamp(PathBuf::from("relative")));
    assert!(validate(&extra).is_err());
    for index in 0..3 {
        let mut bad = good.clone();
        bad.inputs.remove(index);
        assert!(validate(&bad).is_err());
    }
    let mut invalid_utf8 = good.clone();
    invalid_utf8.lookup = std::ffi::OsString::from_vec(b"/fixture/\xff".to_vec()).into();
    assert!(validate(&invalid_utf8).is_err());
    let mut missing = good;
    missing.anchors[0].identity = None;
    assert!(validate(&missing).is_err());
    Ok(())
}

#[test]
fn targets_never_fall_back_to_all_and_preserve_every_selection() -> TestResult {
    let binding = binding();
    let first = binding.destination.join(".a");
    let second = binding.destination.join(".b");
    selection(&binding, &[], true)?;
    assert!(selection(&binding, &[], false).is_err());
    selection(&binding, &[first.clone(), second], false)?;
    for targets in [
        vec!["relative".into()],
        vec!["/outside/a".into()],
        vec![binding.destination.clone()],
        vec![first.clone(), first],
        vec![binding.destination.join("line\nbreak")],
        vec![PathBuf::from(std::ffi::OsString::from_vec(
            b"/fixture/home/dest/\xff".to_vec(),
        ))],
    ] {
        assert!(selection(&binding, &targets, false).is_err());
    }
    assert!(relative(Path::new("a/b")));
    assert!(!relative(Path::new("")));
    assert!(!relative(Path::new("../a")));
    assert!(!relative(Path::new("/a")));
    Ok(())
}

#[test]
fn native_commands_use_root_once_explicit_recursion_and_no_force() -> TestResult {
    let binding = binding();
    let target = binding.destination.join(".a");
    let mut invalid_binding = binding.clone();
    invalid_binding.schema = "invalid".into();
    assert!(command(&invalid_binding, NativeCommand::Status, &[]).is_err());
    let configured = binding.configured_source.to_str().ok_or("utf8")?;
    for operation in [
        NativeCommand::Managed,
        NativeCommand::Status,
        NativeCommand::Apply,
        NativeCommand::Verify,
        NativeCommand::SourcePath,
    ] {
        let output = command(&binding, operation, std::slice::from_ref(&target))?;
        assert!(
            !output
                .args
                .iter()
                .any(|a| a == "--force" || a == "--dry-run")
        );
        assert!(
            output
                .args
                .windows(2)
                .any(|a| a == ["--source", configured])
        );
        assert_eq!(
            output.args.last(),
            target.to_str().map(String::from).as_ref()
        );
        if operation == NativeCommand::Apply {
            assert!(command(&binding, operation, &[]).is_err());
        } else {
            command(&binding, operation, &[])?;
        }
    }
    Ok(())
}

#[test]
fn managed_metadata_rejects_duplicates_escape_and_unbounded_or_wrong_shapes() -> TestResult {
    let binding = binding();
    let good = row(&binding);
    let encoded = json!({".file":good.clone()});
    assert_eq!(managed(&binding, &serde_json::to_vec(&encoded)?)?.len(), 1);
    for bytes in [
        b"[1]".as_slice(),
        b"null",
        b"not json",
        b"{\"a\":{},\"a\":{}}",
    ] {
        assert!(managed(&binding, bytes).is_err());
    }
    for index in 0..5 {
        let mut bad = good.clone();
        match index {
            0 => bad["absolute"] = json!("/outside/file"),
            1 => bad["absolute"] = json!(binding.destination.join(".other")),
            2 => bad["sourceAbsolute"] = json!("/other"),
            3 => {
                bad["sourceRelative"] = json!("../escape");
                bad["sourceAbsolute"] = json!(binding.effective_source.join("../escape"));
            }
            _ => bad["unexpected"] = json!(true),
        }
        assert!(managed(&binding, &serde_json::to_vec(&json!({".file":bad}))?).is_err());
    }
    assert!(
        managed(
            &binding,
            &serde_json::to_vec(&json!({"../file":good.clone()}))?
        )
        .is_err()
    );
    assert!(managed(&binding,&serde_json::to_vec(&json!({binding.destination.join(".file").to_string_lossy().into_owned():good.clone()}))?).is_err());
    let large = (0..4097)
        .map(|n| (n.to_string(), good.clone()))
        .collect::<BTreeMap<_, _>>();
    assert!(managed(&binding, &serde_json::to_vec(&large)?).is_err());
    Ok(())
}

#[test]
fn status_is_path_checked_summary_and_never_free_form_native_output() -> TestResult {
    let binding = binding();
    let path = binding.destination.join(".file");
    let allowed = vec![path.clone()];
    assert_eq!(status(&binding, b"", &allowed)?, []);
    for code in " AMDR?".chars() {
        let output = format!("{code}{code} {}\n", path.display());
        assert_eq!(status(&binding, output.as_bytes(), &allowed)?.len(), 1);
    }
    for bytes in [
        vec![0xff],
        b"x".to_vec(),
        format!(" X {}\n", path.display()).into_bytes(),
        format!("X  {}\n", path.display()).into_bytes(),
        format!(" A!{}\n", path.display()).into_bytes(),
        b" A /outside/file\n".to_vec(),
        format!(" A {0}\n A {0}\n", path.display()).into_bytes(),
    ] {
        assert!(status(&binding, &bytes, &allowed).is_err());
    }
    assert!(status(&binding, format!(" A {}\n", path.display()).as_bytes(), &[]).is_err());
    Ok(())
}

fn plan() -> Result<Plan> {
    let binding = binding();
    let target = binding.destination.join(".file");
    let mut plan = journal()?.plan;
    plan.package = Package {
        id: crate::policy::config_local::config_id(&target)?,
        name: "fixture".into(),
        kind: Kind::Config,
        official_url: "https://www.chezmoi.io/".into(),
        provider: Provider::ConfigObserved {
            target: target.clone(),
            source: Some(binding.effective_source.join("dot_file.tmpl")),
            authority: ConfigAuthority::ChezmoiNative,
        },
        paths: vec![],
        exact_version_required: false,
    };
    plan.action = Action::Apply;
    plan.synchronous = true;
    plan.delegated = false;
    plan.command = Some(command(
        &binding,
        NativeCommand::Apply,
        std::slice::from_ref(&target),
    )?);
    plan.before = Observation::Unknown {
        reason: "observed".into(),
    };
    plan.query = Some(QuerySpec::NativeConfig(Query {
        kind: QueryKind::NativeV1,
        source_adopt: None,
        source_forget: None,
        binding_sha256: "b".repeat(64),
        targets: vec![target],
    }));
    Ok(plan)
}

#[test]
fn native_plan_is_versioned_and_separate_from_legacy_restore() -> TestResult {
    let good = plan()?;
    assert!(is_native(&good));
    validate_plan(&good)?;
    crate::policy::config_local::validate_plan(&good)?;
    for index in 0..8 {
        let mut bad = good.clone();
        match index {
            0 => bad.action = Action::Restore,
            1 => bad.synchronous = false,
            2 => bad.delegated = true,
            3 => bad.command = None,
            4 => bad.package.id = "config:other".into(),
            _ => {
                let Some(QuerySpec::NativeConfig(query)) = &mut bad.query else {
                    return Err("query".into());
                };
                match index {
                    5 => query.binding_sha256 = "bad".into(),
                    6 => query.targets.clear(),
                    _ => query.targets.push("/extra".into()),
                }
            }
        }
        assert!(validate_plan(&bad).is_err());
    }
    let mut relative = good.clone();
    if let Some(QuerySpec::NativeConfig(query)) = &mut relative.query {
        query.targets[0] = "relative".into();
    }
    assert!(validate_plan(&relative).is_err());
    let mut wrong_package = good.clone();
    wrong_package.package.kind = Kind::Cli;
    assert!(crate::policy::config_local::validate_plan(&wrong_package).is_err());
    let mut old = good;
    old.query = None;
    assert!(!is_native(&old));
    validate_plan(&old)?;
    let query = json!({"kind":"chezmoi-native-v1","binding_sha256":"a".repeat(64),"targets":[],"extra":true});
    assert!(serde_json::from_value::<Query>(query).is_err());
    Ok(())
}

#[test]
fn native_query_cannot_claim_legacy_or_other_package_authority() -> TestResult {
    let value = plan()?;
    let query = value.query.as_ref().ok_or("query")?;
    let mut wrong = value.package.clone();
    wrong.kind = Kind::Cli;
    assert!(crate::policy::config_local::binding(&wrong, query).is_err());
    wrong = value.package.clone();
    wrong.provider = Provider::Macops;
    assert!(crate::policy::config_local::binding(&wrong, query).is_err());
    let old = Observation::ConfigFile {
        authority: ConfigAuthority::ChezmoiNative,
        target: Some(crate::domain::ConfigFileStamp {
            identity: FileId {
                device: 1,
                inode: 2,
            },
            mode: 0o600,
            content_digest: "a".repeat(64),
            fingerprint: "b".repeat(64),
        }),
        source: None,
        mode_matches_source: None,
    };
    assert!(crate::policy::config_local::adoptable_observation(&value.package, &old).is_err());
    Ok(())
}
