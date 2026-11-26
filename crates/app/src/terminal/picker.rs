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
    let result = loop {
        if redraw {
            if showing {
                clear(&mut output, size)?;
            }
            draw(
                &mut output,
                labels,
                &marked,
                active,
                (size, width),
                multiple,
            )?;
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

fn draw(
    output: &mut impl std::io::Write,
    labels: &[String],
    marked: &[usize],
    active: usize,
    (size, width): (usize, u16),
    multiple: bool,
) -> std::io::Result<()> {
    let start = active.saturating_sub(size / 2).min(labels.len() - size);
    let limit = usize::from(width.saturating_sub(7) / 2).max(6);
    for (index, label) in labels.iter().enumerate().skip(start).take(size) {
        let cursor = if active == index { '›' } else { ' ' };
        let check = if multiple {
            if marked.contains(&index) {
                "● "
            } else {
                "○ "
            }
        } else {
            ""
        };
        let label = crate::output::sanitize(label, false);
        let text: String = label.chars().take(limit).collect();
        let end = if text.len() < label.len() { "…" } else { "" };
        write!(output, "  {cursor} {check}{text}{end}\r\n")?;
    }
    let hint = if multiple {
        "↑/↓ 移动 · 空格多选 · Enter 确定 · Esc 返回"
    } else {
        "↑/↓ 选择 · Enter 确定 · Esc 返回"
    };
    write!(output, "  {hint}\r\n")?;
    output.flush()
}
