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
    usize::from(size().0)
}

pub(super) fn size() -> (u16, u16) {
    crossterm::terminal::size()
        .ok()
        .filter(|&(width, height)| width > 0 && height > 0)
        .unwrap_or((80, 24))
}

pub(crate) fn table_width() -> usize {
    columns().saturating_sub(5).min(92)
}

pub(crate) fn column_widths(
    headers: &[&str],
    rows: &[Vec<String>],
    budget: usize,
) -> Option<Vec<usize>> {
    let minimum: Vec<_> = headers.iter().map(|label| width(label).max(2)).collect();
    let available = budget.checked_sub(headers.len().saturating_sub(1) * 2)?;
    if minimum.iter().sum::<usize>() > available {
        return None;
    }
    let mut widths = minimum.clone();
    for row in rows {
        for (current, value) in widths.iter_mut().zip(row) {
            *current = (*current).max(width(value).min(available));
        }
    }
    let mut total = widths.iter().sum::<usize>();
    while total > available {
        let index = widths
            .iter()
            .zip(&minimum)
            .enumerate()
            .filter(|(_, (current, floor))| current > floor)
            .max_by_key(|(_, (current, _))| *current)
            .map(|(index, _)| index)?;
        widths[index] -= 1;
        total -= 1;
    }
    Some(widths)
}

pub(crate) fn visible_columns<'a>(
    headers: &'a [&str],
    rows: &[Vec<String>],
    budget: usize,
) -> (&'a [&'a str], Vec<usize>) {
    (1..=headers.len())
        .rev()
        .find_map(|count| {
            let headers = &headers[..count];
            column_widths(headers, rows, budget).map(|widths| (headers, widths))
        })
        .unwrap_or((&headers[..1], vec![budget]))
}

fn pad_row(out: &mut String, cells: impl Iterator<Item = impl AsRef<str>>, widths: &[usize]) {
    for (index, (value, columns)) in cells.zip(widths).enumerate() {
        let value = value.as_ref();
        out.push_str(value);
        if index + 1 < widths.len() {
            out.push_str(&" ".repeat(columns.saturating_sub(width(value)) + 2));
        }
    }
    out.push('\n');
}

pub(super) fn picker_labels(
    headers: &[&str],
    rows: &[Vec<String>],
    budget: usize,
) -> (String, Vec<String>) {
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|value| crate::output::sanitize(value, false))
                .collect()
        })
        .collect();
    let (headers, widths) = visible_columns(headers, &rows, budget);
    let mut header = String::new();
    pad_row(&mut header, headers.iter().copied(), &widths);
    header.pop();
    let labels = rows
        .iter()
        .map(|row| {
            let mut label = String::new();
            let cells = row
                .iter()
                .zip(&widths)
                .map(|(cell, columns)| fit(cell, *columns));
            pad_row(&mut label, cells, &widths);
            label.pop();
            label
        })
        .collect();
    (header, labels)
}

pub(crate) fn table(headers: &[&str], rows: &[Vec<String>], budget: usize) -> String {
    if rows.is_empty() {
        return "本次没有记录。\n".into();
    }
    let rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|value| crate::output::sanitize(value, false))
                .collect()
        })
        .collect();
    let mut out = String::new();
    let Some(widths) = column_widths(headers, &rows, budget) else {
        for (index, row) in rows.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            for (header, value) in headers.iter().zip(row) {
                let _ = writeln!(out, "{header}　{value}");
            }
        }
        return out;
    };
    pad_row(&mut out, headers.iter().copied(), &widths);
    pad_row(
        &mut out,
        widths.iter().map(|columns| "─".repeat(*columns)),
        &widths,
    );
    for row in &rows {
        let mut remaining: Vec<_> = row.iter().take(widths.len()).map(String::as_str).collect();
        loop {
            let cells = remaining.iter_mut().zip(&widths).map(|(rest, columns)| {
                let (cell, tail) = rest.split_at(prefix_len(rest, *columns));
                *rest = tail;
                cell
            });
            pad_row(&mut out, cells, &widths);
            if remaining.iter().all(|part| part.is_empty()) {
                break;
            }
        }
    }
    out
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
#[path = "../../tests/unit/presentation.rs"]
mod tests;
