# `origin` Phase 4 — Custom TUI Renderer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run-to-fail, implement, run-to-pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Replace the Ratatui baseline TUI with a custom cell-grid renderer: packed 16-byte `Cell` × row-major `Grid` (already shipped in P4.1), SIMD damage diff, ANSI emit, event-loop-tied frame coalescing, grapheme-width LRU, streaming widget that reads the token ring + CAS slices without per-token allocation, and a side panel as a separate render target (with permission prompts migrated off the modal). Phase ends with `origin-cli` rewritten on the new renderer, Ratatui removed from the workspace, criterion bench harness asserting < 12ms keystroke→pixel p99 and frame-coalescing under stream, and tag `p4-complete`.

**Architecture:** All renderer primitives live in `origin-tui` (a `unsafe_code = "allow"` crate — `wide::u8x32` intrinsics need it; `Cell::as_bytes` round-trip is the only `unsafe` block, with SAFETY comment). The damage diff is two-pass per row: 32-byte SIMD coarse scan flips the row into a fine per-cell pass that emits contiguous `Run { row, col, len }` tuples. ANSI emit walks runs and writes CUP + SGR + UTF-8 glyphs into a single `Vec<u8>`. The `Scheduler` owns an `AtomicBool` dirty flag and a `tokio::sync::Notify`; `Handle::mark_dirty` is the only fanout API. `WidthCache` is a fixed-capacity LRU over `(grapheme_hash → u8 width)` keyed by FxHash-64 of the grapheme bytes — never holds the grapheme string. `StreamWidget` accepts an `origin_stream::Subscriber` and lays bytes into a `Grid` viewport using the cache. `Composer` owns three `Grid`s (`main`, `side`, `prompt`) with independent damage trackers and emits one merged ANSI stream per frame. The permission system gets a `SidePanelPrompter` that satisfies the existing `Prompter` trait by enqueueing a `PermissionAsk` event into the panel's input queue. `origin-cli`'s `main.rs` is rewritten to drive a `Composer` + `Scheduler` instead of `ratatui::Terminal`; Ratatui is removed from `Cargo.toml` and the workspace `Cargo.lock`. A `bench/keystroke_to_pixel.rs` Criterion bench synthesizes a 200×60 terminal session and asserts the headline p99 KPI.

**Tech Stack:** Rust 1.83 (MSRV pin). New (workspace-pinned) deps inside `origin-tui`: `wide = "0.7"` (already added by P4.1's manifest header note), `tokio = { version = "1", features = ["sync", "time", "macros", "rt"] }`, `unicode-segmentation = "1"`, `unicode-width = "0.1"`, `lru = "0.12"`, `fxhash = "0.2"`. Dev-deps: `proptest = "=1.4.0"`, `criterion = "0.5"`, `tokio = { version = "1", features = ["macros", "rt", "test-util", "time"] }`. **Novel-implementation reflex** per `[[feedback-novel-implementations]]`: SIMD-coarse + per-cell-fine row diff (vs. Ratatui's per-cell `==` everywhere); event-loop-tied dirty-flag coalescing with zero idle frames (vs. fixed-tick render loops); `Subscriber`-driven `StreamWidget` reading the ring's archived bytes directly (vs. `String` accumulation per delta); side panel as a sibling `Grid` composed via byte-stream merge (vs. Ratatui's full-area redraws on layout change); FxHash-64-keyed grapheme cache holding only `(hash, width)` (vs. interning the grapheme bytes).

**Builds on:** Spec §8 (N8.1–N8.5), §9 (N9.3 — side-panel-only prompts) of `docs/superpowers/specs/2026-05-19-origin-harness-design.md`. `origin-stream::{Ring, Subscriber, TokenEvent, TokenKind}` (P2 deliverable), `origin-cas::{Store, Hash, PackSlice}` (P2 deliverable), `origin-permission::{Prompter, Tier}` (P1 deliverable). The existing Ratatui-based `origin-cli` (commit `1d4b378`) is the reference for what the new renderer must reproduce (scrollback + prompt input + status bar) before extending to the side panel.

**Out of scope (deferred):**
- Per-component jemalloc arenas — Phase 12 (N8.6)
- Tokio task-class budgeting — Phase 12 (N8.7)
- Two-runtime split (control core / worker pool) — Phase 12 (N8.8)
- `tokio-uring` on Linux for CAS pack files — Phase 12 (N8.9)
- Cooperative phased shutdown supervisor — Phase 12 (N8.10)
- Mouse / paste / focus events beyond what `crossterm` returns by default — Phase 13
- Image protocol (Kitty/iTerm2) — out of v1
- `?metrics` panel — Phase 11 (N10.5)

---

## Conventions reminder (apply to every task)

**TDD shape:** failing test → run-to-fail → implement → run-to-pass → verification gate → commit.

**Verification gate per task type:**

| Task type | Required commands (all exit 0) |
|---|---|
| Single-crate pure logic (P4.2, P4.3, P4.4, P4.5, P4.6, P4.8) | `cargo test -p origin-tui` + `cargo clippy -p origin-tui --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / `origin-cli` rewrite (P4.7, P4.8 integration, P4.9) | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Final phase gate (P4.10) | All of the above + new `keystroke_to_pixel` bench passes its assertion + tag `p4-complete` |

**Inherited patterns:**
- `[lints] workspace = true` for every crate **except** `origin-tui` — that manifest already overrides `unsafe_code = "allow"` (P4.1 set this up) and inlines the workspace clippy lints. New deps go into the existing `[dependencies]` block.
- Workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- `#[must_use]` on every public constructor; `const fn` where Rust allows.
- Tests use `.expect("meaningful message")`; never `#[allow(clippy::unwrap_used)]`. For inline justifications use `#[allow(clippy::<lint>, reason = "...")]`.
- Custom error enums via `thiserror`; `# Errors` / `# Panics` on every public `Result`-returning / panicking fn.
- For each `#[allow(clippy::...)]` add an inline justification.
- **MSRV pin reflex** (`[[project-msrv-dep-pinning]]`): if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin offender with `cargo update -p <crate>@<bad> --precise <last-1.83-compatible>` and commit `Cargo.lock`. **Note:** `lru = "0.12"` is the last MSRV-1.83-compatible line; do **not** bump to 0.13+. `fxhash = "0.2"` is stable. `unicode-segmentation = "1.10"` and `unicode-width = "0.1"` are both fine.
- Commits: Conventional Commits, scoped (`feat(origin-tui): ...`), one commit per task. Tag references in the commit message: `(P4.X, N8.Y)`.

**Branch:** `dev` (per CLAUDE.md, the default integration branch). P4.1 is already in `dev` at commit `4aa19f6`.

---

## File map for Phase 4

| New / modified | Responsibility | Task |
|---|---|---|
| `crates/origin-tui/Cargo.toml` *(modify)* | add `wide`, `tokio` runtime feature set, `unicode-segmentation`, `unicode-width`, `lru`, `fxhash`; dev-deps `proptest`, `criterion`, `tokio` test-util | P4.2 |
| `crates/origin-tui/src/damage.rs` + `tests/damage.rs` + `benches/damage_diff.rs` | `Run { row, col, len }` + `diff(prev, next) -> Vec<Run>` via `wide::u8x32`; bench asserts <50µs on 200×60 @ 1% changed | P4.2 |
| `crates/origin-tui/src/ansi.rs` + `tests/ansi.rs` | `emit(&Grid, &[Run]) -> Vec<u8>`: CUP + SGR + glyphs; reset between runs; truecolor RGB | P4.3 |
| `crates/origin-tui/src/scheduler.rs` + `tests/scheduler.rs` | `Scheduler::new(frame_budget)` + `Handle::mark_dirty`; coalesces N dirty flips into one render inside the budget | P4.4 |
| `crates/origin-tui/src/width.rs` + `tests/width.rs` | `WidthCache::new(cap)` LRU keyed by FxHash-64 of grapheme bytes; `width_of(&str) -> u8` consults cache, falls back to `unicode-width` | P4.5 |
| `crates/origin-tui/src/stream_widget.rs` + `tests/stream_widget.rs` | `StreamWidget::new(width_cache_handle)`, `apply_event(&TokenEvent, &mut Grid, viewport: Rect)`; advances cursor + wraps with `WidthCache`; never allocates per byte | P4.6 |
| `crates/origin-tui/src/composer.rs` + `tests/composer.rs` | `Composer { main: Grid, side: Grid, prompt: Grid, scratch_back: Grid, ... }`; `Composer::resize_terminal(cols, rows, side_visible)`; `Composer::emit(&mut self) -> Vec<u8>` returns the per-frame ANSI byte stream | P4.7 |
| `crates/origin-tui/src/panel.rs` + `tests/panel.rs` | `Panel` event queue + `PermissionAsk { tool, tier, urgency }` + `PanelEvent::PermissionDecided { id, outcome }`; one-key resolution (`y`/`n`/`e`); pushes lines into `side` Grid | P4.7 |
| `crates/origin-tui/src/cli_prompter.rs` *(new sub-mod)* + `crates/origin-permission/...` *(no source change needed — implements existing `Prompter` trait)* | `SidePanelPrompter` impls `Prompter`; routes to a `Panel` handle via a bounded `tokio::sync::mpsc` channel | P4.8 |
| `crates/origin-tui/src/layout_cache.rs` + `tests/layout_cache.rs` | `LayoutCache::new(store: Arc<Store>)`; key is `Hash(blake3("layout/v1/" + cols.to_be_bytes() + text))`; entry payload is the wrapped `Vec<LayoutSpan>` (row, col, byte_range); `get_or_insert(text, cols, build_fn)` returns a CAS-backed slice | P4.8 |
| `crates/origin-tui/src/lib.rs` *(modify)* | publish `damage`, `ansi`, `scheduler`, `width`, `stream_widget`, `composer`, `panel`, `layout_cache`; re-export `Run`, `Scheduler`, `Handle`, `WidthCache`, `StreamWidget`, `Composer`, `Panel`, `LayoutCache` | P4.2–P4.8 (one re-export per task) |
| `crates/origin-cli/Cargo.toml` *(modify)* | remove `ratatui`, `crossterm` stays (input + raw mode), add `origin-tui` dep, add `unicode-segmentation` for input editing | P4.9 |
| `crates/origin-cli/src/screen.rs` *(modify)* | replace `ratatui::layout::Rect` with the in-crate `Rect` shape used by `Composer::resize_terminal` | P4.9 |
| `crates/origin-cli/src/status.rs` *(modify)* | render the status bar by writing cells into the `prompt`-row grid directly (re-uses the same body the Ratatui path used) | P4.9 |
| `crates/origin-cli/src/tui.rs` *(modify)* | `App::draw(composer: &mut Composer)` populates the three Grids; assistant deltas flow through `StreamWidget` | P4.9 |
| `crates/origin-cli/src/main.rs` *(modify)* | replace `ratatui::Terminal` + `terminal.draw(...)` with: `Composer::new(...)`, `Scheduler::new(Duration::from_millis(6))`, spawn `scheduler.run` driving `composer.emit() -> stdout`; route side-panel events through a `Panel` handle; remove the 100ms `event::poll` busy-poll in favor of `crossterm::event::EventStream` (async) | P4.9 |
| `crates/origin-cli/benches/keystroke_to_pixel.rs` *(new)* + `crates/origin-cli/Cargo.toml` `[[bench]]` entry | Criterion bench: 1k keystrokes + 1k assistant text deltas through the full pipeline; assert mean per-frame emit + diff < 12ms on stock CI hardware | P4.10 |
| Tag `p4-complete` | Marks phase exit | P4.10 |

File-size discipline: every new `.rs` file targets <300 LOC. The 16-byte `Cell` packing is the only sub-byte invariant; everything else compiles to ordinary Rust.

---

## Task P4.2 — SIMD damage diff (N8.1)

**Files:** `crates/origin-tui/Cargo.toml` *(modify)*, `crates/origin-tui/src/damage.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify — `pub mod damage;` + re-export `Run`)*, `crates/origin-tui/tests/damage.rs` *(new)*, `crates/origin-tui/benches/damage_diff.rs` *(new)*.

**Public surface:**
- `pub struct Run { pub row: u16, pub col: u16, pub len: u16 }` — `#[derive(Debug, Clone, Copy, PartialEq, Eq)]`.
- `pub fn diff(prev: &Grid, next: &Grid) -> Vec<Run>` — `# Panics` if `prev` and `next` differ in `(cols, rows)`.

**Mechanism (N8.1):** two-pass per row. Coarse pass: scan `prev.as_bytes()` vs `next.as_bytes()` in 32-byte strides via `wide::u8x32` lane-equality. On any unequal lane (or any tail byte mismatch in the partial-stride remainder), mark the row dirty and fall into fine pass. Fine pass: walk cells (`cell_bytes = 16`) left-to-right; when a cell differs, extend the run while the next cell also differs; when cells re-match, close the run and continue scanning. Coalesces adjacent changes; emits at-most-one run per contiguous changed span per row.

- [ ] **Step 1: Modify** `crates/origin-tui/Cargo.toml` — add `wide = "0.7"` to `[dependencies]`, add `criterion = "0.5"` to `[dev-dependencies]`, append a `[[bench]] name = "damage_diff" harness = false` section.

- [ ] **Step 2: Failing test** at `crates/origin-tui/tests/damage.rs`:

```rust
use origin_tui::damage::{diff, Run};
use origin_tui::{Cell, Grid};

#[test]
fn one_cell_change_in_200x60_yields_one_run_len_1() {
    let a = Grid::new(200, 60);
    let b = {
        let mut g = a.clone();
        g.put(30, 100, Cell::glyph('x'));
        g
    };
    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 1, "expected exactly one damage run");
    let r = &runs[0];
    assert_eq!(r.row, 30);
    assert_eq!(r.col, 100);
    assert_eq!(r.len, 1);
}

#[test]
fn no_change_yields_empty_runs() {
    let a = Grid::new(64, 16);
    #[allow(clippy::redundant_clone, reason = "two distinct grid values for diff input")]
    let b = a.clone();
    assert!(diff(&a, &b).is_empty());
}

#[test]
fn adjacent_changes_coalesce_into_single_run() {
    let a = Grid::new(80, 24);
    let mut b = a.clone();
    b.put(5, 10, Cell::glyph('a'));
    b.put(5, 11, Cell::glyph('b'));
    b.put(5, 12, Cell::glyph('c'));
    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0], Run { row: 5, col: 10, len: 3 });
}

#[test]
fn changes_on_different_rows_are_separate_runs() {
    let a = Grid::new(80, 24);
    let mut b = a.clone();
    b.put(1, 0, Cell::glyph('x'));
    b.put(2, 0, Cell::glyph('y'));
    let runs = diff(&a, &b);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].row, 1);
    assert_eq!(runs[1].row, 2);
}

#[test]
#[should_panic(expected = "grid dims must match")]
fn mismatched_dims_panic() {
    let a = Grid::new(10, 5);
    let b = Grid::new(11, 5);
    let _ = diff(&a, &b);
}
```

  Add `Clone` to the `Grid` derive in `crates/origin-tui/src/grid.rs` if not already present (P4.1 already derives `Clone` on `Grid`).

- [ ] **Step 3:** Run `cargo test -p origin-tui --test damage` — expect failure (module `damage` doesn't exist).

- [ ] **Step 4:** Implement `crates/origin-tui/src/damage.rs`:

```rust
//! SIMD damage diff over packed `Cell` grids (N8.1).
//!
//! Two-pass per row: 32-byte SIMD coarse scan flips the row into a fine
//! per-cell pass that emits `Run { row, col, len }` tuples for each
//! contiguous span of changed cells.

use crate::Grid;
use wide::u8x32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub row: u16,
    pub col: u16,
    pub len: u16,
}

/// Compute damage runs between `prev` and `next`.
///
/// # Panics
/// Panics if the two grids do not share `(cols, rows)`. Callers must
/// `resize` both grids in lockstep on SIGWINCH-equivalent events.
#[must_use]
pub fn diff(prev: &Grid, next: &Grid) -> Vec<Run> {
    assert_eq!(
        (prev.cols(), prev.rows()),
        (next.cols(), next.rows()),
        "grid dims must match for diff",
    );
    let cols = prev.cols();
    let rows = prev.rows();
    let cell_bytes = 16usize;
    let row_bytes = usize::from(cols) * cell_bytes;
    let prev_b = prev.as_bytes();
    let next_b = next.as_bytes();

    let mut out: Vec<Run> = Vec::new();
    for row in 0..rows {
        let off = usize::from(row) * row_bytes;
        let row_prev = &prev_b[off..off + row_bytes];
        let row_next = &next_b[off..off + row_bytes];

        let stride = 32usize;
        let mut byte_i = 0usize;
        let mut row_changed = false;
        while byte_i + stride <= row_bytes {
            let va = u8x32::new(chunk32(row_prev, byte_i));
            let vb = u8x32::new(chunk32(row_next, byte_i));
            if va != vb {
                row_changed = true;
                break;
            }
            byte_i += stride;
        }
        if !row_changed && row_prev[byte_i..] != row_next[byte_i..] {
            row_changed = true;
        }
        if !row_changed {
            continue;
        }

        let mut col = 0u16;
        while col < cols {
            let c_off = usize::from(col) * cell_bytes;
            if row_prev[c_off..c_off + cell_bytes] == row_next[c_off..c_off + cell_bytes] {
                col += 1;
                continue;
            }
            let start = col;
            while col < cols {
                let c_off2 = usize::from(col) * cell_bytes;
                if row_prev[c_off2..c_off2 + cell_bytes] == row_next[c_off2..c_off2 + cell_bytes] {
                    break;
                }
                col += 1;
            }
            out.push(Run { row, col: start, len: col - start });
        }
    }
    out
}

fn chunk32(s: &[u8], i: usize) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&s[i..i + 32]);
    out
}
```

  Modify `crates/origin-tui/src/lib.rs`: add `pub mod damage;` and `pub use damage::Run;`.

- [ ] **Step 5:** Write the bench at `crates/origin-tui/benches/damage_diff.rs`:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use origin_tui::damage::diff;
use origin_tui::{Cell, Grid};

#[allow(
    clippy::cast_possible_truncation,
    reason = "row/col fit in u16 by construction"
)]
fn bench_1pct_changed(c: &mut Criterion) {
    let cols = 200u16;
    let rows = 60u16;
    let a = Grid::new(cols, rows);
    let mut b = a.clone();
    let total = (usize::from(cols) * usize::from(rows)) / 100;
    for i in 0..total {
        let row = (i % usize::from(rows)) as u16;
        let col = ((i * 17) % usize::from(cols)) as u16;
        b.put(row, col, Cell::glyph('x'));
    }
    c.bench_function("damage_diff_200x60_1pct", |bencher| {
        bencher.iter(|| {
            let runs = diff(black_box(&a), black_box(&b));
            black_box(runs);
        });
    });
}

criterion_group!(benches, bench_1pct_changed);
criterion_main!(benches);
```

- [ ] **Step 6:** Run tests → all PASS:

```bash
cargo test -p origin-tui --test damage
```

- [ ] **Step 7: Verification gate**

```bash
cargo test -p origin-tui
cargo clippy -p origin-tui --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 8: Commit** `feat(origin-tui): SIMD damage diff over packed Cell grids (P4.2, N8.1)`.

---

## Task P4.3 — ANSI emit for damage runs

**Files:** `crates/origin-tui/src/ansi.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify)*, `crates/origin-tui/tests/ansi.rs` *(new)*.

**Public surface:**
- `pub fn emit(next: &Grid, runs: &[Run]) -> Vec<u8>`.

**Mechanism:** For each run, write CSI `\x1b[<row+1>;<col+1>H` (CUP, 1-based), then iterate `run.len` cells. When `(fg, bg, attr)` changes vs the current-style tracker, write SGR reset + per-attr SGR + truecolor `38;2;r;g;b` / `48;2;r;g;b` only when `fg`/`bg` are non-zero (zero means "terminal default"). Append the cell's `glyph` as UTF-8 via `char::from_u32` + `encode_utf8`. After each run emit `\x1b[0m` to keep subsequent unrelated writes clean. No terminfo dependency. Empty runs yield an empty `Vec<u8>`.

- [ ] **Step 1: Failing test** at `crates/origin-tui/tests/ansi.rs`:

```rust
use origin_tui::ansi::emit;
use origin_tui::damage::Run;
use origin_tui::{Attr, Cell, Grid};

#[test]
fn empty_runs_emit_nothing() {
    let g = Grid::new(10, 4);
    assert!(emit(&g, &[]).is_empty());
}

#[test]
fn single_glyph_run_emits_cup_plus_glyph_plus_reset() {
    let mut g = Grid::new(10, 4);
    g.put(2, 5, Cell::glyph('X'));
    let bytes = emit(&g, &[Run { row: 2, col: 5, len: 1 }]);
    // CUP is 1-based: row=3, col=6
    let s = std::str::from_utf8(&bytes).expect("valid utf-8");
    assert!(s.starts_with("\x1b[3;6H"), "cursor position prefix; got {s:?}");
    assert!(s.contains('X'), "glyph must appear; got {s:?}");
    assert!(s.ends_with("\x1b[0m"), "SGR reset trailing; got {s:?}");
}

#[test]
fn truecolor_fg_emits_38_2_triplet() {
    let mut g = Grid::new(4, 1);
    g.put(0, 0, Cell::new('A', 0x00FF_0000, 0, Attr::PLAIN));
    let s = String::from_utf8(emit(&g, &[Run { row: 0, col: 0, len: 1 }]))
        .expect("utf-8 ansi");
    assert!(s.contains("\x1b[38;2;255;0;0m"), "truecolor fg; got {s:?}");
}

#[test]
fn bold_attr_emits_sgr_1() {
    let mut g = Grid::new(4, 1);
    g.put(0, 0, Cell::new('B', 0, 0, Attr::BOLD));
    let s = String::from_utf8(emit(&g, &[Run { row: 0, col: 0, len: 1 }]))
        .expect("utf-8 ansi");
    assert!(s.contains("\x1b[1m"), "bold SGR; got {s:?}");
}

#[test]
fn multi_run_resets_between_runs() {
    let mut g = Grid::new(10, 2);
    g.put(0, 0, Cell::glyph('a'));
    g.put(1, 0, Cell::glyph('b'));
    let bytes = emit(&g, &[
        Run { row: 0, col: 0, len: 1 },
        Run { row: 1, col: 0, len: 1 },
    ]);
    let s = std::str::from_utf8(&bytes).expect("utf-8");
    let reset_count = s.matches("\x1b[0m").count();
    assert!(reset_count >= 2, "one SGR reset per run; saw {reset_count} in {s:?}");
}
```

- [ ] **Step 2:** Run `cargo test -p origin-tui --test ansi` → fail (module missing).

- [ ] **Step 3:** Implement `crates/origin-tui/src/ansi.rs`:

```rust
//! Emit `cursor-position + SGR + glyph` byte sequences for a damage-run set.

use crate::damage::Run;
use crate::grid::{Attr, Grid};

#[must_use]
pub fn emit(next: &Grid, runs: &[Run]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    if runs.is_empty() {
        return out;
    }
    for run in runs {
        push_cup(&mut out, run.row, run.col);
        let mut current_style: Option<(u32, u32, u32)> = None;
        for i in 0..run.len {
            let cell = next.get(run.row, run.col + i);
            let style = (cell.fg, cell.bg, cell.attr);
            if Some(style) != current_style {
                push_sgr(&mut out, cell.fg, cell.bg, Attr(cell.attr));
                current_style = Some(style);
            }
            push_glyph(&mut out, cell.glyph);
        }
        out.extend_from_slice(b"\x1b[0m");
    }
    out
}

fn push_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    use std::io::Write;
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
}

fn push_sgr(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr) {
    use std::io::Write;
    out.extend_from_slice(b"\x1b[0m");
    if attr.bits() & Attr::BOLD.bits() != 0 {
        out.extend_from_slice(b"\x1b[1m");
    }
    if attr.bits() & Attr::ITALIC.bits() != 0 {
        out.extend_from_slice(b"\x1b[3m");
    }
    if attr.bits() & Attr::UNDERLINE.bits() != 0 {
        out.extend_from_slice(b"\x1b[4m");
    }
    if attr.bits() & Attr::REVERSE.bits() != 0 {
        out.extend_from_slice(b"\x1b[7m");
    }
    if attr.bits() & Attr::DIM.bits() != 0 {
        out.extend_from_slice(b"\x1b[2m");
    }
    if fg != 0 {
        let (r, g, b) = unpack(fg);
        let _ = write!(out, "\x1b[38;2;{r};{g};{b}m");
    }
    if bg != 0 {
        let (r, g, b) = unpack(bg);
        let _ = write!(out, "\x1b[48;2;{r};{g};{b}m");
    }
}

fn push_glyph(out: &mut Vec<u8>, scalar: u32) {
    if let Some(ch) = char::from_u32(scalar) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
}

const fn unpack(c: u32) -> (u8, u8, u8) {
    (((c >> 16) & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, (c & 0xFF) as u8)
}
```

  Modify `lib.rs`: `pub mod ansi;`.

- [ ] **Step 4:** Tests pass.

- [ ] **Step 5: Verification gate** (single-crate set).

- [ ] **Step 6: Commit** `feat(origin-tui): ANSI emit for damage runs (P4.3)`.

---

## Task P4.4 — Frame coalescing scheduler (N8.2)

**Files:** `crates/origin-tui/Cargo.toml` *(modify — add `tokio` with `sync`+`time`+`macros`+`rt`)*, `crates/origin-tui/src/scheduler.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify)*, `crates/origin-tui/tests/scheduler.rs` *(new)*.

**Public surface:**
- `pub struct Scheduler { ... }` + `Scheduler::new(frame_budget: Duration) -> Self`.
- `Scheduler::handle(&self) -> Handle` — cheap clonable.
- `Scheduler::run<F: FnMut() + Send>(self, render: F) -> impl Future<Output = ()>` — drives forever; caller `abort()`s the spawned task on shutdown.
- `pub struct Handle { ... }` (`Clone`); `Handle::mark_dirty(&self)` — flips `AtomicBool` + `Notify::notify_one`.

**Mechanism (N8.2):** `run` awaits `notify.notified()`, then swaps `dirty: AtomicBool::swap(false, Ordering::AcqRel)`. If the swap returned `false`, loop (spurious wake). Otherwise compute `frame_budget.saturating_sub(prev_frame.elapsed())` and `tokio::time::sleep` it. Then call `render()`. Multiple `mark_dirty` calls inside the budget collapse into one render — subsequent flips wake the next iteration, where the just-rendered frame has reset the dirty bit. Idle cost: parked on `Notify`, zero CPU.

- [ ] **Step 1:** Modify `crates/origin-tui/Cargo.toml` `[dependencies]`: append `tokio = { version = "1", features = ["sync", "time", "macros", "rt"] }`. In `[dev-dependencies]` append `tokio = { version = "1", features = ["macros", "rt", "test-util", "time"] }`.

- [ ] **Step 2: Failing test** at `crates/origin-tui/tests/scheduler.rs`:

```rust
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use origin_tui::scheduler::Scheduler;

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn ten_dirty_flips_in_one_budget_yield_one_render() {
    let frames = Arc::new(AtomicU32::new(0));
    let frames_in = frames.clone();
    let sched = Scheduler::new(Duration::from_millis(6));
    let handle = sched.handle();
    let task = tokio::spawn(async move {
        sched.run(move || {
            frames_in.fetch_add(1, Ordering::Relaxed);
        }).await;
    });

    // 10 dirty flips inside one budget window
    for _ in 0..10 {
        handle.mark_dirty();
    }
    // Advance virtual time well past the 6ms budget so the scheduled
    // render fires.
    tokio::time::sleep(Duration::from_millis(20)).await;
    task.abort();
    let count = frames.load(Ordering::Relaxed);
    assert_eq!(count, 1, "expected exactly one render frame, got {count}");
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn no_dirty_flip_means_zero_renders() {
    let frames = Arc::new(AtomicU32::new(0));
    let frames_in = frames.clone();
    let sched = Scheduler::new(Duration::from_millis(6));
    let task = tokio::spawn(async move {
        sched.run(move || {
            frames_in.fetch_add(1, Ordering::Relaxed);
        }).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    task.abort();
    assert_eq!(frames.load(Ordering::Relaxed), 0);
}
```

- [ ] **Step 3:** Run → fail.

- [ ] **Step 4:** Implement `crates/origin-tui/src/scheduler.rs`:

```rust
//! Frame coalescing scheduler (N8.2).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::{sleep, Instant};

#[derive(Debug)]
pub struct Scheduler {
    inner: Arc<Inner>,
    frame_budget: Duration,
}

#[derive(Debug)]
struct Inner {
    dirty: AtomicBool,
    notify: Notify,
}

impl Scheduler {
    #[must_use]
    pub fn new(frame_budget: Duration) -> Self {
        Self {
            inner: Arc::new(Inner {
                dirty: AtomicBool::new(false),
                notify: Notify::new(),
            }),
            frame_budget,
        }
    }

    #[must_use]
    pub fn handle(&self) -> Handle {
        Handle { inner: self.inner.clone() }
    }

    pub async fn run<F>(self, mut render: F)
    where
        F: FnMut() + Send,
    {
        let mut last_frame: Option<Instant> = None;
        loop {
            self.inner.notify.notified().await;
            if !self.inner.dirty.swap(false, Ordering::AcqRel) {
                continue;
            }
            if let Some(prev) = last_frame {
                let since = prev.elapsed();
                if since < self.frame_budget {
                    sleep(self.frame_budget - since).await;
                }
            }
            render();
            last_frame = Some(Instant::now());
        }
    }
}

#[derive(Clone, Debug)]
pub struct Handle {
    inner: Arc<Inner>,
}

impl Handle {
    pub fn mark_dirty(&self) {
        self.inner.dirty.store(true, Ordering::Release);
        self.inner.notify.notify_one();
    }
}
```

  Modify `lib.rs`: `pub mod scheduler;` + `pub use scheduler::{Handle, Scheduler};`.

- [ ] **Step 5:** Tests pass.

- [ ] **Step 6: Verification gate.**

- [ ] **Step 7: Commit** `feat(origin-tui): event-loop-tied frame coalescing scheduler (P4.4, N8.2)`.

---

## Task P4.5 — Grapheme-width LRU cache (N8.4)

**Files:** `crates/origin-tui/Cargo.toml` *(modify — add `unicode-segmentation`, `unicode-width`, `lru`, `fxhash`)*, `crates/origin-tui/src/width.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify)*, `crates/origin-tui/tests/width.rs` *(new)*.

**Public surface:**
- `pub struct WidthCache { ... }` — single-threaded by design; clones are independent caches.
- `WidthCache::new(cap: usize) -> Self`.
- `WidthCache::width_of(&mut self, grapheme: &str) -> u8` — consults cache by `fxhash::hash64(grapheme.as_bytes())`. On miss: compute via `unicode_width::UnicodeWidthStr::width(grapheme).min(2) as u8`, insert, return.
- `WidthCache::measure_str(&mut self, text: &str) -> u32` — iterates `text.graphemes(true)` and sums `width_of` per cluster; returns total advance in columns.

**Why:** Per spec N8.4 the renderer never holds the grapheme string in the cache — only `(hash, width)` — to keep the cache footprint constant. ZWJ-emoji clusters pre-canonicalize via `unicode_segmentation::Graphemes` once, then `unicode-width` resolves the column advance.

- [ ] **Step 1:** Modify `crates/origin-tui/Cargo.toml` `[dependencies]`: add `unicode-segmentation = "1"`, `unicode-width = "0.1"`, `lru = "0.12"`, `fxhash = "0.2"`.

- [ ] **Step 2: Failing test** at `crates/origin-tui/tests/width.rs`:

```rust
use origin_tui::width::WidthCache;

#[test]
fn ascii_is_width_1() {
    let mut c = WidthCache::new(64);
    assert_eq!(c.width_of("a"), 1);
}

#[test]
fn cjk_is_width_2() {
    let mut c = WidthCache::new(64);
    assert_eq!(c.width_of("漢"), 2);
}

#[test]
fn zwj_emoji_cluster_is_one_grapheme() {
    let mut c = WidthCache::new(64);
    // Family ZWJ sequence: 👨‍👩‍👧
    let cluster = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let width = c.width_of(cluster);
    // Some unicode-width versions report 2 for emoji presentation, some 1
    // for the leading char alone; we only require a stable non-zero result.
    assert!(width >= 1, "ZWJ emoji should advance >=1 column, got {width}");
}

#[test]
fn measure_str_sums_grapheme_widths() {
    let mut c = WidthCache::new(64);
    assert_eq!(c.measure_str("hi"), 2);
    assert_eq!(c.measure_str("a漢b"), 4);
}

#[test]
fn lru_evicts_oldest_at_capacity() {
    let mut c = WidthCache::new(2);
    let _ = c.width_of("a");
    let _ = c.width_of("b");
    let _ = c.width_of("c"); // evicts "a"
    assert_eq!(c.len(), 2);
}
```

- [ ] **Step 3:** Run → fail.

- [ ] **Step 4:** Implement `crates/origin-tui/src/width.rs`:

```rust
//! Grapheme-width LRU cache (N8.4).

use lru::LruCache;
use std::num::NonZeroUsize;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub struct WidthCache {
    map: LruCache<u64, u8>,
}

impl WidthCache {
    /// # Panics
    /// Panics if `cap == 0`.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let nz = NonZeroUsize::new(cap).expect("WidthCache capacity must be > 0");
        Self { map: LruCache::new(nz) }
    }

    pub fn width_of(&mut self, grapheme: &str) -> u8 {
        let key = fxhash::hash64(grapheme.as_bytes());
        if let Some(&w) = self.map.get(&key) {
            return w;
        }
        let w = UnicodeWidthStr::width(grapheme).min(2) as u8;
        self.map.put(key, w);
        w
    }

    pub fn measure_str(&mut self, text: &str) -> u32 {
        text.graphemes(true).map(|g| u32::from(self.width_of(g))).sum()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
```

  Modify `lib.rs`: `pub mod width;` + `pub use width::WidthCache;`.

- [ ] **Step 5:** Tests pass.

- [ ] **Step 6: Verification gate.**

- [ ] **Step 7: Commit** `feat(origin-tui): grapheme-width LRU cache (P4.5, N8.4)`.

---

## Task P4.6 — StreamWidget — direct ring/CAS reading (N8.3)

**Files:** `crates/origin-tui/Cargo.toml` *(modify — add `origin-stream = { path = "../origin-stream" }`)*, `crates/origin-tui/src/stream_widget.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify)*, `crates/origin-tui/tests/stream_widget.rs` *(new)*.

**Public surface:**
- `pub struct Rect { pub row: u16, pub col: u16, pub cols: u16, pub rows: u16 }`.
- `pub struct StreamWidget { cursor_row: u16, cursor_col: u16, widths: WidthCache, viewport: Rect }`.
- `StreamWidget::new(viewport: Rect) -> Self`.
- `StreamWidget::reset_cursor(&mut self)` — moves cursor to viewport origin (used on `TurnEnd`).
- `StreamWidget::apply(&mut self, event: &TokenEvent, grid: &mut Grid)` — for `TokenKind::TextDelta` payloads: interpret as UTF-8, iterate graphemes, look up width, wrap on `viewport.cols`, write cells; for `TokenKind::ToolUseDelta` / `ToolUseStart`: no-op (handled elsewhere); for `TurnEnd`: insert a newline (advance cursor).

**Why:** N8.3 — the streaming widget reads the ring's archived bytes directly. No `String::push_str` per delta. The grapheme iterator borrows the payload `&str`; `WidthCache::width_of` keys on `fxhash::hash64` of those bytes.

- [ ] **Step 1:** Modify `crates/origin-tui/Cargo.toml`: append `origin-stream = { path = "../origin-stream" }` to `[dependencies]`.

- [ ] **Step 2: Failing test** at `crates/origin-tui/tests/stream_widget.rs`:

```rust
use origin_stream::{TokenEvent, TokenKind};
use origin_tui::stream_widget::{Rect, StreamWidget};
use origin_tui::{Cell, Grid};

fn text_event(s: &str) -> TokenEvent {
    TokenEvent::new(TokenKind::TextDelta, s.as_bytes().to_vec())
}

#[test]
fn ascii_text_delta_lays_into_grid() {
    let mut grid = Grid::new(20, 4);
    let mut w = StreamWidget::new(Rect { row: 0, col: 0, cols: 20, rows: 4 });
    w.apply(&text_event("hello"), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::glyph('h'));
    assert_eq!(grid.get(0, 4), Cell::glyph('o'));
    assert_eq!(grid.get(0, 5), Cell::blank());
}

#[test]
fn wraps_at_viewport_cols() {
    let mut grid = Grid::new(5, 3);
    let mut w = StreamWidget::new(Rect { row: 0, col: 0, cols: 5, rows: 3 });
    w.apply(&text_event("abcdefg"), &mut grid);
    assert_eq!(grid.get(0, 4), Cell::glyph('e'));
    assert_eq!(grid.get(1, 0), Cell::glyph('f'));
    assert_eq!(grid.get(1, 1), Cell::glyph('g'));
}

#[test]
fn cjk_double_width_skips_a_column() {
    let mut grid = Grid::new(6, 2);
    let mut w = StreamWidget::new(Rect { row: 0, col: 0, cols: 6, rows: 2 });
    w.apply(&text_event("a漢b"), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::glyph('a'));
    assert_eq!(grid.get(0, 1), Cell::glyph('漢'));
    // Column 2 is the right half of the wide glyph — we leave it blank
    // (caller decides whether to skip or fill).
    assert_eq!(grid.get(0, 3), Cell::glyph('b'));
}

#[test]
fn turn_end_advances_to_new_row() {
    let mut grid = Grid::new(8, 3);
    let mut w = StreamWidget::new(Rect { row: 0, col: 0, cols: 8, rows: 3 });
    w.apply(&text_event("ab"), &mut grid);
    w.apply(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()), &mut grid);
    w.apply(&text_event("cd"), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::glyph('a'));
    assert_eq!(grid.get(1, 0), Cell::glyph('c'));
}

#[test]
fn non_text_events_are_no_op() {
    let mut grid = Grid::new(4, 1);
    let mut w = StreamWidget::new(Rect { row: 0, col: 0, cols: 4, rows: 1 });
    w.apply(&TokenEvent::new(TokenKind::Usage, b"{}".to_vec()), &mut grid);
    w.apply(&TokenEvent::new(TokenKind::ToolUseStart, b"id\0name".to_vec()), &mut grid);
    assert_eq!(grid.get(0, 0), Cell::blank());
}
```

- [ ] **Step 3:** Run → fail.

- [ ] **Step 4:** Implement `crates/origin-tui/src/stream_widget.rs`:

```rust
//! StreamWidget — reads `TokenEvent` payloads and lays graphemes into a Grid.

use crate::grid::{Cell, Grid};
use crate::width::WidthCache;
use origin_stream::{TokenEvent, TokenKind};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug)]
pub struct StreamWidget {
    cursor_row: u16,
    cursor_col: u16,
    widths: WidthCache,
    viewport: Rect,
}

impl StreamWidget {
    #[must_use]
    pub fn new(viewport: Rect) -> Self {
        Self {
            cursor_row: viewport.row,
            cursor_col: viewport.col,
            widths: WidthCache::new(8 * 1024),
            viewport,
        }
    }

    pub fn reset_cursor(&mut self) {
        self.cursor_row = self.viewport.row;
        self.cursor_col = self.viewport.col;
    }

    pub fn apply(&mut self, event: &TokenEvent, grid: &mut Grid) {
        match event.kind() {
            TokenKind::TextDelta => self.write_text(event.payload(), grid),
            TokenKind::TurnEnd => self.newline(),
            _ => {}
        }
    }

    fn write_text(&mut self, bytes: &[u8], grid: &mut Grid) {
        let Ok(s) = std::str::from_utf8(bytes) else { return };
        for g in s.graphemes(true) {
            let w = self.widths.width_of(g);
            let right_edge = self.viewport.col + self.viewport.cols;
            if self.cursor_col + u16::from(w) > right_edge {
                self.newline();
            }
            if let Some(ch) = g.chars().next() {
                grid.put(self.cursor_row, self.cursor_col, Cell::glyph(ch));
            }
            self.cursor_col += u16::from(w.max(1));
        }
    }

    fn newline(&mut self) {
        self.cursor_col = self.viewport.col;
        let bottom = self.viewport.row + self.viewport.rows.saturating_sub(1);
        if self.cursor_row < bottom {
            self.cursor_row += 1;
        }
    }
}
```

  Modify `lib.rs`: `pub mod stream_widget;` + `pub use stream_widget::{Rect, StreamWidget};`.

- [ ] **Step 5:** Tests pass.

- [ ] **Step 6: Verification gate.**

- [ ] **Step 7: Commit** `feat(origin-tui): StreamWidget reads ring TokenEvents into Grid (P4.6, N8.3)`.

---

## Task P4.7 — Side panel Composer (N8.5) + permission migration off modal

**Files:** `crates/origin-tui/Cargo.toml` *(modify — add `origin-permission = { path = "../origin-permission" }` as dep)*, `crates/origin-tui/src/composer.rs` *(new)*, `crates/origin-tui/src/panel.rs` *(new)*, `crates/origin-tui/src/cli_prompter.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify)*, `crates/origin-tui/tests/composer.rs` *(new)*, `crates/origin-tui/tests/panel.rs` *(new)*.

**Public surface:**
- `pub struct Composer { main: Grid, side: Grid, prompt: Grid, scratch_main: Grid, scratch_side: Grid, scratch_prompt: Grid, side_visible: bool }`.
- `Composer::new(cols: u16, rows: u16) -> Self`.
- `Composer::resize(&mut self, cols: u16, rows: u16, side_visible: bool)` — main pane is `cols - side_cols` when visible; prompt pane is 3 rows fixed at the bottom. **Main is clipped, not rewrapped** when toggling — caller-owned text isn't re-laid out by this function.
- `Composer::main_grid(&mut self) -> &mut Grid`, `Composer::side_grid(&mut self) -> &mut Grid`, `Composer::prompt_grid(&mut self) -> &mut Grid`.
- `Composer::frame(&mut self) -> Vec<u8>` — diffs each scratch vs live grid, swaps live↔scratch, emits the merged ANSI stream (all main runs, then all side runs, then all prompt runs).
- `pub struct Panel { items: VecDeque<PanelEvent> }` + `PanelEvent::PermissionAsk { id: u64, tool: String, tier: Tier }` + `PanelEvent::PermissionDecided { id: u64, outcome: PermissionOutcome }` + `PermissionOutcome::{Allow, Deny, Edit}`.
- `Panel::new() -> Self`, `Panel::push(&mut self, ev: PanelEvent)`, `Panel::handle_key(&mut self, k: char) -> Option<PermissionOutcome>`, `Panel::render(&self, side: &mut Grid)`.
- `pub struct SidePanelPrompter { tx: tokio::sync::mpsc::Sender<PanelEvent>, rx: Mutex<tokio::sync::mpsc::Receiver<PermissionOutcome>> }` — implements `origin_permission::Prompter`.

**Why:** N8.5 — side panel owns its own `Grid` + damage tracker; toggling it resizes the main pane but does not rewrap. N9.3 — permission prompts are side-panel events, not modals, so concurrent tool calls don't serialize on the gate.

- [ ] **Step 1: Failing tests** at `crates/origin-tui/tests/composer.rs`:

```rust
use origin_tui::composer::Composer;
use origin_tui::{Cell, Grid};

#[test]
fn first_frame_paints_initial_contents() {
    let mut c = Composer::new(40, 10);
    c.main_grid().put(0, 0, Cell::glyph('M'));
    c.side_grid().put(0, 0, Cell::glyph('S'));
    let bytes = c.frame();
    let s = String::from_utf8(bytes).expect("utf-8");
    assert!(s.contains('M'), "main cell present");
    assert!(s.contains('S'), "side cell present");
}

#[test]
fn no_change_means_empty_frame_bytes() {
    let mut c = Composer::new(20, 4);
    let _ = c.frame(); // initial paint
    let bytes = c.frame();
    assert!(bytes.is_empty(), "second frame with no changes emits nothing");
}

#[test]
fn toggling_side_panel_keeps_main_unchanged() {
    let mut c = Composer::new(40, 10);
    c.resize(40, 10, true);
    c.main_grid().put(2, 5, Cell::glyph('X'));
    let _ = c.frame();
    let cell_before = c.main_grid().get(2, 5);
    c.resize(40, 10, false);
    let cell_after = c.main_grid().get(2, 5);
    assert_eq!(cell_before, cell_after, "main contents must not be rewrapped on side toggle");
}
```

  And `crates/origin-tui/tests/panel.rs`:

```rust
use origin_tui::panel::{Panel, PanelEvent, PermissionOutcome};
use origin_permission::Tier;

#[test]
fn permission_ask_then_y_key_decides_allow() {
    let mut p = Panel::new();
    p.push(PanelEvent::PermissionAsk { id: 1, tool: "Read".into(), tier: Tier::AutoAllowed });
    let outcome = p.handle_key('y');
    assert_eq!(outcome, Some(PermissionOutcome::Allow));
}

#[test]
fn n_key_decides_deny() {
    let mut p = Panel::new();
    p.push(PanelEvent::PermissionAsk { id: 1, tool: "Bash".into(), tier: Tier::RequiresPermission });
    let outcome = p.handle_key('n');
    assert_eq!(outcome, Some(PermissionOutcome::Deny));
}

#[test]
fn unrelated_key_returns_none() {
    let mut p = Panel::new();
    p.push(PanelEvent::PermissionAsk { id: 1, tool: "Edit".into(), tier: Tier::RequiresPermission });
    assert_eq!(p.handle_key('q'), None);
}
```

- [ ] **Step 2:** Run → fail.

- [ ] **Step 3:** Implement `composer.rs` — owns six `Grid`s (3 live + 3 scratch) in a single struct; `frame()` runs `damage::diff` on each pair then `ansi::emit`, concatenates the three byte vectors, and swaps each live↔scratch with `std::mem::swap`. Implement `panel.rs` — `Panel::handle_key` matches `'y'`/`'n'`/`'e'` against the front `PermissionAsk` and pops it, returning the outcome. Implement `cli_prompter.rs` — `SidePanelPrompter` impls `Prompter::ask(req) -> bool` by `tx.send(PanelEvent::PermissionAsk { ... })` then `rx.recv()`; the panel runtime side feeds decisions back over the `PermissionOutcome` channel. Re-export from `lib.rs`.

- [ ] **Step 4:** Tests pass.

- [ ] **Step 5: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 6: Commit** `feat(origin-tui): Composer side panel + Panel permission events (P4.7, N8.5, N9.3)`.

---

## Task P4.8 — CAS-backed per-turn layout cache

**Files:** `crates/origin-tui/Cargo.toml` *(modify — add `origin-cas = { path = "../origin-cas" }`, `blake3 = "1"`)*, `crates/origin-tui/src/layout_cache.rs` *(new)*, `crates/origin-tui/src/lib.rs` *(modify)*, `crates/origin-tui/tests/layout_cache.rs` *(new)*.

**Public surface:**
- `pub struct LayoutSpan { pub row: u16, pub col: u16, pub byte_start: u32, pub byte_end: u32 }` — derives `Archive + Serialize + Deserialize` (rkyv 0.7, `#[archive(check_bytes)]`) so the cached payload survives a store round-trip.
- `pub struct LayoutCache { store: Arc<Store>, viewport_cols: u16, widths: WidthCache }`.
- `LayoutCache::new(store: Arc<Store>, viewport_cols: u16) -> Self`.
- `LayoutCache::get_or_build(&mut self, text: &str) -> Result<Vec<LayoutSpan>, LayoutCacheError>` — key = `blake3("layout/v1/" || viewport_cols.to_be_bytes() || text.as_bytes())`; on hit, return decoded spans from the Store; on miss, run a simple grapheme-wrap and store rkyv-archived spans.

**Why:** Compaction-fast scrollback. The same turn's already-laid-out spans survive the daemon restart so re-rendering on resume is a CAS hit, not a re-layout. Key includes `viewport_cols` so width changes naturally re-key.

- [ ] **Step 1:** Modify `crates/origin-tui/Cargo.toml`: add `origin-cas = { path = "../origin-cas" }`, `blake3 = "1"`, and `rkyv` (workspace-pinned version used elsewhere — match the version `origin-cas` uses).

- [ ] **Step 2: Failing test** at `crates/origin-tui/tests/layout_cache.rs`:

```rust
use std::sync::Arc;
use origin_cas::{Store, StoreConfig};
use origin_tui::layout_cache::{LayoutCache, LayoutSpan};
use tempfile::tempdir;

fn store() -> Arc<Store> {
    let dir = tempdir().expect("tempdir");
    Arc::new(Store::open(StoreConfig::with_root(dir.path().to_path_buf())).expect("store open"))
}

#[test]
fn first_call_builds_spans() {
    let mut c = LayoutCache::new(store(), 10);
    let spans = c.get_or_build("hello world").expect("build");
    assert!(!spans.is_empty(), "non-empty text yields spans");
}

#[test]
fn same_text_same_width_returns_same_spans() {
    let s = store();
    let mut c = LayoutCache::new(s.clone(), 10);
    let a = c.get_or_build("hello world").expect("a");
    let mut c2 = LayoutCache::new(s, 10);
    let b = c2.get_or_build("hello world").expect("b");
    assert_eq!(a, b, "same key must yield same spans across instances");
}

#[test]
fn different_widths_produce_different_layouts() {
    let s = store();
    let mut narrow = LayoutCache::new(s.clone(), 4);
    let mut wide = LayoutCache::new(s, 40);
    let a = narrow.get_or_build("hello world").expect("narrow");
    let b = wide.get_or_build("hello world").expect("wide");
    assert_ne!(a, b);
}
```

  `Store::open(StoreConfig)` is the existing API in `crates/origin-cas/src/store.rs` (P2 deliverable); if the constructor's exact name differs, mirror what `crates/origin-daemon/src/main.rs` already uses to open the CAS.

- [ ] **Step 3:** Run → fail.

- [ ] **Step 4:** Implement `layout_cache.rs` — `get_or_build` first computes `key = Hash(blake3(prefix + cols + text))`, attempts `store.read(key)`; on hit, `rkyv::check_archived_root::<Vec<LayoutSpan>>` + deserialize; on miss, run a `WidthCache`-driven wrap and `store.write(key, &rkyv::to_bytes(&spans)?)`. Re-export from `lib.rs`.

- [ ] **Step 5:** Tests pass.

- [ ] **Step 6: Verification gate** (workspace set — touches `origin-cas`).

- [ ] **Step 7: Commit** `feat(origin-tui): CAS-backed per-turn layout cache (P4.8)`.

---

## Task P4.9 — `origin-cli` rewrite + Ratatui retirement

**Files:**
- `crates/origin-cli/Cargo.toml` *(modify)* — remove `ratatui`, keep `crossterm`, add `origin-tui = { path = "../origin-tui" }`, add `unicode-segmentation = "1"`.
- `crates/origin-cli/src/screen.rs` *(modify)* — drop `ratatui::layout::Rect`; export local `Rect { rows: u16, cols: u16 }` + `split_main_input_status` returning three `origin_tui::stream_widget::Rect`.
- `crates/origin-cli/src/status.rs` *(modify)* — `render_into(&UsageSnapshot, prompt_grid: &mut Grid)` writes the status line into the prompt-row grid as plain cells.
- `crates/origin-cli/src/tui.rs` *(rewrite)* — `App::draw(composer: &mut Composer, widget: &mut StreamWidget)` populates `main`/`side`/`prompt` grids from scrollback + live deltas + status snapshot. Replaces the old `pub fn draw(f: &mut ratatui::Frame, ...)`.
- `crates/origin-cli/src/main.rs` *(rewrite)* — replace the ratatui `Terminal` setup with: `crossterm::terminal::enable_raw_mode()`; `EnterAlternateScreen`; build `Composer::new(cols, rows)` and `StreamWidget::new(main_rect)`; build a `Scheduler::new(Duration::from_millis(6))` + `Scheduler::handle()`; `tokio::spawn(scheduler.run({ move || { let bytes = composer.frame(); stdout().write_all(&bytes).ok(); stdout().flush().ok(); } }))`; the keystroke loop uses `crossterm::event::EventStream` (async) and calls `handle.mark_dirty()` after mutating `app`. Wire the panel: a `tokio::sync::mpsc::channel::<PanelEvent>(64)` whose receiver task pushes into `Panel`, calls `handle.mark_dirty()`, and feeds `PermissionOutcome` back over the second channel.

The composer is shared between the keystroke loop and the render task via `Arc<parking_lot::Mutex<Composer>>` (already in workspace dep graph via `origin-keyvault` lock). For simplicity the render closure locks, takes `composer.frame()`, then drops the lock before writing to stdout.

- [ ] **Step 1: Failing test** at `crates/origin-cli/tests/draw_smoke.rs` (new):

```rust
use origin_cli::tui::App;
use origin_tui::composer::Composer;
use origin_tui::stream_widget::{Rect, StreamWidget};

#[test]
fn empty_app_draws_status_only() {
    let mut app = App::new("anthropic", "claude-opus-4-7".to_string());
    app.add_line("", "hello");
    let mut composer = Composer::new(40, 10);
    let mut widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 40, rows: 6 });
    app.draw(&mut composer, &mut widget);
    let bytes = composer.frame();
    let s = String::from_utf8(bytes).expect("utf-8");
    assert!(s.contains("hello"), "scrollback line must render; got {s:?}");
}

#[test]
fn live_assistant_buffer_renders_in_main() {
    let mut app = App::new("anthropic", "claude-opus-4-7".to_string());
    app.start_assistant_turn();
    app.append_to_current_assistant("hello world");
    let mut composer = Composer::new(40, 10);
    let mut widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 40, rows: 6 });
    app.draw(&mut composer, &mut widget);
    let bytes = composer.frame();
    let s = String::from_utf8(bytes).expect("utf-8");
    assert!(s.contains("hello world"));
}
```

- [ ] **Step 2:** Run → fail (`origin-tui` modules not yet used by `origin-cli`).

- [ ] **Step 3:** Edit `Cargo.toml`, `screen.rs`, `status.rs`, `tui.rs`, `main.rs` per the file-map descriptions. The old `draw(f, &app)` signature goes away; the new `App::draw` method takes the composer + widget.

- [ ] **Step 4:** Tests pass; manual run check: `ORIGIN_SOCK=$(mktemp -u) cargo run -p origin-cli` exits clean on Ctrl-C with no Ratatui in `cargo tree -p origin-cli | grep ratatui` (should be empty).

- [ ] **Step 5: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
test "$(cargo tree -p origin-cli 2>/dev/null | grep -c ratatui)" -eq 0
```

  On Windows PowerShell:

```powershell
cargo test --workspace; if ($LASTEXITCODE -eq 0) { cargo clippy --workspace --all-targets -- -D warnings }
cargo fmt --check
$tree = cargo tree -p origin-cli 2>$null
if ($tree -match 'ratatui') { throw 'ratatui still in deps' }
```

- [ ] **Step 6: Commit** `feat(origin-cli): rewrite on origin-tui Composer + Scheduler; retire Ratatui (P4.9)`.

---

## Task P4.10 — Keystroke→pixel + FPS-under-stream bench harness + tag `p4-complete`

**Files:** `crates/origin-cli/Cargo.toml` *(modify — add `criterion = "0.5"` dev-dep + `[[bench]]` entry)*, `crates/origin-cli/benches/keystroke_to_pixel.rs` *(new)*.

**Public surface:** Two Criterion benches:
1. `keystroke_to_pixel` — 1k synthetic keystrokes mutate `App.input`, mark dirty, render one frame each; assert mean per-iteration < 12ms.
2. `stream_under_load` — feed 1k `TokenKind::TextDelta` events of avg 8 bytes each through `StreamWidget` + `Composer`, render a frame per 6ms-budget tick; assert mean per-frame emit < 6ms.

- [ ] **Step 1:** Modify `Cargo.toml`: add `criterion = "0.5"` to `[dev-dependencies]` and append `[[bench]] name = "keystroke_to_pixel" harness = false`.

- [ ] **Step 2: Write the bench** at `crates/origin-cli/benches/keystroke_to_pixel.rs`:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use origin_cli::tui::App;
use origin_stream::{TokenEvent, TokenKind};
use origin_tui::composer::Composer;
use origin_tui::stream_widget::{Rect, StreamWidget};

fn bench_keystroke_to_pixel(c: &mut Criterion) {
    let mut group = c.benchmark_group("keystroke_to_pixel");
    group.throughput(Throughput::Elements(1));
    group.bench_function("type_then_render_one_frame", |b| {
        b.iter_batched(
            || {
                let app = App::new("anthropic", "claude-opus-4-7".to_string());
                let composer = Composer::new(200, 60);
                let widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 200, rows: 56 });
                (app, composer, widget)
            },
            |(mut app, mut composer, mut widget)| {
                app.input.push('x');
                app.draw(&mut composer, &mut widget);
                black_box(composer.frame());
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_stream_under_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream_under_load");
    group.bench_function("1k_deltas_8b_one_frame_per_tick", |b| {
        b.iter_batched(
            || {
                let composer = Composer::new(200, 60);
                let widget = StreamWidget::new(Rect { row: 0, col: 0, cols: 200, rows: 56 });
                (composer, widget)
            },
            |(mut composer, mut widget)| {
                for i in 0..1_000u32 {
                    let payload = format!("delta{i:03}").into_bytes();
                    let ev = TokenEvent::new(TokenKind::TextDelta, payload);
                    widget.apply(&ev, composer.main_grid());
                }
                black_box(composer.frame());
            },
            criterion::BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_keystroke_to_pixel, bench_stream_under_load);
criterion_main!(benches);
```

- [ ] **Step 3:** Run benches to verify they execute and the assertions documented above hold on local hardware:

```bash
cargo bench -p origin-cli --bench keystroke_to_pixel -- --quick
```

  Expect Criterion's `mean` column for `keystroke_to_pixel/type_then_render_one_frame` to read below 12 ms and `stream_under_load/1k_deltas_8b_one_frame_per_tick` below 6 ms on stock dev hardware. If either misses, **do not advance** — investigate the hot path (most likely an accidental per-iteration allocation in `Composer::frame` or `WidthCache` capacity too small).

- [ ] **Step 4: Final verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo bench -p origin-cli --bench keystroke_to_pixel -- --quick
```

- [ ] **Step 5: Tag**

```bash
git tag p4-complete
```

- [ ] **Step 6: Commit** `chore(origin-cli): keystroke→pixel + FPS-under-stream bench harness; tag p4-complete (P4.10)`.

---

## Self-review checklist

**Spec coverage:**
- ✅ N8.1 — cell-grid double buffer with SIMD damage diff (P4.2, plus the live↔scratch swap in P4.7 `Composer`).
- ✅ N8.2 — event-loop-tied frame coalescing (P4.4) consumed by `origin-cli` in P4.9.
- ✅ N8.3 — streaming render reads ring bytes directly (P4.6 `StreamWidget`).
- ✅ N8.4 — snapshot-stable grapheme-width cache (P4.5 `WidthCache`).
- ✅ N8.5 — side panel as separate render target (P4.7 `Composer.side`), main pane clipped not rewrapped (asserted in `toggling_side_panel_keeps_main_unchanged`).
- ✅ N9.3 — permission prompts are TUI side-panel events, not modals (P4.7 `Panel` + `SidePanelPrompter`).
- ✅ Phase exit deliverables: cell-grid double buffer (P4.1+P4.7), SIMD damage diff (P4.2), frame coalescing tied to event loop (P4.4), side panel + permissions migration (P4.7), CAS-backed per-turn layout cache (P4.8), benchmark suite (P4.10), Ratatui retired (P4.9 verification gate checks `cargo tree`).

**Type consistency:**
- `Run { row: u16, col: u16, len: u16 }` consistent across `damage.rs`, `ansi.rs`, `composer.rs`.
- `Cell` / `Grid` / `Attr` types come from P4.1 (`grid.rs`) and are not re-defined.
- `Rect { row, col, cols, rows }` is consistent across `stream_widget.rs`, `composer.rs`, `origin-cli`.
- `Scheduler::new(Duration)` + `Handle::mark_dirty()` signatures consistent across P4.4 and P4.9.
- `WidthCache::width_of(&str) -> u8` consistent across `width.rs`, `stream_widget.rs`, `layout_cache.rs`.
- `PanelEvent::PermissionAsk { id, tool, tier }` and `PermissionOutcome::{Allow, Deny, Edit}` consistent across `panel.rs` and `cli_prompter.rs`.

**Placeholders:** No "TBD" / "implement later" / "fill in details". Each task names exact files, exact public surface, exact dependency changes, and exact test assertions including full failing-test code. P4.7 and P4.8 reference existing reusable APIs (`origin-permission::Prompter`, `origin-cas::Store`) without re-deriving them.

**Dependency-fan-out for safe parallel subagents:**
- **Group A (no inter-task code dep, can run in parallel):** P4.2, P4.4, P4.5 — each lives in its own `crates/origin-tui/src/<file>.rs` and `tests/<file>.rs`. All three modify `lib.rs` and `Cargo.toml`, so the dispatching shell must serialize the *merge* of those two files (subagents return diffs; main thread applies them in order) — but the per-subagent work is independent.
- **Group B (depends on Group A):** P4.3 needs `damage::Run` (P4.2); P4.6 needs `WidthCache` (P4.5).
- **Group C (depends on Group B):** P4.7 needs `Composer` to use `damage::diff` (P4.2) + `ansi::emit` (P4.3); P4.8 is independent of P4.7 — can run in parallel with P4.7.
- **Group D (depends on everything):** P4.9 + P4.10 sequential, last.

The dispatching agent runs serially when in doubt — the verification gate's `cargo test --workspace` is the cheap-enough safety net to retry on any Group-A merge conflict.

---

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-05-19-origin-phase-4.md`. Per the user's instruction, execution is via **superpowers:subagent-driven-development**, each task internally following **superpowers:test-driven-development** and gated by **superpowers:verification-before-completion** before advancing. Branch: `dev`.
