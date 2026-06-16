// SPDX-License-Identifier: Apache-2.0
//! Interactive choice component (single + multi select): a pure reducer +
//! painter, reused by both `ask_user` questions and permission asks.
//!
//! Wave 0 stub: locks `PickerOption`/`PickerState`/`PickerKey`/`PickerOutcome`
//! and the `reduce`/`layout_picker` signatures. Wave 1 (Task C) fills the
//! bodies. Field names mirror the `ChoiceAsk` protocol (Task F) so
//! `PickerOutcome::Selected{indices,custom}` maps straight to
//! `ChoiceDecision{selected,custom}`.

// The stub bodies look const-foldable now; Wave 1 fills them with the real
// (non-const) reducer + painter logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use super::tokens::{RenderRow, Tokens};

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
pub fn reduce(_state: &mut PickerState, _key: PickerKey) -> Option<PickerOutcome> {
    // Wave 1 (Task C) fills this.
    None
}

/// Lay out the picker into [`RenderRow`]s: the bold question, each option row
/// (`▸` cursor / `□■` boxes / number hints / `muted` description), and the
/// `✎ type your own…` row when allowed.
#[must_use]
pub fn layout_picker(_state: &PickerState, _width: u16, _tok: &Tokens) -> Vec<RenderRow> {
    // Wave 1 (Task C) fills this.
    Vec::new()
}
