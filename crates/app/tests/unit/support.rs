#![cfg(test)]
use crate::domain::*;
use std::{collections::BTreeMap, path::PathBuf};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;
pub(crate) struct PluginTargetFixture;
pub fn package() -> Package {
    Package {
        id: "fixture:demo".into(),
        name: "demo".into(),
        kind: Kind::Cli,
        official_url: "https://example.org/demo".into(),
        provider: Provider::Command {
            query: command("query"),
            available: None,
            install: Some(command("install")),
            upgrade: Some(command("upgrade")),
            uninstall: Some(command("uninstall")),
            synchronous: true,
        },
        paths: vec![],
        exact_version_required: false,
    }
}
pub fn command(argument: &str) -> CommandSpec {
    CommandSpec {
        program: "/fixture/bin/demo".into(),
        args: vec![argument.into()],
    }
}
pub fn scope() -> Scope {
    Scope {
        home: "/fixture/home".into(),
        paths: BTreeMap::new(),
        service_identity: None,
    }
}
pub fn journal() -> Result<Journal, crate::error::Error> {
    let p = package();
    let scope = scope();
    let plan = Plan {
        brew: None,
        npm_binding: None,
        uv_owner: None,
        timeout_secs: 5,
        package: p.clone(),
        action: Action::Install,
        before: Observation::Missing { evidence: None },
        target: Some("1.0.0".into()),
        command: Some(command("install")),
        query: Some(command("query").into()),
        recipe_sha256: fingerprint(&p)?,
        warnings: vec![],
        synchronous: true,
        delegated: false,
    };
    Ok(Journal {
        schema_version: 1,
        id: "OP-1".into(),
        phase: Phase::Running,
        plan_sha256: fingerprint(&plan)?,
        plan,
        scope_sha256: fingerprint(&scope)?,
        home: scope.home.clone(),
        scope,
        state: "/fixture/state".into(),
        cache: "/fixture/cache".into(),
        settings: "/fixture/settings".into(),
        process_group: Some(999),
        started_at: 1,
        message: String::new(),
    })
}
pub fn output(text: &str) -> ProcessOutput {
    ProcessOutput {
        code: Some(0),
        stdout: text.into(),
        stderr: String::new(),
        uncertain: false,
    }
}
pub fn identity() -> Identity {
    Identity {
        root: FileId {
            device: 1,
            inode: 2,
        },
        indexes: FileId {
            device: 1,
            inode: 3,
        },
        scope_id: "a".repeat(64),
    }
}
pub fn book() -> Book {
    Book {
        path: PathBuf::from("/fixture/cache"),
        identity: identity(),
        current: None,
        owned: std::collections::BTreeSet::default(),
        queried_at: BTreeMap::default(),
    }
}

#[path = "brew_support.rs"]
pub(crate) mod brew;
