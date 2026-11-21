#![cfg(test)]
use super::*;
use crate::{
    resource::{self, Content, Data, Management, Origin, Resource, Revision, State},
    test_support::TestResult,
};
use std::path::{Path, PathBuf};

#[test]
fn category_filters_and_path_components_are_closed() -> TestResult {
    assert!(Filter::parse(None)? == Filter::All);
    for (name, kind) in [
        ("all", Filter::All),
        ("apps", Filter::Applications),
        ("harnesses", Filter::Agents),
        ("skills", Filter::Capabilities),
        ("configs", Filter::Configs),
    ] {
        assert!(Filter::parse(Some(name))? == kind);
    }
    assert!(Filter::parse(Some("bad")).is_err());
    assert!(Filter::All.includes(Filter::Agents));
    assert!(Filter::Agents.includes(Filter::Agents));
    assert!(!Filter::Configs.includes(Filter::Agents));
    assert!(component("中文-skill"));
    for name in ["", ".", "..", "a/b", "a\\b", "a\0b", "a\nb", "a\rb"] {
        assert!(!component(name));
    }
    Ok(())
}

#[test]
fn model_metadata_is_a_whitelist_not_an_environment_dump() -> TestResult {
    let raw=br#"{"model":"chosen","env":{"ANTHROPIC_MODEL":"fallback","ANTHROPIC_DEFAULT_OPUS_MODEL":"o","ANTHROPIC_DEFAULT_SONNET_MODEL":"s","ANTHROPIC_DEFAULT_HAIKU_MODEL":"h","ANTHROPIC_BASE_URL":"https://SECRET_URL","ANTHROPIC_AUTH_TOKEN":"SECRET_TOKEN"}}"#;
    let model = claude_model(raw)?;
    assert_eq!(model.selected.as_deref(), Some("chosen"));
    assert_eq!(model.aliases.len(), 3);
    assert!(model.custom_provider_configured);
    assert!(!serde_json::to_string(&model)?.contains("SECRET"));
    assert_eq!(
        claude_model(br#"{"env":{"ANTHROPIC_MODEL":"fallback"}}"#)?
            .selected
            .as_deref(),
        Some("fallback")
    );
    assert!(claude_model(br"{}")?.selected.is_none());
    assert!(!claude_model(br#"{"env":{"ANTHROPIC_BASE_URL":""}}"#)?.custom_provider_configured);
    for bad in [
        b"bad JSON".as_slice(),
        br#"{"model":true}"#,
        br#"{"env":{"ANTHROPIC_DEFAULT_OPUS_MODEL":false}}"#,
    ] {
        assert!(claude_model(bad).is_err());
    }
    Ok(())
}

#[test]
fn native_release_schemas_reject_unusable_inputs() -> TestResult {
    let mise=mise_releases(br#"{"claude":[{"version":"1.0.0","active":true,"install_path":"/one"},{"version":"2.0.0"}]}"#)?;
    assert_eq!(mise["claude"].len(), 2);
    assert!(!mise["claude"][1].active);
    assert!(mise["claude"][1].install_path.is_none());
    assert!(mise_releases(b"[]").is_err());
    let brew=brew_book(br#"{"formulae":[{"name":"demo","installed":[{"version":"1.0.0"}]}],"casks":[{"token":"app","installed":"2.0.0"}]}"#)?;
    assert_eq!(brew.casks[0].installed.as_deref(), Some("2.0.0"));
    assert!(brew.formulae[0].full_name.is_none());
    assert!(brew_book(b"{}").is_err());
    Ok(())
}

#[test]
fn source_labels_never_echo_url_credentials_or_signed_queries() {
    assert_eq!(source_label("owner/repo"), "owner/repo");
    assert_eq!(source_label("@scope/pkg"), "@scope/pkg");
    assert_eq!(
        source_label("https://user:SECRET@host/repo?secret=SECRET"),
        "https://host"
    );
    assert_eq!(source_label("https://host?SECRET"), "https://host");
    assert_eq!(source_label("user:SECRET@host"), "private-source");
    assert_eq!(source_label("plain?SECRET"), "private-source");
}

#[test]
fn lock_versions_and_native_scope_are_part_of_capability_identity() -> TestResult {
    let raw = br#"{"version":3,"skills":{"demo":{"source":"owner/repo","sourceType":"github"}}}"#;
    assert_eq!(skill_lock(raw, false)?.skills.len(), 1);
    assert!(skill_lock(raw, true).is_err());
    assert!(skill_lock(br#"{"version":1,"skills":{}}"#, false).is_err());
    assert!(
        skill_lock(
            br#"{"version":3,"skills":{"../bad":{"source":"x","sourceType":"github"}}}"#,
            false
        )
        .is_err()
    );
    assert!(skill_lock(b"{}", false).is_err());
    assert!(
        skill_lock(br#"{"version":2,"skills":{}}"#, true)?
            .skills
            .is_empty()
    );
    let book=plugin_book(br#"{"version":2,"plugins":{"demo@market":[{"scope":"user","installPath":"/cache/demo","version":"1"},{"scope":"project","projectPath":"/project","installPath":"/cache/other"}]}}"#)?;
    assert_eq!(book.plugins["demo@market"].len(), 2);
    for raw in [br#"{"version":1,"plugins":{}}"#.as_slice(),br#"{"version":2,"plugins":{"bad":[]}}"#,br#"{"version":2,"plugins":{"../bad@market":[]}}"#,br#"{"version":2,"plugins":{"demo@../market":[]}}"#,br#"{"version":2,"plugins":{"demo@market":[{"scope":"other","installPath":"/cache"}]}}"#,br#"{"version":2,"plugins":{"demo@market":[{"scope":"local","installPath":"/cache"}]}}"#,br#"{"version":2,"plugins":{"demo@market":[{"scope":"user","installPath":"relative"}]}}"#,br#"{"version":2,"plugins":{"demo@market":[{"scope":"project","projectPath":"relative","installPath":"/cache"}]}}"#,b"{}"] { assert!(plugin_book(raw).is_err()); }
    Ok(())
}

#[test]
fn shared_content_does_not_merge_other_names_scopes_or_origins() -> TestResult {
    let path = PathBuf::from("/canonical/demo");
    let mut r = sample_skill();
    assert!(skill_target_matches(
        &r,
        "demo",
        &path,
        Some(&path),
        "canonical",
        None
    ));
    assert!(!skill_target_matches(
        &r,
        "other",
        &path,
        Some(&path),
        "user",
        None
    ));
    assert!(!skill_target_matches(
        &r,
        "demo",
        &path,
        Some(Path::new("/other")),
        "user",
        None
    ));
    assert!(skill_target_matches(
        &r,
        "demo",
        Path::new("/profile/skills/demo"),
        Some(&path),
        "user",
        None
    ));
    assert!(!skill_target_matches(
        &r,
        "demo",
        Path::new("/profile/skills/demo"),
        Some(&path),
        "system",
        None
    ));
    assert!(!skill_target_matches(
        &r,
        "demo",
        Path::new("/project/skills/demo"),
        Some(&path),
        "project",
        Some(Path::new("/project"))
    ));
    if let Data::Capability { origin, .. } = &mut r.data {
        origin.record = "/project/skills-lock.json".into();
    }
    assert!(skill_target_matches(
        &r,
        "demo",
        Path::new("/project/skills/demo"),
        Some(&path),
        "project",
        Some(Path::new("/project"))
    ));
    if let Data::Capability { origin, .. } = &mut r.data {
        origin.manager = "other-owner".into();
    }
    assert!(!skill_target_matches(
        &r,
        "demo",
        Path::new("/project/skills/demo"),
        Some(&path),
        "project",
        Some(Path::new("/project"))
    ));
    let app = application(
        "mise",
        "claude",
        "Claude",
        Path::new("/root"),
        vec![resource::Release {
            version: "1".into(),
            path: Some("/one".into()),
            active: true,
        }],
    )?;
    assert!(!skill_target_matches(
        &app,
        "demo",
        &path,
        Some(&path),
        "user",
        None
    ));
    assert_eq!(
        bind_runtime(Path::new("/one/claude"), std::slice::from_ref(&app)).as_deref(),
        Some(app.id.as_str())
    );
    assert!(bind_runtime(Path::new("/other/claude"), std::slice::from_ref(&app)).is_none());
    assert!(bind_runtime(Path::new("/one/claude"), &[app.clone(), app]).is_none());
    Ok(())
}

#[test]
fn launcher_hints_are_read_as_data_and_do_not_accept_extra_shell_commands() {
    let body = b"claude_resolved_bin() {\n  mise which claude\n}\n";
    assert!(legacy_mise_resolver(body));
    for bad in [
        b"\xff".as_slice(),
        b"no function",
        b"claude_resolved_bin() {",
        b"claude_resolved_bin() {\nother\n}\n",
        b"claude_resolved_bin() {\n}\nclaude_resolved_bin() {\n}",
    ] {
        assert!(!legacy_mise_resolver(bad));
    }
    assert!(legacy_claude_wrapper(
        b"#!/bin/sh\nset -eu\nexec /helper run mi \"$@\"\n",
        Path::new("/helper"),
        "mi"
    ));
    assert!(!legacy_claude_wrapper(b"\xff", Path::new("/helper"), "mi"));
    assert!(!legacy_claude_wrapper(
        b"exec /helper run mi \"$@\"\necho extra",
        Path::new("/helper"),
        "mi"
    ));
}

#[test]
fn plugin_flags_and_mcp_metadata_do_not_return_credentials_or_commands() -> TestResult {
    assert_eq!(
        plugin_enabled(br#"{"enabledPlugins":{"a@b":true},"apiKey":"SECRET"}"#)?.get("a@b"),
        Some(&true)
    );
    assert!(plugin_enabled(b"{}").is_ok());
    assert!(plugin_enabled(br#"{"enabledPlugins":false}"#).is_err());
    let mcp=mcp_json(br#"{"mcpServers":{"stdio":{"command":"/bin/npx","args":["SECRET"],"env":{"TOKEN":"SECRET"},"disabled":true},"http":{"url":"https://SECRET","disabled":false},"custom":{"command":"sh -c SECRET"},"empty":{}}}"#)?;
    assert_eq!(mcp.len(), 4);
    assert!(mcp.iter().all(|r| !format!("{r:?}").contains("SECRET")));
    assert!(
        mcp.iter()
            .any(|r| r.runner.as_deref() == Some("remote") && r.enabled == Some(true))
    );
    assert!(
        mcp.iter()
            .any(|r| r.runner.as_deref() == Some("npx") && r.enabled == Some(false))
    );
    assert!(
        mcp.iter()
            .any(|r| r.runner.as_deref() == Some("custom-command"))
    );
    assert_eq!(runner_label(""), "custom-command");
    assert_eq!(runner_label("https://SECRET"), "custom-command");
    assert!(mcp_json(br#"{"mcpServers":{"../bad":{}}}"#).is_err());
    assert!(mcp_json(b"bad").is_err());
    Ok(())
}

#[test]
fn codex_plugin_and_mcp_tables_parse_without_exposing_other_settings() -> TestResult {
    let toml=b"model='other'\napi_key='SECRET'\n[plugins.'demo@market']\nenabled=true\n[mcp_servers.demo]\ncommand='npx'\nenabled=false\n[mcp_servers.remote]\nurl='https://SECRET'\n";
    assert_eq!(codex_plugins(toml)?.get("demo@market"), Some(&Some(true)));
    let mcp = codex_mcp(toml)?;
    assert_eq!(mcp.len(), 2);
    assert!(!format!("{mcp:?}").contains("SECRET"));
    assert_eq!(codex_plugins(b"")?.len(), 0);
    assert_eq!(codex_mcp(b"")?.len(), 0);
    assert_eq!(
        codex_plugins(b"[plugins.some_without_market]\nenabled=true\n")?.len(),
        0
    );
    assert_eq!(
        codex_plugins(b"[plugins.'demo@market']\n")?.get("demo@market"),
        Some(&None)
    );
    assert!(codex_plugins(b"plugins=42").is_err());
    assert!(codex_plugins(b"[plugins.'../x@market']").is_err());
    assert!(codex_plugins(b"[plugins.'x@../market']").is_err());
    assert!(codex_plugins(b"[plugins.'x@market']\nenabled='SECRET'").is_err());
    assert!(codex_plugins(b"\xff").is_err());
    assert!(codex_plugins(b"invalid toml !").is_err());
    assert!(codex_mcp(b"\xff").is_err());
    assert!(codex_mcp(b"invalid toml !").is_err());
    assert!(codex_mcp(b"mcp_servers=42").is_err());
    assert!(codex_mcp(b"[mcp_servers.'../x']").is_err());
    assert!(codex_mcp(b"[mcp_servers.x]\nenabled='SECRET'").is_err());
    assert!(codex_mcp(b"[mcp_servers.x]\ncommand=42").is_err());
    Ok(())
}

#[test]
fn text_inventory_grammars_keep_malformed_and_duplicate_entries_unknown() -> TestResult {
    assert_eq!(
        text_packages("demo v1.0.0\n- demo\n", false)?
            .get("demo")
            .map(String::as_str),
        Some("1.0.0")
    );
    assert_eq!(text_packages("demo v1.0.0:\n    demo\n", true)?.len(), 1);
    assert!(text_packages("", false)?.is_empty());
    for (bad, cargo) in [
        ("    orphan", true),
        ("bad", false),
        ("demo not-version", false),
        ("demo v1.0.0", true),
        ("../bad v1.0.0", false),
        ("demo vbad", false),
        ("demo v1.0.0\ndemo v1.0.0", false),
    ] {
        assert!(text_packages(bad, cargo).is_err());
    }
    let (root, packages) = bun_packages("/bun node_modules (1)\n└── @scope/demo@1.0.0\n")?;
    assert_eq!(root, PathBuf::from("/bun"));
    assert_eq!(packages.len(), 1);
    assert!(bun_packages("bad").is_err());
    Ok(())
}

#[test]
fn malformed_codex_rows_and_local_mcp_metadata_stay_unknown() -> TestResult {
    assert!(codex_plugins(b"[plugins]\n'x@market'=42").is_err());
    assert!(codex_mcp(b"[mcp_servers]\nx=42").is_err());
    assert!(codex_mcp(b"[mcp_servers.x]\nurl=42").is_err());
    assert_eq!(
        codex_mcp(b"[mcp_servers.x]\nurl='https://SECRET'")?
            .first()
            .and_then(|r| r.runner.as_deref()),
        Some("remote")
    );
    let records=mcp_local(br#"{"projects":{"/project":{"mcpServers":{"x":{"command":"npx","disabled":true},"remote":{"url":"https://SECRET"},"empty":{}}}}}"#,Path::new("/project"))?;
    assert_eq!(records.len(), 3);
    assert!(!format!("{records:?}").contains("SECRET"));
    assert!(mcp_local(b"bad", Path::new("/project")).is_err());
    assert!(
        mcp_local(
            br#"{"projects":{"/project":{"mcpServers":{"../bad":{}}}}}"#,
            Path::new("/project")
        )
        .is_err()
    );
    assert!(mcp_local(br"{}", Path::new("/project"))?.is_empty());
    Ok(())
}

#[test]
fn invalid_identity_paths_abort_application_construction() {
    use std::os::unix::ffi::OsStringExt;
    let invalid = PathBuf::from(std::ffi::OsString::from_vec(vec![b'/', 0xff]));
    assert!(application("mise", "demo", "Demo", &invalid, vec![]).is_err());
    assert!(bun_packages("/Users/example/job node_modules (0)\n").is_err());
}

fn sample_skill() -> Resource {
    let path = PathBuf::from("/canonical/demo");
    Resource {
        id: "capability:x".into(),
        name: "demo".into(),
        aliases: vec![],
        management: Management::Native,
        status: State::Present,
        data: Data::Capability {
            delivery_kind: "skill".into(),
            native_id: "demo".into(),
            origin: Origin {
                manager: "skills-cli:github".into(),
                label: None,
                record: "/home/.agents/.skill-lock.json".into(),
                native_key: "demo".into(),
            },
            revision: Revision::default(),
            contents: vec![Content {
                id: "content:x".into(),
                path: path.clone(),
                realpath: Some(path.clone()),
                status: State::Present,
            }],
            deployments: vec![],
            configured_enabled: None,
            loaded: None,
            baseline: None,
        },
    }
}

#[test]
fn duplicate_native_context_keeps_all_content_candidates_and_clears_selected_revision() {
    let mut records = BTreeMap::new();
    let mut first = sample_skill();
    first.id = "same".into();
    if let Data::Capability { revision, .. } = &mut first.data {
        revision.recorded = Some("1".into());
        revision.source = Some("aaa".into());
    }
    assert!(!insert_capability(&mut records, first.clone()));
    assert!(insert_capability(&mut records, first.clone()));
    let mut next = first.clone();
    if let Data::Capability {
        contents, revision, ..
    } = &mut next.data
    {
        contents[0].path = "/other".into();
        contents[0].realpath = Some("/other".into());
        revision.recorded = Some("2".into());
        revision.source = Some("bbb".into());
    }
    assert!(insert_capability(&mut records, next.clone()));
    assert!(insert_capability(&mut records, next));
    let current = &records["same"];
    assert_eq!(current.status, State::Unknown);
    if let Data::Capability {
        contents, revision, ..
    } = &current.data
    {
        assert_eq!(contents.len(), 2);
        assert!(revision.recorded.is_none());
        assert!(revision.source.is_none());
    } else {
        panic!("expected capability");
    }
    let mut wrong = first.clone();
    wrong.data = Data::Config {
        target: "/cfg".into(),
        source_candidates: vec![],
        owner: "user".into(),
        consumers: vec![],
        observed_type: "file".into(),
        mode: None,
        baseline: None,
    };
    assert!(insert_capability(&mut records, wrong));
}

#[test]
fn plugin_configuration_paths_follow_native_scope() {
    let mut record = PluginRecord {
        scope: "user".into(),
        path: "/cache".into(),
        version: None,
        revision: None,
        project: None,
    };
    let root = Path::new("/profile");
    assert_eq!(
        plugin_settings_path(root, &record),
        Some(root.join("settings.json"))
    );
    record.scope = "project".into();
    assert!(plugin_settings_path(root, &record).is_none());
    record.project = Some("/project".into());
    assert_eq!(
        plugin_settings_path(root, &record),
        Some(PathBuf::from("/project/.claude/settings.json"))
    );
    record.scope = "local".into();
    assert_eq!(
        plugin_settings_path(root, &record),
        Some(PathBuf::from("/project/.claude/settings.local.json"))
    );
    record.scope = "other".into();
    assert!(plugin_settings_path(root, &record).is_none());
}

#[test]
fn native_json_objects_cannot_be_replaced_by_positional_struct_arrays() {
    assert!(skill_lock(b"[3,{}]", false).is_err());
    assert!(
        skill_lock(
            br#"{"version":3,"skills":{"demo":["owner/repo","github"]}}"#,
            false
        )
        .is_err()
    );
    assert!(plugin_book(b"[2,{}]").is_err());
    assert!(
        plugin_book(br#"{"version":2,"plugins":{"demo@market":[["user","/home/plugin"]]}}"#)
            .is_err()
    );
    assert!(claude_model(b"[null,{}]").is_err());
    assert!(claude_model(br#"{"env":[null,null,null,null,null]}"#).is_err());
    assert!(mcp_json(br#"{"mcpServers":{"server":["command",null,false]}}"#).is_err());
    assert!(mise_releases(br#"{"node":[["22.0.0",null,true]]}"#).is_err());
}

#[test]
fn invalid_mise_json_and_duplicate_tool_names_do_not_become_an_empty_inventory() {
    assert!(mise_releases(b"{").is_err());
    assert!(mise_releases(br#"{"node":[],"node":[]}"#).is_err());
}

#[test]
fn brew_records_and_nested_installed_releases_require_object_shapes() -> TestResult {
    let ordinary = br#"{"formulae":[{"name":"demo","installed":[{"version":"1.0.0"}]}],"casks":[{"token":"app","installed":"2.0.0"}]}"#;
    let book = brew_book(ordinary)?;
    assert_eq!(book.formulae[0].installed[0].version, "1.0.0");
    assert_eq!(book.casks[0].token, "app");
    for positional in [
        br#"{"formulae":[["demo",null,[{"version":"1.0.0"}],null]],"casks":[]}"#.as_slice(),
        br#"{"formulae":[],"casks":[["app",null,"2.0.0"]]}"#,
        br#"{"formulae":[{"name":"demo","installed":[["1.0.0"]]}],"casks":[]}"#,
    ] {
        let error = brew_book(positional)
            .err()
            .ok_or("positional object unexpectedly accepted")?;
        assert_eq!(error.code, "invalid_brew_inventory");
    }
    Ok(())
}
