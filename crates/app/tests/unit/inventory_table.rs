use super::*;

fn rows() -> Vec<Vec<String>> {
    vec![
        [
            "fixture",
            "原生登记",
            "1.2.345",
            "mise",
            "/fixture/a/long/installation/path",
            "1",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
    ]
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

#[test]
fn horizontal_navigation_keeps_the_name_and_can_reach_the_last_column() {
    let mut state = State::default();
    let area = Rect::new(0, 2, 38, 10);
    let rows = rows();
    state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    assert_eq!(state.layout.columns[0].id, 0);
    assert!(state.layout.can_scroll_right);
    assert!(!state.layout.columns.iter().any(|column| column.id == 5));
    let pinned_width = state.layout.columns[0].width;
    while state.layout.can_scroll_right {
        state.slide(true);
        state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
        assert_eq!(state.layout.columns[0].id, 0);
        assert_eq!(state.layout.columns[0].width, pinned_width);
    }
    assert!(state.layout.columns.iter().any(|column| column.id == 5));
    for _ in 0..state.layout.end {
        state.slide(false);
    }
    assert_eq!(state.settings.start, 0);
}

#[test]
fn drag_can_grow_then_shrink_and_only_a_released_width_survives() {
    let mut state = State::default();
    let area = Rect::new(0, 2, 46, 10);
    let rows = rows();
    state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    state.enter_resize();
    let divider = state.layout.columns[2].divider;
    let row = area.y + 1;
    state.mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, row));
    state.mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        u16::MAX,
        row,
    ));
    assert_eq!(state.settings.widths.len(), 1);
    assert_eq!(
        state.settings.widths.get(&2),
        Some(&state.layout.columns[2].maximum)
    );
    state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    assert!(state.layout.columns.iter().any(|column| column.id == 2));
    state.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), divider, row));
    state.mouse(mouse(MouseEventKind::Up(MouseButton::Left), divider, row));
    let committed = state.settings.widths[&2];
    state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    let divider = state.layout.columns[2].divider;
    state.mouse(mouse(MouseEventKind::Down(MouseButton::Left), divider, row));
    state.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 0, row));
    state.leave_resize();
    assert_eq!(state.settings.widths[&2], committed);
    assert!(!state.resizing());
}

#[test]
fn keyboard_resize_and_reset_handle_empty_and_narrow_views() {
    let mut state = State::default();
    state.layout = Layout::build(Kind::Apps, &[], Rect::new(0, 0, 26, 8), &state.settings);
    state.enter_resize();
    state.resize_key(KeyCode::Right);
    state.resize_key(KeyCode::Up);
    assert!(state.settings.widths.contains_key(&1));
    state.resize_key(KeyCode::Down);
    state.resize_key(KeyCode::Char('0'));
    assert_eq!(state.settings.start, 0);
    assert!(state.settings.widths.is_empty());

    state.layout = Layout::build(Kind::Apps, &[], Rect::new(0, 0, 2, 3), &state.settings);
    state.resize_key(KeyCode::Up);
    state.resize_key(KeyCode::Down);
    assert_eq!(state.settings.widths.get(&0), Some(&0));
}

#[test]
fn keyboard_can_return_from_the_last_column_to_a_column_offscreen_on_the_left() {
    let mut state = State::default();
    let area = Rect::new(0, 0, 25, 8);
    let rows = rows();
    state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    state.enter_resize();
    for _ in 0..5 {
        state.resize_key(KeyCode::Right);
        state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    }
    assert!(!state.layout.columns.iter().any(|column| column.id == 4));
    state.resize_key(KeyCode::Left);
    state.layout = Layout::build(Kind::Apps, &rows, area, &state.settings);
    assert_eq!(state.layout.columns[1].id, 4);
    state.resize_key(KeyCode::Down);
    assert!(state.settings.widths.contains_key(&4));
}
