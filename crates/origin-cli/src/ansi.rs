// SPDX-License-Identifier: Apache-2.0
//! ANSI escape helpers for pre-TUI terminal output (onboarding, init).
//!
//! These use the same RGB values as `theme.rs` so the onboarding screens
//! feel like they belong to the same product as the TUI.

use crate::theme;

fn rgb_fg(color: u32) -> String {
    let r = (color >> 16) & 0xFF;
    let g = (color >> 8) & 0xFF;
    let b = color & 0xFF;
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn rgb_bg(color: u32) -> String {
    let r = (color >> 16) & 0xFF;
    let g = (color >> 8) & 0xFF;
    let b = color & 0xFF;
    format!("\x1b[48;2;{r};{g};{b}m")
}

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";

#[must_use]
pub fn accent(s: &str) -> String {
    format!("{}{s}{RESET}", rgb_fg(theme::ACCENT))
}

#[must_use]
pub fn bright(s: &str) -> String {
    format!("{}{BOLD}{s}{RESET}", rgb_fg(theme::BRIGHT))
}

#[must_use]
pub fn muted(s: &str) -> String {
    format!("{}{DIM}{s}{RESET}", rgb_fg(theme::MUTED))
}

#[must_use]
pub fn green(s: &str) -> String {
    format!("{}{s}{RESET}", rgb_fg(theme::GREEN))
}

#[must_use]
pub fn red(s: &str) -> String {
    format!("{}{s}{RESET}", rgb_fg(theme::RED))
}

#[must_use]
pub fn yellow(s: &str) -> String {
    format!("{}{s}{RESET}", rgb_fg(theme::YELLOW))
}

#[must_use]
pub fn heading(s: &str) -> String {
    format!("{}{BOLD}{s}{RESET}", rgb_fg(theme::ACCENT))
}

#[must_use]
pub fn prompt_arrow() -> String {
    format!("{}{BOLD}\u{276F}{RESET}", rgb_fg(theme::ACCENT))
}

#[must_use]
pub fn highlight_row(s: &str) -> String {
    format!(
        "{}{}{BOLD}{s}{RESET}",
        rgb_bg(theme::SURFACE),
        rgb_fg(theme::BRIGHT)
    )
}

#[must_use]
pub fn section_rule(width: usize) -> String {
    let line: String = "\u{2500}".repeat(width);
    format!("{}{line}{RESET}", rgb_fg(theme::BORDER))
}

#[must_use]
pub fn step_number(n: usize, total: usize) -> String {
    format!("{}{DIM}[{n}/{total}]{RESET}", rgb_fg(theme::MUTED))
}
