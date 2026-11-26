#![cfg(test)]
use super::*;
use crate::{capability::rules::delivery::Profile, domain::FileId, test_support::TestResult};
use std::{ffi::OsString, os::unix::ffi::OsStringExt};
fn dir(path: &str) -> DirectoryBinding {
    let identity = FileId {
        device: 1,
        inode: 2,
    };
    DirectoryBinding {
        requested: path.into(),
        canonical: path.into(),
        identity: identity.clone(),
        lineage: vec![identity],
        uid: 501,
        mode: 0o700,
    }
}
fn owner() -> Owner {
    Owner {
        profile: ConfigProfile::Claude(Profile::Mi),
        role: Role::Live,
        intent: Intent::Plugins,
    }
}
fn client() -> Client {
    Client {
        owner: owner(),
        live_root: dir("/home/.claude-mi"),
        live: "/home/.claude-mi/settings.json".into(),
        source: None,
    }
}
fn authority() -> Authority {
    let client = client();
    Authority {
        mapping_inputs: ConfigRootInputs {
            source: None,
            destination: None,
            config: None,
            env: BTreeMap::new(),
        },
        isolated: true,
        codex_root: None,
        targets: BTreeMap::from([(
            client.live.clone(),
            Target {
                clients: BTreeMap::from([(client.owner.id(), client)]),
            },
        )]),
    }
}
fn source() -> StaticSource {
    StaticSource {
        root: dir("/home/source"),
        destination: "/home".into(),
        relative: "dot_claude-mi/settings.json".into(),
    }
}
#[test]
fn static_source_proof_rejects_escape_templates_links_and_broken_directory_bindings() -> TestResult
{
    assert_eq!(
        source_target(&source())?,
        (
            "/home/source/dot_claude-mi/settings.json".into(),
            "/home/.claude-mi/settings.json".into()
        )
    );
    for relative in [
        "",
        "/absolute",
        "../escape",
        "dot_claude-mi/settings.json.tmpl",
        "dot_claude-mi/symlink_settings.json",
        ".chezmoiignore",
        "./settings.json",
        "settings.json.literal",
        "bad\0name",
    ] {
        let mut s = source();
        s.relative = relative.into();
        assert!(source_target(&s).is_err(), "{relative}");
    }
    let mut deep = source();
    deep.relative = (0..33).map(|_| "d").collect::<PathBuf>();
    assert!(source_target(&deep).is_err());
    let mut unicode = source();
    unicode.relative = PathBuf::from(OsString::from_vec(vec![0xff]));
    assert!(source_target(&unicode).is_err());
    let mut destination = source();
    destination.destination = "relative".into();
    assert!(source_target(&destination).is_err());
    for binding in [
        dir("relative"),
        DirectoryBinding {
            canonical: "/other".into(),
            ..dir("/home/source")
        },
        DirectoryBinding {
            identity: FileId {
                device: 0,
                inode: 2,
            },
            ..dir("/home/source")
        },
        DirectoryBinding {
            identity: FileId {
                device: 1,
                inode: 0,
            },
            ..dir("/home/source")
        },
        DirectoryBinding {
            lineage: vec![],
            ..dir("/home/source")
        },
        DirectoryBinding {
            lineage: vec![
                FileId {
                    device: 1,
                    inode: 2
                };
                65
            ],
            ..dir("/home/source")
        },
        DirectoryBinding {
            mode: 0o10000,
            ..dir("/home/source")
        },
    ] {
        let mut s = source();
        s.root = binding;
        assert!(source_target(&s).is_err());
    }
    Ok(())
}
#[test]
fn authority_binds_each_claim_to_its_exact_live_root_and_source_pair() -> TestResult {
    let home = Path::new("/home");
    let good = authority();
    good.validate(home)?;
    assert!(good.validate(Path::new("relative")).is_err());
    assert_eq!(
        good.target_kind(Path::new("/home/.claude-mi/settings.json"))?,
        crate::capability::rules::delivery::ChangeKind::ProfileJson
    );
    assert!(good.target_kind(Path::new("/unknown")).is_err());
    let mut cases = vec![];
    let mut a = good.clone();
    a.codex_root = Some("relative".into());
    cases.push(a);
    let mut a = good.clone();
    a.targets = BTreeMap::from([(
        "relative".into(),
        Target {
            clients: BTreeMap::new(),
        },
    )]);
    cases.push(a);
    let mut a = good.clone();
    a.targets.values_mut().for_each(|t| t.clients.clear());
    cases.push(a);
    let mut a = good.clone();
    let row = a.targets.values_mut().next().ok_or("row")?;
    let c = row.clients.values().next().ok_or("client")?.clone();
    row.clients = BTreeMap::from([("wrong".into(), c)]);
    cases.push(a);
    for change in [0, 1, 2, 3, 4, 5, 6, 7] {
        let mut a = good.clone();
        let t = a.targets.values_mut().next().ok_or("row")?;
        let mut c = t.clients.values().next().ok_or("client")?.clone();
        match change {
            0 => c.owner.intent = Intent::CodexSkills,
            1 => c.live_root.canonical = "/other".into(),
            2 => c.live_root = dir("/other"),
            3 => c.live = "/home/.claude-mi/other.json".into(),
            4 => c.source = Some(source()),
            5 => c.owner.role = Role::Source,
            6 => {
                c.owner.role = Role::Source;
                c.source = Some(source());
            }
            _ => c.owner.profile = ConfigProfile::Codex,
        }
        t.clients = BTreeMap::from([(c.owner.id(), c)]);
        cases.push(a);
    }
    for a in cases {
        assert!(a.validate(home).is_err());
    }
    let mut a = good.clone();
    let t = a.targets.values().next().ok_or("target")?.clone();
    a.targets = (0..513)
        .map(|n| (PathBuf::from(format!("/item{n}")), t.clone()))
        .collect();
    assert!(a.validate(home).is_err());
    let mut a = good.clone();
    let c = client();
    a.targets
        .values_mut()
        .for_each(|t| t.clients = (0..10001).map(|n| (n.to_string(), c.clone())).collect());
    assert!(a.validate(home).is_err());
    Ok(())
}
#[test]
fn configuration_file_formats_and_codex_roots_cannot_be_confused() -> TestResult {
    let mut a = authority();
    let mut c = client();
    c.owner = Owner {
        profile: ConfigProfile::Codex,
        role: Role::Live,
        intent: Intent::CodexSkills,
    };
    c.live_root = dir("/home/.codex");
    c.live = "/home/.codex/config.toml".into();
    a.codex_root = Some(c.live_root.requested.clone());
    a.targets = BTreeMap::from([(
        c.live.clone(),
        Target {
            clients: BTreeMap::from([(c.owner.id(), c.clone())]),
        },
    )]);
    a.validate(Path::new("/home"))?;
    let mut wrong_root = a.clone();
    wrong_root.codex_root = Some("/home/other".into());
    assert!(wrong_root.validate(Path::new("/home")).is_err());
    assert_eq!(
        a.target_kind(&c.live)?,
        crate::capability::rules::delivery::ChangeKind::ProfileToml
    );
    let mut s = client();
    s.owner.role = Role::Source;
    s.source = Some(source());
    a.targets.insert(
        "/home/source/dot_claude-mi/settings.json".into(),
        Target {
            clients: BTreeMap::from([(s.owner.id(), s)]),
        },
    );
    a.validate(Path::new("/home"))?;
    let mut broken_source = a.clone();
    for client in broken_source
        .targets
        .values_mut()
        .flat_map(|t| t.clients.values_mut())
    {
        if let Some(source) = &mut client.source {
            source.relative = "../escape".into();
        }
    }
    assert!(broken_source.validate(Path::new("/home")).is_err());
    let target = a.targets.get_mut(&c.live).ok_or("target")?;
    let mut foreign = client();
    foreign.owner.intent = Intent::CodexSkills;
    target.clients = BTreeMap::from([(foreign.owner.id(), foreign.clone())]);
    assert!(a.target_kind(&c.live).is_err());
    foreign.owner.intent = Intent::Plugins;
    a.targets.get_mut(&c.live).ok_or("target")?.clients =
        BTreeMap::from([(c.owner.id(), c.clone()), (foreign.owner.id(), foreign)]);
    assert!(a.target_kind(&c.live).is_err());
    let mut a = authority();
    a.targets.values_mut().for_each(|t| t.clients.clear());
    assert!(
        a.target_kind(Path::new("/home/.claude-mi/settings.json"))
            .is_err()
    );
    Ok(())
}
