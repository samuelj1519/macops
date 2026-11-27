#![cfg(test)]
use super::*;

#[test]
fn table_aligns_chinese_and_keeps_the_whole_wrapped_path() {
    let path = "/fixture/config/long/profile/settings.json";
    let rows = vec![
        vec!["本机".into(), path.into()],
        vec!["CLI".into(), "—".into()],
    ];
    let text = table(&["名称", "配置目录"], &rows, 24);
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(
        width(&lines[0][..lines[0].find("配置目录").unwrap_or_default()]),
        6
    );
    assert!(lines.iter().all(|line| width(line) <= 24));
    let wrapped: String = lines[2..lines.len() - 1]
        .iter()
        .map(|line| line.trim_start_matches("本机").trim())
        .collect();
    assert_eq!(wrapped, path);
    assert!(table(&["名称", "配置目录"], &rows, 5).contains(&format!("配置目录　{path}")));
}

#[test]
fn panels_keep_chinese_and_wrapped_links_within_the_border() {
    let content = "已有技能 · 9 项\nhttps://example.org/long/installation/instructions";
    let panel = panel_text("本机状态", content, 30, false);
    assert!(panel.contains("已有技能 · 9 项"));
    assert!(panel.contains("instructions"));
    assert!(panel.lines().all(|line| width(line) == 30));
    assert!(!panel.contains('\x1b'));
    assert!(panel_text("本机状态", content, 30, true).contains("\x1b[1;36m本机状态"));
    let combined = "#\u{fe0f}";
    let panel = panel_text("标题", &format!("{}{combined}", "x".repeat(19)), 24, false);
    assert!(panel.contains(combined));
    assert!(panel.lines().all(|line| width(line) == 24));
    let oversized = vec!["❤"; 30].join("\u{200d}");
    let panel = panel_text("标题", &format!("{oversized}后续"), 24, false);
    assert!(panel.contains('…') && panel.contains("后续"));
    assert!(panel.lines().all(|line| width(line) == 24));
}

#[test]
fn sections_style_commands_and_keep_source_links_available() {
    let url = "https://github.com/samuelj1519/devnorm-kit/blob/1cda102d7ec8984c95cc51940467d0916d36d0a6/README.md";
    let command = "npx skills add samuelj1519/devnorm-kit@v1.12.0";
    let lines = [
        PanelLine::Section("手动安装"),
        PanelLine::Item("Skills CLI".into()),
        PanelLine::Command(command.into()),
        PanelLine::Note("选择安装范围。".into()),
        PanelLine::Section("来源文档"),
        PanelLine::Document {
            name: "README.md".into(),
            url: url.into(),
        },
    ];
    let colored = section_text("安装方案", &lines, Some(60), true, false);
    assert!(colored.contains("◆ \x1b[1;36m手动安装"));
    assert!(colored.contains(&format!("▸ \x1b[1m{command}")));
    assert!(colored.contains(&format!("\x1b]8;;{url}\x1b\\")));
    assert!(colored.contains("--verbose 显示完整链接"));
    assert_eq!(colored.matches(url).count(), 1);
    let plain = section_text("安装方案", &lines, Some(60), false, false);
    assert!(plain.contains(url));
    assert!(!plain.contains('\x1b'));
    assert!(
        plain
            .lines()
            .filter(|line| !line.contains(url))
            .all(|line| width(line) == 60)
    );
    let narrow = section_text("安装方案", &lines, Some(24), false, true);
    assert!(!narrow.contains("│"));
    assert!(narrow.contains(command) && narrow.contains(url));
    assert!(section_text("安装方案", &lines, None, false, false).contains(url));
}
