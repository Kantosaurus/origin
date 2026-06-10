// SPDX-License-Identifier: Apache-2.0
//! `origin-ui-preview` — hot-reload terminal preview of the origin harness UI.
//!
//! Renders the "Burnished Copper" identity (and the other `/theme` presets)
//! as palette swatches plus a fake transcript, so design changes to
//! `origin-cli/src/theme.rs` / `ansi.rs` can be eyeballed instantly without
//! launching the full TUI or a daemon.
//!
//! The theme sources are included via `#[path]`, **not** via a dependency on
//! `origin-cli`, so the edit → rebuild → render loop compiles two files
//! instead of the entire harness.
//!
//! Usage:
//!   origin-ui-preview              # all themes
//!   origin-ui-preview dark         # one theme
//!   origin-ui-preview --swatches   # palette grid only
//!   origin-ui-preview --transcript # mock transcript only
//!
//! Hot reload (pick one):
//!   cargo watch -x 'run -p origin-ui-preview'        # if cargo-watch installed
//!   bacon run -- -p origin-ui-preview                # if bacon installed
//!   ./scripts/ui-preview-watch.ps1                   # zero-install fallback

// The included modules are libraries for origin-cli; the preview only
// exercises a subset, and `ansi.rs` carries no `#[cfg(test)]` gates here.
#![allow(dead_code)]

#[path = "../../origin-cli/src/theme.rs"]
mod theme;

#[path = "../../origin-cli/src/ansi.rs"]
mod ansi;

use theme::{palette, Palette, Theme};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";

fn fg(c: u32) -> String {
    if c == 0 {
        return String::new();
    }
    format!("\x1b[38;2;{};{};{}m", (c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
}

fn bg(c: u32) -> String {
    if c == 0 {
        return String::new();
    }
    format!("\x1b[48;2;{};{};{}m", (c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
}

fn hex(c: u32) -> String {
    format!("#{:02X}{:02X}{:02X}", (c >> 16) & 0xFF, (c >> 8) & 0xFF, c & 0xFF)
}

/// One labelled swatch: a colored block, the field name, and the hex value.
fn swatch(name: &str, c: u32) -> String {
    format!("{}      {RESET} {}{name:<14}{RESET} {}", bg(c), fg(c), hex(c))
}

fn print_swatch_grid(p: &Palette) {
    let entries: [(&str, u32); 22] = [
        ("surface", p.surface),
        ("surface_raised", p.surface_raised),
        ("border", p.border),
        ("muted", p.muted),
        ("body", p.body),
        ("bright", p.bright),
        ("accent", p.accent),
        ("accent_dim", p.accent_dim),
        ("user", p.user),
        ("tool", p.tool),
        ("code_fg", p.code_fg),
        ("code_bg", p.code_bg),
        ("green", p.green),
        ("yellow", p.yellow),
        ("red", p.red),
        ("dim", p.dim),
        ("rule", p.rule),
        ("panel_header", p.panel_header),
        ("panel_bg", p.panel_bg),
        ("h1", p.h1),
        ("h2", p.h2),
        ("h3", p.h3),
    ];
    // Two columns to keep it compact on an 80-col terminal.
    for pair in entries.chunks(2) {
        let left = swatch(pair[0].0, pair[0].1);
        let right = pair.get(1).map_or(String::new(), |(n, c)| swatch(n, *c));
        println!("  {left}   {right}");
    }
}

/// A mock transcript that exercises every visual role the real TUI uses:
/// headings, user/assistant turns, tool activity, a code block, a diff,
/// status colors, and the panel chrome.
fn print_transcript(p: &Palette) {
    let rule: String = "\u{2500}".repeat(64);

    println!("  {}{BOLD}# Heading 1 — release notes{RESET}", fg(p.h1));
    println!("  {}{BOLD}## Heading 2 — what changed{RESET}", fg(p.h2));
    println!("  {}### Heading 3 — details{RESET}", fg(p.h3));
    println!();
    println!("  {}{BOLD}\u{276F}{RESET} {}refactor the damage diff to use SIMD{RESET}", fg(p.accent), fg(p.user));
    println!();
    println!("  {}Sure — I'll start by reading the current implementation, then{RESET}", fg(p.body));
    println!("  {}rewrite the inner loop with `wide::u8x32` lanes.{RESET}", fg(p.body));
    println!();
    println!("  {}\u{25CF} Read{RESET} {}crates/origin-tui/src/damage.rs{RESET}", fg(p.tool), fg(p.muted));
    println!("  {}  \u{2514} 412 lines{RESET}", fg(p.dim));
    println!("  {}\u{25CF} Edit{RESET} {}crates/origin-tui/src/damage.rs{RESET}", fg(p.tool), fg(p.muted));
    println!();
    // Code block on its own background.
    let code_lines = [
        "let lanes = u8x32::from_slice(&row[i..i + 32]);",
        "let mask = lanes.cmp_eq(prev).to_bitmask();",
        "if mask != u32::MAX { dirty.push(i); }",
    ];
    for line in code_lines {
        println!("  {}{}{line:<60}{RESET}", bg(p.code_bg), fg(p.code_fg));
    }
    println!();
    // Diff rows.
    println!("  {}{}- if row[i] != prev[i] {{ dirty.push(i); }}                   {RESET}", bg(theme::DIFF_DEL_BG), fg(theme::DIFF_DEL_FG));
    println!("  {}{}+ if mask != u32::MAX {{ dirty.push(i); }}                    {RESET}", bg(theme::DIFF_ADD_BG), fg(theme::DIFF_ADD_FG));
    println!();
    // Status line states.
    println!("  {}\u{2713} 42 tests passed{RESET}   {}\u{26A0} 3 warnings{RESET}   {}\u{2717} 1 failure{RESET}", fg(p.green), fg(p.yellow), fg(p.red));
    println!();
    // Panel chrome.
    println!("  {}{rule}{RESET}", fg(p.rule));
    println!("  {}{}{BOLD} PLAN {RESET}{}{}  1. read damage.rs   2. SIMD inner loop   3. bench       {RESET}", bg(p.panel_bg), fg(p.panel_header), bg(p.panel_bg), fg(p.body));
    println!("  {}{rule}{RESET}", fg(p.rule));
    println!();
    println!("  {}{}{BOLD} input \u{2502} {RESET}{}{} type a message\u{2026}                                      {RESET}", bg(p.surface_raised), fg(p.accent), bg(p.surface_raised), fg(p.muted));
}

/// The pre-TUI chrome (onboarding / init screens) from `ansi.rs`, which is
/// hard-wired to the Default theme constants.
fn print_ansi_helpers() {
    println!("  {}", ansi::heading("origin — first run"));
    println!("  {}", ansi::section_rule(64));
    println!("  {} {}", ansi::step_number(1, 3), ansi::bright("Pick a provider"));
    println!("    {} {}", ansi::prompt_arrow(), ansi::highlight_row(" anthropic (recommended) "));
    println!("      {}", ansi::muted("openai"));
    println!("      {}", ansi::muted("ollama (local)"));
    println!("  {} {}", ansi::step_number(2, 3), ansi::bright("Port skills"));
    println!("    {} {}", ansi::green("\u{2713}"), ansi::accent("3 skills found in ~/.claude/skills"));
    println!("  {} {}", ansi::step_number(3, 3), ansi::bright("Verify"));
    println!("    {}  {}  {}", ansi::green("ok: keyvault"), ansi::yellow("warn: no GPU"), ansi::red("err: none"));
}

fn render_theme(t: Theme, swatches: bool, transcript: bool) {
    let p = palette(t);
    println!();
    println!("\u{2554}{}\u{2557}", "\u{2550}".repeat(68));
    println!("\u{2551} {BOLD}theme: {:<60}{RESET}\u{2551}", t.name());
    println!("\u{255A}{}\u{255D}", "\u{2550}".repeat(68));
    if swatches {
        println!();
        print_swatch_grid(&p);
    }
    if transcript {
        println!();
        print_transcript(&p);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut swatches = true;
    let mut transcript = true;
    let mut themes: Vec<Theme> = Vec::new();

    for a in &args {
        match a.as_str() {
            "--swatches" => transcript = false,
            "--transcript" => swatches = false,
            other => {
                if let Some(t) = Theme::parse(other) {
                    themes.push(t);
                } else {
                    eprintln!("unknown theme or flag: {other}");
                    eprintln!("usage: origin-ui-preview [default|dark|light|high-contrast] [--swatches|--transcript]");
                    std::process::exit(2);
                }
            }
        }
    }

    if themes.is_empty() {
        themes = vec![Theme::Default, Theme::Dark, Theme::Light, Theme::HighContrast];
    }

    // Clear + home so a watch loop re-render reads like a live preview.
    print!("\x1b[2J\x1b[H");

    for t in themes {
        render_theme(t, swatches, transcript);
    }

    // The ansi.rs helpers are Default-theme only; show them once.
    println!();
    println!("\u{2554}{}\u{2557}", "\u{2550}".repeat(68));
    println!("\u{2551} {BOLD}{:<67}{RESET}\u{2551}", "pre-TUI chrome (ansi.rs — onboarding/init)");
    println!("\u{255A}{}\u{255D}", "\u{2550}".repeat(68));
    println!();
    print_ansi_helpers();
    println!();
}
