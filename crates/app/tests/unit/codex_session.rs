use super::*;
use crate::codex_install::{
    InitialActivation, install_using, managed_profiles,
    tests::{FIXTURE as CODEX_FIXTURE, fake_binary, fixture_context, offer},
};
use crate::test_support::TestResult;
use std::{fs, os::unix::fs::symlink};

const FIXTURE: SessionIo<'static> = SessionIo {
    codex: &CODEX_FIXTURE,
    claude: claude_install::session_target,
};

fn installed()
-> std::result::Result<(tempfile::TempDir, Context, String), Box<dyn std::error::Error>> {
    let (temp, ctx) = fixture_context()?;
    install_using(
        &ctx,
        offer()?,
        fake_binary(&ctx)?,
        &CODEX_FIXTURE,
        InitialActivation::Disabled,
    )?;
    let (name, _) = managed_profiles(&ctx)?.pop().ok_or("managed profile")?;
    let name = name.strip_prefix(".codex-").ok_or("profile suffix")?;
    Ok((temp, ctx, format!("codex:{name}")))
}

#[test]
fn session_preference_is_separate_and_preserves_named_models_and_native_files() -> TestResult {
    let (_temp, ctx, label) = installed()?;
    let workspace = Directory::open(&ctx.home)?;
    let original = select(&ctx, &label, &FIXTURE)?;
    let profile = &original.profile.path;
    let config = profile.join(CONFIG);
    let base = [&b"model = \"fixture-base\"\n"[..], &fs::read(&config)?].concat();
    fs::write(&config, &base)?;
    let auth = profile.join("auth.json");
    fs::write(&auth, b"synthetic credential stays with native owner")?;
    fs::write(
        profile.join("test-model.config.toml"),
        b"model = \"fixture-named\"\n",
    )?;
    let receipts = fs::read(ctx.state.join("receipts.json"))?;
    assert!(original.preference.is_none());
    let inherited = admit(&ctx, &original, &workspace, None, None, &FIXTURE)?;
    assert_eq!(inherited.get_args().count(), 0);
    set_mode(&ctx, &original, Mode::Enabled, &FIXTURE)?;
    assert!(set_mode(&ctx, &original, Mode::Disabled, &FIXTURE).is_err());
    let enabled = select(&ctx, &label, &FIXTURE)?;
    assert_eq!(enabled.revision(), 1);
    assert_eq!(enabled.mode().label(), "启用");
    let layer = named_layer(&enabled.profile, Some("test-model"))?;
    let command = admit(
        &ctx,
        &enabled,
        &workspace,
        Some("test-model"),
        layer.as_ref(),
        &FIXTURE,
    )?;
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "--profile",
            "test-model",
            "-c",
            concat!("plugins.item@market", ".enabled=true")
        ],
    );
    assert_eq!(command.get_current_dir(), Some(workspace.path.as_path()));
    let environment: std::collections::BTreeMap<_, _> = command.get_envs().collect();
    assert_eq!(
        environment.get(OsStr::new("CODEX_HOME")),
        Some(&Some(profile.as_os_str()))
    );
    assert!(environment.keys().all(|name| {
        [
            "HOME",
            "CODEX_HOME",
            "TMPDIR",
            "PATH",
            "TERM",
            "COLORTERM",
            "LANG",
            "LC_CTYPE",
        ]
        .iter()
        .any(|key| *name == OsStr::new(key))
    }));
    let policy = enabled.root.path.join(PREFERENCE);
    let first = fs::read(&policy)?;
    set_mode(&ctx, &enabled, Mode::Enabled, &FIXTURE)?;
    assert_eq!(fs::read(&policy)?, first);
    set_mode(&ctx, &enabled, Mode::Disabled, &FIXTURE)?;
    let disabled = select(&ctx, &label, &FIXTURE)?;
    assert_eq!(disabled.revision(), 2);
    assert_eq!(disabled.mode().label(), "停用");
    assert_eq!(
        admit(&ctx, &disabled, &workspace, None, None, &FIXTURE)?
            .get_args()
            .collect::<Vec<_>>(),
        ["-c", concat!("plugins.item@market", ".enabled=false")],
    );
    set_mode(&ctx, &disabled, Mode::Native, &FIXTURE)?;
    let native = select(&ctx, &label, &FIXTURE)?;
    assert_eq!(native.revision(), 3);
    assert_eq!(native.mode().label(), "按原生设置");
    assert_eq!(fs::read(config)?, base);
    assert_eq!(
        fs::read(auth)?,
        b"synthetic credential stays with native owner"
    );
    assert_eq!(fs::read(ctx.state.join("receipts.json"))?, receipts);
    Ok(())
}

#[test]
fn session_refuses_changed_named_scope_preferences_payload_and_unmanaged_roots() -> TestResult {
    let (_temp, mut ctx, label) = installed()?;
    let workspace = Directory::open(&ctx.home)?;
    assert!(select(&ctx, "codex:default", &FIXTURE).is_err());
    let selected = select(&ctx, &label, &FIXTURE)?;
    let named = selected.profile.path.join("test-model.config.toml");
    fs::write(&named, b"model = \"one\"\n")?;
    let layer = named_layer(&selected.profile, Some("test-model"))?;
    fs::write(&named, b"model = \"two\"\n")?;
    assert!(
        admit(
            &ctx,
            &selected,
            &workspace,
            Some("test-model"),
            layer.as_ref(),
            &FIXTURE
        )
        .is_err()
    );
    fs::remove_file(&named)?;
    symlink(selected.profile.path.join(CONFIG), &named)?;
    assert!(named_layer(&selected.profile, Some("test-model")).is_err());
    assert!(named_layer(&selected.profile, Some("../outside")).is_err());
    for invalid in [
        "dot.plugin@market",
        concat!("item@", "dot.market"),
        "item@@market",
        "item @market",
    ] {
        assert!(Mode::Enabled.flag(invalid).is_err());
        assert!(Mode::Native.flag(invalid)?.is_none());
    }
    let policy = selected.root.path.join(PREFERENCE);
    fs::write(&policy, b"{\"injected\":true}")?;
    assert!(select(&ctx, &label, &FIXTURE).is_err());
    assert!(set_mode(&ctx, &selected, Mode::Enabled, &FIXTURE).is_err());
    assert_eq!(fs::read(&policy)?, b"{\"injected\":true}");
    fs::remove_file(&policy)?;
    ctx.chezmoi.config = Some(ctx.home.join("other-context"));
    assert!(select(&ctx, &label, &FIXTURE).is_err());
    ctx.chezmoi.config = None;
    let payload = selected
        .profile
        .path
        .join("plugins/cache/market/item/1.0.0");
    let added = payload.join("foreign");
    fs::write(&added, b"changed payload")?;
    assert!(select(&ctx, &label, &FIXTURE).is_err());
    assert!(!policy.exists());
    Ok(())
}

#[test]
fn session_admission_fences_market_registration_but_accepts_native_boolean_edits() -> TestResult {
    let (_temp, ctx, label) = installed()?;
    let selected = select(&ctx, &label, &FIXTURE)?;
    let workspace = Directory::open(&ctx.home)?;
    let config = selected.profile.path.join(CONFIG);
    let original = fs::read(&config)?;
    let (_, native_toggle) = crate::codex_install::set_enabled(&original, "item@market", true)?;
    fs::write(&config, &native_toggle)?;
    admit(&ctx, &selected, &workspace, None, None, &FIXTURE)?;
    let original_url = offer()?.url()?;
    let replaced = String::from_utf8(native_toggle)?
        .replace(&original_url, "https://github.com/other/publisher.git");
    fs::write(&config, &replaced)?;
    assert!(admit(&ctx, &selected, &workspace, None, None, &FIXTURE).is_err());
    assert!(set_mode(&ctx, &selected, Mode::Enabled, &FIXTURE).is_err());
    assert_eq!(fs::read_to_string(&config)?, replaced);
    assert!(!selected.root.path.join(PREFERENCE).exists());
    fs::write(&config, &original)?;
    fs::write(
        selected
            .profile
            .path
            .join(".tmp/marketplaces/market/foreign"),
        b"changed marketplace payload",
    )?;
    assert!(admit(&ctx, &selected, &workspace, None, None, &FIXTURE).is_err());
    assert!(!selected.root.path.join(PREFERENCE).exists());
    Ok(())
}

#[test]
fn session_rechecks_frozen_directory_identities_after_native_evidence_reads() -> TestResult {
    let (_temp, ctx, label) = installed()?;
    let selected = select(&ctx, &label, &FIXTURE)?;
    let workspace = Directory::open(&ctx.home)?;
    let replacement = NativeIo {
        verify_binary: |reader, binary| {
            (CODEX_FIXTURE.verify_binary)(reader, binary)?;
            let state = binary
                .parent()
                .ok_or_else(stale)?
                .join(".local/state/macops");
            let allocation = fs::read_dir(state)?
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .find(|path| crate::codex_install::reserved_name(path))
                .ok_or_else(stale)?;
            fs::rename(allocation.join("tmp"), allocation.join("original-tmp"))?;
            symlink("/private/tmp", allocation.join("tmp"))?;
            Ok(())
        },
        ..CODEX_FIXTURE
    };
    let changed = SessionIo {
        codex: &replacement,
        ..FIXTURE
    };
    assert!(admit(&ctx, &selected, &workspace, None, None, &changed).is_err());
    assert!(fs::symlink_metadata(selected.root.path.join("tmp"))?.is_symlink());
    assert!(selected.root.path.join("original-tmp").is_dir());
    assert!(!selected.root.path.join(PREFERENCE).exists());
    Ok(())
}

#[test]
fn session_dialog_displays_the_owned_plugin_and_writes_only_after_confirmation() -> TestResult {
    let (_temp, ctx, label) = installed()?;
    let selected = select(&ctx, &label, &FIXTURE)?;
    let path = selected.root.path.join(PREFERENCE);
    assert!(details_using(&ctx, "codex:default", &FIXTURE).is_none());
    let base = details_using(&ctx, &label, &FIXTURE).ok_or("profile details")?;
    fs::create_dir(ctx.home.join(".codex"))?;
    let (profiles, _) = crate::resource_agents::collect(&ctx, None, &[])?;
    assert!(profiles.iter().any(|profile| profile.name == label
        && profile.management == crate::resource::Management::Adopted));
    assert!(
        profiles
            .iter()
            .any(|profile| profile.name == "codex:default"
                && profile.management == crate::resource::Management::Untracked)
    );
    assert_eq!(base.plugin, "item@market");
    assert_eq!(base.preference, "按原生设置");
    assert!(base.observed_model.is_none());
    let cancelled = configure_using(&ctx, &label, &FIXTURE, &b"\n"[..], |_, _| {
        panic!("empty choice must not reach confirmation")
    })
    .err()
    .ok_or("cancelled")?;
    assert_eq!(cancelled.code, "cancelled");
    let refused = configure_using(&ctx, &label, &FIXTURE, &b"1\n"[..], |prompt, yes| {
        assert!(!yes);
        assert!(
            prompt.contains("启用") && prompt.contains(&label) && prompt.contains("item@market")
        );
        Err(Error::attention("cancelled", "fixture cancellation"))
    })
    .err()
    .ok_or("declined")?;
    assert_eq!(refused.code, "cancelled");
    assert!(!path.exists());
    configure_using(&ctx, &label, &FIXTURE, &b"2\n"[..], |prompt, yes| {
        assert!(!yes && prompt.contains("停用"));
        Ok(())
    })?;
    assert_eq!(select(&ctx, &label, &FIXTURE)?.mode(), Mode::Disabled);
    configure_using(&ctx, &label, &FIXTURE, &b"3\n"[..], |prompt, yes| {
        assert!(!yes && prompt.contains("按原生设置"));
        Ok(())
    })?;
    assert_eq!(select(&ctx, &label, &FIXTURE)?.mode(), Mode::Native);
    assert!(
        configure_using(&ctx, &label, &FIXTURE, &b"\xFF\n"[..], |_, _| panic!(
            "invalid terminal input"
        ))
        .is_err()
    );
    Ok(())
}

#[test]
fn session_launch_preview_and_confirmation_preserve_the_same_workspace_and_arguments() -> TestResult
{
    let (_temp, ctx, label) = installed()?;
    let selected = select(&ctx, &label, &FIXTURE)?;
    set_mode(&ctx, &selected, Mode::Enabled, &FIXTURE)?;
    fs::write(
        selected.profile.path.join("chosen.config.toml"),
        b"model='isolated'\n",
    )?;
    let config = fs::read(selected.profile.path.join(CONFIG))?;
    let launch = LaunchOptions {
        name: Some("chosen"),
        dry: true,
        yes: false,
        interactive: false,
    };
    let unsupported = |_: &str, _: bool| panic!("should not prompt");
    let never_launch = |_| panic!("should not launch");
    let preview = launch_using(&ctx, &label, launch, &FIXTURE, unsupported, never_launch)?;
    assert_eq!(
        preview["argv"],
        json!([
            "--profile",
            "chosen",
            "-c",
            concat!("plugins.item@market", ".enabled=true"),
        ])
    );
    assert_eq!(preview["persistentWrites"], false);
    assert_eq!(preview["nativeProfile"], "chosen");
    assert_eq!(preview["preference"], "enabled");
    let nonterminal = LaunchOptions {
        dry: false,
        ..launch
    };
    let error = launch_using(
        &ctx,
        &label,
        nonterminal,
        &FIXTURE,
        unsupported,
        never_launch,
    )
    .err()
    .ok_or("nonterminal")?;
    assert_eq!(error.code, "interactive_terminal_required");
    let interactive = LaunchOptions {
        interactive: true,
        ..nonterminal
    };
    assert!(
        launch_using(
            &ctx,
            &label,
            interactive,
            &FIXTURE,
            |prompt, yes| {
                assert!(!yes && prompt.contains("工作目录") && prompt.contains("原生档案 chosen"));
                Err(Error::attention("cancelled", "fixture cancellation"))
            },
            never_launch
        )
        .is_err()
    );
    let confirmed = launch_using(
        &ctx,
        &label,
        interactive,
        &FIXTURE,
        |prompt, _| {
            assert!(prompt.contains(&label) && prompt.contains("启用"));
            Ok(())
        },
        |command| {
            Ok(json!({
                "argv": command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>()
            }))
        },
    )?;
    assert_eq!(confirmed["argv"], preview["argv"]);
    assert_eq!(fs::read(selected.profile.path.join(CONFIG))?, config);
    Ok(())
}

#[test]
fn session_rejects_forged_preference_bindings_and_incomplete_publication() -> TestResult {
    let (_temp, ctx, label) = installed()?;
    let selected = select(&ctx, &label, &FIXTURE)?;
    let workspace = Directory::open(&ctx.home)?;
    let preference = selected.root.path.join(PREFERENCE);
    let mut payload = json!({
        "schema_version":1,"authority":selected.authority,
        "revision":1,"mode":"disabled","updated_at":0,
    });
    let correct = payload.clone();
    for (key, changed) in [
        ("schema_version", json!(2)),
        ("authority", json!("foreign")),
        ("revision", json!(0)),
    ] {
        payload[key] = changed;
        let bytes = serde_json::to_vec(&payload)?;
        fs::write(&preference, &bytes)?;
        assert!(admit(&ctx, &selected, &workspace, None, None, &FIXTURE).is_err());
        assert_eq!(fs::read(&preference)?, bytes);
        payload = correct.clone();
    }
    fs::remove_file(&preference)?;
    let entry = owner(&ctx, &label)?.ok_or("published profile")?;
    let journal_path = ctx
        .state
        .join("operations")
        .join(format!("{}.json", entry.adoption.id));
    let mut pending = entry.adoption;
    pending.phase = Phase::Prepared;
    let bytes = serde_json::to_vec(&pending)?;
    fs::write(&journal_path, &bytes)?;
    assert!(select(&ctx, &label, &FIXTURE).is_err());
    assert!(set_mode(&ctx, &selected, Mode::Enabled, &FIXTURE).is_err());
    assert_eq!(fs::read(journal_path)?, bytes);
    assert!(!preference.exists());
    Ok(())
}

#[test]
fn session_admission_keeps_pending_transactions_and_rebound_workspaces_intact() -> TestResult {
    let (_temp, ctx, label) = installed()?;
    let selected = select(&ctx, &label, &FIXTURE)?;
    let project = ctx.home.join("test-workspace");
    fs::create_dir(&project)?;
    let workspace = Directory::open(&project)?;
    fs::rename(&project, ctx.home.join("test-workspace-original"))?;
    fs::create_dir(&project)?;
    assert!(admit(&ctx, &selected, &workspace, None, None, &FIXTURE).is_err());

    let workspace = Directory::open(&project)?;
    let mut active = owner(&ctx, &label)?.ok_or("publication")?.adoption;
    active.phase = Phase::Unknown;
    let path = ctx.state.join("active.json");
    let bytes = serde_json::to_vec(&active)?;
    fs::write(&path, &bytes)?;
    let error = admit(&ctx, &selected, &workspace, None, None, &FIXTURE)
        .err()
        .ok_or("pending launch must be blocked")?;
    assert_eq!(error.code, "recovery_required");
    let error = set_mode(&ctx, &selected, Mode::Enabled, &FIXTURE)
        .err()
        .ok_or("pending preference update must be blocked")?;
    assert_eq!(error.code, "recovery_required");
    assert_eq!(fs::read(&path)?, bytes);
    assert!(!selected.root.path.join(PREFERENCE).exists());
    Ok(())
}
