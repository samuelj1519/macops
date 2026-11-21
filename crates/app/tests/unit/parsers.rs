#![cfg(test)]
use super::*;
use crate::{domain::*, test_support::*};
use serde_json::json;

fn parsed(provider: Provider, text: &str) -> crate::error::Result<Observation> {
    let mut p = package();
    p.provider = provider;
    parse_observation(&p, &output(text))
}

#[test]
fn native_observation_schemas_reject_unknown_and_ambiguous_data() -> TestResult {
    let mut out = output("{}");
    out.uncertain = true;
    assert!(parse_observation(&package(), &out).is_err());
    out.uncertain = false;
    out.code = Some(9);
    assert!(parse_observation(&package(), &out).is_err());
    assert!(parsed(package().provider, r#"{"status":"present","version":""}"#).is_err());
    assert_eq!(
        parsed(package().provider, r#"{"status":"missing"}"#)?,
        Observation::Missing { evidence: None }
    );
    assert!(parsed(package().provider, "invalid-json").is_err());
    let brew = Provider::Brew {
        package: "demo".into(),
        cask: false,
    };
    for text in ["demo 1.0.0", "demo 1.0.0\n"] {
        assert_eq!(parsed(brew.clone(), text)?.version(), Some("1.0.0"));
    }
    assert_eq!(
        parsed(brew.clone(), "")?,
        Observation::Missing { evidence: None }
    );
    for text in ["other 1.0.0", "demo 1 2", "demo 1\ndemo 2"] {
        assert!(parsed(brew.clone(), text).is_err());
    }
    let mut p = package();
    p.provider = brew;
    let mut empty = output("");
    empty.code = Some(1);
    assert_eq!(
        parse_observation(&p, &empty)?,
        Observation::Missing { evidence: None }
    );
    empty.stderr = "permission denied".into();
    assert!(parse_observation(&p, &empty).is_err());
    let mise = Provider::Mise {
        package: "npm:demo".into(),
    };
    for text in [
        r#"[{"version":"1.0.0","active":true}]"#,
        r#"[{"version":"1.0.0"}]"#,
        r#"{"demo":[{"version":"1.0.0"}]}"#,
        r#"{"npm:demo":[{"version":"1.0.0","active":true}]}"#,
    ] {
        assert_eq!(parsed(mise.clone(), text)?.version(), Some("1.0.0"));
    }
    for text in [
        r#"{"other":[]}"#,
        r#"[{"version":"1","active":true},{"version":"2","active":true}]"#,
        r#"[{"version":"1"},{"version":"2"}]"#,
        r#"[{"version":""}]"#,
    ] {
        assert!(parsed(mise.clone(), text).is_err());
    }
    assert_eq!(parsed(mise, "[]")?, Observation::Missing { evidence: None });
    assert!(parsed(Provider::Macops, "{}").is_err());
    Ok(())
}

#[test]
fn text_native_inventories_have_complete_grammar() -> TestResult {
    for provider in [
        Provider::Uv {
            binding: None,
            package: "demo".into(),
        },
        Provider::Cargo {
            package: "demo".into(),
            binary: "demo".into(),
        },
    ] {
        let cargo = matches!(provider, Provider::Cargo { .. });
        let good = if cargo {
            "demo v1.0.0:\n    demo\n"
        } else {
            "demo v1.0.0\n    - demo\n"
        };
        assert_eq!(parsed(provider.clone(), good)?.version(), Some("1.0.0"));
        assert_eq!(
            parsed(provider.clone(), "")?,
            Observation::Missing { evidence: None }
        );
        for bad in [
            "not an inventory",
            "    orphan",
            "demo invalid:",
            "demo vlatest:",
        ] {
            assert!(parsed(provider.clone(), bad).is_err());
        }
        let duplicate = if cargo {
            "demo v1.0.0:\ndemo v2.0.0:"
        } else {
            "demo v1.0.0\ndemo v2.0.0"
        };
        assert!(parsed(provider.clone(), duplicate).is_err());
        assert_eq!(
            parsed(
                provider,
                if cargo {
                    "other v1.0.0:"
                } else {
                    "other v1.0.0"
                }
            )?,
            Observation::Missing { evidence: None }
        );
    }
    let provider = Provider::Bun {
        package: "@scope/demo".into(),
    };
    assert_eq!(
        parsed(
            provider.clone(),
            "/global node_modules (1)\n└── @scope/demo@1.0.0"
        )?
        .version(),
        Some("1.0.0")
    );
    assert_eq!(
        parsed(provider.clone(), "/global node_modules (0)")?,
        Observation::Missing { evidence: None }
    );
    assert_eq!(
        parsed(
            provider.clone(),
            "/global node_modules (4)\n├── other@2.0.0\n└── extra@3.0.0"
        )?,
        Observation::Missing { evidence: None }
    );
    for bad in [
        "",
        "node_modules corrupted",
        "/global node_modules (",
        "/global node_modules (x)",
        "relative node_modules (0)",
        "/global node_modules ()",
        "/global node_modules (1)",
        "/global node_modules (0)\n└── demo@1",
        "/global node_modules (2)\n└── a@1\n└── b@2",
        "/global node_modules (1)\n└── no-version",
        "/global node_modules (1)\n└── @broken@1",
        "/global node_modules (1)\n└── bad/name@1",
        "/global node_modules (2)\n├── Demo@1\n└── demo@2",
        "/global node_modules (1)\n└── demo@latest",
    ] {
        assert!(parsed(provider.clone(), bad).is_err(), "{bad}");
    }
    Ok(())
}

#[test]
fn version_metadata_and_exact_selection_preserve_native_meanings() -> TestResult {
    let mut p = package();
    for (provider, text, expected) in [
        (package().provider, r#"{"version":"1.0.0"}"#, "1.0.0"),
        (
            Provider::Npm {
                package: "demo".into(),
                prefix: "/prefix".into(),
                authority: None,
            },
            r#""1.0.0""#,
            "1.0.0",
        ),
        (
            Provider::Bun {
                package: "demo".into(),
            },
            r#""1.0.0""#,
            "1.0.0",
        ),
        (
            Provider::Mise {
                package: "demo".into(),
            },
            " 1.0.0\n",
            "1.0.0",
        ),
        (
            Provider::Brew {
                package: "demo".into(),
                cask: false,
            },
            r#"{"formulae":[{"name":"demo","revision":1,"versions":{"stable":"1.0.0"}}]}"#,
            "1.0.0_1",
        ),
        (
            Provider::Brew {
                package: "demo".into(),
                cask: true,
            },
            r#"{"casks":[{"token":"demo","version":"1.0.0"}]}"#,
            "1.0.0",
        ),
    ] {
        p.provider = provider;
        assert_eq!(available(&p, &output(text))?, expected);
    }
    p.provider = Provider::Brew {
        package: "demo".into(),
        cask: false,
    };
    for text in [
        "{}",
        r#"{"formulae":[]}"#,
        r#"{"formulae":[{},{}]}"#,
        r#"{"formulae":[{"name":"other"}]}"#,
        r#"{"formulae":[{"name":"demo"}]}"#,
        r#"{"formulae":[{"name":"demo","versions":{"stable":"1.0.0"},"revision":"1"}]}"#,
    ] {
        assert!(available(&p, &output(text)).is_err());
    }
    assert_eq!(
        available(
            &p,
            &output(r#"{"formulae":[{"name":"demo","versions":{"stable":"1.0.0"}}]}"#)
        )?,
        "1.0.0"
    );
    assert_eq!(
        available(
            &p,
            &output(r#"{"formulae":[{"name":"demo","versions":{"stable":"1.0.0"},"revision":0}]}"#)
        )?,
        "1.0.0"
    );
    let mut bad = output("");
    bad.code = Some(1);
    assert!(available(&p, &bad).is_err());
    bad.code = Some(0);
    bad.uncertain = true;
    assert!(available(&p, &bad).is_err());
    p.provider = Provider::Macops;
    assert!(available(&p, &output("{}")).is_err());
    Ok(())
}

#[test]
fn manifests_service_and_plist_validate_structure_before_health() -> TestResult {
    assert_eq!(
        tsv("name\tversion\ndemo\t1\n\n", &["name", "version"])?.len(),
        1
    );
    assert_eq!(tsv("name\tversion\n", &["name"])?, Vec::new());
    for bad in [
        "",
        "wrong\nvalue\n",
        "name\tname\nx\ty",
        "name\tversion\nx",
        "name\tversion\nx\t",
    ] {
        assert!(tsv(bad, &["name", "version"]).is_err());
    }
    assert_eq!(parse_conda(r#"{"envs":[]}"#)?, json!({"envs":[]}));
    assert!(parse_conda(r#"{"envs":"bad"}"#).is_err());
    assert!(!parse_service(r#"[{"name":"pg","running":false}]"#, "pg")?.running);
    for bad in [
        "[]",
        r#"[{"name":"other","running":true}]"#,
        r#"[{"name":"pg","running":true},{"name":"pg","running":false}]"#,
        r#"[{"name":"pg","running":"false"}]"#,
    ] {
        assert!(parse_service(bad, "pg").is_err());
    }
    let mut dictionary = plist::Dictionary::new();
    dictionary.insert(
        "CFBundleIdentifier".into(),
        plist::Value::String("com.demo".into()),
    );
    dictionary.insert(
        "CFBundleShortVersionString".into(),
        plist::Value::String("1.0.0".into()),
    );
    assert_eq!(
        app_plist(
            &plist::Value::Dictionary(dictionary.clone()),
            "com.demo",
            "app"
        )?
        .version(),
        Some("1.0.0")
    );
    assert!(
        app_plist(
            &plist::Value::String("not a dictionary".into()),
            "com.demo",
            "app"
        )
        .is_err()
    );
    assert!(
        app_plist(
            &plist::Value::Dictionary(dictionary.clone()),
            "other",
            "app"
        )
        .is_err()
    );
    dictionary.remove("CFBundleShortVersionString");
    assert!(app_plist(&plist::Value::Dictionary(dictionary), "com.demo", "app").is_err());
    Ok(())
}

#[test]
fn invalid_json_is_not_mistaken_for_empty_native_state() {
    for provider in [
        Provider::Mise {
            package: "demo".into(),
        },
        Provider::Npm {
            package: "demo".into(),
            prefix: "/prefix".into(),
            authority: None,
        },
    ] {
        assert!(parsed(provider, "invalid-json").is_err());
    }
    let mut p = package();
    p.provider = Provider::Npm {
        package: "demo".into(),
        prefix: "/prefix".into(),
        authority: None,
    };
    let mut out = output("invalid-json");
    out.code = Some(1);
    assert!(parse_observation(&p, &out).is_err());
    for provider in [
        package().provider,
        Provider::Npm {
            package: "demo".into(),
            prefix: "/prefix".into(),
            authority: None,
        },
        Provider::Bun {
            package: "demo".into(),
        },
        Provider::Brew {
            package: "demo".into(),
            cask: false,
        },
    ] {
        p.provider = provider;
        assert!(available(&p, &output("invalid-json")).is_err());
    }
}

#[test]
fn manager_asset_and_discovery_parsing_keep_unobserved_distinct() -> TestResult {
    assert_eq!(
        manager_observation(&output(" 1.0.0\n"))?.version(),
        Some("1.0.0")
    );
    assert!(manager_observation(&output(" ")).is_err());
    let mut bad = output("");
    bad.code = Some(1);
    assert!(manager_observation(&bad).is_err());
    bad.code = Some(0);
    bad.uncertain = true;
    assert!(manager_observation(&bad).is_err());
    assert_eq!(
        managed_asset(&json!({"assets":{"demo":{"revision":"commit"}}}), "demo").version(),
        Some("commit")
    );
    assert!(matches!(
        managed_asset(&json!({}), "demo"),
        Observation::Unknown { .. }
    ));
    for (manager, text) in [
        ("conda", r#"{"envs":[]}"#),
        ("mise", r#"{"demo":[{"version":"1"}]}"#),
        ("brew", r#"{"formulae":[],"casks":[]}"#),
        ("uv", "demo v1.0.0"),
    ] {
        let value = discovery(manager, &output(text))?;
        assert!(!value.is_null());
    }
    for manager in ["conda", "mise", "brew"] {
        assert!(discovery(manager, &output("broken")).is_err());
    }
    let mut bad = output("");
    bad.code = Some(1);
    assert!(discovery("uv", &bad).is_err());
    bad.code = Some(0);
    bad.uncertain = true;
    assert!(discovery("uv", &bad).is_err());
    Ok(())
}

#[test]
fn exact_versions_and_sync_capabilities() -> TestResult {
    for value in ["", "latest", "v1.0.0", "^1.0.0", "1 2", "1;rm"] {
        assert!(exact_version(value).is_err());
    }
    assert_eq!(exact_version("1.0.0-rc.1+build")?, "1.0.0-rc.1+build");
    let mut p = package();
    assert!(synchronous(&p));
    if let Provider::Command { synchronous, .. } = &mut p.provider {
        *synchronous = false;
    }
    assert!(!synchronous(&p));
    p.provider = Provider::Legacy {
        category: "skills".into(),
        selector: "demo".into(),
    };
    assert!(!synchronous(&p));
    p.provider = Provider::Brew {
        package: "demo".into(),
        cask: false,
    };
    assert!(synchronous(&p));

    Ok(())
}

#[test]
fn service_status_cannot_be_used_as_a_software_version_or_echo_values() -> TestResult {
    let p = package();
    let out = output(r#"{"status":"service","running":true,"loaded":true,"registered":true}"#);
    assert!(parse_observation(&p, &out).is_err());
    let error = parse_service(r#"[{"name":"pg","running":"SYNTHETIC_SECRET"}]"#, "pg")
        .err()
        .ok_or("error")?;
    assert!(!error.to_string().contains("SYNTHETIC_SECRET"));
    Ok(())
}
