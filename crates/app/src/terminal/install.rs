use crate::{output::TerminalLine, terminal::feedback as terminal};
use serde_json::Value;
use std::path::Path;

pub(crate) fn report_install(result: Value) -> Value {
    show_install_result(&result);
    result
}

pub(crate) fn show_install_result(result: &Value) {
    use crate::browser::sanitize;
    if crate::policy::cli::continuation(result).is_some() {
        eprintln!(
            "{}",
            crate::operations::recovery_text(result, false).trim_end()
        );
        return;
    }
    let journal = result.get("journal").unwrap_or(result);
    let phase = journal["phase"].as_str().unwrap_or_default();
    terminal(
        if phase == "succeeded" {
            TerminalLine::Done
        } else {
            TerminalLine::Note
        },
        &format!("结果 {}", crate::output::phase_text(phase)),
    );
    if let Some(message) = journal["message"].as_str() {
        eprintln!("{}", sanitize(message, false));
    }
    if let Some(hint) = crate::output::recovery_hint(journal) {
        eprintln!("{hint}");
    }
    if let (Some(name), Some(path)) = (result["targetName"].as_str(), result["profile"].as_str()) {
        eprintln!(
            "安装位置 {} · {}",
            sanitize(
                &crate::agent::resource_agents::profile_label(name, Path::new(path)),
                false
            ),
            sanitize(path, false)
        );
    }
    if let Some(path) = result["codexHome"].as_str() {
        eprintln!("Codex 档案 {}", sanitize(path, false));
        eprintln!("运行 mo agent 查看已纳入管理的单独配置。");
    } else if let Some(path) = result["claudeConfigDir"].as_str() {
        eprintln!("Claude Code 档案 {}", sanitize(path, false));
        eprintln!("运行 mo agent 查看已纳入管理的单独配置。");
    }
}
