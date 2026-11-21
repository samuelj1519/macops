#![cfg(test)]
use super::*;
use crate::test_support::*;
use std::os::unix::ffi::OsStringExt;

#[test]
fn catalog_boundaries_and_unique_selection() -> TestResult {
    let p = package();
    let mut c = Catalog {
        schema_version: 1,
        packages: vec![p.clone()],
    };
    c.validate()?;
    assert_eq!(c.select("fixture:demo")?.id, p.id);
    assert_eq!(c.select("demo")?.id, p.id);
    assert_eq!(
        c.select("absent").err().ok_or("not found")?.code,
        "not_found"
    );
    for mutate in [0, 1, 2, 3, 4, 5, 6, 7] {
        let mut bad = c.clone();
        match mutate {
            0 => bad.schema_version = 2,
            1 => bad.packages[0].id = String::new(),
            2 => bad.packages[0].id = "bad name".into(),
            3 => bad.packages.push(p.clone()),
            4 => bad.packages[0].official_url = "http://example.org".into(),
            5 => bad.packages[0].official_url = "https://".into(),
            6 => {
                if let Provider::Command { query, .. } = &mut bad.packages[0].provider {
                    query.program = "relative".into();
                }
            }
            _ => {
                bad.packages[0].provider = Provider::Npm {
                    package: "--unsafe".into(),
                    prefix: "/fixture/prefix".into(),
                    authority: None,
                }
            }
        }
        assert!(bad.validate().is_err(), "{mutate}");
    }
    for name in ["", "bad name"] {
        let mut bad = c.clone();
        bad.packages[0].provider = Provider::Brew {
            package: name.into(),
            cask: false,
        };
        assert!(bad.validate().is_err());
    }
    let mut bad_npm_auth = c.clone();
    bad_npm_auth.packages[0].provider = Provider::Npm {
        package: "demo".into(),
        prefix: "/fixture/npm".into(),
        authority: Some(crate::domain::NpmWriteAuthority {
            allow_remove: true,
            registry: Some(crate::domain::NpmRegistryAuthority {
                url: "http://insecure.invalid".into(),
                scope_url: None,
                allow_scripts: false,
            }),
        }),
    };
    assert!(bad_npm_auth.validate().is_err());
    let mut other = p.clone();
    other.id = "other:demo".into();
    c.packages.push(other);
    assert_eq!(c.select("demo").err().ok_or("ambiguous")?.code, "ambiguous");
    let app = Package {
        id: "app:demo".into(),
        name: "DemoApp".into(),
        kind: Kind::App,
        official_url: String::new(),
        provider: Provider::ObservedApp {
            path: "/fixture/Demo.app".into(),
            bundle_id: "com.example.demo".into(),
            historical_version: "1".into(),
            owner: String::new(),
            source: String::new(),
            uninstaller: None,
        },
        paths: vec![],
        exact_version_required: false,
    };
    c.packages = vec![app];
    c.validate()?;
    assert_eq!(c.select("com.example.demo")?.id, "app:demo");
    assert_eq!(c.select("Demo.app")?.id, "app:demo");
    Ok(())
}

#[test]
fn observation_index_and_serialization_contracts() -> TestResult {
    assert_eq!(Observation::Missing { evidence: None }.version(), None);
    assert_eq!(
        Observation::Unknown {
            reason: "unknown".into()
        }
        .version(),
        None
    );
    assert_eq!(
        Observation::Present {
            evidence: None,
            version: "1".into()
        }
        .version(),
        Some("1")
    );
    assert_eq!(fingerprint(&package())?, fingerprint(&package())?);
    let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![255]));
    assert!(fingerprint(&invalid).is_err());
    let good = Index::empty();
    good.validate()?;
    let mut bad = Index::empty();
    bad.schema_version = 2;
    assert!(bad.validate().is_err());
    bad.schema_version = 1;
    bad.format = "other".into();
    assert!(bad.validate().is_err());
    Ok(())
}

#[test]
fn legacy_scope_serialization_stays_identical_and_service_kind_is_checked() -> TestResult {
    let old = r#"{"home":"/fixture/home","paths":{}}"#;
    let scope: Scope = serde_json::from_str(old)?;
    assert!(scope.service_identity.is_none());
    assert_eq!(serde_json::to_string(&scope)?, old);
    let mut p = crate::policy::service::package();
    let validate = |p: Package| {
        Catalog {
            schema_version: 1,
            packages: vec![p],
        }
        .validate()
    };
    validate(p.clone())?;
    p.kind = Kind::Cli;
    assert!(validate(p.clone()).is_err());
    p.kind = Kind::Service;
    p.provider = Provider::Command {
        query: command("query"),
        available: None,
        install: None,
        upgrade: None,
        uninstall: None,
        synchronous: true,
    };
    assert!(validate(p).is_err());
    let mut p = crate::policy::service::package();
    p.provider = Provider::Service {
        formula: "other".into(),
    };
    assert!(validate(p).is_err());
    assert_eq!(
        Observation::Service {
            running: false,
            loaded: false,
            registered: false
        }
        .version(),
        None
    );
    let cfg_target = PathBuf::from("/fixture/home/.zshrc");
    let cfg = Package {
        id: crate::policy::config_local::config_id(&cfg_target)?,
        name: cfg_target.display().to_string(),
        kind: Kind::Config,
        official_url: String::new(),
        provider: Provider::ConfigObserved {
            target: cfg_target,
            source: None,
            authority: ConfigAuthority::Untracked,
        },
        paths: vec![],
        exact_version_required: false,
    };
    validate(cfg.clone())?;
    let mut bad_cfg = cfg;
    bad_cfg.kind = Kind::Cli;
    assert!(validate(bad_cfg).is_err());
    assert_eq!(
        Observation::ConfigFile {
            authority: ConfigAuthority::Untracked,
            target: None,
            source: None,
            mode_matches_source: None,
        }
        .version(),
        None
    );
    assert_eq!(
        Observation::CapabilityPack {
            capability: CapabilityKind::SkillsCli,
            pack: None,
        }
        .version(),
        None
    );
    let mut bad_cap = Package {
        id: "capability:invalid".into(),
        name: "demo".into(),
        kind: Kind::Cli,
        official_url: String::new(),
        provider: Provider::CapabilityObserved {
            capability: CapabilityKind::SkillsCli,
            native_id: "demo".into(),
            manager: "skills-cli:github".into(),
            record: "/fixture/lock.json".into(),
            payload: "/fixture/demo".into(),
            source: Some("o/r".into()),
            profile_id: None,
            settings: None,
        },
        paths: vec![],
        exact_version_required: false,
    };
    assert!(validate(bad_cap.clone()).is_err());
    bad_cap.kind = Kind::Skill;
    assert!(validate(bad_cap).is_err());
    Ok(())
}

#[test]
fn scope_service_identity_requires_an_object_and_preserves_the_native_shape() -> TestResult {
    let object = serde_json::json!({"home":"/fixture/home","paths":{},"service_identity":{"name":"fixture","uid":501}});
    let scope: Scope = serde_json::from_value(object.clone())?;
    assert_eq!(scope.service_identity.as_ref().map(|v| v.uid), Some(501));
    let mut array = object;
    array["service_identity"] = serde_json::json!(["fixture", 501]);
    assert!(serde_json::from_value::<Scope>(array).is_err());
    Ok(())
}
