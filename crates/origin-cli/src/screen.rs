//! Screen layout primitives.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

#[must_use]
pub fn split_main_input(area: Rect) -> (Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(3)])
        .split(area);
    (chunks[0], chunks[1])
}
