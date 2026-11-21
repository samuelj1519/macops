#![cfg(test)]
use super::*;
use crate::{domain::*, test_support::*};

fn args(template: Template) -> Vec<String> {
    match template {
        Template::Exact(c) => c.args,
        Template::Named { args, .. } => args,
    }
}

#[test]
fn unbound_formula_mutations_reject_and_cask_templates_keep_scope() -> TestResult {
    let home = Path::new("/fixture/home");
    let mut p = package();
    for cask in [false, true] {
        p.provider = Provider::Brew {
            package: "demo".into(),
            cask,
        };
        for (action, verb) in [
            (Action::Install, "install"),
            (Action::Upgrade, "upgrade"),
            (Action::Uninstall, "uninstall"),
        ] {
            if cask {
                assert_eq!(
                    args(mutation(home, false, &p, action, None)?),
                    vec![verb, "--cask", "demo"]
                );
            } else {
                assert_eq!(
                    mutation(home, false, &p, action, None)
                        .err()
                        .ok_or("unbound formula")?
                        .code,
                    "brew_context_binding_required"
                );
            }
        }
        assert!(mutation(home, false, &p, Action::Install, Some("1.0.0")).is_err());
        assert!(mutation(home, false, &p, Action::Update, None).is_err());
    }
    Ok(())
}

#[test]
fn native_mutation_templates_bind_target_scope_and_supported_actions() -> TestResult {
    let home = Path::new("/fixture/home");
    let mut p = package();
    p.provider = Provider::Mise {
        package: "npm:demo".into(),
    };
    for action in [Action::Install, Action::Upgrade] {
        assert_eq!(
            args(mutation(home, false, &p, action, Some("1.0.0"))?),
            vec!["use", "--global", "--pin", "npm:demo@1.0.0"]
        );
    }
    for (action, version, owned) in [
        (Action::Uninstall, Some("1.0.0"), false),
        (Action::Install, None, false),
        (Action::Adopt, Some("1.0.0"), false),
        (Action::Install, Some("1.0.0"), true),
    ] {
        assert!(mutation(home, owned, &p, action, version).is_err());
    }
    for (provider, install, remove) in [
        (
            Provider::Uv {
                binding: None,
                package: "demo".into(),
            },
            vec!["tool", "install", "demo==1.0.0"],
            vec!["tool", "uninstall", "demo"],
        ),
        (
            Provider::Npm {
                package: "demo".into(),
                prefix: "/prefix".into(),
                authority: None,
            },
            vec!["install", "--global", "--prefix", "/prefix", "demo@1.0.0"],
            vec!["uninstall", "--global", "--prefix", "/prefix", "demo"],
        ),
        (
            Provider::Bun {
                package: "demo".into(),
            },
            vec!["add", "--global", "demo@1.0.0"],
            vec!["remove", "--global", "demo"],
        ),
        (
            Provider::Cargo {
                package: "demo".into(),
                binary: "demo".into(),
            },
            vec![
                "install",
                "demo",
                "--version",
                "1.0.0",
                "--locked",
                "--root",
                "/fixture/home/.cargo",
            ],
            vec!["uninstall", "demo", "--root", "/fixture/home/.cargo"],
        ),
    ] {
        p.provider = provider;
        if crate::policy::uv_local::is_uv(&p.provider) {
            uv_mutations_blocked(home, &p);
            continue;
        }
        for action in [Action::Install, Action::Upgrade] {
            assert_eq!(
                args(mutation(home, false, &p, action, Some("1.0.0"))?),
                install
            );
        }
        assert_eq!(
            args(mutation(home, false, &p, Action::Uninstall, None)?),
            remove
        );
        assert!(mutation(home, false, &p, Action::Install, None).is_err());
        assert!(mutation(home, false, &p, Action::Cleanup, None).is_err());
    }
    p.provider = Provider::Macops;
    assert!(mutation(home, false, &p, Action::Install, None).is_err());
    Ok(())
}

#[test]
fn relative_npm_prefix_is_rejected_for_mutations() {
    let mut p = package();
    p.provider = Provider::Npm {
        package: "demo".into(),
        prefix: "relative".into(),
        authority: None,
    };
    assert!(
        mutation(
            Path::new("/fixture/home"),
            false,
            &p,
            Action::Install,
            Some("1.0.0")
        )
        .is_err()
    );
}

#[test]
fn custom_templates_expand_version_as_an_argument_not_shell() -> TestResult {
    let mut p = package();
    if let Provider::Command { install, .. } = &mut p.provider {
        *install = Some(CommandSpec {
            program: "/installer".into(),
            args: vec!["--version={version}".into()],
        });
    }
    assert_eq!(
        args(mutation(
            Path::new("/home"),
            false,
            &p,
            Action::Install,
            Some("1.0.0")
        )?),
        vec!["--version=1.0.0"]
    );
    assert!(mutation(Path::new("/home"), false, &p, Action::Install, None).is_err());
    assert!(mutation(Path::new("/home"), false, &p, Action::Update, None).is_err());
    p = package();
    for action in [Action::Upgrade, Action::Uninstall] {
        assert_eq!(
            args(mutation(
                Path::new("/home"),
                false,
                &p,
                action,
                Some("1.0.0")
            )?)
            .len(),
            1
        );
    }
    p.provider = Provider::Macops;
    assert!(custom_spec(&p, Action::Install, Some("1.0.0")).is_err());
    Ok(())
}

#[test]
fn query_templates_only_use_read_interfaces() -> TestResult {
    let home = Path::new("/fixture/home");
    let mut p = package();
    assert_eq!(args(query(home, &p)?), vec!["query"]);
    for (provider, expected) in [
        (
            Provider::Brew {
                package: "demo".into(),
                cask: false,
            },
            vec!["list", "--versions", "--formula", "demo"],
        ),
        (
            Provider::Brew {
                package: "demo".into(),
                cask: true,
            },
            vec!["list", "--versions", "--cask", "demo"],
        ),
        (
            Provider::Mise {
                package: "demo".into(),
            },
            vec!["ls", "--installed", "--json", "demo"],
        ),
        (
            Provider::Uv {
                binding: None,
                package: "demo".into(),
            },
            vec!["tool", "list"],
        ),
        (
            Provider::Bun {
                package: "demo".into(),
            },
            vec!["pm", "ls", "-g"],
        ),
        (
            Provider::Cargo {
                package: "demo".into(),
                binary: "demo".into(),
            },
            vec!["install", "--list", "--root", "/fixture/home/.cargo"],
        ),
    ] {
        p.provider = provider;
        if crate::policy::uv_local::is_uv(&p.provider) {
            assert_eq!(
                query(home, &p).err().map(|e| e.code),
                Some("uv_local_query_required".into())
            );
            continue;
        }
        assert_eq!(args(query(home, &p)?), expected);
    }
    p.provider = Provider::Macops;
    assert!(query(home, &p).is_err());
    p.provider = Provider::Npm {
        package: "demo".into(),
        prefix: "relative".into(),
        authority: None,
    };
    assert!(query(home, &p).is_err());
    Ok(())
}

#[test]
fn mutation_never_passes_a_moving_version_tag() {
    assert!(
        mutation(
            Path::new("/home"),
            false,
            &package(),
            Action::Install,
            Some("latest")
        )
        .is_err()
    );
}

#[test]
fn discovery_and_available_queries_do_not_become_writes() -> TestResult {
    for manager in ["brew", "mise", "uv", "cargo", "bun", "rustup", "conda"] {
        if manager == "uv" {
            assert_eq!(
                discovery_query(manager).err().map(|e| e.code),
                Some("uv_local_query_required".into())
            );
            continue;
        }
        let template = discovery_query(manager)?;
        assert!(matches!(template, Template::Named { .. }));
    }
    assert!(discovery_query("unknown").is_err());
    assert!(discovery_query("npm").is_err());
    let mut p = package();
    let home = Path::new("/fixture/home");
    assert!(available_query(home, &p).is_err());
    if let Provider::Command { available, .. } = &mut p.provider {
        *available = Some(command("available"));
    }
    assert_eq!(args(available_query(home, &p)?), vec!["available"]);
    for (provider, expected) in [
        (
            Provider::Brew {
                package: "demo".into(),
                cask: false,
            },
            vec!["info", "--json=v2", "--formula", "demo"],
        ),
        (
            Provider::Brew {
                package: "demo".into(),
                cask: true,
            },
            vec!["info", "--json=v2", "--cask", "demo"],
        ),
        (
            Provider::Mise {
                package: "demo".into(),
            },
            vec!["latest", "demo"],
        ),
        (
            Provider::Npm {
                package: "demo".into(),
                prefix: "/prefix".into(),
                authority: None,
            },
            vec!["view", "demo", "version", "--json"],
        ),
        (
            Provider::Bun {
                package: "demo".into(),
            },
            vec!["view", "demo", "version", "--json"],
        ),
    ] {
        p.provider = provider;
        assert_eq!(args(available_query(home, &p)?), expected);
    }
    p.provider = Provider::Npm {
        package: "demo".into(),
        prefix: "relative".into(),
        authority: None,
    };
    assert!(available_query(home, &p).is_err());
    Ok(())
}

#[test]
fn services_only_offer_declared_control_commands_and_identity_queries() -> TestResult {
    let p = crate::policy::service::package();
    assert_eq!(
        query(Path::new("/fixture/home"), &p)?,
        Template::Named {
            name: "brew".into(),
            args: vec![
                "services".into(),
                "info".into(),
                "postgresql@16".into(),
                "--json".into()
            ]
        }
    );
    for (action, verb) in [(Action::Start, "start"), (Action::Stop, "stop")] {
        assert_eq!(
            mutation(Path::new("/fixture/home"), false, &p, action, None)?,
            Template::Named {
                name: "brew".into(),
                args: vec!["services".into(), verb.into(), "postgresql@16".into()]
            }
        );
    }
    assert!(mutation(Path::new("/fixture/home"), false, &p, Action::Install, None).is_err());
    assert!(
        mutation(
            Path::new("/fixture/home"),
            false,
            &p,
            Action::Start,
            Some("1.0.0")
        )
        .is_err()
    );
    assert!(
        crate::policy::state::plan_inputs(
            &p,
            Action::Install,
            &Observation::Service {
                running: false,
                loaded: false,
                registered: false
            },
            None
        )
        .is_err()
    );
    assert!(
        crate::policy::state::plan_inputs(
            &package(),
            Action::Start,
            &Observation::Missing { evidence: None },
            None
        )
        .is_err()
    );
    Ok(())
}

fn uv_mutations_blocked(home: &Path, p: &Package) {
    for action in [Action::Install, Action::Upgrade, Action::Uninstall] {
        assert_eq!(
            mutation(home, false, p, action, Some("1.0.0"))
                .err()
                .map(|e| e.code),
            Some("uv_write_blocked_review".into())
        );
    }
}
