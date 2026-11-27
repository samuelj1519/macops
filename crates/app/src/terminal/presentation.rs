use ratatui::{
    style::Style,
    text::{Line, Span},
};
use std::{fmt::Write as _, io::IsTerminal as _};

#[derive(Clone, Copy)]
pub(super) enum Tone {
    Active,
    Emphasis,
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
        Tone::Emphasis => "1",
        Tone::Success => "1;32",
        Tone::Warning => "1;33",
        Tone::Muted => "2",
    };
    format!("\x1b[{style}m{text}\x1b[0m")
}

pub(super) fn width(text: &str) -> usize {
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
    let mut out = Panel::new(title, columns, color);
    for line in content.lines() {
        out.line("", line, None, None);
    }
    out.finish()
}

pub(crate) enum PanelLine {
    Section(&'static str),
    Item(String),
    Command(String),
    Note(String),
    Text(String),
    Document { name: String, url: String },
}

pub(crate) fn section_panel(title: &str, lines: &[PanelLine], show_urls: bool) {
    let columns = columns().saturating_sub(1).min(96);
    let framed = interactive() && columns >= 24;
    eprint!(
        "{}",
        section_text(title, lines, framed.then_some(columns), color(), show_urls)
    );
}

pub(crate) fn section_text(
    title: &str,
    lines: &[PanelLine],
    columns: Option<usize>,
    color: bool,
    show_urls: bool,
) -> String {
    use crate::output::sanitize;

    let columns = columns.filter(|columns| {
        lines.iter().all(|line| match line {
            PanelLine::Command(text) => width(&sanitize(text, false)) + 8 <= *columns,
            _ => true,
        })
    });
    let mut panel = Panel::new(&sanitize(title, false), columns.unwrap_or(0), color);
    let mut has_document = false;
    let mut urls = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        match line {
            PanelLine::Section(label) => {
                if index != 0 {
                    panel.line("", "", None, None);
                }
                panel.line("◆ ", label, Some(Tone::Active), None);
            }
            PanelLine::Item(text) => {
                panel.line("  ", &sanitize(text, false), Some(Tone::Emphasis), None);
            }
            PanelLine::Command(text) => {
                panel.line("  ▸ ", &sanitize(text, false), Some(Tone::Emphasis), None);
            }
            PanelLine::Note(text) => {
                panel.line("    ", &sanitize(text, false), Some(Tone::Muted), None);
            }
            PanelLine::Text(text) => panel.line("  ", &sanitize(text, false), None, None),
            PanelLine::Document { name, url } => {
                let link =
                    url.starts_with("https://") && url.bytes().all(|byte| byte.is_ascii_graphic());
                panel.line(
                    "  ↗ ",
                    &sanitize(name, false),
                    Some(Tone::Active),
                    link.then_some(url),
                );
                if link && (show_urls || !color) {
                    urls.push((sanitize(name, false), url));
                }
                has_document |= link;
            }
        }
    }
    if has_document && color && !show_urls {
        panel.line("    ", "--verbose 显示完整链接", Some(Tone::Muted), None);
    }
    let mut out = panel.finish();
    for (name, url) in urls {
        let _ = writeln!(out, "  {name}  {url}");
    }
    out
}

struct Panel {
    out: String,
    inner: Option<usize>,
    color: bool,
}

impl Panel {
    fn new(title: &str, columns: usize, color: bool) -> Self {
        let inner = columns.checked_sub(4).filter(|width| *width >= 20);
        let mut out = String::new();
        if let Some(inner) = inner {
            let title = fit(title, inner - 2);
            let top = format!(" {}╮", "─".repeat(inner.saturating_sub(width(&title) + 1)));
            let _ = writeln!(
                out,
                "{}{}{}",
                paint("╭─ ", Tone::Muted, color),
                paint(&title, Tone::Active, color),
                paint(&top, Tone::Muted, color)
            );
        } else {
            let _ = writeln!(out, "{}", paint(title, Tone::Active, color));
        }
        Self { out, inner, color }
    }

    fn line(&mut self, prefix: &str, text: &str, tone: Option<Tone>, link: Option<&str>) {
        let lead = width(prefix);
        let columns = self.inner.map_or(usize::MAX, |inner| inner - lead);
        let hanging = " ".repeat(lead);
        let mut prefix = prefix;
        let mut rest = text;
        loop {
            let (part, tail) = panel_line_chunk(rest, columns);
            let styled = tone.map_or_else(|| part.into(), |tone| paint(part, tone, self.color));
            let styled = match link.filter(|_| self.color) {
                Some(url) => format!("\x1b]8;;{url}\x1b\\{styled}\x1b]8;;\x1b\\"),
                None => styled,
            };
            if self.inner.is_some() {
                let padding = " ".repeat(columns.saturating_sub(width(part)));
                let _ = writeln!(
                    self.out,
                    "{}{prefix}{styled}{padding}{}",
                    paint("│ ", Tone::Muted, self.color),
                    paint(" │", Tone::Muted, self.color)
                );
            } else {
                let _ = writeln!(self.out, "{prefix}{styled}");
            }
            if tail.is_empty() {
                break;
            }
            prefix = &hanging;
            rest = tail;
        }
    }

    fn finish(mut self) -> String {
        if let Some(inner) = self.inner {
            let border = format!("╰{}╯", "─".repeat(inner + 2));
            let _ = writeln!(self.out, "{}", paint(&border, Tone::Muted, self.color));
        }
        self.out
    }
}

fn panel_line_chunk(text: &str, columns: usize) -> (&str, &str) {
    if width(text) <= columns {
        return (text, "");
    }
    let span = Span::raw(text);
    let mut graphemes = span.styled_graphemes(Style::default()).peekable();
    let first_len = graphemes
        .peek()
        .map_or(text.len(), |item| item.symbol.len());
    let mut remaining = columns;
    let split = graphemes
        .map_while(|grapheme| {
            remaining = remaining.checked_sub(width(grapheme.symbol))?;
            Some(grapheme.symbol.len())
        })
        .sum();
    if split == 0 {
        ("…", &text[first_len..])
    } else {
        text.split_at(split)
    }
}
#[cfg(test)]
#[path = "../../tests/unit/presentation.rs"]
mod tests;
