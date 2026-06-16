// SPDX-License-Identifier: Apache-2.0
//! Interactive choice component (single + multi select): a pure reducer +
//! painter, reused by both `ask_user` questions and permission asks.
//!
//! Wave 0 stub: locks `PickerOption`/`PickerState`/`PickerKey`/`PickerOutcome`
//! and the `reduce`/`layout_picker` signatures. Wave 1 (Task C) fills the
//! bodies. Field names mirror the `ChoiceAsk` protocol (Task F) so
//! `PickerOutcome::Selected{indices,custom}` maps straight to
//! `ChoiceDecision{selected,custom}`.

use origin_tui::grid::Attr;

use super::tokens::{glyph, RenderRow, RowSpan, Tokens};

/// One selectable option, with an optional description shown in `muted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerOption {
    pub label: String,
    pub description: Option<String>,
}

/// The full state of an interactive picker (single or multi select), driven by
/// [`reduce`] and rendered by [`layout_picker`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerState {
    /// The prompt shown above the options (bold).
    pub question: String,
    /// The selectable options.
    pub options: Vec<PickerOption>,
    /// Whether multiple options may be checked (`space` toggles).
    pub multi: bool,
    /// Whether a `✎ type your own…` free-text row is offered.
    pub allow_custom: bool,
    /// The currently-highlighted option row.
    pub cursor: usize,
    /// Per-option checked state (multi-select); parallel to `options`.
    pub checked: Vec<bool>,
    /// The free-text custom answer being typed, if any.
    pub custom: Option<String>,
    /// Whether the picker is currently in free-text custom-entry mode.
    pub typing_custom: bool,
}

/// An input event the picker reducer consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKey {
    Up,
    Down,
    /// Toggle the checked state of the cursor row (multi-select).
    Toggle,
    /// Confirm the current selection.
    Confirm,
    /// Jump to / select the option at the given 1-based digit.
    Digit(u8),
    /// Enter free-text custom-entry mode (only when `allow_custom`).
    Custom,
    /// Dismiss the picker.
    Cancel,
    /// A typed character while in custom-entry mode.
    Char(char),
    /// Delete the last custom character.
    Backspace,
}

/// The terminal result of a picker interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerOutcome {
    /// One or more options (and/or a custom string) were chosen. `indices` are
    /// sorted option indices; `custom` carries free-text when entered.
    Selected {
        indices: Vec<usize>,
        custom: Option<String>,
    },
    /// The user dismissed without choosing.
    Cancelled,
}

/// Apply one key event to the picker state, returning `Some(outcome)` when the
/// interaction terminates (confirm / select / cancel) or `None` to keep going.
///
/// Behaviour (spec §"Picker behavior"):
/// - **Cancel** always terminates with [`PickerOutcome::Cancelled`].
/// - While **typing a custom answer** (`typing_custom`): `Char`/`Backspace` edit
///   the buffer, `Confirm` emits `Selected{ indices: [], custom: Some(text) }`,
///   and navigation/toggle keys are inert (text entry has focus).
/// - **Up/Down** move the cursor (saturating at the ends), skipping when there
///   are no options.
/// - **Digit(n)** (1-based) selects option `n-1` immediately in single-select,
///   or moves the cursor to it in multi-select.
/// - **Toggle** flips `checked[cursor]` in multi-select (inert otherwise).
/// - **Custom** enters free-text mode, but only when `allow_custom`.
/// - **Confirm** emits the current selection: the cursor row (single) or every
///   checked index, sorted ascending (multi).
pub fn reduce(state: &mut PickerState, key: PickerKey) -> Option<PickerOutcome> {
    // Cancel short-circuits regardless of mode.
    if key == PickerKey::Cancel {
        return Some(PickerOutcome::Cancelled);
    }

    // Free-text custom entry captures the keyboard until confirmed/cancelled.
    if state.typing_custom {
        match key {
            PickerKey::Char(c) => {
                state.custom.get_or_insert_with(String::new).push(c);
                None
            }
            PickerKey::Backspace => {
                if let Some(buf) = state.custom.as_mut() {
                    buf.pop();
                }
                None
            }
            PickerKey::Confirm => Some(PickerOutcome::Selected {
                indices: Vec::new(),
                custom: Some(state.custom.clone().unwrap_or_default()),
            }),
            // All other keys are inert while typing free text.
            _ => None,
        }
    } else {
        match key {
            PickerKey::Up => {
                state.cursor = state.cursor.saturating_sub(1);
                None
            }
            PickerKey::Down => {
                // The cursor ranges over `[0, options.len())`, plus the trailing
                // `✎ type your own…` row at index `options.len()` when
                // `allow_custom` (so Down can reach it and Enter→Custom there).
                let last = if state.allow_custom {
                    state.options.len()
                } else {
                    state.options.len().saturating_sub(1)
                };
                if state.cursor < last {
                    state.cursor += 1;
                }
                None
            }
            PickerKey::Toggle => {
                if state.multi {
                    ensure_checked_len(state);
                    if let Some(slot) = state.checked.get_mut(state.cursor) {
                        *slot = !*slot;
                    }
                }
                None
            }
            PickerKey::Digit(n) => {
                let idx = (n as usize).checked_sub(1)?;
                if idx >= state.options.len() {
                    return None;
                }
                state.cursor = idx;
                if state.multi {
                    // In multi-select a digit just jumps the cursor; the user
                    // still toggles + confirms explicitly.
                    None
                } else {
                    // Single-select: jump-and-emit immediately.
                    Some(PickerOutcome::Selected {
                        indices: vec![idx],
                        custom: None,
                    })
                }
            }
            PickerKey::Custom => {
                if state.allow_custom {
                    state.typing_custom = true;
                    state.custom.get_or_insert_with(String::new);
                }
                None
            }
            PickerKey::Confirm => Some(confirm_selection(state)),
            // Char/Backspace outside custom mode, and Cancel (handled above).
            PickerKey::Char(_) | PickerKey::Backspace | PickerKey::Cancel => None,
        }
    }
}

/// Grow `checked` to match `options` so a never-toggled multi-select still has a
/// slot per option (the default state may carry an empty `checked`).
fn ensure_checked_len(state: &mut PickerState) {
    if state.checked.len() < state.options.len() {
        state.checked.resize(state.options.len(), false);
    }
}

/// Build the [`PickerOutcome::Selected`] for a non-custom confirm.
fn confirm_selection(state: &PickerState) -> PickerOutcome {
    if state.multi {
        let indices = state
            .checked
            .iter()
            .enumerate()
            .filter_map(|(i, &on)| if on && i < state.options.len() { Some(i) } else { None })
            .collect();
        PickerOutcome::Selected {
            indices,
            custom: None,
        }
    } else {
        // Single-select confirm picks the highlighted row (clamped if the option
        // list is empty → no index).
        let indices = if state.cursor < state.options.len() {
            vec![state.cursor]
        } else {
            Vec::new()
        };
        PickerOutcome::Selected {
            indices,
            custom: None,
        }
    }
}

/// Lay out the picker into [`RenderRow`]s: the bold question, each option row
/// (`▸` cursor / `□■` boxes / number hints / `muted` description), and the
/// `✎ type your own…` row when allowed.
///
/// One row per option, preceded by the bold question and followed (when
/// `allow_custom`) by the custom row, so the total height is
/// `1 + options.len() + allow_custom as usize`. `width` clips long descriptions.
#[must_use]
pub fn layout_picker(state: &PickerState, width: u16, tok: &Tokens) -> Vec<RenderRow> {
    let mut rows = Vec::with_capacity(state.options.len() + 2);

    // Question header (bold, bright).
    rows.push(RenderRow::one(RowSpan::new(
        state.question.clone(),
        tok.bright,
        0,
        Attr::BOLD,
    )));

    for (i, opt) in state.options.iter().enumerate() {
        let cursored = i == state.cursor && !state.typing_custom;
        let checked = state.checked.get(i).copied().unwrap_or(false);
        let mut spans = Vec::new();

        // Cursor caret column (`▸` on the active row, else a space to keep
        // columns aligned).
        let caret = if cursored { glyph::CURSOR } else { ' ' };
        let caret_fg = if cursored { tok.accent } else { tok.muted };
        spans.push(RowSpan::plain(format!("{caret} "), caret_fg, 0));

        // Multi-select checkbox.
        if state.multi {
            let (box_glyph, box_fg) = if checked {
                (glyph::BOX_CHECKED, tok.accent)
            } else {
                (glyph::BOX_UNCHECKED, tok.muted)
            };
            spans.push(RowSpan::plain(format!("{box_glyph} "), box_fg, 0));
        }

        // 1-based number hint.
        spans.push(RowSpan::plain(
            format!("{}. ", i + 1),
            tok.muted,
            0,
        ));

        // Label — bright when cursored, body otherwise.
        let label_fg = if cursored { tok.bright } else { tok.body };
        let label_attr = if cursored { Attr::BOLD } else { Attr::PLAIN };
        spans.push(RowSpan::new(opt.label.clone(), label_fg, 0, label_attr));

        // Optional description in muted, clipped to the remaining width.
        if let Some(desc) = opt.description.as_deref() {
            if !desc.is_empty() {
                let used: usize = spans.iter().map(|s| s.text.chars().count()).sum();
                let avail = (width as usize).saturating_sub(used + 3);
                let shown = clip_chars(desc, avail);
                if !shown.is_empty() {
                    spans.push(RowSpan::plain(format!("  {shown}"), tok.muted, 0));
                }
            }
        }

        rows.push(RenderRow { spans, indent: 0 });
    }

    // `✎ type your own…` row.
    if state.allow_custom {
        let active = state.typing_custom;
        // The caret shows while typing OR when the cursor rests on this row (so a
        // user who arrowed down to it gets the same `▸` affordance as an option).
        let cursored = active || state.cursor == state.options.len();
        let caret = if cursored { glyph::CURSOR } else { ' ' };
        let caret_fg = if cursored { tok.accent } else { tok.muted };
        let mut spans = vec![
            RowSpan::plain(format!("{caret} "), caret_fg, 0),
            RowSpan::plain(format!("{} ", glyph::EDIT), tok.accent_dim, 0),
        ];
        if active {
            // Show the prompt glyph + the in-progress custom text.
            spans.push(RowSpan::plain(
                format!("{} ", glyph::PROMPT),
                tok.accent,
                0,
            ));
            let typed = state.custom.as_deref().unwrap_or("");
            spans.push(RowSpan::new(typed.to_string(), tok.bright, 0, Attr::PLAIN));
        } else {
            spans.push(RowSpan::plain("type your own\u{2026}", tok.muted, 0));
        }
        rows.push(RenderRow { spans, indent: 0 });
    }

    rows
}

/// Take the first `max` chars of `s` (char-, not byte-, bounded so we never
/// split a UTF-8 codepoint). `max == 0` yields an empty string.
fn clip_chars(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    s.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fresh single-select state over `n` plainly-labelled options.
    fn single(n: usize) -> PickerState {
        PickerState {
            question: "pick one".into(),
            options: (0..n)
                .map(|i| PickerOption {
                    label: format!("opt{i}"),
                    description: None,
                })
                .collect(),
            multi: false,
            allow_custom: false,
            cursor: 0,
            checked: Vec::new(),
            custom: None,
            typing_custom: false,
        }
    }

    /// Build a fresh multi-select state over `n` options (empty `checked`).
    fn multi(n: usize) -> PickerState {
        let mut s = single(n);
        s.multi = true;
        s.question = "pick many".into();
        s
    }

    // ---- single-select reducer --------------------------------------------

    #[test]
    fn up_down_move_cursor_and_saturate() {
        let mut s = single(3);
        assert!(reduce(&mut s, PickerKey::Down).is_none());
        assert_eq!(s.cursor, 1);
        assert!(reduce(&mut s, PickerKey::Down).is_none());
        assert_eq!(s.cursor, 2);
        // Past the last option ⇒ stays clamped.
        assert!(reduce(&mut s, PickerKey::Down).is_none());
        assert_eq!(s.cursor, 2, "Down saturates at the last option");
        reduce(&mut s, PickerKey::Up);
        reduce(&mut s, PickerKey::Up);
        reduce(&mut s, PickerKey::Up);
        assert_eq!(s.cursor, 0, "Up saturates at the first option");
    }

    #[test]
    fn single_confirm_emits_cursor_index() {
        let mut s = single(3);
        reduce(&mut s, PickerKey::Down);
        let out = reduce(&mut s, PickerKey::Confirm);
        assert_eq!(
            out,
            Some(PickerOutcome::Selected {
                indices: vec![1],
                custom: None,
            })
        );
    }

    #[test]
    fn single_digit_jump_selects_and_emits() {
        // Digit(3) on a single-select picks option index 2 immediately.
        let mut s = single(4);
        let out = reduce(&mut s, PickerKey::Digit(3));
        assert_eq!(
            out,
            Some(PickerOutcome::Selected {
                indices: vec![2],
                custom: None,
            })
        );
        assert_eq!(s.cursor, 2, "cursor also moved to the chosen row");
    }

    #[test]
    fn out_of_range_digit_is_inert() {
        let mut s = single(2);
        assert!(reduce(&mut s, PickerKey::Digit(9)).is_none());
        assert!(reduce(&mut s, PickerKey::Digit(0)).is_none(), "0 has no 0-based row");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn single_toggle_is_inert() {
        // Toggle only applies to multi-select.
        let mut s = single(3);
        assert!(reduce(&mut s, PickerKey::Toggle).is_none());
        assert!(s.checked.is_empty());
    }

    // ---- multi-select reducer ---------------------------------------------

    #[test]
    fn multi_toggle_then_confirm_sorted_indices() {
        let mut s = multi(4);
        // Toggle option 2 (cursor at 0 → down,down), then option 0.
        reduce(&mut s, PickerKey::Down);
        reduce(&mut s, PickerKey::Down);
        reduce(&mut s, PickerKey::Toggle); // checks index 2
        // Jump back to 0 and check it.
        reduce(&mut s, PickerKey::Up);
        reduce(&mut s, PickerKey::Up);
        reduce(&mut s, PickerKey::Toggle); // checks index 0
        let out = reduce(&mut s, PickerKey::Confirm);
        assert_eq!(
            out,
            Some(PickerOutcome::Selected {
                indices: vec![0, 2],
                custom: None,
            }),
            "confirm yields all checked indices, sorted ascending"
        );
    }

    #[test]
    fn multi_toggle_twice_unchecks() {
        let mut s = multi(2);
        reduce(&mut s, PickerKey::Toggle);
        reduce(&mut s, PickerKey::Toggle);
        let out = reduce(&mut s, PickerKey::Confirm);
        assert_eq!(
            out,
            Some(PickerOutcome::Selected {
                indices: vec![],
                custom: None,
            }),
            "toggling the same row twice leaves nothing checked"
        );
    }

    #[test]
    fn multi_digit_jumps_cursor_without_emitting() {
        let mut s = multi(4);
        // In multi mode a digit moves the cursor but does not terminate.
        assert!(reduce(&mut s, PickerKey::Digit(3)).is_none());
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn multi_confirm_empty_yields_empty_indices() {
        let mut s = multi(3);
        let out = reduce(&mut s, PickerKey::Confirm);
        assert_eq!(
            out,
            Some(PickerOutcome::Selected {
                indices: vec![],
                custom: None,
            })
        );
    }

    // ---- custom free-text flow --------------------------------------------

    #[test]
    fn custom_flow_types_edits_and_confirms() {
        let mut s = single(2);
        s.allow_custom = true;
        // Entering custom mode requires allow_custom.
        assert!(reduce(&mut s, PickerKey::Custom).is_none());
        assert!(s.typing_custom, "Custom enters free-text mode");
        // Type "hi!", backspace the '!', then confirm.
        reduce(&mut s, PickerKey::Char('h'));
        reduce(&mut s, PickerKey::Char('i'));
        reduce(&mut s, PickerKey::Char('!'));
        reduce(&mut s, PickerKey::Backspace);
        let out = reduce(&mut s, PickerKey::Confirm);
        assert_eq!(
            out,
            Some(PickerOutcome::Selected {
                indices: vec![],
                custom: Some("hi".into()),
            })
        );
    }

    #[test]
    fn down_reaches_custom_row_when_allowed() {
        // With allow_custom, the cursor extends one past the last option onto the
        // trailing `✎ type your own…` row so Down can reach it (Enter→Custom
        // there is decided by the key mapper in main.rs).
        let mut s = single(2);
        s.allow_custom = true;
        reduce(&mut s, PickerKey::Down); // 0 → 1 (last option)
        reduce(&mut s, PickerKey::Down); // 1 → 2 (custom row)
        assert_eq!(s.cursor, s.options.len(), "cursor reaches the custom row");
        // Further Down saturates at the custom row.
        reduce(&mut s, PickerKey::Down);
        assert_eq!(s.cursor, s.options.len(), "Down saturates on the custom row");
    }

    #[test]
    fn down_stops_at_last_option_without_custom() {
        // Without allow_custom there is no custom row, so Down still saturates at
        // the last option (unchanged behavior).
        let mut s = single(2);
        reduce(&mut s, PickerKey::Down);
        reduce(&mut s, PickerKey::Down);
        assert_eq!(s.cursor, 1, "Down saturates at the last option");
    }

    #[test]
    fn custom_blocked_when_not_allowed() {
        let mut s = single(2);
        assert!(reduce(&mut s, PickerKey::Custom).is_none());
        assert!(!s.typing_custom, "Custom is inert without allow_custom");
    }

    #[test]
    fn navigation_inert_while_typing_custom() {
        let mut s = single(3);
        s.allow_custom = true;
        reduce(&mut s, PickerKey::Custom);
        reduce(&mut s, PickerKey::Down); // should NOT move the cursor
        assert_eq!(s.cursor, 0, "navigation is inert while typing free text");
        // Backspace on an empty buffer is harmless.
        reduce(&mut s, PickerKey::Backspace);
        assert_eq!(s.custom.as_deref(), Some(""));
    }

    // ---- cancel ------------------------------------------------------------

    #[test]
    fn cancel_always_cancels() {
        let mut s = single(3);
        assert_eq!(reduce(&mut s, PickerKey::Cancel), Some(PickerOutcome::Cancelled));
        // Even mid-custom-entry.
        let mut s2 = single(2);
        s2.allow_custom = true;
        reduce(&mut s2, PickerKey::Custom);
        reduce(&mut s2, PickerKey::Char('x'));
        assert_eq!(reduce(&mut s2, PickerKey::Cancel), Some(PickerOutcome::Cancelled));
    }

    // ---- layout_picker -----------------------------------------------------

    #[test]
    fn layout_single_row_count_is_question_plus_options() {
        let tok = Tokens::default_tokens();
        let s = single(3);
        let rows = layout_picker(&s, 80, &tok);
        // 1 question + 3 options, no custom row.
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn layout_multi_with_custom_adds_one_row() {
        let tok = Tokens::default_tokens();
        let mut s = multi(3);
        s.allow_custom = true;
        let rows = layout_picker(&s, 80, &tok);
        // 1 question + 3 options + 1 custom row.
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn layout_question_row_is_bold() {
        let tok = Tokens::default_tokens();
        let s = single(2);
        let rows = layout_picker(&s, 80, &tok);
        let q = &rows[0];
        assert_eq!(q.spans.len(), 1);
        assert!(
            q.spans[0].attr.bits() & Attr::BOLD.bits() != 0,
            "question is bold"
        );
        assert_eq!(q.spans[0].text, "pick one");
    }

    #[test]
    fn layout_multi_rows_carry_checkbox_glyph() {
        let tok = Tokens::default_tokens();
        let mut s = multi(2);
        s.checked = vec![true, false];
        let rows = layout_picker(&s, 80, &tok);
        // The first option row joins to a string containing the checked box.
        let joined: String = rows[1].spans.iter().map(|sp| sp.text.as_str()).collect();
        assert!(
            joined.contains(glyph::BOX_CHECKED),
            "checked option shows ■, got {joined:?}"
        );
        let joined2: String = rows[2].spans.iter().map(|sp| sp.text.as_str()).collect();
        assert!(
            joined2.contains(glyph::BOX_UNCHECKED),
            "unchecked option shows □, got {joined2:?}"
        );
    }

    #[test]
    fn layout_cursor_row_carries_caret() {
        let tok = Tokens::default_tokens();
        let mut s = single(3);
        s.cursor = 1;
        let rows = layout_picker(&s, 80, &tok);
        // rows[0] = question, rows[1] = option0, rows[2] = option1 (cursored).
        let cursored: String = rows[2].spans.iter().map(|sp| sp.text.as_str()).collect();
        assert!(cursored.contains(glyph::CURSOR), "cursor row shows ▸");
        let other: String = rows[1].spans.iter().map(|sp| sp.text.as_str()).collect();
        assert!(!other.contains(glyph::CURSOR), "non-cursor row has no ▸");
    }

    #[test]
    fn layout_custom_row_shows_typed_text_when_active() {
        let tok = Tokens::default_tokens();
        let mut s = single(1);
        s.allow_custom = true;
        s.typing_custom = true;
        s.custom = Some("hello".into());
        let rows = layout_picker(&s, 80, &tok);
        let last = rows.last().expect("layout always yields rows");
        let custom_row: String = last.spans.iter().map(|sp| sp.text.as_str()).collect();
        assert!(custom_row.contains("hello"), "active custom row echoes the buffer");
    }
}
