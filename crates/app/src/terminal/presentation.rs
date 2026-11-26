use ratatui::text::Line;
use std::{fmt::Write as _, io::IsTerminal as _};

#[derive(Clone, Copy)]
pub(super) enum Tone {
    Active,
    Success,
    Warning,
    Muted,
}

pub(super) fn interactive() -> bool {
    std::io::stderr().is_terminal()
        && std::env::var_os("TERM").is_some_and(|term| !term.is_empty() && term != "dumb")
}

pub(super) fn color() -> bool {
    interactive() && std::env::var_os("NO_COLOR").is_none()
}

pub(super) fn paint(text: &str, tone: Tone, color: bool) -> String {
    if !color {
        return text.into();
    }
    let style = match tone {
        Tone::Active => "1;36",
        Tone::Success => "1;32",
        Tone::Warning => "1;33",
        Tone::Muted => "2",
    };
    format!("\x1b[{style}m{text}\x1b[0m")
}

fn width(text: &str) -> usize {
    Line::from(text).width()
}

fn prefix_len(text: &str, columns: usize) -> usize {
    let mut remaining = columns;
    for (index, ch) in text.char_indices() {
        let size = width(ch.encode_utf8(&mut [0; 4]));
        if size > remaining {
            return index;
        }
        remaining -= size;
    }
    text.len()
}

pub(super) fn fit(text: &str, columns: usize) -> String {
    if width(text) <= columns {
        text.into()
    } else if columns == 0 {
        String::new()
    } else {
        format!("{}…", &text[..prefix_len(text, columns - 1)])
    }
}

pub(super) fn columns() -> usize {
    crossterm::terminal::size()
        .ok()
        .map(|(width, _)| usize::from(width))
        .filter(|width| *width > 0)
        .unwrap_or(80)
}

pub(crate) fn panel(title: &str, content: &str) {
    let content = crate::output::sanitize(content, true);
    let title = crate::output::sanitize(title, false);
    let columns = columns().saturating_sub(1).min(96);
    if !interactive() || columns < 24 {
        eprintln!("{title}\n{content}");
        return;
    }
    eprint!("{}", panel_text(&title, &content, columns, color()));
}

fn panel_text(title: &str, content: &str, columns: usize, color: bool) -> String {
    let border = |text: &str| paint(text, Tone::Muted, color);
    let inner = columns - 4;
    let title = fit(title, inner - 2);
    let mut out = String::new();
    let top = format!(" {}╮", "─".repeat(inner - width(&title) - 1));
    let _ = writeln!(
        out,
        "{}{}{}",
        border("╭─ "),
        paint(&title, Tone::Active, color),
        border(&top)
    );
    let mut row = |part: &str| {
        let padding = " ".repeat(inner.saturating_sub(width(part)));
        let _ = writeln!(out, "{}{part}{padding}{}", border("│ "), border(" │"));
    };
    for line in content.lines() {
        let mut rest = line;
        while width(rest) > inner {
            let (head, tail) = rest.split_at(prefix_len(rest, inner));
            row(head);
            rest = tail;
        }
        row(rest);
    }
    let _ = writeln!(out, "{}", border(&format!("╰{}╯", "─".repeat(columns - 2))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
