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
        (Action::Adopt, "纳管"),
        (Action::Cleanup, "清理"),
        (Action::Start, "启动"),
        (Action::Stop, "停止"),
        (Action::Apply, "应用配置"),
        (Action::Restore, "还原"),
        (Action::Reconcile, "核对"),
    ] {
        journal.plan.action = action;
        let row = history_item(journal.clone(), Some(false));
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
        let row = history_item(journal.clone(), Some(false));
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
    let unpublished = history_item(journal.clone(), Some(true));
    assert!(unpublished.summary.contains("原生安装完成，待登记"));
    assert_eq!(unpublished.details.len(), 7);
    let unverifiable = history_item(journal, None);
    assert!(unverifiable.summary.contains("登记状态待查"));
    assert_eq!(unverifiable.details.len(), 4);
    Ok(())
}
