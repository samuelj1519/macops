#![cfg(test)]
use super::*;
use crate::test_support::TestResult;

#[test]
fn multi_picker_distinguishes_focus_and_marked_rows_independently() -> TestResult {
    let labels = ["选中".into(), "聚焦".into(), "未选".into()];
    let mut bytes = vec![];
    let view = View {
        rows: 3,
        columns: 70,
        multiple: true,
        prompt: "选择技能",
        header: None,
        color: true,
    };
    draw(&mut bytes, &labels, &[0], 1, &view)?;
    let output = String::from_utf8(bytes)?;
    assert!(output.contains("\x1b[1;32m●\x1b[0m 选中"));
    assert!(output.contains("\x1b[1;36m›\x1b[0m \x1b[1;36m○"));
    assert!(output.contains("\x1b[2m○\x1b[0m 未选"));
    assert!(output.contains("确定 1 项"));
    Ok(())
}
