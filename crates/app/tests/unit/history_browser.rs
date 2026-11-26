use super::*;
use crate::{
    domain::{Action, Phase},
    test_support::{TestResult, journal},
};

#[test]
fn terminal_history_shows_typed_operations_and_limits_recovery_to_pending() -> TestResult {
    let mut journal = journal()?;
    for (action, label) in [
        (Action::Maintain, "维护"),
        (Action::Install, "安装"),
        (Action::Upgrade, "升级"),
        (Action::Uninstall, "卸载"),
        (Action::Update, "刷新索引"),
        (Action::Adopt, "纳入管理"),
        (Action::Cleanup, "清理"),
        (Action::Start, "启动"),
        (Action::Stop, "停止"),
        (Action::Apply, "应用配置"),
        (Action::Restore, "还原"),
        (Action::Reconcile, "核对"),
    ] {
        journal.plan.action = action;
        let row = history_item(journal.clone(), RecoveryState::Settled);
        assert!(row.summary.starts_with(label), "{}", row.summary);
        assert_eq!(row.id, journal.id);
    }
    journal.message = "SYNTHETIC_PRIVATE_COMMAND_OUTPUT".into();
    for (phase, label, count) in [
        (Phase::Prepared, "已准备", 7),
        (Phase::Running, "运行中", 7),
        (Phase::Unknown, "待核对", 7),
        (Phase::Failed, "失败", 4),
        (Phase::Succeeded, "已完成", 4),
    ] {
        journal.phase = phase;
        let recovery = if crate::policy::operation::state::pending(phase) {
            RecoveryState::Pending
        } else {
            RecoveryState::Settled
        };
        let row = history_item(journal.clone(), recovery);
        assert!(row.summary.ends_with(label), "{}", row.summary);
        assert_eq!(row.details.len(), count);
        assert!(
            row.details
                .contains(&format!("原状态目录 {}", journal.state.display()))
        );
        assert!(
            !row.details
                .iter()
                .any(|line| line.contains("SYNTHETIC_PRIVATE_COMMAND_OUTPUT"))
        );
    }
    let unpublished = history_item(journal.clone(), RecoveryState::CodexPublication);
    assert!(unpublished.summary.contains("原生安装完成，待登记"));
    assert_eq!(unpublished.details.len(), 7);
    let unverifiable = history_item(journal, RecoveryState::Unverified);
    assert!(unverifiable.summary.contains("登记状态待查"));
    assert_eq!(unverifiable.details.len(), 4);
    Ok(())
}

#[test]
fn browser_rows_keep_application_and_profile_resource_ids() -> TestResult {
    use clap::Parser;
    use std::fs;

    let temp = tempfile::tempdir_in("/private/tmp")?;
    let home = temp.path().join("home");
    fs::create_dir(&home)?;
    for profile in ["mi", "glm"] {
        let path = home.join(format!(".claude-{profile}"));
        fs::create_dir(&path)?;
        fs::write(
            path.join("settings.json"),
            serde_json::to_vec(&serde_json::json!({"model":format!("fixture-{profile}")}))?,
        )?;
    }
    let catalog = temp.path().join("catalog.json");
    fs::write(
        &catalog,
        serde_json::to_vec(&crate::domain::Catalog {
            schema_version: 1,
            packages: vec![crate::test_support::package()],
        })?,
    )?;
    let cli = Cli::try_parse_from([
        "mo",
        "--home",
        home.to_str().ok_or("home")?,
        "--catalog",
        catalog.to_str().ok_or("catalog")?,
        "app",
    ])?;
    let ctx = Context::from_cli(&cli, &mut vec![])?;
    for (kind, category) in [(Kind::Apps, "applications"), (Kind::Agents, "agents")] {
        let snapshot = crate::catalog::resource_inventory::snapshot(&ctx, Some(category), None)?;
        let (_, view) = Session::load(&ctx, kind)?;
        assert!(!snapshot.resources.is_empty());
        for resource in &snapshot.resources {
            let item = view
                .items
                .iter()
                .find(|item| item.id == resource.id)
                .ok_or("ID missing in browser")?;
            assert_eq!(item.name, resource.name);
            if let crate::domain::resource::Data::Agent { model, profile, .. } = &resource.data {
                let expected = format!("fixture-{profile}");
                assert_eq!(model.selected.as_deref(), Some(expected.as_str()));
                assert!(item.details.contains(&format!("模型 {expected}")));
            }
        }
    }
    Ok(())
}

#[test]
fn agent_row_hides_the_generated_label_suffix_and_keeps_the_profile_identity() -> TestResult {
    use crate::domain::resource::{Data, Management, Model, Resource, RuntimeBinding, State};
    let path = std::path::PathBuf::from("/fixture/claude-profile.AbC123/home/.claude");
    let item = resource_item(
        Resource {
            id: "agent:stable".into(),
            name: "claude:mo-example-AbC123".into(),
            aliases: vec![],
            management: Management::Adopted,
            status: State::Present,
            data: Data::Agent {
                client: "claude".into(),
                profile: "mo-example-AbC123".into(),
                config_dir: path.clone(),
                model: Model::default(),
                runtime: RuntimeBinding {
                    selector: "fixture".into(),
                    context: path.clone(),
                    binary: None,
                    install_ref: None,
                    evidence: None,
                },
            },
        },
        false,
    )
    .ok_or("agent row")?;
    assert_eq!(item.name, "Claude Code · example");
    assert_eq!(item.summary, "claude · 单独配置");
    assert_eq!(item.id, "agent:stable");
    assert_eq!(item.path.as_ref(), Some(&path));
    Ok(())
}
