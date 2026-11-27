use super::presentation;
use crate::browser::{self, Item, Kind};
use crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Margin, Rect},
    style::Modifier,
    widgets::{
        Block, Borders, HighlightSpacing, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, TableState,
    },
};
use std::collections::BTreeMap;

const SPACING: usize = 2;
const GUTTER: u16 = 2;

#[derive(Default)]
pub(super) struct Settings {
    start: usize,
    pinned: Option<usize>,
    widths: BTreeMap<usize, usize>,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) settings: Settings,
    resize: Option<Resize>,
    pub(super) layout: Layout,
}

struct Resize {
    column: usize,
    drag: Option<Drag>,
}

struct Drag {
    origin: u16,
    width: usize,
    before: Option<usize>,
}

#[derive(Default)]
pub(super) struct Layout {
    columns: Vec<Column>,
    end: usize,
    can_scroll_right: bool,
    area: Rect,
}

struct Column {
    id: usize,
    width: usize,
    minimum: usize,
    maximum: usize,
    divider: u16,
}

impl Layout {
    fn build(kind: Kind, rows: &[Vec<String>], area: Rect, settings: &Settings) -> Self {
        let headers = browser::list_headers(kind);
        let budget = usize::from(area.width.saturating_sub(GUTTER + 2));
        let end = headers.len().saturating_sub(2);
        let start = settings.start.min(end);
        let indices = std::iter::once(0)
            .chain((start + 1)..headers.len())
            .collect::<Vec<_>>();
        let minimum = |id: usize| {
            if id == 2 && matches!(kind, Kind::Apps | Kind::Agents) {
                8
            } else {
                presentation::width(headers[id]).max(2)
            }
        };
        let mut floors = Vec::with_capacity(indices.len());
        let mut desired = Vec::with_capacity(indices.len());
        for (position, &id) in indices.iter().enumerate() {
            let floor = minimum(id);
            let reserved = if position == 0 {
                indices.get(1).map_or(0, |&next| minimum(next) + SPACING)
            } else {
                floors[0] + SPACING
            };
            let limit = budget.saturating_sub(reserved);
            let custom = settings
                .widths
                .get(&id)
                .copied()
                .or(if id == 0 { settings.pinned } else { None })
                .map(|width| width.clamp(floor, limit.max(floor)));
            floors.push(custom.unwrap_or(floor));
            desired.push(custom.unwrap_or_else(|| {
                rows.iter()
                    .map(|row| presentation::width(&row[id]))
                    .max()
                    .unwrap_or(floor)
                    .max(floor)
            }));
        }
        let widths = (1..=indices.len())
            .rev()
            .find_map(|count| presentation::fit_widths(&floors[..count], &desired[..count], budget))
            .unwrap_or_else(|| vec![budget]);
        let can_scroll_right = start < end
            && (widths.len() < indices.len()
                || widths
                    .iter()
                    .zip(&desired)
                    .skip(1)
                    .any(|(width, ideal)| width < ideal));
        let mut x = area.x.saturating_add(1 + GUTTER);
        let columns = indices
            .into_iter()
            .zip(widths)
            .map(|(id, width)| {
                let maximum =
                    usize::from(area.right().saturating_sub(1).saturating_sub(x)).max(width);
                let divider = x.saturating_add(u16::try_from(width).unwrap_or(area.width));
                x = divider.saturating_add(2);
                Column {
                    id,
                    width,
                    minimum: minimum(id).min(maximum),
                    maximum,
                    divider,
                }
            })
            .collect();
        Self {
            columns,
            end,
            can_scroll_right,
            area,
        }
    }
}

impl State {
    pub(super) fn resizing(&self) -> bool {
        self.resize.is_some()
    }

    pub(super) fn enter_resize(&mut self) {
        self.resize = Some(Resize {
            column: 0,
            drag: None,
        });
    }

    pub(super) fn leave_resize(&mut self) {
        self.cancel_drag();
        self.resize = None;
    }

    pub(super) fn slide(&mut self, right: bool) {
        let start = self.settings.start.min(self.layout.end);
        let next = if right && self.layout.can_scroll_right {
            (start + 1).min(self.layout.end)
        } else if right {
            start
        } else {
            start.saturating_sub(1)
        };
        self.show_from(next);
    }

    fn show_from(&mut self, start: usize) {
        if start == 0 {
            self.settings.pinned = None;
        } else if self.settings.start == 0 {
            self.settings.pinned = self.layout.columns.first().map(|column| column.width);
        }
        self.settings.start = start;
    }

    pub(super) fn resize_key(&mut self, key: KeyCode) {
        self.cancel_drag();
        let Some(resize) = &mut self.resize else {
            return;
        };
        match key {
            KeyCode::Left | KeyCode::Right => {
                resize.column = if key == KeyCode::Left {
                    resize.column.saturating_sub(1)
                } else if resize.column == 0 {
                    self.settings.start + 1
                } else {
                    (resize.column + 1).min(self.layout.end + 1)
                };
                if resize.column > 0
                    && !self
                        .layout
                        .columns
                        .iter()
                        .any(|column| column.id == resize.column)
                {
                    let start = resize.column - 1;
                    self.show_from(start);
                }
            }
            KeyCode::Up | KeyCode::Down => {
                if let Some(column) = self
                    .layout
                    .columns
                    .iter()
                    .find(|column| column.id == resize.column)
                {
                    let size = if key == KeyCode::Up {
                        column.width + 1
                    } else {
                        column.width.saturating_sub(1)
                    };
                    self.settings
                        .widths
                        .insert(column.id, size.clamp(column.minimum, column.maximum));
                }
            }
            KeyCode::Char('0') => {
                self.settings = Settings::default();
                resize.column = 0;
            }
            _ => {}
        }
    }

    pub(super) fn mouse(&mut self, event: MouseEvent) {
        if event.kind == MouseEventKind::Down(MouseButton::Left) {
            self.cancel_drag();
        }
        let Some(resize) = &mut self.resize else {
            return;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left)
                if self.layout.area.width > 4
                    && self.layout.area.height > 2
                    && event.row == self.layout.area.y.saturating_add(1) =>
            {
                if let Some(column) = self.layout.columns.iter().find(|column| {
                    column.divider == event.column && event.column < self.layout.area.right()
                }) {
                    resize.column = column.id;
                    resize.drag = Some(Drag {
                        origin: event.column,
                        width: column.width,
                        before: self.settings.widths.get(&column.id).copied(),
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(drag) = &resize.drag
                    && let Some(column) = self
                        .layout
                        .columns
                        .iter()
                        .find(|column| column.id == resize.column)
                {
                    let width = if event.column >= drag.origin {
                        drag.width + usize::from(event.column - drag.origin)
                    } else {
                        drag.width
                            .saturating_sub(usize::from(drag.origin - event.column))
                    }
                    .clamp(column.minimum, column.maximum);
                    self.settings.widths.insert(column.id, width);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => resize.drag = None,
            _ => {}
        }
    }

    pub(super) fn cancel_drag(&mut self) {
        if let Some(resize) = &mut self.resize
            && let Some(drag) = resize.drag.take()
        {
            if let Some(width) = drag.before {
                self.settings.widths.insert(resize.column, width);
            } else {
                self.settings.widths.remove(&resize.column);
            }
        }
    }
}

pub(super) fn draw(
    frame: &mut Frame,
    area: Rect,
    kind: Kind,
    items: &[&Item],
    selected: usize,
    control: Option<&State>,
) -> Layout {
    let headers = browser::list_headers(kind);
    let rows = items
        .iter()
        .map(|item| {
            browser::list_row(kind, item)
                .into_iter()
                .map(|value| browser::sanitize(&value, false))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let defaults = Settings::default();
    let settings = control.map_or(&defaults, |state| &state.settings);
    let layout = Layout::build(kind, &rows, area, settings);
    let resize = control.and_then(|state| state.resize.as_ref());
    let widths = layout
        .columns
        .iter()
        .map(|column| Constraint::Length(u16::try_from(column.width).unwrap_or(area.width)));
    let values = rows
        .iter()
        .map(|row| Row::new(layout.columns.iter().map(|column| row[column.id].as_str())));
    let header = Row::new(layout.columns.iter().map(|column| {
        let cell = ratatui::widgets::Cell::from(headers[column.id]);
        if resize.is_some_and(|resize| resize.column == column.id) {
            cell.style(Modifier::BOLD | Modifier::REVERSED)
        } else {
            cell.style(Modifier::BOLD)
        }
    }));
    let list = Table::new(values, widths)
        .header(header)
        .column_spacing(u16::try_from(SPACING).unwrap_or(2))
        .block(Block::default().borders(Borders::ALL))
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always);
    let mut selected = TableState::default().with_selected((!rows.is_empty()).then_some(selected));
    frame.render_stateful_widget(list, area, &mut selected);

    if control.is_some() && area.width > 4 && area.height > 2 {
        if settings.start > 0 || layout.can_scroll_right {
            let mut scroll = ScrollbarState::new(layout.end + 1)
                .position(settings.start)
                .viewport_content_length(1);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::HorizontalBottom),
                area.inner(Margin::new(1, 0)),
                &mut scroll,
            );
        }
        if resize.is_some() {
            for column in &layout.columns {
                if column.divider < area.right() {
                    frame.buffer_mut()[(column.divider, area.y + 1)].set_symbol("│");
                }
            }
        }
    }
    layout
}

#[cfg(test)]
#[path = "../../tests/unit/inventory_table.rs"]
mod tests;
