use super::*;

#[test]
fn inventory_filters_managers_and_actual_skill_locations() {
    use crate::domain::resource::State;
    let item = |name: &str, manager: &str, agents: &[&str]| Item {
        id: name.into(),
        name: name.into(),
        summary: String::new(),
        managed: true,
        management: Management::Native,
        directory: false,
        path: None,
        source: None,
        agents: agents.iter().map(|x| (*x).into()).collect(),
        loaded: None,
        details: vec![],
        inventory: Some(crate::browser::InventoryInfo {
            manager: manager.into(),
            groups: agents.iter().map(|x| (*x).into()).collect(),
            status: State::Present,
            version: None,
            aliases: vec![],
            repository: None,
            native_record: None,
        }),
    };
    let view = |items| View {
        scope: "global".into(),
        items,
        coverage: "observed",
        difference_coverage: "not_applicable",
        observed_at: None,
        note: String::new(),
        incomplete: false,
    };
    let mut app = Browser::new(
        view(vec![
            item("brew-jq", "brew", &[]),
            item("npm-jq", "npm", &[]),
        ]),
        Kind::Apps,
    );
    assert_eq!(app.visible().len(), 2);
    app.switch_group(true);
    assert!(
        app.group_tabs(32)
            .is_some_and(|tabs| tabs.contains("[Homebrew]"))
    );
    assert_eq!(app.selected().as_deref(), Some("brew-jq"));
    app.switch_group(true);
    assert_eq!(app.selected().as_deref(), Some("npm-jq"));
    app.switch_group(true);
    assert_eq!(app.selected().as_deref(), Some("npm-jq"));
    assert_eq!(app.visible().len(), 2);

    let mut skills = Browser::new(
        view(vec![
            item("claude-glm-skill", "skills-cli", &["claude:glm"]),
            item("claude-mi-skill", "skills-cli", &["claude:mi"]),
            item("shared", "skills-cli", &["shared"]),
            item("cursor-only", "skills-cli", &["cursor:default"]),
            item("unverified", "skills-cli", &[]),
        ]),
        Kind::Skills,
    );
    assert_eq!(skills.visible().len(), 5);
    skills.switch_group(true);
    assert_eq!(skills.group.as_deref(), Some("claude:glm"));
    assert_eq!(skills.selected().as_deref(), Some("claude-glm-skill"));
    skills.switch_group(true);
    assert_eq!(skills.group.as_deref(), Some("claude:mi"));
    assert_eq!(skills.selected().as_deref(), Some("claude-mi-skill"));
    skills.switch_group(true);
    assert_eq!(skills.group.as_deref(), Some("cursor:default"));
    assert_eq!(skills.selected().as_deref(), Some("cursor-only"));
    assert_eq!(skills.visible().len(), 1);
    skills.switch_group(true);
    assert_eq!(skills.selected().as_deref(), Some("shared"));
    assert_eq!(skills.visible().len(), 1);
    skills.switch_group(true);
    assert_eq!(skills.selected().as_deref(), Some("unverified"));
}

#[test]
fn install_progress_describes_the_stage_and_stays_quiet_for_machine_output() {
    for (stage, expected) in [
        (Stage::VerifySource, "核对固定提交与插件文件"),
        (Stage::RegisterMarketplace, "在隔离目录登记插件市场"),
        (Stage::InstallPlugin, "在隔离目录安装插件"),
        (Stage::CheckActivation, "核对插件启用设置"),
        (Stage::Publish, "校验后写入选定配置"),
    ] {
        assert_eq!(install_stage_text(stage, true), Some(expected));
        assert_eq!(install_stage_text(stage, false), None);
    }
}

#[test]
fn recovery_report_displays_the_explicit_continuation_command() {
    assert_eq!(
        recovery_notice(&serde_json::json!({"reason":"operation_busy"})),
        "另一个操作正在使用记录。等它结束后再检查。",
    );
    assert_eq!(
        recovery_notice(
            &serde_json::json!({"journal":{"phase":"unknown","message":"原载荷无法核验"},"recoveryBlocker":"ai_payload_changed"}),
        ),
        "仍待核对 · 原载荷无法核验",
    );
    let report = serde_json::json!({
        "journal": {"phase": "succeeded"},
        "continuationRequired": true,
        "continuationCommand": "mo agent resume OP-fixture",
    });
    assert_eq!(
        recovery_notice(&report),
        "原阶段已核对。继续请运行 mo agent resume OP-fixture"
    );
    assert_eq!(
        recovery_notice(&serde_json::json!({"status": "unknown"})),
        "核对结果 仍待核对"
    );
    let unverified = serde_json::json!({
        "journal": {"phase": "succeeded"},
        "continuationRequired": false,
    });
    assert_eq!(recovery_notice(&unverified), "核对结果 后续步骤待核对");
}

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
                inventory: None,
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
        agent_removable: false,
        skills_native: false,
    }));
    assert!(browser.agent_action('e').is_none());
    let session = crate::agent::codex_install::session::Details {
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
        agent_removable: true,
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
        "agent",
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
