// SPDX-License-Identifier: Apache-2.0
//! Pure search + cursor reducer for the onboarding picker.
//!
//! A terminal-free state machine: a list of [`Row`]s, a query string the user
//! types to narrow the visible set, and a cursor that moves within the
//! *filtered* set. [`reduce`] folds one [`PickKey`] into the state and returns
//! a [`PickResult`] when the interaction is finished (a row was selected or the
//! user backed out). Because it touches no I/O, the whole reducer is unit-tested
//! directly (no crossterm, no `Grid`).
//!
//! The crossterm shell ([`crate::onboarding::screen`]) owns only the terminal:
//! it maps real `KeyEvent`s to [`PickKey`], calls [`reduce`], and renders the
//! [`filtered`] view each frame.

/// One selectable row.
///
/// `value` is the machine-facing payload returned by [`PickResult::Selected`]
/// (e.g. a brand key or a model id); `label` is what the user sees and what the
/// query filters against; `note` is optional muted trailing text (auth options,
/// context size, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub value: String,
    pub label: String,
    pub note: Option<String>,
}

impl Row {
    /// A row with no trailing note.
    #[must_use]
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            note: None,
        }
    }

    /// A row with a muted trailing note.
    #[must_use]
    pub fn with_note(value: impl Into<String>, label: impl Into<String>, note: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            note: Some(note.into()),
        }
    }
}

/// The picker's mutable state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerState {
    pub items: Vec<Row>,
    pub query: String,
    pub cursor: usize,
}

impl PickerState {
    /// Fresh state over `items` with an empty query and the cursor at the top.
    #[must_use]
    pub const fn new(items: Vec<Row>) -> Self {
        Self {
            items,
            query: String::new(),
            cursor: 0,
        }
    }
}

/// One normalized key the reducer understands. The screen maps crossterm
/// `KeyEvent`s onto this so the reducer never sees a terminal type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKey {
    Char(char),
    Backspace,
    Up,
    Down,
    Enter,
    Esc,
}

/// The terminal result of a picker run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickResult {
    /// A row was chosen — carries that row's `value`.
    Selected(String),
    /// The user pressed `esc` to step back one stage.
    Back,
}

/// Indices into `state.items` whose `label` (lowercased) contains the query
/// (lowercased). An empty query matches everything (the indices `0..len`).
///
/// Case-insensitive substring match — the same rule the screen renders and the
/// reducer clamps the cursor against, so all three always agree on "the visible
/// set".
#[must_use]
pub fn filtered(state: &PickerState) -> Vec<usize> {
    if state.query.is_empty() {
        return (0..state.items.len()).collect();
    }
    let needle = state.query.to_lowercase();
    state
        .items
        .iter()
        .enumerate()
        .filter(|(_, row)| row.label.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect()
}

/// Fold one key into the state, returning `Some` when the run is over.
///
/// - `Char` pushes to the query and resets the cursor to 0 (the filtered set
///   just changed under it).
/// - `Backspace` pops the query and resets the cursor to 0.
/// - `Up`/`Down` move the cursor within `0..filtered.len()`, saturating (no
///   wrap), and are no-ops on an empty filtered set.
/// - `Enter` returns `Some(Selected(value))` for the highlighted row, or `None`
///   when the filtered set is empty (nothing to select).
/// - `Esc` returns `Some(Back)`.
pub fn reduce(state: &mut PickerState, key: PickKey) -> Option<PickResult> {
    match key {
        PickKey::Char(c) => {
            state.query.push(c);
            state.cursor = 0;
            None
        }
        PickKey::Backspace => {
            state.query.pop();
            state.cursor = 0;
            None
        }
        PickKey::Up => {
            state.cursor = state.cursor.saturating_sub(1);
            None
        }
        PickKey::Down => {
            let count = filtered(state).len();
            if count > 0 && state.cursor + 1 < count {
                state.cursor += 1;
            }
            None
        }
        PickKey::Enter => {
            let view = filtered(state);
            view.get(state.cursor)
                .map(|&idx| PickResult::Selected(state.items[idx].value.clone()))
        }
        PickKey::Esc => Some(PickResult::Back),
    }
}

/// Abbreviate a context-window size for a model `note`.
///
/// `1_000_000` ⇒ `"1M"`, `200_000` ⇒ `"200K"`. Exact powers of 1M/1K render
/// without a decimal (`"1M"`, not `"1.0M"`); other values keep one decimal place
/// (`"1.5M"`).
#[must_use]
pub fn format_ctx(window: u32) -> String {
    if window >= 1_000_000 {
        let m = f64::from(window) / 1_000_000.0;
        if (m.fract()).abs() < f64::EPSILON {
            format!("{m:.0}M")
        } else {
            format!("{m:.1}M")
        }
    } else if window >= 1_000 {
        let k = f64::from(window) / 1_000.0;
        if (k.fract()).abs() < f64::EPSILON {
            format!("{k:.0}K")
        } else {
            format!("{k:.1}K")
        }
    } else {
        window.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PickerState {
        PickerState::new(vec![
            Row::new("anthropic", "Anthropic"),
            Row::new("openai", "OpenAI"),
            Row::new("google", "Google"),
        ])
    }

    #[test]
    fn empty_query_matches_all() {
        let s = sample();
        assert_eq!(filtered(&s), vec![0, 1, 2]);
    }

    #[test]
    fn filter_narrows_case_insensitively() {
        let mut s = sample();
        s.query = "OO".to_string(); // matches only "Google" (oo)
        assert_eq!(filtered(&s), vec![2]);
        s.query = "EN".to_string(); // matches only "OpenAI"
        assert_eq!(filtered(&s), vec![1]);
    }

    #[test]
    fn typing_narrows_and_resets_cursor_to_zero() {
        let mut s = sample();
        s.cursor = 2;
        let r = reduce(&mut s, PickKey::Char('g'));
        assert!(r.is_none());
        assert_eq!(s.query, "g");
        assert_eq!(s.cursor, 0, "typing resets the cursor");
        assert_eq!(filtered(&s), vec![2], "only Google contains 'g'");
    }

    #[test]
    fn backspace_edits_query_and_resets_cursor() {
        let mut s = sample();
        s.query = "goo".to_string();
        s.cursor = 0;
        let r = reduce(&mut s, PickKey::Backspace);
        assert!(r.is_none());
        assert_eq!(s.query, "go");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn up_down_clamp_within_filtered_set_no_wrap() {
        let mut s = sample();
        // Down moves 0 -> 1 -> 2, then clamps at the last filtered index.
        reduce(&mut s, PickKey::Down);
        assert_eq!(s.cursor, 1);
        reduce(&mut s, PickKey::Down);
        assert_eq!(s.cursor, 2);
        reduce(&mut s, PickKey::Down);
        assert_eq!(s.cursor, 2, "down saturates at the last row (no wrap)");
        // Up moves back to 0 and clamps there.
        reduce(&mut s, PickKey::Up);
        assert_eq!(s.cursor, 1);
        reduce(&mut s, PickKey::Up);
        assert_eq!(s.cursor, 0);
        reduce(&mut s, PickKey::Up);
        assert_eq!(s.cursor, 0, "up saturates at the first row (no wrap)");
    }

    #[test]
    fn down_clamps_to_filtered_not_total() {
        let mut s = sample();
        // "a" matches Anthropic + OpenAI (not Google) ⇒ filtered = [0, 1].
        s.query = "a".to_string();
        assert_eq!(filtered(&s), vec![0, 1]);
        s.cursor = 0;
        reduce(&mut s, PickKey::Down);
        assert_eq!(s.cursor, 1);
        reduce(&mut s, PickKey::Down);
        assert_eq!(s.cursor, 1, "clamps to the filtered length (2), not items length (3)");
    }

    #[test]
    fn enter_returns_value_not_label() {
        let mut s = sample();
        s.cursor = 1; // OpenAI
        let r = reduce(&mut s, PickKey::Enter);
        assert_eq!(r, Some(PickResult::Selected("openai".to_string())));
    }

    #[test]
    fn enter_selects_the_highlighted_filtered_row() {
        let mut s = sample();
        // "a" matches Anthropic(0) + OpenAI(1) ⇒ filtered = [0, 1].
        s.query = "a".to_string();
        s.cursor = 1; // -> OpenAI (items index 1)
        let r = reduce(&mut s, PickKey::Enter);
        assert_eq!(r, Some(PickResult::Selected("openai".to_string())));
    }

    #[test]
    fn enter_on_empty_filter_returns_none() {
        let mut s = sample();
        s.query = "zzzz".to_string();
        assert!(filtered(&s).is_empty());
        let r = reduce(&mut s, PickKey::Enter);
        assert_eq!(r, None, "nothing to select ⇒ stay on the picker");
    }

    #[test]
    fn down_on_empty_filter_is_a_noop() {
        let mut s = sample();
        s.query = "zzzz".to_string();
        reduce(&mut s, PickKey::Down);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn esc_returns_back() {
        let mut s = sample();
        assert_eq!(reduce(&mut s, PickKey::Esc), Some(PickResult::Back));
    }

    #[test]
    fn format_ctx_known_values() {
        assert_eq!(format_ctx(1_000_000), "1M");
        assert_eq!(format_ctx(200_000), "200K");
        assert_eq!(format_ctx(128_000), "128K");
        assert_eq!(format_ctx(500), "500");
    }

    #[test]
    fn format_ctx_fractional() {
        assert_eq!(format_ctx(1_500_000), "1.5M");
        assert_eq!(format_ctx(1_500), "1.5K");
    }
}
