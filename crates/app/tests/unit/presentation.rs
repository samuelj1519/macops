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
}
