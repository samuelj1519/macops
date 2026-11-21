use super::*;
use crate::{
    native_plugins_test_fixture::{Fixture, error, write_json},
    test_support::TestResult,
};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};

#[test]
fn passive_verification_fences_each_path_identity_and_byte_range() -> TestResult {
    let fixture = Fixture::new()?;
    let file = fixture.config.join("settings");
    let absent = fixture.config.join("would-appear");
    let mut pending = Passive::new()?;
    assert!(pending.file(&absent)?.is_none());
    fs::write(&absent, "now present")?;
    error(pending.verify(), "native_plugin_evidence_changed");

    fs::write(&file, "original")?;
    let mut pending = Passive::new()?;
    assert_eq!(
        pending.file(&file)?.as_deref(),
        Some(b"original".as_slice())
    );
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    error(pending.verify(), "native_plugin_evidence_changed");
    let alias = fixture.config.join("alias");
    symlink(&file, &alias)?;
    error(
        Passive::new()?.file(&alias),
        "unsafe_native_plugin_evidence",
    );
    let oversize = fixture.config.join("oversize");
    fs::File::create(&oversize)?.set_len(4 * 1024 * 1024 + 1)?;
    error(Passive::new()?.file(&oversize), "config_file_limit");

    let absent_dir = fixture.config.join("new-directory");
    let mut pending = Passive::new()?;
    assert!(pending.directory(&absent_dir)?.is_none());
    fs::create_dir(absent_dir)?;
    error(pending.verify(), "native_plugin_evidence_changed");
    let mut pending = Passive::new()?;
    fs::set_permissions(&fixture.binary, fs::Permissions::from_mode(0o700))?;
    let observed = pending.binary(&fixture.binary)?;
    assert_eq!(observed.len(), 64);
    fs::write(&fixture.binary, "#!/bin/sh\nexit 92\n")?;
    error(pending.verify(), "native_plugin_evidence_changed");
    error(Passive::new()?.binary(&alias), "native_binary_unsupported");
    error(Passive::new()?.binary(&file), "native_binary_unsupported");
    error(
        Passive::new()?.binary(&fixture.root().join("missing")),
        "native_binary_missing",
    );
    Ok(())
}

#[test]
fn cache_projection_can_exclude_only_the_selected_plugin_and_not_siblings() -> TestResult {
    let fixture = Fixture::new()?;
    fixture.installed()?;
    let sibling = fixture.config.join("plugins/cache/market/control/2.3.4");
    fs::create_dir_all(&sibling)?;
    let ordinary = sibling.join("README.md");
    fs::write(&ordinary, "control data")?;
    let full = fixture.config.join("plugins/cache");
    let requested = Path::new("market/example");
    let mut selected_free = Passive::new()?;
    let fingerprint = selected_free.projected_tree(&full, Some(requested))?;
    assert_eq!(fingerprint.len(), 64);
    let mut selected_present = Passive::new()?;
    selected_present.projected_tree(&full, None)?;
    fs::write(fixture.cache().join("1.2.3/README.md"), "selected changed")?;
    selected_free.verify()?;
    error(selected_present.verify(), "native_plugin_evidence_changed");
    fs::set_permissions(&sibling, fs::Permissions::from_mode(0o700))?;
    error(selected_free.verify(), "native_plugin_evidence_changed");
    Ok(())
}

#[test]
fn codex_cache_registration_requires_matching_shape_and_single_unlinked_version() -> TestResult {
    let fixture = Fixture::new()?;
    let (instance, recipe) = fixture.binding()?;
    let probe = || observe("codex", &instance, &recipe);
    assert_eq!(
        probe()?.reason(),
        Some("native_plugin_registration_unknown")
    );
    fs::write(fixture.codex_file(), "model='fixture'")?;
    assert_eq!(probe()?.reason(), Some("native_plugin_not_registered"));
    fixture.installed()?;
    assert!(probe()?.reason().is_none());
    fs::write(
        fixture.cache().join(".codex-remote-plugin-install.json"),
        "private marker, no parse required",
    )?;
    assert_eq!(probe()?.reason(), Some("codex_remote_plugin_provenance"));
    fs::remove_file(fixture.cache().join(".codex-remote-plugin-install.json"))?;
    let other = fixture.cache().join("2.3.4");
    fs::create_dir(&other)?;
    error(probe(), "codex_plugin_cache_unproven");
    fs::remove_dir(other)?;
    fs::remove_dir_all(fixture.cache().join("1.2.3"))?;
    assert_eq!(
        probe()?.reason(),
        Some("native_plugin_registration_unknown")
    );
    symlink(fixture.source_member(), fixture.cache().join("1.2.3"))?;
    error(probe(), "codex_plugin_cache_unproven");
    fs::remove_file(fixture.cache().join("1.2.3"))?;
    fs::write(fixture.cache().join("1.2.3"), "regular")?;
    error(probe(), "codex_plugin_cache_unproven");
    Ok(())
}

#[test]
fn codex_invalid_selected_registration_and_config_do_not_learn_alternate_source() -> TestResult {
    let fixture = Fixture::new()?;
    let (instance, recipe) = fixture.binding()?;
    let probe = || observe("codex", &instance, &recipe);
    fixture.installed()?;
    for document in [
        "plugins = 14",
        "marketplaces = 14",
        "[marketplaces]\nmarket = 14",
        "[plugins]\n'example@market' = 14",
        "[marketplaces.market]\nsource='somewhere'",
        "[marketplaces.market]\nsource_type='local'",
        "[plugins.'example@market']\nenabled='true'",
        "[plugins.'example@market']\nenabled=true\nenabled=false",
    ] {
        fs::write(fixture.codex_file(), document)?;
        error(probe(), "invalid_native_plugin_metadata");
    }
    fs::write(fixture.codex_file(), b"\xff")?;
    error(probe(), "invalid_native_plugin_metadata");
    fs::write(
        fixture.codex_file(),
        "[marketplaces.market]\nsource_type='mystery'\nsource='outside'",
    )?;
    error(probe(), "native_plugin_marketplace_type_unsupported");
    fs::write(
        fixture.codex_file(),
        "[marketplaces.market]\nsource_type='git'\nsource='https://github.com/fiction/source.git'",
    )?;
    assert!(probe()?.source_changed);
    Ok(())
}

#[test]
fn codex_manifest_fallback_is_ordered_and_rejects_false_selected_members() -> TestResult {
    let fixture = Fixture::new()?;
    let (instance, recipe) = fixture.binding()?;
    let probe = || observe("codex", &instance, &recipe);
    fixture.installed()?;
    let canonical = fixture.source.join(".agents/plugins/marketplace.json");
    let fallback = fixture.source.join(".cursor-plugin/marketplace.json");
    write_json(
        &fallback,
        &json!({"name":"market","plugins":[{"name":"example","source":"./plugins/example"}]}),
    )?;
    fs::remove_file(&canonical)?;
    assert!(probe()?.reason().is_none());
    fs::write(fixture.cache().join("1.2.3/added"), "drift")?;
    assert_eq!(probe()?.reason(), Some("native_plugin_payload_differs"));
    for invalid in [
        json!({"name":"other","plugins":[{"name":"example","source":"./plugins/example"}]}),
        json!({"name":"market","plugins":{"example":"./plugins/example"}}),
        json!({"name":"market","plugins":[{"name":"example","source":"./plugins/example"},{"name":"example","source":"./plugins/example"}]}),
        json!({"name":"market","plugins":[{"name":"example","source":"plugins/example"}]}),
        json!({"name":"market","plugins":[{"name":"example","source":"./plugins/other"}]}),
        json!({"name":"market","plugins":[{"name":"example","source":{"source":"git","path":"./plugins/example"}}]}),
        json!({"name":"market","plugins":[{"name":"other","source":"./plugins/example"}]}),
    ] {
        write_json(&canonical, &invalid)?;
        error(probe(), "native_plugin_source_manifest_mismatch");
    }
    write_json(&canonical, &json!([]))?;
    error(probe(), "invalid_native_plugin_metadata");
    fs::remove_file(canonical)?;
    fs::remove_file(fallback)?;
    error(probe(), "native_plugin_source_manifest_mismatch");
    Ok(())
}

#[test]
fn claude_project_rows_and_malformed_user_rows_cannot_act_as_local_evidence() -> TestResult {
    let fixture = Fixture::new()?;
    let (instance, recipe) = fixture.binding()?;
    let probe = || observe("claude:mi", &instance, &recipe);
    let plugins = instance.plugin_root();
    let settings = instance.config_root.join("settings.json");
    let known = plugins.join("known_marketplaces.json");
    let installed = plugins.join("installed_plugins.json");
    let stamp = |entries| json!({"version":2,"plugins":entries});
    let user = json!({"scope":"user","installPath":fixture.source_member(),"version":"not/stable"});
    let project =
        json!({"scope":"project","projectPath":fixture.cwd,"installPath":fixture.source_member()});
    write_json(&installed, &stamp(json!({})))?;
    assert_eq!(probe()?.reason(), Some("native_plugin_not_registered"));
    write_json(&installed, &stamp(json!({"example@market":[project]})))?;
    assert_eq!(probe()?.reason(), Some("native_plugin_scope_unsupported"));
    write_json(
        &installed,
        &stamp(json!({"example@market":[user.clone(),user.clone()]})),
    )?;
    error(probe(), "native_plugin_registration_ambiguous");
    write_json(&installed, &stamp(json!({"example@market":[user]})))?;
    let facts = probe()?;
    assert_eq!(facts.reason(), Some("native_plugin_marketplace_unproven"));
    assert!(facts.version.is_none());
    write_json(&settings, &json!({"enabledPlugins":[]}))?;
    error(probe(), "invalid_native_plugin_metadata");
    write_json(&settings, &json!({"enabledPlugins":{"example@market":1}}))?;
    error(probe(), "invalid_native_plugin_metadata");
    write_json(
        &settings,
        &json!({"enabledPlugins":{"example@market":false}}),
    )?;
    write_json(
        &known,
        &json!({"market":{"source":{"source":"directory","path":fixture.source}}}),
    )?;
    error(probe(), "invalid_native_plugin_metadata");
    write_json(
        &known,
        &json!({"market":{"source":{"source":"directory","path":fixture.source},"installLocation":fixture.config}}),
    )?;
    assert!(probe()?.source_changed);
    write_json(
        &known,
        &json!({"market":{"source":{"source":"unverified"}}}),
    )?;
    error(probe(), "native_plugin_marketplace_type_unsupported");
    write_json(
        &known,
        &json!({"market":{"source":{"source":"directory","path":fixture.source},"installLocation":fixture.source}}),
    )?;
    write_json(
        &installed,
        &stamp(json!({"example@market":[{"scope":"user","installPath":fixture.config}]})),
    )?;
    let facts = probe()?;
    assert_eq!(facts.reason(), Some("claude_inplace_payload_unproven"));
    assert_eq!(facts.configured_enabled, Some(false));
    Ok(())
}

#[test]
fn remote_and_accepted_sources_keep_unproven_authority_explicit() -> TestResult {
    let fixture = Fixture::new()?;
    let (instance, mut recipe) = fixture.binding()?;
    fixture.installed()?;
    recipe.source = Source::NativeGit {
        repo: "fiction/remote".into(),
    };
    let registry = "[marketplaces.market]\nsource_type='git'\nsource='https://github.com/fiction/remote.git'\n[plugins.'example@market']\nenabled=true\n";
    fs::write(fixture.codex_file(), registry)?;
    assert_eq!(
        observe("codex", &instance, &recipe)?.reason(),
        Some("native_git_payload_unproven")
    );
    let plugins = instance.plugin_root();
    write_json(
        &plugins.join("known_marketplaces.json"),
        &json!({"market":{"source":{"source":"github","repo":"fiction/remote"}}}),
    )?;
    write_json(
        &plugins.join("installed_plugins.json"),
        &json!({"version":2,"plugins":{"example@market":[{"scope":"user","installPath":fixture.source_member()}]}}),
    )?;
    assert_eq!(
        observe("claude:mi", &instance, &recipe)?.reason(),
        Some("native_git_payload_unproven")
    );

    recipe.source = Source::AcceptedRelease {
        root: fixture.source.clone(),
        member: "plugins/example".into(),
        artifact: "a".repeat(64),
        candidate: "b".repeat(64),
    };
    assert!(observe("claude:mi", &instance, &recipe)?.source_changed);
    fixture.installed()?;
    assert_eq!(
        observe("codex", &instance, &recipe)?.reason(),
        Some("accepted_release_authority_required")
    );
    recipe.source = Source::LocalDirectory {
        root: fixture.source.clone(),
        member: "plugins/example".into(),
        sha256: Some("0".repeat(64)),
    };
    assert_eq!(
        observe("codex", &instance, &recipe)?.reason(),
        Some("native_plugin_source_differs")
    );
    Ok(())
}
