use super::presentation::{self, Tone, fit, paint};
use crate::error::{Error, Result};
use crossterm::{
    cursor::MoveUp,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, Clear, ClearType},
};
use std::io::{IsTerminal as _, Write as _};

struct RawInput(bool);

impl Drop for RawInput {
    fn drop(&mut self) {
        if self.0 {
            let _ = terminal::disable_raw_mode();
        }
    }
}

pub(crate) fn select(
    labels: &[String],
    initial: usize,
    multiple: bool,
    prompt: &str,
    shortcut: impl Fn(&str) -> Option<Vec<usize>>,
) -> Result<Vec<usize>> {
    if !std::io::stdin().is_terminal() {
        return Err(Error::attention(
            "confirmation_required",
            "请在终端中运行原命令，查看将要执行的操作。",
        ));
    }
    if labels.is_empty() {
        return Ok(vec![]);
    }
    let (width, height) = terminal::size()
        .ok()
        .filter(|&(w, h)| w > 0 && h > 0)
        .unwrap_or((80, 24));
    let size = usize::from(height.saturating_sub(5).clamp(1, 8)).min(labels.len());
    let mut active = initial.min(labels.len() - 1);
    let mut marked = vec![];
    let mut typed = String::new();
    let mut showing = false;
    let mut redraw = true;
    let _signals = super::SignalHold::enter()?;
    let restore = !terminal::is_raw_mode_enabled()?;
    if restore {
        terminal::enable_raw_mode()?;
    }
    let _restore = RawInput(restore);
    let mut output = std::io::stderr();
    let view = View {
        rows: size,
        columns: usize::from(width.saturating_sub(1)),
        multiple,
        prompt,
        color: presentation::color(),
    };
    let result = loop {
        if redraw {
            if showing {
                clear(&mut output, size)?;
            }
            draw(&mut output, labels, &marked, active, &view)?;
            showing = true;
            redraw = false;
        }
        let (code, modifiers) = next_key()?;
        match code {
            KeyCode::Esc => break vec![],
            KeyCode::Char('c' | 'd') if modifiers.contains(KeyModifiers::CONTROL) => {
                break vec![];
            }
            KeyCode::Enter if !typed.is_empty() => {
                if let Some(indices) = shortcut(&typed) {
                    break indices;
                }
                if matches!(typed.as_str(), "n" | "no") {
                    break vec![];
                }
                typed.clear();
                clear(&mut output, size)?;
                write!(output, "  请输入列表中的编号，或用方向键选择。\r\n")?;
                showing = false;
                redraw = true;
            }
            KeyCode::Enter if multiple && !marked.is_empty() => break marked,
            KeyCode::Enter => break vec![active],
            KeyCode::Up | KeyCode::Left => {
                typed.clear();
                active = (active + labels.len() - 1) % labels.len();
                redraw = true;
            }
            KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
                typed.clear();
                active = (active + 1) % labels.len();
                redraw = true;
            }
            KeyCode::Char(' ') if multiple && typed.is_empty() => {
                redraw = true;
                if let Some(index) = marked.iter().position(|&value| value == active) {
                    marked.remove(index);
                } else {
                    marked.push(active);
                }
            }
            KeyCode::Char(value) if typed.len() < 128 => {
                if value.is_control() {
                    continue;
                }
                typed.push(value);
            }
            KeyCode::Backspace => {
                typed.pop();
            }
            _ => {}
        }
    };
    clear(&mut output, size)?;
    Ok(result)
}

fn next_key() -> Result<(KeyCode, KeyModifiers)> {
    loop {
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            let code = if key.code == KeyCode::Char('j')
                && key.modifiers.contains(KeyModifiers::CONTROL)
            {
                KeyCode::Enter
            } else {
                key.code
            };
            return Ok((code, key.modifiers));
        }
    }
}

fn clear(output: &mut impl std::io::Write, size: usize) -> std::io::Result<()> {
    execute!(
        output,
        MoveUp(u16::try_from(size + 1).unwrap_or(9)),
        Clear(ClearType::FromCursorDown)
    )
}

struct View<'a> {
    rows: usize,
    columns: usize,
    multiple: bool,
    prompt: &'a str,
    color: bool,
}

fn draw(
    output: &mut impl std::io::Write,
    labels: &[String],
    marked: &[usize],
    active: usize,
    view: &View<'_>,
) -> std::io::Result<()> {
    let View {
        rows: size,
        columns: width,
        multiple,
        prompt,
        color,
    } = *view;
    let start = active.saturating_sub(size / 2).min(labels.len() - size);
    let rail = paint("│", Tone::Muted, color);
    for (index, label) in labels.iter().enumerate().skip(start).take(size) {
        let cursor = if active == index {
            paint("›", Tone::Active, color)
        } else {
            " ".into()
        };
        let (glyph, tone) = match (multiple, marked.contains(&index), active == index) {
            (true, true, _) => ("●", Tone::Success),
            (false, _, true) => ("●", Tone::Active),
            (_, _, true) => ("○", Tone::Active),
            _ => ("○", Tone::Muted),
        };
        let check = paint(glyph, tone, color);
        let label = fit(
            &crate::output::sanitize(label, false),
            width.saturating_sub(6),
        );
        let text = paint(&label, Tone::Active, color && active == index);
        write!(output, "{rail} {cursor} {check} {text}\r\n")?;
    }
    let hint = if multiple {
        if marked.is_empty() {
            "↑/↓ 移动 · 空格多选 · Enter 选当前 · Esc 返回".into()
        } else {
            format!(
                "↑/↓ 移动 · 空格多选 · Enter 确定 {} 项 · Esc 返回",
                marked.len()
            )
        }
    } else {
        "↑/↓ 选择 · Enter 确定 · Esc 返回".into()
    };
    let hint = if prompt.is_empty() {
        hint
    } else {
        format!("{prompt} · {hint}")
    };
    let hint = fit(&hint, width.saturating_sub(2));
    write!(
        output,
        "{} {}\r\n",
        paint("╰", Tone::Muted, color),
        paint(&hint, Tone::Muted, color)
    )?;
    output.flush()
}

#[cfg(test)]
mod tests {
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
}
