use super::*;
use crate::{
    cli::Cli,
    domain::{Action, Kind, Observation, Package, Plan, Provider},
    test_support::TestResult,
};
use clap::Parser as _;
use std::fs;

fn setup() -> Result<(
    tempfile::TempDir,
    Context,
    InstallTarget,
    InstalledPlugin<'static>,
)> {
    let temporary = tempfile::tempdir_in("/private/tmp")?;
    let home = temporary.path().join("home");
    let profile = home.join(".claude-mi");
    fs::create_dir_all(&profile)?;
    let cli = Cli::try_parse_from([
        "macops",
        "--home",
        home.to_str().ok_or_else(conflict)?,
        "app",
    ])
    .map_err(|error| Error::new("fixture_arguments", error.to_string()))?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    fs::write(
        profile.join("settings.json"),
        br#"{"model":"original","env":{"CANARY":"fixture-secret"}}"#,
    )?;
    let candidate = crate::resource_agents::profile_roots(&ctx)?
        .into_iter()
        .find(|resource| resource.name == "claude:mi")
        .ok_or_else(conflict)?;
    let target = InstallTarget::capture(&ctx, &candidate)?;
    let stage = ctx.state.join("claude-profile.Source/home/.claude");
    let payload = stage.join("plugins/cache/fixture-market/item/1.0");
    fs::create_dir_all(&payload)?;
    fs::write(payload.join("SKILL.md"), b"fixture plugin")?;
    fs::write(stage.join("settings.json"),
        br#"{"extraKnownMarketplaces":{"fixture-market":{"source":{"source":"directory"}}},"enabledPlugins":{"item@fixture-market":true}}"#)?;
    fs::write(
        stage.join("plugins/known_marketplaces.json"),
        br#"{"fixture-market":{"fixture":"staged"}}"#,
    )?;
    fs::write(
        stage.join("plugins/installed_plugins.json"),
        br#"{"version":2,"plugins":{"item@fixture-market":[{"version":"1.0","scope":"user"}]}}"#,
    )?;
    let plugin = InstalledPlugin {
        native_id: "item@fixture-market".into(),
        marketplace: "fixture-market",
        offer: "pinned-fixture".into(),
        config: stage.join("settings.json"),
        payload,
        market: None,
        checkout: None,
    };
    Ok((temporary, ctx, target, plugin))
}

fn original(ctx: &Context) -> Result<Journal> {
    let package = Package {
        id: "fixture:plugin".into(),
        name: "item".into(),
        kind: Kind::Plugin,
        official_url: "https://example.org/fixture".into(),
        paths: vec![],
        provider: Provider::Macops,
        exact_version_required: true,
    };
    let plan = Plan {
        action: Action::Install,
        recipe_sha256: fingerprint(&package)?,
        package,
        before: Observation::Missing { evidence: None },
        target: Some("1.0".into()),
        command: None,
        query: None,
        delegated: false,
        synchronous: true,
        warnings: vec![],
        timeout_secs: 30,
        brew: None,
        npm_binding: None,
        uv_owner: None,
    };
    crate::operations::new_journal(ctx, plan)
}

fn journal_fixture(
    ctx: &Context,
    store: &Store,
    native: &Journal,
    target: &InstallTarget,
    plugin: &InstalledPlugin<'_>,
) -> Result<Reserved> {
    let reserved = prepare(ctx, store, native, target, plugin, &target.open()?)?;
    Store::atomic_new(
        &store.root,
        &format!("plugin-target-{}.json", reserved.journal.id),
        &reserved,
    )?;
    Ok(reserved)
}

impl crate::test_support::PluginTargetFixture {
    pub(crate) fn pending_for_recommendation(
        client: &str,
        offer: String,
    ) -> Result<(tempfile::TempDir, Context, InstallTarget, String)> {
        let (temporary, ctx, mut target, mut plugin) = setup()?;
        if client == "codex" {
            let dir = ctx.home.join(".codex");
            fs::create_dir(&dir)?;
            let selected = crate::resource_agents::profile_roots(&ctx)?
                .into_iter()
                .find(|item| item.name == "codex:default")
                .ok_or_else(conflict)?;
            target = InstallTarget::capture(&ctx, &selected)?;
            let stage = ctx.state.join("codex-profile.Probe/home/.codex");
            plugin.config = stage.join("config.toml");
            plugin.payload = stage.join("plugins/cache/fixture-market/item/1.0");
            let market = stage.join(".tmp/marketplaces/fixture-market");
            fs::create_dir_all(&plugin.payload)?;
            fs::create_dir_all(&market)?;
            fs::write(&plugin.config, b"[marketplaces.fixture-market]\nref='pinned'\n[plugins.'item@fixture-market']\nenabled=true\n")?;
            fs::write(plugin.payload.join("README"), b"selected payload")?;
            plugin.market = Some(market);
        }
        plugin.offer = offer;
        let store = Store::lock(&ctx)?;
        let reserved = journal_fixture(&ctx, &store, &original(&ctx)?, &target, &plugin)?;
        store.save(&reserved.journal)?;
        Ok((temporary, ctx, target, reserved.journal.id))
    }
}

fn matches_publication(
    ctx: &Context,
    journal: &Journal,
    target: &InstallTarget,
    offer: &str,
) -> Result<bool> {
    if !is_publication(journal) {
        return Ok(false);
    }
    let directory = Directory::open(&ctx.state)?;
    let saved = frozen(directory.file(), journal)?;
    Ok(saved.profile == *target && saved.offer == offer)
}

#[test]
fn recovers_after_the_payload_and_first_record_were_published() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let reserved = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    store.save(&reserved.journal)?;
    let other_path = ctx.home.join(".claude-other");
    fs::create_dir(&other_path)?;
    let other_profile = crate::resource_agents::profile_roots(&ctx)?
        .into_iter()
        .find(|item| item.name == "claude:other")
        .ok_or("other profile")?;
    let other = InstallTarget::capture(&ctx, &other_profile)?;
    drop(store);
    let blocked = crate::operations::recover_before_write_checked(&ctx, None, |journal| {
        recovery_matches(journal, Some(&other))
    })
    .err()
    .ok_or("another target should not finish pending writes")?;
    assert_eq!(blocked.code, "recovery_required");
    assert!(plugin.payload.is_dir());
    assert!(recovery_matches(&reserved.journal, Some(&target))?);
    let store = Store::lock(&ctx)?;
    move_tree(&reserved.publication.trees[0])?;
    crate::source_files::publish_checked(&reserved.publication.files[0], || Ok(()))?;
    drop(store);
    let recovered = crate::operations::recover_before_write_checked(&ctx, None, |journal| {
        recovery_matches(journal, Some(&target))
    })?
    .ok_or("recovered operation")?
    .result;
    assert_eq!(recovered["journal"]["phase"], "succeeded");
    assert_eq!(recovered["journal"]["id"], reserved.journal.id);
    assert!(!plugin.payload.exists());
    let live = target.open()?.path;
    assert_eq!(
        fs::read(live.join("plugins/cache/fixture-market/item/1.0/SKILL.md"))?,
        b"fixture plugin"
    );
    let settings: Value = crate::paths::regular_json(&live.join("settings.json"))?;
    assert_eq!(settings["model"], "original");
    assert_eq!(settings["enabledPlugins"]["item@fixture-market"], true);
    assert_eq!(
        target.directory,
        super::target(&reserved.journal)?.ok_or("target")?.directory
    );
    assert!(matches_publication(
        &ctx,
        &reserved.journal,
        &target,
        &plugin.offer
    )?);
    assert!(!matches_publication(
        &ctx,
        &reserved.journal,
        &target,
        "other offer"
    )?);
    let store = Store::lock(&ctx)?;
    assert_eq!(
        resume_publication(&ctx, &store, &native, &target)?.ok_or("result")?,
        recovered
    );
    Ok(())
}

#[test]
fn blocks_a_changed_configuration_and_preserves_the_prepared_objects() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let reserved = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    let settings = target.open()?.path.join("settings.json");
    let changed = br#"{"model":"outside edit"}"#;
    fs::write(&settings, changed)?;
    let result = resume_publication(&ctx, &store, &native, &target)?.ok_or("publication")?;
    assert_eq!(result["journal"]["phase"], "unknown");
    assert_eq!(fs::read(&settings)?, changed);
    assert!(plugin.payload.is_dir());
    assert_eq!(
        crate::source_files::position(&reserved.publication.files[2])?,
        Position::Unknown
    );
    let mut journal = crate::operations::locked_journal(&store, &reserved.journal.id)?;
    let marked = recover(&ctx, &store, &mut journal, true, true)?;
    assert_eq!(marked["journal"]["phase"], "failed");
    assert_eq!(fs::read(settings)?, changed);
    assert!(previous(&ctx, &store, &target, &plugin.offer)?.is_none());
    assert!(previous_stage(&ctx, |op| Ok(op.id == journal.id))?.is_none());
    Ok(())
}

#[test]
fn refuses_a_permissions_change_in_the_prepared_checkout() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let child = plugin.payload.join(".git");
    fs::create_dir(&child)?;
    fs::write(child.join("HEAD"), b"fixture HEAD")?;
    let mut reserved = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    store.save(&reserved.journal)?;
    let original_mode = fs::metadata(&child)?.permissions();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o755 | 0o020))?;
    let result = recover(&ctx, &store, &mut reserved.journal, false, false)?;
    assert_eq!(result["journal"]["phase"], "unknown");
    assert!(plugin.payload.is_dir());
    assert!(
        !target
            .open()?
            .path
            .join("plugins/installed_plugins.json")
            .exists()
    );
    fs::set_permissions(child, original_mode)?;
    let result = recover(&ctx, &store, &mut reserved.journal, false, false)?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    Ok(())
}

#[test]
fn cannot_publish_into_a_shared_or_replaced_destination() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let sibling = ctx.home.join("other.json");
    fs::hard_link(root.path.join("settings.json"), &sibling)?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert!(plugin.payload.is_dir());
    assert!(
        !root
            .path
            .join("plugins/cache/fixture-market/item/1.0")
            .exists()
    );
    fs::remove_file(sibling)?;
    let lease = lock(&root)?;
    assert!(lock(&root).is_err());
    drop(lease);
    fs::rename(&root.path, ctx.home.join("old-profile"))?;
    fs::create_dir(&root.path)?;
    assert!(target.open().is_err());
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    Ok(())
}

#[test]
fn replaces_interrupted_private_staging_without_reusing_its_secret_copy() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let staged = prepare(&ctx, &store, &native, &target, &plugin, &target.open()?)?;
    let reserved_candidate = staged.publication.files[2].retained.path();
    assert!(fs::read_to_string(&reserved_candidate)?.contains("fixture-secret"));
    assert!(fs::read_to_string(target.open()?.path.join("settings.json"))?.contains("original"));
    assert!(!is_publication(&native));
    assert!(resume_publication(&ctx, &store, &native, &target)?.is_none());
    let next = publish(&ctx, &store, &native, &target, &plugin)?;
    assert_eq!(next["journal"]["phase"], "succeeded");
    assert!(!reserved_candidate.exists());
    assert!(!target.open()?.path.join(".claude.json").exists());
    Ok(())
}

#[test]
fn failure_while_staging_last_file_cleans_all_earlier_preimages() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let settings = root.path.join("settings.json");
    let plugins = root.path.join("plugins");
    fs::create_dir(&plugins)?;
    let stage = plugin.config.parent().ok_or("staged profile")?;
    let updates = claude_config(&root, stage, &plugin)?;
    let external = br#"{"model":"changed while preparing"}"#;
    fs::write(&settings, external)?;
    let name = format!("plugin-target-{}-staging.json", native.id);
    assert!(stage_updates(&store, &name, &target, updates).is_err());
    assert_eq!(fs::read(settings)?, external);
    for directory in [&root.path, &plugins] {
        for entry in fs::read_dir(directory)? {
            assert!(
                !entry?
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".macops-source-ledger-")
            );
        }
    }
    assert!(
        !ctx.state
            .join(format!("plugin-target-{}-staging.json", native.id))
            .exists()
    );
    assert!(plugin.payload.exists());
    let history = crate::store::history(&ctx)?;
    assert!(history.is_empty(), "{history:?}");
    Ok(())
}

#[test]
fn occupied_payload_and_locked_profile_preserve_the_original_registry() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let reserved = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    let root = target.open()?;
    let transfer = &reserved.publication.trees[0];
    fs::create_dir(transfer.target.path())?;
    fs::write(transfer.target.path().join("foreign"), b"foreign")?;
    let before = fs::read(root.path.join("settings.json"))?;
    let lease = lock(&root)?;
    assert!(resume_publication(&ctx, &store, &native, &target).is_err());
    drop(lease);
    let refused = resume_publication(&ctx, &store, &native, &target)?.ok_or("publication")?;
    assert_eq!(refused["journal"]["phase"], "unknown");
    assert_eq!(
        fs::read(transfer.target.path().join("foreign"))?,
        b"foreign"
    );
    assert!(plugin.payload.is_dir());
    assert_eq!(fs::read(root.path.join("settings.json"))?, before);
    Ok(())
}

#[test]
fn replacement_of_the_held_profile_lock_cannot_authorize_a_write() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let marker = root.path.join(LOCK_FILE);
    let untouched = ctx.home.join("sibling-lock");
    fs::write(&untouched, b"owner canary")?;
    symlink(&untouched, &marker)?;
    assert!(lock(&root).is_err());
    assert_eq!(fs::read(&untouched)?, b"owner canary");
    fs::remove_file(&marker)?;
    fs::hard_link(&untouched, &marker)?;
    assert!(lock(&root).is_err());
    fs::remove_file(&marker)?;
    fs::write(&marker, b"")?;
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o644))?;
    assert!(lock(&root).is_err());
    fs::remove_file(&marker)?;
    let original = lock(&root)?;
    let mut reserved = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    store.save(&reserved.journal)?;
    fs::rename(&marker, root.path.join("old-lock"))?;
    fs::write(&marker, b"")?;
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600))?;
    let refused = settle(
        &store,
        &mut reserved.journal,
        &reserved.publication,
        &original,
    )?;
    assert_eq!(refused["journal"]["phase"], "unknown");
    assert!(plugin.payload.exists());
    assert!(!root.path.join("plugins/installed_plugins.json").exists());
    Ok(())
}

#[test]
fn refuses_preexisting_source_entries_and_invalid_config_before_staging() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let profile = target.open()?.path;
    let settings = profile.join("settings.json");
    let staging = format!("plugin-target-{}-staging.json", native.id);
    for contents in [
        br#"{"extraKnownMarketplaces":{"fixture-market":{"source":{"source":"other"}}}}"#
            .as_slice(),
        br#"{"enabledPlugins":{"item@fixture-market":false}}"#,
        br#"{"enabledPlugins":true}"#,
        b"not JSON",
    ] {
        fs::write(&settings, contents)?;
        assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
        assert_eq!(fs::read(&settings)?, contents);
        assert!(!profile.join("plugins").exists());
        assert!(!ctx.state.join(&staging).exists());
    }
    fs::write(settings, b"{}")?;
    fs::create_dir(profile.join("plugins"))?;
    let registrations = profile.join("plugins/installed_plugins.json");
    fs::write(&registrations, br#"{"version":1,"plugins":{}}"#)?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert_eq!(fs::read(registrations)?, br#"{"version":1,"plugins":{}}"#);
    assert!(plugin.payload.exists());
    Ok(())
}

#[test]
fn rejects_rewritten_frozen_paths_and_an_unrelated_active_operation() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let mut reserved = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    let owner = reserved.publication.profile.client.clone();
    reserved.publication.profile.client = "cursor".into();
    assert!(reserved.publication.verify_paths(&ctx.state).is_err());
    reserved.publication.profile.client = owner;
    let step = reserved.publication.files.pop().ok_or("file step")?;
    assert!(reserved.publication.verify_paths(&ctx.state).is_err());
    reserved.publication.files.push(step);
    let original_slot = reserved.publication.files[0].live.clone();
    reserved.publication.files[0].live.name = "other.json".into();
    assert!(reserved.publication.verify_paths(&ctx.state).is_err());
    reserved.publication.files[0].live = original_slot;
    reserved.publication.trees[0].target.name = "other".into();
    assert!(reserved.publication.verify_paths(&ctx.state).is_err());
    store.save(&native)?;
    assert!(resume_publication(&ctx, &store, &native, &target).is_err());
    assert!(plugin.payload.exists());
    assert!(
        !target
            .open()?
            .path
            .join("plugins/known_marketplaces.json")
            .exists()
    );
    Ok(())
}

#[test]
fn refuses_linked_plugin_directories_and_versions_of_the_wrong_format() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let sibling = ctx.home.join("sibling-plugins");
    fs::create_dir(&sibling)?;
    let plugins = root.path.join("plugins");
    symlink(&sibling, &plugins)?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert_eq!(fs::read_dir(&sibling)?.count(), 0);
    fs::remove_file(&plugins)?;
    fs::create_dir(&plugins)?;
    fs::set_permissions(&plugins, fs::Permissions::from_mode(0o777))?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert!(plugin.payload.exists());
    let config = plugin
        .config
        .parent()
        .ok_or("stage")?
        .join("plugins/installed_plugins.json");
    fs::write(
        &config,
        br#"{"version":2,"plugins":{"item@fixture-market":[]}}"#,
    )?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    fs::write(
        &config,
        br#"{"version":2,"plugins":{"item@fixture-market":{}}}"#,
    )?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert_eq!(fs::read_dir(sibling)?.count(), 0);
    Ok(())
}

#[test]
fn codex_config_rejects_same_market_and_non_table_plugin_entries() -> TestResult {
    let (_temporary, ctx, _, mut plugin) = setup()?;
    let profile = ctx.home.join(".codex");
    fs::create_dir(&profile)?;
    let root = Directory::open(&profile)?;
    let staged = plugin.config.with_file_name("config.toml");
    fs::write(&staged, b"[plugins.'item@fixture-market']\nenabled=true\n[marketplaces.fixture-market]\nref='fixture'\n")?;
    plugin.config = staged;
    let real = profile.join("config.toml");
    for text in [
        "plugins = 'bad'",
        "marketplaces = 'bad'",
        "[marketplaces.fixture-market]\nref='another'",
        "[plugins.'item@fixture-market']\nenabled=false",
        "[invalid",
    ] {
        fs::write(&real, text)?;
        assert!(codex_config(&root, &plugin).is_err(), "{text}");
        assert_eq!(fs::read_to_string(&real)?, text);
    }
    fs::write(&plugin.config, b"[not_the_selected_plugin]\nvalue=1\n")?;
    assert!(codex_config(&root, &plugin).is_err());
    Ok(())
}

#[test]
fn recovery_rejects_a_changed_reservation_and_allows_no_new_target() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let mut record = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    store.save(&record.journal)?;
    assert!(!recovery_matches(&record.journal, None)?);
    assert!(recovery_matches(&native, Some(&target))?);
    let name = format!("plugin-target-{}.json", record.journal.id);
    let good = fs::read(ctx.state.join(&name))?;
    for field in ["id", "plan_sha256", "scope"] {
        let mut altered: Value = serde_json::from_slice(&good)?;
        if field == "scope" {
            altered["journal"]["scope"]["home"] = json!(ctx.home.join("different-home"));
        } else {
            altered["journal"][field] = json!("different");
        }
        Store::atomic(&store.root, &name, &altered)?;
        assert!(frozen(&store.root, &record.journal).is_err(), "{field}");
    }
    fs::remove_file(ctx.state.join(&name))?;
    assert!(frozen(&store.root, &record.journal).is_err());
    Store::atomic_bytes(&store.root, &name, &good, true)?;
    record.journal.started_at += 1;
    assert!(frozen(&store.root, &record.journal).is_err());
    record.journal.started_at -= 1;
    record.journal.plan.warnings.clear();
    assert!(frozen(&store.root, &record.journal).is_err());
    record = serde_json::from_slice(&good)?;
    let journal = &mut record.journal;
    for role in ["home", "state", "settings", "cache"] {
        let mut altered = ctx.clone();
        let path = altered.home.join("external-root");
        match role {
            "home" => altered.home = path,
            "state" => altered.state = path,
            "settings" => altered.settings = path,
            _ => altered.cache = path,
        }
        let error = recover(&altered, &store, journal, false, false)
            .err()
            .ok_or("changed scope")?;
        assert_eq!(error.code, "plugin_target_scope_changed");
    }
    assert!(plugin.payload.is_dir());
    assert!(
        !target
            .open()?
            .path
            .join("plugins/known_marketplaces.json")
            .exists()
    );
    assert_eq!(fs::read(ctx.state.join(name))?, good);
    Ok(())
}

#[test]
fn interrupted_staging_refuses_cleanup_from_a_different_binding_or_unowned_path() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let prepared = prepare(&ctx, &store, &native, &target, &plugin, &root)?;
    let name = format!("plugin-target-{}-staging.json", native.id);
    let mut staging: Staging = Store::read(&store.root, &name)?.ok_or("staging journal")?;
    staging.profile.name = "different".into();
    Store::atomic(&store.root, &name, &staging)?;
    assert!(clear_staging(&store, &name, &target).is_err());
    staging.profile.name.clone_from(&target.name);
    staging.slots[0].name = "unexpected".into();
    Store::atomic(&store.root, &name, &staging)?;
    assert!(clear_staging(&store, &name, &target).is_err());
    assert!(resume_publication(&ctx, &store, &native, &target)?.is_none());
    assert!(prepared.publication.files[0].retained.path().is_file());
    assert!(!root.path.join("plugins/known_marketplaces.json").exists());
    let resigned = Staging {
        profile: target.clone(),
        slots: prepared
            .publication
            .files
            .iter()
            .map(|file| file.retained.clone())
            .collect(),
    };
    Store::atomic(&store.root, &name, &resigned)?;
    clear_staging(&store, &name, &target)?;
    assert!(!prepared.publication.files[0].retained.path().exists());
    Ok(())
}

#[test]
fn refuses_to_continue_from_a_changed_pinned_marketplace_checkout() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let remote = ctx.state.join("claude-profile.Source/checkout");
    fs::create_dir(&remote)?;
    fs::write(remote.join("manifest"), b"fixed manifest")?;
    let original = slot(&remote)?;
    let witness = crate::source_tree::observe(&original)?.ok_or("marketplace evidence")?;
    let checkout = (original, witness);
    let plugin = InstalledPlugin {
        checkout: Some(&checkout),
        ..plugin
    };
    let mut operation = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    store.save(&operation.journal)?;
    fs::write(remote.join("manifest"), b"changed manifest")?;
    let refusal = recover(&ctx, &store, &mut operation.journal, false, false)?;
    assert_eq!(refusal["journal"]["phase"], "unknown");
    assert!(plugin.payload.exists());
    assert!(
        !target
            .open()?
            .path
            .join("plugins/installed_plugins.json")
            .exists()
    );
    fs::write(remote.join("manifest"), b"fixed manifest")?;
    let output = recover(&ctx, &store, &mut operation.journal, false, false)?;
    assert_eq!(output["journal"]["phase"], "succeeded");
    fs::write(remote.join("manifest"), b"third manifest")?;
    assert!(previous(&ctx, &store, &target, &plugin.offer).is_err());
    Ok(())
}

#[test]
fn refuses_to_move_a_staged_payload_that_contains_a_link_outside_its_sandbox() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let other = ctx.home.join("unrelated.txt");
    fs::write(&other, b"unrelated")?;
    std::os::unix::fs::symlink(&other, plugin.payload.join("foreign"))?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert_eq!(fs::read(other)?, b"unrelated");
    assert!(plugin.payload.exists());
    assert!(!root.path.join("plugins/installed_plugins.json").exists());
    Ok(())
}

#[test]
fn failed_reservation_write_keeps_authoritative_sidecar_and_leaves_live_files_intact() -> TestResult
{
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let name = format!("plugin-target-{}-receipt.json", native.id);
    Store::atomic_new(&store.root, &name, &"reserved by another attempt")?;
    let before = fs::read(ctx.state.join(&name))?;
    assert!(publish(&ctx, &store, &native, &target, &plugin).is_err());
    assert_eq!(fs::read(ctx.state.join(name))?, before);
    assert!(crate::store::active(&ctx)?.is_none());
    assert!(plugin.payload.exists());
    assert!(
        !target
            .open()?
            .path
            .join("plugins/installed_plugins.json")
            .exists()
    );
    Ok(())
}

#[test]
fn a_new_target_cannot_claim_an_old_prepared_publication() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let prepared = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    let mut other = target.clone();
    other.name = "claude:wrong-selection".into();
    assert!(resume_publication(&ctx, &store, &native, &other).is_err());
    assert!(matches_publication(
        &ctx,
        &prepared.journal,
        &target,
        &plugin.offer
    )?);
    assert!(!matches_publication(
        &ctx,
        &prepared.journal,
        &other,
        &plugin.offer
    )?);
    let settings = target.open()?.path.join("settings.json");
    assert!(fs::read_to_string(settings)?.contains("original"));
    assert!(crate::store::active(&ctx)?.is_none());
    Ok(())
}

#[test]
fn wrong_client_cannot_leave_an_unregistered_copy_of_prepared_settings() -> TestResult {
    let (_temporary, ctx, mut target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let root = target.open()?;
    let settings = root.path.join("settings.json");
    let before = fs::read(&settings)?;
    target.client = "codex".into();
    assert!(prepare(&ctx, &store, &native, &target, &plugin, &root).is_err());
    for directory in [&root.path, &root.path.join("plugins")] {
        for entry in fs::read_dir(directory)? {
            assert!(
                !entry?
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".macops-source-ledger-")
            );
        }
    }
    assert_eq!(fs::read(settings)?, before);
    assert!(plugin.payload.exists());
    Ok(())
}

#[test]
fn candidate_tree_paths_cannot_escape_the_selected_profile() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let root = target.open()?;
    assert!(within(&root, Path::new("../outside")).is_err());
    let empty = ctx.home.join("unrelated");
    fs::create_dir(&empty)?;
    assert!(transfer(&root, &root.path, &plugin.payload).is_err());
    assert!(transfer(&root, &ctx.home, &empty.join("missing-payload")).is_err());
    assert_eq!(fs::read_dir(&empty)?.count(), 0);
    Ok(())
}

#[test]
fn a_replaced_payload_file_cannot_resume_even_if_its_contents_match() -> TestResult {
    let (_temporary, ctx, target, plugin) = setup()?;
    let store = Store::lock(&ctx)?;
    let native = original(&ctx)?;
    let mut operation = journal_fixture(&ctx, &store, &native, &target, &plugin)?;
    store.save(&operation.journal)?;
    let member = plugin.payload.join("SKILL.md");
    let retained = ctx.state.join("original-member");
    fs::rename(&member, &retained)?;
    fs::write(&member, b"fixture plugin")?;
    let refused = recover(&ctx, &store, &mut operation.journal, false, false)?;
    assert_eq!(refused["journal"]["phase"], "unknown");
    assert!(
        !target
            .open()?
            .path
            .join("plugins/installed_plugins.json")
            .exists()
    );
    fs::remove_file(&member)?;
    fs::rename(&retained, &member)?;
    let result = recover(&ctx, &store, &mut operation.journal, false, false)?;
    assert_eq!(result["journal"]["phase"], "succeeded");
    Ok(())
}
