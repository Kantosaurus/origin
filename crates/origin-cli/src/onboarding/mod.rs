// SPDX-License-Identifier: Apache-2.0
//! Interactive `origin init` onboarding TUI.
//!
//! Reuses the existing onboarding *logic* in [`crate::init`] (catalog,
//! credential capture, `/models` probe, config persistence) and changes only
//! the *interaction*: instead of a numbered line-based wizard, a TTY gets a
//! crossterm raw-mode picker (banner → search → providers → auth → model with
//! context size). When stdin is not a TTY the line-based wizard
//! ([`crate::init::run_with`]) still runs unchanged, so CI and the existing
//! scripted tests are untouched.
//!
//! Decomposition:
//! - [`picker`] — a pure, terminal-free search + cursor reducer (unit-tested).
//! - [`flow`]   — brand grouping + row builders + the per-role async driver
//!   that wires the picker to the reused credential/probe/config helpers.
//! - [`screen`] — the thin crossterm raw-mode shell (RAII terminal restore,
//!   one-frame render into a [`origin_tui::Grid`], masked text field).

pub mod flow;
pub mod picker;
pub mod screen;

pub use flow::run_interactive;
