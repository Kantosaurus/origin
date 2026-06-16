// SPDX-License-Identifier: Apache-2.0
//! Persistent chrome: the top context strip + the bottom status zone.
//!
//! Wave 0 stub: locks `ChromeCtx`/`StatusCtx` and the `draw_top`/`draw_status`
//! signatures. Wave 1 (Task D) fills the bodies. Field names are derived from
//! the live `App`/`UsageSnapshot` state the painter reads (model, cwd, branch,
//! session clock, context-fill, spinner/phase/tokens/cost).

// The stub bodies look const-foldable now; Wave 1 fills them with real
// (non-const) layout logic, so they are deliberately left non-const.
#![allow(dead_code, clippy::missing_const_for_fn)] // Wave 1 fills this

use origin_tui::grid::Grid;

use super::tokens::{Region, Tokens};

/// The data the top context strip renders: wordmark context (model · cwd ·
/// branch) on the left, session clock + context-fill on the right.
#[derive(Debug, Clone, Default)]
pub struct ChromeCtx {
    /// Active model name (from `UsageSnapshot::model`).
    pub model: String,
    /// Current working directory, truncated middle on narrow widths.
    pub cwd: String,
    /// Git branch, if resolvable (`⎇ branch`); `None` outside a repo.
    pub branch: Option<String>,
    /// Pre-formatted session clock (e.g. `1m 04s`), from `usage.elapsed` plus
    /// any in-flight `turn_started`.
    pub elapsed: String,
    /// Context-window fill percentage (0–100), colorized warn/err past
    /// thresholds. Derived from `last_ctx_tokens` / `context_window_for(model)`.
    pub ctx_pct: u8,
}

/// The data the bottom status zone renders: the quiet spinner/phase/metrics
/// line above the composer's rule.
#[derive(Debug, Clone, Default)]
pub struct StatusCtx {
    /// Current spinner frame while a turn is in flight; `None` when idle.
    pub spinner: Option<String>,
    /// A short phase label (e.g. `thinking`, goal status), if any.
    pub phase: Option<String>,
    /// Total tokens this session (input + output), for the metrics readout.
    pub tokens: u32,
    /// Session cost in USD, if cost tracking is on.
    pub cost: Option<f64>,
    /// Whether a turn is currently in flight (dims/animates the zone).
    pub in_flight: bool,
}

/// Paint the persistent top context strip into `region`.
pub fn draw_top(_grid: &mut Grid, _region: Region, _ctx: &ChromeCtx, _tok: &Tokens) {
    // Wave 1 (Task D) fills this.
}

/// Paint the bottom status zone into `region` (above the composer rule).
pub fn draw_status(_grid: &mut Grid, _region: Region, _st: &StatusCtx, _tok: &Tokens) {
    // Wave 1 (Task D) fills this.
}
