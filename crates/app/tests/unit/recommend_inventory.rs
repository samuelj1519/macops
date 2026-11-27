use super::*;

fn fixture_context(home: &std::path::Path) -> crate::error::Result<Context> {
    let mut cli = <crate::cli::Cli as clap::Parser>::parse_from(["mo", "app"]);
    cli.home = Some(home.to_path_buf());
    Context::from_cli(&cli, &mut vec![])
}

#[test]
fn local_inventory_matches_canonical_repository_without_guessing_from_the_name()
-> crate::test_support::TestResult {
    let root = tempfile::tempdir_in("/private/tmp")?;
    let ctx = fixture_context(root.path())?;
    let mut item = Item {
        id: "capability:fixture".into(),
        name: "devnorm".into(),
        summary: String::new(),
        managed: true,
        management: Management::Adopted,
        directory: false,
        path: None,
        source: None,
        agents: vec!["codex:default".into(), "claude:fixture".into()],
        loaded: None,
        details: vec![],
        list: crate::browser::ListDetail::Plain,
        inventory: Some(crate::browser::InventoryInfo {
            groups: vec![],
            manager: "skills-cli".into(),
            status: State::Present,
            version: Some("1.11.0".into()),
            aliases: vec!["skill:devnorm".into()],
            native_record: None,
            repository: Some(crate::policy::source_git::Repository::parse(
                "owner/devnorm-kit",
            )?),
        }),
    };
    assert!(related_item(
        &ctx,
        &item,
        Kind::Capabilities,
        "owner/devnorm-kit",
        Some("OWNER/devnorm-kit")
    )?);
    assert!(!related_item(
        &ctx,
        &item,
        Kind::Capabilities,
        "devnorm",
        Some("other/devnorm-kit")
    )?);
    assert!(related_item(
        &ctx,
        &item,
        Kind::Capabilities,
        "skill:devnorm",
        None
    )?);
    assert!(related_item(
        &ctx,
        &item,
        Kind::Capabilities,
        "capability:fixture",
        None
    )?);
    assert!(!related_item(
        &ctx,
        &item,
        Kind::Capabilities,
        "norm",
        None
    )?);
    item.inventory.as_mut().ok_or("inventory")?.status = State::Declared;
    assert!(!related_item(
        &ctx,
        &item,
        Kind::Apps,
        "owner/devnorm-kit",
        Some("owner/devnorm-kit")
    )?);
    item.inventory.as_mut().ok_or("inventory")?.status = State::Present;
    let related = Related {
        views: vec![(
            Kind::Capabilities,
            View {
                scope: "global".into(),
                items: vec![item],
                coverage: "partial",
                difference_coverage: "not_applicable",
                observed_at: None,
                note: String::new(),
                incomplete: true,
            },
        )],
        incomplete: true,
        repository_query: false,
    };
    let status = related.status();
    assert!(status.contains("已接管 1") && status.contains("1.11.0"));
    assert!(status.contains("codex:default、claude:fixture") && status.contains("无法核验"));
    let maintenance = related.maintenance(3);
    assert!(
        maintenance
            .lines()
            .any(|line| { line.split_whitespace().take(2).eq(["3", "查看技能与插件"]) })
    );
    Ok(())
}

#[test]
fn repository_lookup_associates_plugins_using_their_native_market_registration()
-> crate::test_support::TestResult {
    let root = tempfile::tempdir_in("/private/tmp")?;
    let ctx = fixture_context(root.path())?;
    let plugins = root.path().join("plugins");
    std::fs::create_dir(&plugins)?;
    let mut item = Item {
        id: "capability:fixture-plugin".into(),
        name: "devnorm@local".into(),
        summary: String::new(),
        managed: true,
        management: Management::Native,
        directory: false,
        path: None,
        source: None,
        agents: vec!["claude:fixture".into()],
        loaded: None,
        details: vec![],
        list: crate::browser::ListDetail::Plain,
        inventory: Some(crate::browser::InventoryInfo {
            groups: vec![],
            manager: "claude-plugin".into(),
            status: State::Present,
            version: Some("1.11.0".into()),
            aliases: vec![],
            repository: None,
            native_record: Some(plugins.join("installed_plugins.json")),
        }),
    };
    let matched = |item: &Item| {
        related_item(
            &ctx,
            item,
            Kind::Capabilities,
            "owner/devnorm-kit",
            Some("owner/devnorm-kit"),
        )
    };
    assert!(!matched(&item)?);
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        br#"{"local":{"source":{"source":"github","repo":"owner/devnorm-kit"}}}"#,
    )?;
    assert!(matched(&item)?);
    assert!(!related_item(
        &ctx,
        &item,
        Kind::Capabilities,
        "devnorm",
        Some("other/devnorm-kit")
    )?);
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        br#"{"local":{"source":{"source":"git","url":"https://github.com/owner/devnorm-kit.git"}}}"#,
    )?;
    assert!(matched(&item)?);
    std::fs::write(plugins.join("known_marketplaces.json"), b"{")?;
    assert!(matched(&item).is_err());
    let config = root.path().join("config.toml");
    std::fs::write(
        &config,
        "[marketplaces.local]\nsource_type = 'git'\nsource = 'https://github.com/owner/devnorm-kit.git'\n",
    )?;
    let info = item.inventory.as_mut().ok_or("inventory")?;
    info.manager = "codex-config".into();
    info.status = State::Declared;
    info.native_record = Some(config);
    assert!(matched(&item)?);
    Ok(())
}

#[test]
fn recommendation_reports_market_declaration_as_evidence_and_keeps_overflow_reachable() {
    let plugin = |name: &str, state, groups: Vec<String>| Item {
        id: format!("capability:{name}"),
        name: name.into(),
        summary: String::new(),
        managed: false,
        management: Management::Native,
        directory: false,
        path: None,
        source: None,
        agents: vec![],
        loaded: None,
        details: vec![],
        list: crate::browser::ListDetail::Plain,
        inventory: Some(crate::browser::InventoryInfo {
            groups,
            manager: "claude-plugin".into(),
            status: state,
            version: None,
            aliases: vec![],
            native_record: None,
            repository: None,
        }),
    };
    let related = Related {
        views: vec![(
            Kind::Capabilities,
            View {
                scope: "global".into(),
                items: vec![
                    plugin("declared", State::Declared, vec!["claude:mi".into()]),
                    plugin("unknown", State::Unknown, vec![]),
                    plugin("missing", State::Missing, vec![]),
                    plugin("extra", State::Present, vec![]),
                ],
                coverage: "partial",
                difference_coverage: "not_applicable",
                observed_at: None,
                note: String::new(),
                incomplete: true,
            },
        )],
        incomplete: true,
        repository_query: true,
    };
    let status = related.status();
    for label in [
        "匹配依据",
        "市场登记",
        "已声明",
        "待核验",
        "载荷缺失",
        "另有 1 项",
    ] {
        assert!(status.contains(label), "{label}\n{status}");
    }
    assert!(!status.contains("extra"));
    assert!(related.maintenance(1).contains("查看技能与插件"));
}
