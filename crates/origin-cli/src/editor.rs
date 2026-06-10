// SPDX-License-Identifier: Apache-2.0
//! Cursor-aware input editor + per-session prompt history.
//!
//! This module owns the user's in-flight prompt text and the cursor
//! position inside it. The render layer (`tui.rs`) reads from it; the
//! event loop (`main.rs`) drives it through small typed actions.
//!
//! Goals:
//! - **Cursor**: byte-indexed into the buffer. All edit ops respect it.
//!   `Left`/`Right` move it; `Home`/`End` (within input) jump to start
//!   and end of the *visual* line; `Insert`/`Backspace`/`Delete` operate
//!   at the cursor (not the end).
//! - **Vertical navigation**: `Up`/`Down` move across visual lines when
//!   the input has multiple lines. When the cursor is on the FIRST visual
//!   line and the user presses `Up`, the previous submitted message is
//!   recalled into the buffer (history-up). Symmetric on the last line
//!   with `Down`.
//! - **History**: a flat `Vec<String>` of past submissions. `push_history`
//!   appends on submit; `history_up`/`history_down` walk the list. A
//!   *draft* slot remembers what the user had typed before they started
//!   browsing history, so coming back "below" the newest entry restores
//!   it.
//!
//! Pure — no I/O, no terminal calls. Cursor renderer logic lives here
//! (`wrap_with_cursor`) so it's unit-testable.

use unicode_width::UnicodeWidthChar;

/// In-flight input buffer with cursor + history.
#[derive(Debug, Default)]
pub struct Editor {
    /// Current text. May contain `\n` (multi-line input).
    buffer: String,
    /// Cursor position as a **byte** index into `buffer`. Always lies on
    /// a UTF-8 char boundary and satisfies `cursor <= buffer.len()`.
    cursor: usize,
    /// User-submitted messages in chronological order.
    history: Vec<String>,
    /// When `Some(i)`, the editor is browsing `history[i]`. `None` means
    /// the user is composing fresh text in `buffer` (the `draft`).
    history_pos: Option<usize>,
    /// Stash for the in-flight buffer when the user starts browsing
    /// history. Restored on `history_down` past the newest entry, and on
    /// cancelling a reverse search.
    draft: String,
    /// Active Ctrl-R reverse-incremental history search, if any.
    search: Option<RevSearch>,
    /// Messages queued while a turn is in flight, in submission (FIFO)
    /// order: `queued[0]` is the next to auto-submit. Pressing `Up` on the
    /// top-most visual line edits these (newest first) before falling back
    /// to history recall.
    queued: Vec<String>,
    /// When `Some(i)`, the buffer is editing `queued[i]` in place. Edits
    /// commit back into the slot on navigation or Enter.
    queue_pos: Option<usize>,
    /// Stash for the in-flight buffer when the user starts editing the
    /// queue. Restored when editing ends (commit or Down past the newest).
    queue_draft: String,
}

/// Live Ctrl-R reverse-incremental history search state.
///
/// `query` is the text typed so far; `match_idx` is the index into
/// [`Editor::history`] of the current match (searched newest-first), or `None`
/// when nothing matches the query yet.
#[derive(Debug, Default, Clone)]
struct RevSearch {
    query: String,
    match_idx: Option<usize>,
}

impl Editor {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// The cursor position as a **character** count (not byte offset).
    ///
    /// The opt-in vim layer reasons in character units (its motions operate on
    /// `chars().count()`), so this and [`set_cursor_chars`](Self::set_cursor_chars)
    /// are the char-indexed bridge between the vim reducer and the byte-indexed
    /// editor cursor.
    #[must_use]
    pub fn cursor_chars(&self) -> usize {
        self.buffer[..self.cursor].chars().count()
    }

    /// Set the cursor from a **character** index, clamped to `0..=len_chars` and
    /// snapped to the matching UTF-8 byte boundary. Used by the vim layer to map
    /// a char-indexed motion result back onto the byte-indexed cursor.
    pub fn set_cursor_chars(&mut self, char_idx: usize) {
        self.cursor = self
            .buffer
            .char_indices()
            .map(|(i, _)| i)
            .nth(char_idx)
            .unwrap_or(self.buffer.len());
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Append `text` to history (call on submit) and clear the buffer +
    /// any in-progress draft. Empty submissions are not recorded.
    pub fn push_history(&mut self, text: &str) {
        if !text.is_empty() {
            self.history.push(text.to_string());
        }
        self.buffer.clear();
        self.cursor = 0;
        self.history_pos = None;
        self.draft.clear();
    }

    /// Replace the current buffer with `text` and put the cursor at end.
    /// Clears any history-browsing state.
    pub fn set_buffer(&mut self, text: String) {
        self.cursor = text.len();
        self.buffer = text;
        self.history_pos = None;
        self.draft.clear();
    }

    /// Insert `c` at the cursor; advance the cursor past it.
    pub fn insert_char(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Insert `s` at the cursor; advance the cursor to its end.
    pub fn insert_str(&mut self, s: &str) {
        self.buffer.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Insert a newline at the cursor.
    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Delete one char to the left of the cursor (backspace). No-op when
    /// the cursor is at position 0.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Find the byte index of the previous char boundary.
        let prev = self.buffer[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
        self.buffer.replace_range(prev..self.cursor, "");
        self.cursor = prev;
    }

    /// Delete one char at the cursor (forward delete). No-op at end.
    pub fn delete(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let next = self.buffer[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8());
        self.buffer.replace_range(self.cursor..next, "");
    }

    /// Move the cursor one char left. No-op at start.
    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.buffer[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i);
        self.cursor = prev;
    }

    /// Move the cursor one char right. No-op at end.
    pub fn move_right(&mut self) {
        if self.cursor >= self.buffer.len() {
            return;
        }
        let c = self.buffer[self.cursor..].chars().next();
        if let Some(c) = c {
            self.cursor += c.len_utf8();
        }
    }

    /// Jump the cursor to the start of the current visual line, given
    /// the wrap width that the renderer is using. Falls back to buffer
    /// start when `width` is 0.
    pub fn move_home(&mut self, width: usize) {
        let layout = wrap_with_cursor(&self.buffer, width, self.cursor);
        if let Some(line) = layout.lines.get(layout.cursor_row) {
            self.cursor = line.byte_start;
        } else {
            self.cursor = 0;
        }
    }

    /// Jump the cursor to the end of the current visual line.
    pub fn move_end(&mut self, width: usize) {
        let layout = wrap_with_cursor(&self.buffer, width, self.cursor);
        if let Some(line) = layout.lines.get(layout.cursor_row) {
            self.cursor = line.byte_end;
        } else {
            self.cursor = self.buffer.len();
        }
    }

    /// Vertical-up: move the cursor to the previous visual line at the
    /// same display column, if possible. Returns `true` when the cursor
    /// moved. Returns `false` when already on the first visual line —
    /// caller should then fall back to history-up.
    pub fn move_up_visual(&mut self, width: usize) -> bool {
        let layout = wrap_with_cursor(&self.buffer, width, self.cursor);
        if layout.cursor_row == 0 {
            return false;
        }
        let target = &layout.lines[layout.cursor_row - 1];
        self.cursor = byte_at_display_col(
            &self.buffer[target.byte_start..target.byte_end],
            layout.cursor_col,
        ) + target.byte_start;
        true
    }

    /// Vertical-down: symmetric. Returns `false` when already on the
    /// last visual line — caller falls back to history-down.
    pub fn move_down_visual(&mut self, width: usize) -> bool {
        let layout = wrap_with_cursor(&self.buffer, width, self.cursor);
        if layout.cursor_row + 1 >= layout.lines.len() {
            return false;
        }
        let target = &layout.lines[layout.cursor_row + 1];
        self.cursor = byte_at_display_col(
            &self.buffer[target.byte_start..target.byte_end],
            layout.cursor_col,
        ) + target.byte_start;
        true
    }

    /// Recall the previous (older) history entry. Stashes the live
    /// draft into `self.draft` the first time it's invoked. Returns
    /// `true` if anything changed.
    pub fn history_up(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        let new_pos = match self.history_pos {
            None => {
                // Entering history — stash the draft.
                self.draft = std::mem::take(&mut self.buffer);
                self.history.len() - 1
            }
            Some(0) => return false,
            Some(i) => i - 1,
        };
        self.history_pos = Some(new_pos);
        self.buffer = self.history[new_pos].clone();
        self.cursor = self.buffer.len();
        true
    }

    /// Recall the next (newer) history entry, or restore the draft when
    /// stepping past the newest. Returns `true` if anything changed.
    pub fn history_down(&mut self) -> bool {
        match self.history_pos {
            None => false,
            Some(i) if i + 1 < self.history.len() => {
                self.history_pos = Some(i + 1);
                self.buffer = self.history[i + 1].clone();
                self.cursor = self.buffer.len();
                true
            }
            Some(_) => {
                // Step past the newest entry — restore the stashed draft.
                self.history_pos = None;
                self.buffer = std::mem::take(&mut self.draft);
                self.cursor = self.buffer.len();
                true
            }
        }
    }

    // -- Queued messages ----------------------------------------------------

    /// Append `text` to the message queue (called when the user submits
    /// while a turn is already in flight). Empty texts are ignored.
    pub fn queue_message(&mut self, text: &str) {
        if !text.is_empty() {
            self.queued.push(text.to_string());
        }
    }

    /// Number of queued messages.
    #[must_use]
    pub fn queued_len(&self) -> usize {
        self.queued.len()
    }

    /// Read-only view of the queued messages (FIFO order).
    #[must_use]
    pub fn queued_messages(&self) -> &[String] {
        &self.queued
    }

    /// Whether the buffer is currently editing a queued message.
    #[must_use]
    pub const fn editing_queue(&self) -> bool {
        self.queue_pos.is_some()
    }

    /// Whether the editor is currently browsing prompt history (the buffer
    /// shows a recalled history entry rather than fresh draft text). Used by
    /// the key reducer to keep history browsing and queue editing distinct.
    #[must_use]
    pub const fn browsing_history(&self) -> bool {
        self.history_pos.is_some()
    }

    /// Pop the next (oldest) queued message for auto-submission, if any.
    /// If that slot is the one being edited, the live buffer content is
    /// what's popped (the user's edits win) and editing ends.
    pub fn pop_queued(&mut self) -> Option<String> {
        if self.queued.is_empty() {
            return None;
        }
        let text = match self.queue_pos {
            Some(0) => {
                // The user is editing the very message being dequeued: take
                // the live buffer, restore their pre-edit draft.
                self.queue_pos = None;
                let live = std::mem::replace(&mut self.buffer, std::mem::take(&mut self.queue_draft));
                self.cursor = self.buffer.len();
                self.queued.remove(0);
                live
            }
            Some(i) => {
                // Editing a later slot: shift its index down by one.
                self.queue_pos = Some(i - 1);
                self.queued.remove(0)
            }
            None => self.queued.remove(0),
        };
        if text.is_empty() {
            // An edit emptied this slot — skip it and try the next.
            return self.pop_queued();
        }
        Some(text)
    }

    /// Begin editing the newest queued message, or step to the next-older
    /// one when already editing. Stashes the live draft on entry; commits
    /// the buffer back into the current slot before stepping. Returns
    /// `true` when anything changed (i.e. there was a queue to edit).
    ///
    /// Invoked from `Up` on the top-most visual line — queued messages are
    /// reviewed before history, since they are the messages most likely to
    /// need a quick fix-up before the daemon consumes them.
    pub fn queue_edit_up(&mut self) -> bool {
        if self.queued.is_empty() {
            return false;
        }
        let new_pos = match self.queue_pos {
            None => {
                // Entering the queue — stash the live draft.
                self.queue_draft = std::mem::take(&mut self.buffer);
                self.queued.len() - 1
            }
            Some(0) => return false, // already at the oldest queued message
            Some(i) => {
                self.commit_queue_edit(i);
                i - 1
            }
        };
        self.queue_pos = Some(new_pos);
        self.buffer = self.queued[new_pos].clone();
        self.cursor = self.buffer.len();
        true
    }

    /// Step to the next-newer queued message, or restore the stashed draft
    /// when stepping past the newest. Commits the buffer into the current
    /// slot first. Returns `true` when anything changed. No-op (`false`)
    /// when not editing the queue — the caller falls through to plain
    /// history-down handling.
    pub fn queue_edit_down(&mut self) -> bool {
        let Some(i) = self.queue_pos else {
            return false;
        };
        self.commit_queue_edit(i);
        if i + 1 < self.queued.len() {
            self.queue_pos = Some(i + 1);
            self.buffer = self.queued[i + 1].clone();
        } else {
            // Past the newest — leave queue editing, restore the draft.
            self.queue_pos = None;
            self.buffer = std::mem::take(&mut self.queue_draft);
        }
        self.cursor = self.buffer.len();
        true
    }

    /// Commit the live buffer back into queue slot `i`. An emptied slot is
    /// kept (it's dropped at `pop_queued` time) so indices stay stable
    /// while the user is mid-review.
    fn commit_queue_edit(&mut self, i: usize) {
        if let Some(slot) = self.queued.get_mut(i) {
            slot.clone_from(&self.buffer);
        }
    }

    /// Finish editing the current queue slot: commit the buffer into it and
    /// restore the pre-edit draft. Used when Enter is pressed while editing
    /// a queued message — the edit updates the queue rather than submitting
    /// a new turn. Returns `true` when an edit was committed.
    pub fn queue_edit_commit(&mut self) -> bool {
        let Some(i) = self.queue_pos.take() else {
            return false;
        };
        self.commit_queue_edit(i);
        // Drop a slot the user deliberately emptied.
        if self.queued.get(i).is_some_and(String::is_empty) {
            self.queued.remove(i);
        }
        self.buffer = std::mem::take(&mut self.queue_draft);
        self.cursor = self.buffer.len();
        true
    }

    // -- Ctrl-R reverse-incremental history search -------------------------

    /// Whether a Ctrl-R reverse history search is currently active.
    #[must_use]
    pub const fn reverse_search_active(&self) -> bool {
        self.search.is_some()
    }

    /// The current reverse-search query (for rendering a `(reverse-i-search)`
    /// prompt), or `None` when not searching.
    #[must_use]
    pub fn reverse_search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|s| s.query.as_str())
    }

    /// Enter reverse-search mode, stashing the in-flight buffer as the draft so
    /// a later cancel can restore it. No-op (false) if already searching.
    pub fn start_reverse_search(&mut self) -> bool {
        if self.search.is_some() {
            return false;
        }
        self.history_pos = None;
        self.draft = std::mem::take(&mut self.buffer);
        self.cursor = 0;
        self.search = Some(RevSearch::default());
        true
    }

    /// Newest history index `<= start` whose entry contains `query`; `None` for
    /// an empty query / empty history / no match.
    fn find_match_at_or_below(&self, start: usize, query: &str) -> Option<usize> {
        if query.is_empty() || self.history.is_empty() {
            return None;
        }
        let top = start.min(self.history.len() - 1);
        (0..=top).rev().find(|&i| self.history[i].contains(query))
    }

    /// Mirror the current match (if any) into the buffer; clear it otherwise.
    fn apply_search_match(&mut self) {
        let idx = self.search.as_ref().and_then(|s| s.match_idx);
        if let Some(i) = idx {
            self.buffer = self.history[i].clone();
        } else {
            self.buffer.clear();
        }
        self.cursor = self.buffer.len();
    }

    /// Append `c` to the query and re-search from the newest entry. Returns
    /// `false` when not in search mode.
    pub fn reverse_search_push(&mut self, c: char) -> bool {
        let Some(query) = self.search.as_mut().map(|s| {
            s.query.push(c);
            s.query.clone()
        }) else {
            return false;
        };
        let top = self.history.len().saturating_sub(1);
        let m = self.find_match_at_or_below(top, &query);
        if let Some(s) = self.search.as_mut() {
            s.match_idx = m;
        }
        self.apply_search_match();
        true
    }

    /// Delete the last query char and re-search. Returns `false` when inactive.
    pub fn reverse_search_backspace(&mut self) -> bool {
        let Some(query) = self.search.as_mut().map(|s| {
            s.query.pop();
            s.query.clone()
        }) else {
            return false;
        };
        let top = self.history.len().saturating_sub(1);
        let m = self.find_match_at_or_below(top, &query);
        if let Some(s) = self.search.as_mut() {
            s.match_idx = m;
        }
        self.apply_search_match();
        true
    }

    /// Cycle to the next OLDER match for the current query (a second Ctrl-R).
    /// Returns `false` when inactive.
    pub fn reverse_search_again(&mut self) -> bool {
        let Some((cur, query)) = self.search.as_ref().map(|s| (s.match_idx, s.query.clone())) else {
            return false;
        };
        if let Some(c) = cur {
            if c > 0 {
                if let Some(next) = self.find_match_at_or_below(c - 1, &query) {
                    if let Some(s) = self.search.as_mut() {
                        s.match_idx = Some(next);
                    }
                }
            }
        }
        self.apply_search_match();
        true
    }

    /// Accept the current match: keep it in the buffer and exit search mode.
    /// With no match the pre-search draft is restored. Returns the resulting
    /// buffer text.
    pub fn accept_reverse_search(&mut self) -> String {
        if let Some(s) = self.search.take() {
            if s.match_idx.is_none() {
                self.buffer = std::mem::take(&mut self.draft);
                self.cursor = self.buffer.len();
            }
            self.draft.clear();
        }
        self.buffer.clone()
    }

    /// Cancel search: restore the pre-search draft and exit search mode.
    pub fn cancel_reverse_search(&mut self) {
        if self.search.take().is_some() {
            self.buffer = std::mem::take(&mut self.draft);
            self.cursor = self.buffer.len();
        }
    }
}

// ---------------------------------------------------------------------------
// Layout helpers — pure functions used by both the editor (for vertical
// navigation) and the renderer (for drawing the cursor at the right cell).
// ---------------------------------------------------------------------------

/// One visual line of wrapped input: byte slice + display width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualLine {
    /// Byte index in the underlying buffer where this visual line starts.
    pub byte_start: usize,
    /// Byte index where this visual line ends (exclusive).
    pub byte_end: usize,
}

/// Result of laying out a buffer for a given wrap width, with the
/// cursor's position mapped into the visual grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// One entry per visual line.
    pub lines: Vec<VisualLine>,
    /// Visual row (0-based) where the cursor sits.
    pub cursor_row: usize,
    /// Visual column (0-based, display width units) where the cursor sits.
    pub cursor_col: usize,
}

/// Wrap `buffer` to `width` columns (display-width aware) AND locate the
/// cursor. `cursor` is a byte index `<= buffer.len()` on a char boundary.
///
/// Rules:
/// - Explicit `\n` always ends a visual line.
/// - When `width == 0`, soft-wrap is disabled; only `\n` splits lines.
/// - Lines record byte ranges into `buffer`. The newline char itself is
///   **excluded** from the line slice; the next line begins after it.
/// - The cursor maps to the visual line that *contains* its byte index.
///   When the cursor sits exactly at a wrap or newline boundary, it
///   prefers the next line (so pressing `End` then typing extends the
///   freshly-wrapped row).
///
/// # Panics
///
/// Panics if `buffer` is not valid UTF-8 at an internal slice boundary
/// (unreachable for a well-formed `&str`, whose byte offsets always fall
/// on char boundaries).
#[must_use]
pub fn wrap_with_cursor(buffer: &str, width: usize, cursor: usize) -> Layout {
    let mut lines: Vec<VisualLine> = Vec::new();
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    let mut cursor_placed = false;

    let mut line_start = 0usize; // byte offset where current visual line starts
    let mut col_w = 0usize; // display width accumulated on current visual line

    let bytes = buffer.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Decode the next char.
        let ch = buffer[i..].chars().next().expect("char boundary");
        let ch_len = ch.len_utf8();

        // Place the cursor if it falls before this char.
        if !cursor_placed && cursor == i {
            cursor_row = lines.len();
            cursor_col = col_w;
            cursor_placed = true;
        }

        if ch == '\n' {
            // Hard break: emit current line up to `i`, advance past `\n`.
            lines.push(VisualLine {
                byte_start: line_start,
                byte_end: i,
            });
            line_start = i + ch_len;
            col_w = 0;
            i += ch_len;
            continue;
        }

        let w = UnicodeWidthChar::width(ch).unwrap_or(1);
        if width > 0 && col_w + w > width && line_start < i {
            // Soft wrap before this char.
            lines.push(VisualLine {
                byte_start: line_start,
                byte_end: i,
            });
            line_start = i;
            col_w = 0;
            // If the cursor sits exactly at this wrap byte, the top-of-loop
            // block placed it at the end of the line we just pushed. It really
            // belongs at column 0 of the new line — re-place it. (`cursor == i`
            // is false when cursor_placed came from an earlier position, so this
            // only fires for a cursor at the boundary.)
            if cursor_placed && cursor == i {
                cursor_row = lines.len();
                cursor_col = 0;
            }
        }
        col_w += w;
        i += ch_len;
    }

    // Tail: emit the final visual line.
    lines.push(VisualLine {
        byte_start: line_start,
        byte_end: bytes.len(),
    });

    // Handle the cursor when it lies at end-of-buffer.
    if !cursor_placed {
        cursor_row = lines.len() - 1;
        cursor_col = display_width(&buffer[line_start..]);
    }

    // Always at least one line — even for empty buffer.
    if lines.is_empty() {
        lines.push(VisualLine {
            byte_start: 0,
            byte_end: 0,
        });
    }

    Layout {
        lines,
        cursor_row,
        cursor_col,
    }
}

/// Display width (in cells) of `s`, summing unicode-width per char.
fn display_width(s: &str) -> usize {
    s.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(1)).sum()
}

/// Byte offset within `s` of the char whose display column is `>= col`.
/// Returns `s.len()` when `col` is past the end. Used to map a target
/// visual column onto a concrete cursor byte position on a different
/// visual line.
fn byte_at_display_col(s: &str, col: usize) -> usize {
    let mut accum = 0usize;
    for (i, c) in s.char_indices() {
        if accum >= col {
            return i;
        }
        accum += UnicodeWidthChar::width(c).unwrap_or(1);
    }
    s.len()
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn insert_at_cursor_in_middle() {
        let mut e = Editor::new();
        e.set_buffer("helo".into());
        e.cursor = 3; // between "hel" and "o"
        e.insert_char('l');
        assert_eq!(e.buffer(), "hello");
        assert_eq!(e.cursor(), 4);
    }

    fn seeded() -> Editor {
        let mut e = Editor::new();
        for s in [
            "cargo build",
            "cargo test --workspace",
            "git status",
            "cargo clippy",
        ] {
            e.push_history(s);
        }
        e
    }

    #[test]
    fn reverse_search_matches_cycles_and_accepts() {
        let mut e = seeded();
        e.set_buffer("draft text".into());
        assert!(e.start_reverse_search());
        assert!(e.reverse_search_active());
        // Typing "cargo" matches the NEWEST cargo entry first.
        e.reverse_search_push('c');
        e.reverse_search_push('a');
        e.reverse_search_push('r');
        e.reverse_search_push('g');
        e.reverse_search_push('o');
        assert_eq!(e.buffer(), "cargo clippy");
        // Ctrl-R again steps to the next older cargo match.
        e.reverse_search_again();
        assert_eq!(e.buffer(), "cargo test --workspace");
        e.reverse_search_again();
        assert_eq!(e.buffer(), "cargo build");
        // Accept keeps the match and exits search.
        let accepted = e.accept_reverse_search();
        assert_eq!(accepted, "cargo build");
        assert!(!e.reverse_search_active());
    }

    #[test]
    fn reverse_search_backspace_widens_and_cancel_restores_draft() {
        let mut e = seeded();
        e.set_buffer("my draft".into());
        e.start_reverse_search();
        e.reverse_search_push('g'); // matches "cargo clippy" (newest with 'g')
        e.reverse_search_push('i'); // "git status" is newest containing "gi"
        assert_eq!(e.buffer(), "git status");
        e.reverse_search_backspace(); // back to "g"
        assert_eq!(e.reverse_search_query(), Some("g"));
        // Cancel restores the pre-search draft.
        e.cancel_reverse_search();
        assert!(!e.reverse_search_active());
        assert_eq!(e.buffer(), "my draft");
    }

    #[test]
    fn reverse_search_no_match_restores_draft_on_accept() {
        let mut e = seeded();
        e.set_buffer("keep me".into());
        e.start_reverse_search();
        e.reverse_search_push('z'); // nothing matches
        assert_eq!(e.buffer(), "");
        let out = e.accept_reverse_search();
        assert_eq!(out, "keep me", "no match restores the draft");
    }

    #[test]
    fn backspace_at_cursor_in_middle() {
        let mut e = Editor::new();
        e.set_buffer("hello".into());
        e.cursor = 3; // between "hel" and "lo"
        e.backspace();
        assert_eq!(e.buffer(), "helo");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn backspace_at_zero_is_noop() {
        let mut e = Editor::new();
        e.set_buffer("hi".into());
        e.cursor = 0;
        e.backspace();
        assert_eq!(e.buffer(), "hi");
        assert_eq!(e.cursor(), 0);
    }

    #[test]
    fn delete_forward_at_cursor() {
        let mut e = Editor::new();
        e.set_buffer("hello".into());
        e.cursor = 2;
        e.delete();
        assert_eq!(e.buffer(), "helo");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn left_right_traverse_chars() {
        let mut e = Editor::new();
        e.set_buffer("abc".into());
        assert_eq!(e.cursor(), 3);
        e.move_left();
        assert_eq!(e.cursor(), 2);
        e.move_left();
        e.move_left();
        e.move_left(); // bounded
        assert_eq!(e.cursor(), 0);
        e.move_right();
        assert_eq!(e.cursor(), 1);
    }

    #[test]
    fn left_respects_utf8_boundaries() {
        let mut e = Editor::new();
        e.set_buffer("aé".into()); // 'é' is 2 bytes
        assert_eq!(e.cursor(), 3);
        e.move_left();
        // Should jump back the full 'é', not land mid-codepoint.
        assert_eq!(e.cursor(), 1);
    }

    // -- queued-message editing --------------------------------------------

    #[test]
    fn queue_fifo_pop_order() {
        let mut e = Editor::new();
        e.queue_message("first");
        e.queue_message("second");
        e.queue_message(""); // ignored
        assert_eq!(e.queued_len(), 2);
        assert_eq!(e.pop_queued().as_deref(), Some("first"));
        assert_eq!(e.pop_queued().as_deref(), Some("second"));
        assert_eq!(e.pop_queued(), None);
    }

    #[test]
    fn queue_edit_up_enters_newest_first_and_walks_older() {
        let mut e = Editor::new();
        e.queue_message("first");
        e.queue_message("second");
        e.set_buffer("draft".into());
        assert!(e.queue_edit_up(), "Up at top enters the queue");
        assert_eq!(e.buffer(), "second", "newest queued message first");
        assert!(e.editing_queue());
        assert!(e.queue_edit_up(), "Up again steps to the older message");
        assert_eq!(e.buffer(), "first");
        assert!(!e.queue_edit_up(), "at the oldest — no further");
    }

    #[test]
    fn queue_edit_commits_changes_back_into_slot() {
        let mut e = Editor::new();
        e.queue_message("fix the bug");
        e.set_buffer("draft".into());
        assert!(e.queue_edit_up());
        e.set_cursor_chars(e.buffer().chars().count());
        e.insert_str(" in main.rs");
        assert!(e.queue_edit_commit(), "Enter commits the edit");
        assert!(!e.editing_queue());
        assert_eq!(e.buffer(), "draft", "draft restored after commit");
        assert_eq!(e.pop_queued().as_deref(), Some("fix the bug in main.rs"));
    }

    #[test]
    fn queue_edit_down_commits_and_restores_draft() {
        let mut e = Editor::new();
        e.queue_message("alpha");
        e.queue_message("beta");
        e.set_buffer("draft".into());
        e.queue_edit_up(); // editing "beta"
        e.queue_edit_up(); // editing "alpha"
        e.insert_str("!");
        assert!(e.queue_edit_down(), "Down steps back to beta");
        assert_eq!(e.buffer(), "beta");
        assert!(e.queue_edit_down(), "Down past newest restores draft");
        assert_eq!(e.buffer(), "draft");
        assert!(!e.editing_queue());
        // The alpha edit stuck.
        assert_eq!(e.pop_queued().as_deref(), Some("alpha!"));
        assert_eq!(e.pop_queued().as_deref(), Some("beta"));
    }

    #[test]
    fn queue_edit_emptied_slot_is_dropped_on_commit() {
        let mut e = Editor::new();
        e.queue_message("delete me");
        e.queue_edit_up();
        // User selects-all + deletes: empty the buffer in place. (set_buffer
        // clears history-browsing state but queue editing survives it.)
        e.set_buffer(String::new());
        assert!(e.editing_queue(), "queue editing survives a buffer replace");
        assert!(e.queue_edit_commit());
        assert_eq!(e.queued_len(), 0, "emptied slot dropped");
        assert_eq!(e.pop_queued(), None);
    }

    #[test]
    fn pop_queued_while_editing_slot_zero_takes_live_buffer() {
        let mut e = Editor::new();
        e.queue_message("original");
        e.set_buffer("draft".into());
        e.queue_edit_up(); // editing "original" (slot 0)
        e.insert_str(" amended");
        // Turn ends mid-edit: the drain must take the user's live edits.
        assert_eq!(e.pop_queued().as_deref(), Some("original amended"));
        assert!(!e.editing_queue());
        assert_eq!(e.buffer(), "draft", "draft restored");
    }

    #[test]
    fn pop_queued_while_editing_later_slot_shifts_index() {
        let mut e = Editor::new();
        e.queue_message("first");
        e.queue_message("second");
        e.set_buffer("draft".into());
        e.queue_edit_up(); // editing "second" (slot 1)
        assert_eq!(e.pop_queued().as_deref(), Some("first"));
        assert!(e.editing_queue(), "still editing after the pop");
        e.insert_str(" edited");
        assert!(e.queue_edit_commit());
        assert_eq!(e.pop_queued().as_deref(), Some("second edited"));
    }

    #[test]
    fn queue_preferred_over_history_on_up() {
        // With both history and a queue, Up at the top edits the queue.
        let mut e = Editor::new();
        e.push_history("old prompt");
        e.queue_message("queued msg");
        assert!(e.queue_edit_up());
        assert_eq!(e.buffer(), "queued msg");
        // Queue empty → falls back to history (caller's responsibility,
        // mirrored in input::reduce_editor).
        let mut e2 = Editor::new();
        e2.push_history("old prompt");
        assert!(!e2.queue_edit_up());
        assert!(e2.history_up());
        assert_eq!(e2.buffer(), "old prompt");
    }

    #[test]
    fn wrap_empty_yields_one_line() {
        let l = wrap_with_cursor("", 10, 0);
        assert_eq!(l.lines.len(), 1);
        assert_eq!(l.cursor_row, 0);
        assert_eq!(l.cursor_col, 0);
    }

    #[test]
    fn wrap_splits_on_newline_and_locates_cursor() {
        // "abc\ndef" with cursor at byte 5 (between 'd' and 'e').
        let l = wrap_with_cursor("abc\ndef", 80, 5);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(
            l.lines[0],
            VisualLine {
                byte_start: 0,
                byte_end: 3
            }
        );
        assert_eq!(
            l.lines[1],
            VisualLine {
                byte_start: 4,
                byte_end: 7
            }
        );
        assert_eq!(l.cursor_row, 1);
        assert_eq!(l.cursor_col, 1);
    }

    #[test]
    fn wrap_soft_breaks_at_width() {
        let l = wrap_with_cursor("abcdefghij", 5, 7);
        assert_eq!(l.lines.len(), 2);
        // First five chars on row 0, rest on row 1.
        assert_eq!(
            l.lines[0],
            VisualLine {
                byte_start: 0,
                byte_end: 5
            }
        );
        assert_eq!(
            l.lines[1],
            VisualLine {
                byte_start: 5,
                byte_end: 10
            }
        );
        assert_eq!(l.cursor_row, 1);
        assert_eq!(l.cursor_col, 2);
    }

    #[test]
    fn wrap_cursor_exactly_at_boundary_lands_on_new_line() {
        // Cursor at byte 5 sits exactly at the soft-wrap point. It must render
        // at the start of the second visual line, not the end of the first.
        let l = wrap_with_cursor("abcdefghij", 5, 5);
        assert_eq!(l.lines.len(), 2);
        assert_eq!(l.cursor_row, 1);
        assert_eq!(l.cursor_col, 0);
    }

    #[test]
    fn move_up_down_navigates_visual_lines() {
        let mut e = Editor::new();
        e.set_buffer("abcdef\nghi".into()); // 6 chars + \n + 3 chars
        e.cursor = 9; // on second line, between 'gh' and 'i'
        assert!(e.move_up_visual(80));
        // Target col was 2 → cursor lands at byte 2 on first line.
        assert_eq!(e.cursor(), 2);
        assert!(e.move_down_visual(80));
        assert_eq!(e.cursor(), 9);
    }

    #[test]
    fn move_up_at_top_returns_false() {
        let mut e = Editor::new();
        e.set_buffer("hello".into());
        e.cursor = 2;
        assert!(!e.move_up_visual(80));
    }

    #[test]
    fn move_down_at_bottom_returns_false() {
        let mut e = Editor::new();
        e.set_buffer("hello".into());
        e.cursor = 5;
        assert!(!e.move_down_visual(80));
    }

    #[test]
    fn history_up_recalls_last_submission() {
        let mut e = Editor::new();
        e.push_history("first message");
        e.push_history("second message");
        // Draft typing.
        e.insert_str("draft");
        assert!(e.history_up());
        assert_eq!(e.buffer(), "second message");
        assert!(e.history_up());
        assert_eq!(e.buffer(), "first message");
        assert!(!e.history_up()); // bounded
    }

    #[test]
    fn history_down_restores_draft() {
        let mut e = Editor::new();
        e.push_history("first");
        e.insert_str("my draft");
        e.history_up();
        assert_eq!(e.buffer(), "first");
        assert!(e.history_down());
        assert_eq!(e.buffer(), "my draft");
        assert!(!e.history_down()); // no further newer entry
    }

    #[test]
    fn home_jumps_to_visual_line_start() {
        let mut e = Editor::new();
        e.set_buffer("abc\ndefgh".into());
        e.cursor = 7; // mid second line
        e.move_home(80);
        assert_eq!(e.cursor(), 4); // start of second visual line
    }

    #[test]
    fn end_jumps_to_visual_line_end() {
        let mut e = Editor::new();
        e.set_buffer("abc\ndefgh".into());
        e.cursor = 5;
        e.move_end(80);
        assert_eq!(e.cursor(), 9);
    }
}
