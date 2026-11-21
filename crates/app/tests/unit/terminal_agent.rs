use super::*;

#[test]
fn agent_details_announce_the_managed_session_without_claiming_it_is_loaded() {
    let mut browser = Browser::new(
        View {
            scope: "global".into(),
            items: vec![Item {
                id: "agent:fixture".into(),
                name: "codex:mo-fixture".into(),
                summary: "Codex".into(),
                managed: true,
                management: Management::Adopted,
                directory: false,
                path: None,
                source: None,
                agents: vec![],
                loaded: None,
                details: vec!["档案 fixture".into()],
            }],
            coverage: "observed",
            difference_coverage: "not_applicable",
            observed_at: None,
            note: String::new(),
            incomplete: false,
        },
        Kind::Agents,
    );
    assert!(browser.agent_action('l').is_none());
    browser.page = Page::Detail(Box::new(Details {
        actual: None,
        source: None,
        recovery: None,
        agent_session: None,
        skills_native: false,
    }));
    assert!(browser.agent_action('e').is_none());
    let session = crate::codex_install::session::Details {
        plugin: "item@market".into(),
        preference: "停用",
        observed_model: Some("fixture-model".into()),
    };
    let displayed = item_details(&browser.view.items[0], Some(&session));
    assert!(displayed.contains("插件 item@market"));
    assert!(displayed.contains("基础模型 fixture-model"));
    assert!(displayed.contains("档案 ID agent:fixture"));
    assert!(displayed.contains("下次由 mo 启动时 停用"));
    assert!(displayed.contains("会话载入 未知"));
    browser.page = Page::Detail(Box::new(Details {
        actual: None,
        source: None,
        recovery: None,
        agent_session: Some(session),
        skills_native: false,
    }));
    assert!(keys(Kind::Agents, &browser.page, false).contains("e 插件设置"));
    assert!(
        matches!(browser.agent_action('e'), Some(NativeAction::AgentPreference(profile)) if profile == "agent:fixture")
    );
    assert!(
        matches!(browser.agent_action('l'), Some(NativeAction::AgentLaunch(profile)) if profile == "agent:fixture")
    );
}

#[test]
fn agent_action_losing_its_owner_returns_to_inventory_without_saving()
-> crate::test_support::TestResult {
    use clap::Parser as _;
    let home = tempfile::tempdir_in("/private/tmp")?;
    std::fs::create_dir(home.path().join(".codex"))?;
    let parser = crate::cli::Cli::try_parse_from([
        "macops",
        "--home",
        home.path().to_str().ok_or("home")?,
        "agents",
    ])?;
    let ctx = Context::from_cli(&parser, &mut Vec::new())?;
    let (mut session, view) = Session::load(&ctx, Kind::Agents)?;
    let mut browser = Browser::new(view, Kind::Agents);
    browser.enter(&ctx, &session, Kind::Agents);
    assert!(browser.agent_action('e').is_none());
    assert!(browser.agent_action('l').is_none());
    for action in [
        NativeAction::AgentPreference("codex:mo-disappeared".into()),
        NativeAction::AgentLaunch("codex:mo-disappeared".into()),
    ] {
        browser.run_native(&ctx, &mut session, action);
        assert!(matches!(browser.page, Page::List));
        assert!(browser.notice.contains("codex_session_profile_unavailable"));
        assert!(!ctx.state.exists());
    }
    Ok(())
}
