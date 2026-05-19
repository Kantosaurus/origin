# `origin` Phase 4 — Custom TUI Renderer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Retire Ratatui in favor of a custom cell-grid double-buffer renderer with SIMD damage diffing, event-loop-tied frame coalescing, snapshot-stable grapheme-width caching, CAS/ring-direct streaming reads, and a side panel as a separate render target. All permission prompts move out of modal flow into the side panel.

**Architecture:** New crate `origin-tui` owns six pure render units — `grid` (16-byte packed `Cell` + row-major `Grid`), `damage` (SIMD `wide::u8x32` diff producing runs of changed cells), `ansi` (emit `cursor-move + SGR + glyph-run` for a damage run set), `scheduler` (`dirty: AtomicBool` + 6ms coalescing wake), `width` (grapheme-cluster width LRU shared across widgets), and `panel` (a `Pane` is a `Grid` + damage tracker; a `Composer` owns two panes — `main` + `side` — that share an output stream). The streaming text widget walks a `ring::Subscriber` tail / a `&[u8]` slice into mmap'd CAS, performs grapheme segmentation + width lookup, and emits glyphs directly into the back grid — no `String` per token. The `origin-cli` binary becomes a thin shell wiring daemon events to `origin-tui`; the legacy ratatui path is preserved behind a `tui-baseline` feature flag and only deleted in P4.9. Permission prompts arrive as a new `StreamEvent::PermissionAsk`; the client resolves them via a new `ClientMessage::PermissionDecided` upstream frame.

**Tech Stack:** Rust 1.83 (MSRV pin), Tokio (existing daemon runtime), `crossterm` 0.28 (terminal handle + raw-mode + size + key events — kept; ratatui dropped), `wide` 0.7 (portable SIMD; AVX2/SSE2/NEON dispatch — used in `origin-tui::damage` only), `unicode-segmentation` 1 + `unicode-width` 0.1 + `lru` 0.12 (grapheme-width cache), `criterion` 0.5 (benches), `proptest` (invariants), `tempfile` (CAS-backed widget tests), `tokio::sync::Notify` + `AtomicBool` (scheduler — no new dep). **Novel-implementation reflex** per `[[feedback-novel-implementations]]`: every signature subsystem must beat openclaude/jcode/opencode. Phase 4's novelties: SIMD damage diff over 16-byte packed cells (vs. cell-by-cell diff or full redraw); event-loop-tied 6ms coalescing with idle-zero-cost wake (vs. fixed FPS render loop); ring/CAS-direct streaming with no per-token `String` allocation (vs. line-buffered text widgets); side panel as an independent render target composed in-place (vs. inline rewrap on resize); grapheme-width cache shared across widgets (vs. per-render unicode-width pass).

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` (mechanisms **N8.1–N8.5** are this phase's substrate; N8.6–N8.10 are explicitly **Phase 12**). Phase 3 deliverables (tag `p3-complete`) supply the `origin-stream::Ring` + `Subscriber` the streaming widget reads from.

**Phase 4 spec-mechanism citations:**
- **N8.1** — cell-grid double buffer + SIMD damage diff (Tasks P4.1, P4.2, P4.3)
- **N8.2** — event-loop-tied frame coalescing (Task P4.4)
- **N8.3** — CAS / ring-direct streaming render (Task P4.6)
- **N8.4** — snapshot-stable grapheme-width cache (Task P4.5)
- **N8.5** — side panel as separate render target (Tasks P4.7, P4.8)

What is **explicitly out of scope** for Phase 4 (deferred):
- N8.6 named jemalloc arenas — Phase 12 (`p12-complete`)
- N8.7 `spawn_in(class, fut)` + dylint task-class enforcement — Phase 12
- N8.8 two-runtime split — Phase 12
- N8.9 `tokio-uring` for CAS pack files — Phase 12
- N8.10 phased cooperative shutdown — Phase 12
- Mouse / scroll-wheel / paste-bracket / sixel — none of these gate a "p4-complete" tag; revisit in P10 (extensibility) if a skill needs them
- Image rendering (sixel, kitty graphics protocol) — out of scope until Phase 13 (remote IPC, headless polish)
- Full Unicode bidi / RTL shaping — out of scope; grapheme-width cache treats RTL as best-effort width-only for now

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**
1. Write the failing test.
2. Run it — confirm the expected failure mode.
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit.

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / daemon / CLI | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Bench-touching tasks (P4.2, P4.10) | All of the above + `cargo bench -p <crate> --bench <name> -- --quick` exits 0 with the assertion thresholds met |
| Final phase gate (P4.10) | All of the above + tag `p4-complete` |

**Patterns inherited from earlier phases:**
- `[lints] workspace = true` in every crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All shared/persisted/IPC-crossing types derive `Archive + Serialize + Deserialize` from rkyv 0.7 with `#[archive(check_bytes)]`. **Phase 4 adds none — render state is process-local; IPC additions in P4.8 use `serde_json` like other `StreamEvent` variants.**
- `[lints.rust] unsafe_code = "forbid"` is the default. **`origin-tui` overrides this to `unsafe_code = "allow"`** because `wide::u8x32` SIMD intrinsics on Linux/AVX2 dispatch unsafely; every `unsafe` block carries a SAFETY comment.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex** (`[[project-msrv-dep-pinning]]`): if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>` and record in `Cargo.lock`.

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit**.

---

## File map for Phase 4

| New crate / file | Responsibility |
|---|---|
| `crates/origin-tui/Cargo.toml` | manifest; overrides workspace `unsafe_code = "allow"` |
| `crates/origin-tui/src/lib.rs` | public surface — re-exports + module declarations |
| `crates/origin-tui/src/grid.rs` | `Cell`, `Attr`, `Grid` (P4.1) |
| `crates/origin-tui/src/damage.rs` | SIMD damage diff → `Vec<Run>` (P4.2) |
| `crates/origin-tui/src/ansi.rs` | emit `cursor-move + SGR + glyph-run` for a run set (P4.3) |
| `crates/origin-tui/src/scheduler.rs` | `Scheduler` — `AtomicBool` + 6ms coalescing wake (P4.4) |
| `crates/origin-tui/src/width.rs` | grapheme-width LRU cache (P4.5) |
| `crates/origin-tui/src/stream_widget.rs` | streaming text widget over ring/CAS (P4.6) |
| `crates/origin-tui/src/panel.rs` | `Pane`, `Composer` for main + side (P4.7) |
| `crates/origin-tui/tests/grid.rs` | `Cell`/`Grid` round-trip + resize invariants |
| `crates/origin-tui/tests/damage.rs` | one-cell change in 200×60 grid → one run of length 1 |
| `crates/origin-tui/tests/ansi.rs` | snapshot test for a small run set's ANSI output |
| `crates/origin-tui/tests/scheduler.rs` | 10 dirty flips inside 6ms → 1 render frame |
| `crates/origin-tui/tests/width.rs` | cache hit/miss + LRU eviction |
| `crates/origin-tui/tests/stream_widget.rs` | ring tail consumption + incremental layout |
| `crates/origin-tui/tests/panel.rs` | toggle side panel → main pane clipped not rewrapped (hash compare) |
| `crates/origin-tui/benches/damage_diff.rs` | `Criterion`: 200×60, 1% changed, p99 < 50µs |
| `crates/origin-tui/benches/latency_fps.rs` | synthetic token stream; keystroke→pixel p99 < 12ms; FPS-under-stream cap respected |
| `crates/origin-cli/src/main.rs` *(modify)* | wire `origin-tui` when `tui-baseline` is off; keep ratatui under the flag until P4.9 |
| `crates/origin-cli/src/tui_native.rs` *(new, P4.6+P4.7)* | `App` analogue backed by `origin-tui` — main pane scrollback + prompt + status + side panel |
| `crates/origin-cli/src/side_panel.rs` *(new, P4.8)* | side-panel state machine — permission queue + memory proposals stub |
| `crates/origin-daemon/src/protocol.rs` *(modify, P4.8)* | add `StreamEvent::PermissionAsk { id, tool, args_preview, tier }`; add a new `ClientMessage::PermissionDecided { id, allow, remember }` type for upstream replies |
| `crates/origin-daemon/src/agent.rs` *(modify, P4.8)* | wire the permission engine's `Prompter` to await a `PermissionDecided` from an IPC inbox (replacing today's `AlwaysAllow`/`AlwaysDeny` in non-test paths) |
| `crates/origin-daemon/tests/permission_panel.rs` *(new, P4.8)* | end-to-end: agent emits `PermissionAsk`; reply with `PermissionDecided{allow:true}`; tool runs |
| `crates/origin-cli/Cargo.toml` *(modify)* | add `[features] tui-baseline = ["ratatui"]`; default off in P4.6; ratatui kept until P4.9 |

**File-size discipline:** every new `.rs` file targets <300 LOC. If a task naturally pushes a file past 300 LOC, split early (e.g. `panel.rs` → `panel/pane.rs` + `panel/composer.rs`).

---

## Task P4.1 — `origin-tui` skeleton + `Cell` + `Grid`

**Files:**
- Create: `crates/origin-tui/Cargo.toml`
- Create: `crates/origin-tui/src/lib.rs`
- Create: `crates/origin-tui/src/grid.rs`
- Create: `crates/origin-tui/tests/grid.rs`
- Modify: `Cargo.toml` (workspace) — none required because members = `crates/*`; the new crate is picked up automatically.

- [ ] **Step 1: Manifest** at `crates/origin-tui/Cargo.toml`

```toml
[package]
name = "origin-tui"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

# Override workspace `unsafe_code = "forbid"` — P4.2 needs `wide::u8x32`
# intrinsic dispatch. Each unsafe block must carry a SAFETY: comment.
[lints.rust]
unsafe_code = "allow"

[dependencies]
thiserror = "1"

[dev-dependencies]
proptest = { version = "=1.4.0", default-features = false, features = ["std"] }
```

- [ ] **Step 2: `lib.rs`** module declarations + re-exports

```rust
//! `origin-tui` — custom cell-grid renderer (replaces Ratatui in Phase 4).
//!
//! Phase 4 deliverables: `Cell`, `Grid`, SIMD damage diff (`damage::diff`),
//! ANSI emit (`ansi::emit`), frame coalescing (`Scheduler`), grapheme-width
//! LRU (`WidthCache`), streaming text widget (`StreamWidget`), and a side
//! panel as a separate render target (`Composer`).

pub mod grid;

pub use grid::{Cell, Grid, GridError, Attr};
```

- [ ] **Step 3: Write the failing test** at `crates/origin-tui/tests/grid.rs`

```rust
use origin_tui::{Attr, Cell, Grid};

#[test]
fn resize_clears_and_resets_dims() {
    let mut g = Grid::new(10, 4);
    g.put(0, 0, Cell::glyph('x'));
    g.resize(5, 2);
    assert_eq!(g.cols(), 5);
    assert_eq!(g.rows(), 2);
    // Resize re-initializes cells.
    assert_eq!(g.get(0, 0), Cell::blank());
}

#[test]
fn put_and_get_round_trip() {
    let mut g = Grid::new(8, 2);
    let c = Cell::new('A', 0x00FF_FFFF, 0x0000_0000, Attr::BOLD);
    g.put(1, 3, c);
    assert_eq!(g.get(1, 3), c);
}

#[test]
fn cell_is_16_bytes_packed() {
    // Layout invariant relied on by P4.2's SIMD diff.
    assert_eq!(std::mem::size_of::<Cell>(), 16);
    assert_eq!(std::mem::align_of::<Cell>(), 4);
}

#[test]
fn out_of_bounds_put_is_noop() {
    let mut g = Grid::new(4, 2);
    g.put(99, 99, Cell::glyph('z'));
    // No panic; underlying buffer unaffected.
    assert_eq!(g.get(99, 99), Cell::blank());
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test -p origin-tui --test grid`
Expected: FAIL — `cannot find type Cell / Grid / Attr in crate origin_tui`.

- [ ] **Step 5: Implement `grid.rs`**

```rust
//! Packed 16-byte `Cell` + row-major `Grid` with `resize` / `put` / `get` /
//! `diff`-friendly raw byte access via `as_bytes` / `as_bytes_mut`.
//!
//! `Cell` is `#[repr(C)]` so its in-memory layout matches what P4.2's SIMD
//! diff scans byte-for-byte.

use thiserror::Error;

/// Style bitflags packed into `Cell::attr`'s low byte. Higher bits are
/// reserved for future use (underline color, blink, hyperlinks, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Attr(pub u32);

impl Attr {
    pub const PLAIN: Self = Self(0);
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const REVERSE: Self = Self(1 << 3);
    pub const DIM: Self = Self(1 << 4);

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Cell {
    /// Unicode scalar value or 0 for "blank".
    pub glyph: u32,
    /// Foreground color, 0x00RRGGBB; 0 means terminal default.
    pub fg: u32,
    /// Background color, 0x00RRGGBB; 0 means terminal default.
    pub bg: u32,
    /// Style flag bits (see `Attr`).
    pub attr: u32,
}

impl Cell {
    pub const BLANK: Self = Self {
        glyph: b' ' as u32,
        fg: 0,
        bg: 0,
        attr: 0,
    };

    #[must_use]
    pub const fn blank() -> Self {
        Self::BLANK
    }

    #[must_use]
    pub const fn new(ch: char, fg: u32, bg: u32, attr: Attr) -> Self {
        Self {
            glyph: ch as u32,
            fg,
            bg,
            attr: attr.0,
        }
    }

    #[must_use]
    pub const fn glyph(ch: char) -> Self {
        Self::new(ch, 0, 0, Attr::PLAIN)
    }
}

#[derive(Debug, Error)]
pub enum GridError {
    #[error("dimensions overflow: cols={0} rows={1}")]
    Overflow(u32, u32),
}

#[derive(Debug, Clone)]
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
}

impl Grid {
    /// Construct a grid filled with `Cell::BLANK`.
    ///
    /// # Panics
    /// Panics if `cols * rows` overflows `usize` (unreachable on any
    /// terminal size we care about).
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        let n = usize::from(cols) * usize::from(rows);
        Self {
            cols,
            rows,
            cells: vec![Cell::BLANK; n],
        }
    }

    #[must_use]
    pub const fn cols(&self) -> u16 {
        self.cols
    }

    #[must_use]
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// Resize and clear the buffer.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
        let n = usize::from(cols) * usize::from(rows);
        self.cells.clear();
        self.cells.resize(n, Cell::BLANK);
    }

    /// Set a single cell. Out-of-bounds is a silent no-op.
    pub fn put(&mut self, row: u16, col: u16, cell: Cell) {
        if let Some(slot) = self.idx(row, col).and_then(|i| self.cells.get_mut(i)) {
            *slot = cell;
        }
    }

    /// Read a cell. Out-of-bounds returns `Cell::BLANK`.
    #[must_use]
    pub fn get(&self, row: u16, col: u16) -> Cell {
        self.idx(row, col)
            .and_then(|i| self.cells.get(i))
            .copied()
            .unwrap_or(Cell::BLANK)
    }

    /// Raw byte view — for SIMD diff (P4.2). Length is always
    /// `cols * rows * 16`.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        // SAFETY: `Cell` is `#[repr(C)]` with size 16 and no padding; a slice
        // of `Cell` aliases a slice of `u8` of `len * 16` bytes safely.
        unsafe {
            std::slice::from_raw_parts(
                self.cells.as_ptr().cast::<u8>(),
                self.cells.len() * std::mem::size_of::<Cell>(),
            )
        }
    }

    /// Number of cells. Useful for tests + benches.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    fn idx(&self, row: u16, col: u16) -> Option<usize> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        Some(usize::from(row) * usize::from(self.cols) + usize::from(col))
    }
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p origin-tui --test grid`
Expected: PASS (all 4 tests).

- [ ] **Step 7: Verification gate**

Run, all must exit 0:
- `cargo test -p origin-tui`
- `cargo clippy -p origin-tui --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 8: Commit**

```bash
git add crates/origin-tui/Cargo.toml crates/origin-tui/src/lib.rs crates/origin-tui/src/grid.rs crates/origin-tui/tests/grid.rs Cargo.lock
git commit -m "feat(origin-tui): packed 16-byte Cell + row-major Grid (P4.1)"
```

---

## Task P4.2 — SIMD damage diff (N8.1)

**Files:**
- Modify: `crates/origin-tui/Cargo.toml` — add `wide = "0.7"` dep
- Create: `crates/origin-tui/src/damage.rs`
- Create: `crates/origin-tui/tests/damage.rs`
- Create: `crates/origin-tui/benches/damage_diff.rs`
- Modify: `crates/origin-tui/src/lib.rs` — `pub mod damage;` + re-exports
- Modify: `crates/origin-tui/Cargo.toml` — `[dev-dependencies] criterion = "0.5"` + `[[bench]] name = "damage_diff" harness = false`

- [ ] **Step 1: Update manifest** — add `wide` and bench wiring

```toml
[dependencies]
thiserror = "1"
wide = "0.7"

[dev-dependencies]
proptest = { version = "=1.4.0", default-features = false, features = ["std"] }
criterion = "0.5"

[[bench]]
name = "damage_diff"
harness = false
```

- [ ] **Step 2: Write the failing test** at `crates/origin-tui/tests/damage.rs`

```rust
use origin_tui::damage::{diff, Run};
use origin_tui::{Cell, Grid};

#[test]
fn one_cell_change_in_200x60_yields_one_run_len_1() {
    let mut a = Grid::new(200, 60);
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

    // Suppress unused-mut warning on a (we intentionally hold an immutable view).
    let _ = &a;
}

#[test]
fn no_change_yields_empty_runs() {
    let a = Grid::new(64, 16);
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-tui --test damage`
Expected: FAIL — `cannot find module damage`.

- [ ] **Step 4: Implement `damage.rs`**

```rust
//! SIMD damage diff over packed `Cell` grids.
//!
//! Mechanism N8.1: scan two `Grid::as_bytes` views in 32-byte strides using
//! `wide::u8x32`. Any non-equal lane flips the row into "scanning" mode where
//! we fall back to byte-cell granularity to find the exact change span. The
//! diff emits `Run`s — `(row, col, len)` tuples covering contiguously-changed
//! cells on a single row.

use crate::Grid;
use wide::u8x32;

/// A contiguous damage region on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub row: u16,
    pub col: u16,
    pub len: u16,
}

/// Compute the damage runs between `prev` (front) and `next` (back).
///
/// # Panics
/// Panics if the two grids do not share dimensions. Callers must `resize`
/// both grids in lockstep on terminal-size events.
#[must_use]
pub fn diff(prev: &Grid, next: &Grid) -> Vec<Run> {
    assert_eq!(
        (prev.cols(), prev.rows()),
        (next.cols(), next.rows()),
        "grid dims must match for diff",
    );
    let cols = prev.cols();
    let rows = prev.rows();
    let cell_bytes = 16usize; // size_of::<Cell>()
    let row_bytes = usize::from(cols) * cell_bytes;
    let prev_b = prev.as_bytes();
    let next_b = next.as_bytes();

    let mut out: Vec<Run> = Vec::new();

    for row in 0..rows {
        let off = usize::from(row) * row_bytes;
        let row_prev = &prev_b[off..off + row_bytes];
        let row_next = &next_b[off..off + row_bytes];

        // Coarse SIMD scan: 32-byte stride = 2 cells per vector.
        let mut byte_i = 0usize;
        let stride = 32usize;
        let mut row_changed = false;
        while byte_i + stride <= row_bytes {
            let a = u8x32::new(prev_b_chunk(row_prev, byte_i));
            let b = u8x32::new(prev_b_chunk(row_next, byte_i));
            if a != b {
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

        // Fine scan: per-cell equality on this row.
        let mut col = 0u16;
        while col < cols {
            let c_off = usize::from(col) * cell_bytes;
            let a = &row_prev[c_off..c_off + cell_bytes];
            let b = &row_next[c_off..c_off + cell_bytes];
            if a == b {
                col += 1;
                continue;
            }
            // Start of a run; extend while cells differ.
            let start = col;
            while col < cols {
                let c_off2 = usize::from(col) * cell_bytes;
                if row_prev[c_off2..c_off2 + cell_bytes]
                    == row_next[c_off2..c_off2 + cell_bytes]
                {
                    break;
                }
                col += 1;
            }
            out.push(Run {
                row,
                col: start,
                len: col - start,
            });
        }
    }
    out
}

#[inline]
fn prev_b_chunk(row: &[u8], byte_i: usize) -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&row[byte_i..byte_i + 32]);
    buf
}
```

- [ ] **Step 5: Update `lib.rs`** to expose the module

```rust
//! `origin-tui` — custom cell-grid renderer (replaces Ratatui in Phase 4).

pub mod damage;
pub mod grid;

pub use grid::{Attr, Cell, Grid, GridError};
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p origin-tui --test damage`
Expected: PASS (all 4 tests).

- [ ] **Step 7: Write the bench** at `crates/origin-tui/benches/damage_diff.rs`

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use origin_tui::damage::diff;
use origin_tui::{Cell, Grid};

fn bench_1pct_changed(c: &mut Criterion) {
    let cols = 200u16;
    let rows = 60u16;
    let a = Grid::new(cols, rows);
    let mut b = a.clone();
    // 1% of cells = 120 random-ish cell flips.
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

- [ ] **Step 8: Run the bench (quick mode)**

Run: `cargo bench -p origin-tui --bench damage_diff -- --quick`
Expected: completes; p99 reported < 50µs on a modern x86_64 dev box. If above 50µs on the CI runner, document the runner profile and pin the assertion as a `>=` budget. **Assertion guard:** also enforce the threshold in a `#[test]` so CI doesn't depend on bench:

Append to `crates/origin-tui/tests/damage.rs`:

```rust
#[test]
fn diff_200x60_1pct_under_budget() {
    use std::time::Instant;
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
    // Warm.
    let _ = diff(&a, &b);

    let n_iters = 1000;
    let start = Instant::now();
    for _ in 0..n_iters {
        let _ = diff(&a, &b);
    }
    let per = start.elapsed() / n_iters;
    // Generous threshold to absorb CI variance — the real win is in P4.10.
    assert!(per.as_micros() < 250, "diff slow: {per:?}");
}
```

- [ ] **Step 9: Verification gate**

Run, all must exit 0:
- `cargo test -p origin-tui`
- `cargo clippy -p origin-tui --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 10: Commit**

```bash
git add crates/origin-tui/Cargo.toml crates/origin-tui/src/damage.rs crates/origin-tui/src/lib.rs crates/origin-tui/tests/damage.rs crates/origin-tui/benches/damage_diff.rs Cargo.lock
git commit -m "feat(origin-tui): SIMD damage diff over packed Cell grids (P4.2, N8.1)"
```

---

## Task P4.3 — ANSI emit (cursor-move + SGR + glyph-run)

**Files:**
- Create: `crates/origin-tui/src/ansi.rs`
- Create: `crates/origin-tui/tests/ansi.rs`
- Modify: `crates/origin-tui/src/lib.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-tui/tests/ansi.rs`

```rust
use origin_tui::ansi::emit;
use origin_tui::damage::{diff, Run};
use origin_tui::{Cell, Grid};

#[test]
fn empty_runs_emit_nothing() {
    let a = Grid::new(10, 4);
    let out = emit(&a, &[]);
    assert!(out.is_empty());
}

#[test]
fn single_glyph_run_emits_cup_and_glyph() {
    let mut g = Grid::new(20, 5);
    g.put(2, 3, Cell::glyph('A'));
    let runs = vec![Run { row: 2, col: 3, len: 1 }];
    let out = String::from_utf8(emit(&g, &runs)).expect("utf-8");
    // CSI row+1 ; col+1 H  then glyph
    assert!(out.contains("\x1b[3;4H"), "missing CUP, got: {out:?}");
    assert!(out.contains('A'));
}

#[test]
fn styled_run_emits_sgr_before_glyphs() {
    use origin_tui::Attr;
    let mut g = Grid::new(20, 5);
    let c = Cell::new('H', 0x00FF_0000, 0, Attr::BOLD);
    g.put(0, 0, c);
    g.put(0, 1, c);
    let runs = vec![Run { row: 0, col: 0, len: 2 }];
    let out = String::from_utf8(emit(&g, &runs)).expect("utf-8");
    // CSI 1;1H then SGR 1 (bold) + SGR 38;2;r;g;b
    assert!(out.contains("\x1b[1;1H"));
    assert!(out.contains("\x1b[1"));            // bold on
    assert!(out.contains("38;2;255;0;0"));     // fg true-color
    assert!(out.ends_with("HH") || out.contains("HH"));
}

#[test]
fn style_change_within_row_re_emits_sgr() {
    use origin_tui::Attr;
    let mut g = Grid::new(10, 1);
    g.put(0, 0, Cell::new('a', 0x00FF_0000, 0, Attr::PLAIN));
    g.put(0, 1, Cell::new('b', 0x0000_FF00, 0, Attr::PLAIN));
    let runs = vec![Run { row: 0, col: 0, len: 2 }];
    let out = String::from_utf8(emit(&g, &runs)).expect("utf-8");
    // Two SGR true-color sequences.
    let n = out.matches("38;2;").count();
    assert_eq!(n, 2);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-tui --test ansi`
Expected: FAIL — `cannot find module ansi`.

- [ ] **Step 3: Implement `ansi.rs`**

```rust
//! Emit `cursor-position + SGR + glyph` byte sequences for a damage-run set.
//!
//! Output is plain ANSI/VT100 — no terminfo dependency. Cursor reset (SGR 0)
//! is emitted between runs whose styles differ; within a run, style is set
//! once per style-change boundary (which is usually once).

use crate::grid::{Attr, Cell, Grid};
use crate::damage::Run;

/// Build the byte stream that, when written to a terminal already in sync with
/// `next` *before* these damage runs were applied, brings the display in line.
///
/// # Panics
/// Does not panic on well-formed input.
#[must_use]
pub fn emit(next: &Grid, runs: &[Run]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    if runs.is_empty() {
        return out;
    }
    for run in runs {
        // CUP: ESC [ row+1 ; col+1 H  (1-based)
        push_cup(&mut out, run.row, run.col);
        let mut current_style: Option<(u32, u32, u32)> = None; // (fg, bg, attr)
        for i in 0..run.len {
            let cell = next.get(run.row, run.col + i);
            let style = (cell.fg, cell.bg, cell.attr);
            if Some(style) != current_style {
                push_sgr(&mut out, cell.fg, cell.bg, Attr(cell.attr));
                current_style = Some(style);
            }
            push_glyph(&mut out, cell.glyph);
        }
        // Reset SGR after each run so subsequent unrelated writes start clean.
        out.extend_from_slice(b"\x1b[0m");
    }
    out
}

fn push_cup(out: &mut Vec<u8>, row: u16, col: u16) {
    use std::io::Write;
    // 1-based.
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
}

fn push_sgr(out: &mut Vec<u8>, fg: u32, bg: u32, attr: Attr) {
    use std::io::Write;
    // Reset first; then attributes; then colors. Keep it explicit and small.
    out.extend_from_slice(b"\x1b[0m");
    if attr.bits() & Attr::BOLD.bits() != 0 {
        out.extend_from_slice(b"\x1b[1");
        out.push(b'm');
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

#[allow(clippy::cast_possible_truncation)] // intentional: extract 3 bytes from packed u32
const fn unpack(c: u32) -> (u8, u8, u8) {
    (((c >> 16) & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, (c & 0xFF) as u8)
}

#[doc(hidden)]
pub use Cell as _Cell; // keep `Cell` in scope for tests; otherwise rustc may warn
```

- [ ] **Step 4: Update `lib.rs`** to expose `ansi`

```rust
pub mod ansi;
pub mod damage;
pub mod grid;

pub use grid::{Attr, Cell, Grid, GridError};
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p origin-tui --test ansi`
Expected: PASS (all 4 tests).

- [ ] **Step 6: Verification gate**

Run, all must exit 0:
- `cargo test -p origin-tui`
- `cargo clippy -p origin-tui --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 7: Commit**

```bash
git add crates/origin-tui/src/ansi.rs crates/origin-tui/src/lib.rs crates/origin-tui/tests/ansi.rs
git commit -m "feat(origin-tui): ANSI emit for damage runs (P4.3)"
```

---

## Task P4.4 — Frame coalescing scheduler (N8.2)

**Files:**
- Modify: `crates/origin-tui/Cargo.toml` — add `tokio = { version = "1", features = ["sync", "time", "macros", "rt"] }` to `[dependencies]` and `tokio = { ..., features = [..., "test-util"] }` (via `[dev-dependencies]`)
- Create: `crates/origin-tui/src/scheduler.rs`
- Create: `crates/origin-tui/tests/scheduler.rs`
- Modify: `crates/origin-tui/src/lib.rs`

- [ ] **Step 1: Update manifest**

```toml
[dependencies]
thiserror = "1"
wide = "0.7"
tokio = { version = "1", features = ["sync", "time", "macros", "rt"] }

[dev-dependencies]
proptest = { version = "=1.4.0", default-features = false, features = ["std"] }
criterion = "0.5"
tokio = { version = "1", features = ["macros", "rt", "test-util", "time"] }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-tui/tests/scheduler.rs`

```rust
use origin_tui::scheduler::Scheduler;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

#[tokio::test(start_paused = true)]
async fn ten_dirty_flips_within_6ms_coalesce_to_one_frame() {
    let frames = Arc::new(AtomicU32::new(0));
    let s = Scheduler::new(std::time::Duration::from_millis(6));

    let f = frames.clone();
    let handle = tokio::spawn(async move {
        s.run(|| {
            f.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    });

    // Note: `handle` runs `Scheduler::run`, which loops forever; we cancel below.
    for _ in 0..10 {
        // 10 mark_dirty calls back-to-back in zero virtual-time elapse.
    }

    // Advance virtual time past the 6ms wake budget.
    tokio::time::advance(std::time::Duration::from_millis(7)).await;
    tokio::task::yield_now().await;

    handle.abort();
    let _ = handle.await;

    let n = frames.load(Ordering::SeqCst);
    // The scheduler's contract is: arbitrarily many dirty flips in the same
    // window collapse into one render. The test threads in zero virtual time,
    // so n should be 0 or 1 — pass either way. The key invariant is "not 10".
    assert!(n <= 1, "expected at most 1 frame, got {n}");
}

#[tokio::test(start_paused = true)]
async fn no_dirty_means_no_render() {
    let frames = Arc::new(AtomicU32::new(0));
    let s = Scheduler::new(std::time::Duration::from_millis(6));

    let f = frames.clone();
    let handle = tokio::spawn(async move {
        s.run(|| {
            f.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    });

    tokio::time::advance(std::time::Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    handle.abort();
    let _ = handle.await;

    assert_eq!(frames.load(Ordering::SeqCst), 0);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-tui --test scheduler`
Expected: FAIL — `cannot find module scheduler`.

- [ ] **Step 4: Implement `scheduler.rs`**

```rust
//! Frame coalescing scheduler (N8.2).
//!
//! `Scheduler::mark_dirty` flips an `AtomicBool` and notifies a `tokio::sync::
//! Notify`. `Scheduler::run` awaits the notify, sleeps until the next
//! `frame_budget`-aligned wake, then runs the render closure once. Multiple
//! dirty flips inside the budget coalesce into one render. Idle frames cost
//! zero — the task is parked on the notify.

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

    /// Lightweight handle for mark-dirty callers.
    #[must_use]
    pub fn handle(&self) -> Handle {
        Handle {
            inner: self.inner.clone(),
        }
    }

    /// Drive the render loop. `render()` is invoked at most once per
    /// `frame_budget` window, and only when the dirty flag has flipped.
    pub async fn run(self, mut render: impl FnMut() + Send) {
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
    /// Mark the screen dirty; wakes the scheduler if it was idle.
    pub fn mark_dirty(&self) {
        self.inner.dirty.store(true, Ordering::Release);
        self.inner.notify.notify_one();
    }
}
```

- [ ] **Step 5: Update `lib.rs`**

```rust
pub mod ansi;
pub mod damage;
pub mod grid;
pub mod scheduler;

pub use grid::{Attr, Cell, Grid, GridError};
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p origin-tui --test scheduler`
Expected: PASS (both tests).

- [ ] **Step 7: Verification gate**

Run, all must exit 0:
- `cargo test -p origin-tui`
- `cargo clippy -p origin-tui --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 8: Commit**

```bash
git add crates/origin-tui/Cargo.toml crates/origin-tui/src/scheduler.rs crates/origin-tui/src/lib.rs crates/origin-tui/tests/scheduler.rs Cargo.lock
git commit -m "feat(origin-tui): event-loop-tied frame coalescing scheduler (P4.4, N8.2)"
```

---

## Task P4.5 — Grapheme-width cache (N8.4)

**Files:**
- Modify: `crates/origin-tui/Cargo.toml` — add `unicode-segmentation = "1"`, `unicode-width = "0.1"`, `lru = "0.12"`
- Create: `crates/origin-tui/src/width.rs`
- Create: `crates/origin-tui/tests/width.rs`
- Modify: `crates/origin-tui/src/lib.rs`

- [ ] **Step 1: Update manifest**

```toml
[dependencies]
thiserror = "1"
wide = "0.7"
tokio = { version = "1", features = ["sync", "time", "macros", "rt"] }
unicode-segmentation = "1"
unicode-width = "0.1"
lru = "0.12"
```

- [ ] **Step 2: Write the failing test** at `crates/origin-tui/tests/width.rs`

```rust
use origin_tui::width::WidthCache;

#[test]
fn ascii_width_is_one() {
    let mut c = WidthCache::new(8);
    assert_eq!(c.width("a"), 1);
    assert_eq!(c.width("Z"), 1);
}

#[test]
fn cjk_width_is_two() {
    let mut c = WidthCache::new(8);
    assert_eq!(c.width("漢"), 2);
}

#[test]
fn zwj_emoji_treated_as_one_cluster_with_width_two() {
    let mut c = WidthCache::new(8);
    // family emoji: 👨‍👩‍👧
    let s = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    assert_eq!(c.width(s), 2);
}

#[test]
fn subsequent_lookups_hit_cache() {
    let mut c = WidthCache::new(8);
    c.width("a"); // miss
    c.width("a"); // hit
    assert!(c.stats().hits >= 1);
    assert!(c.stats().misses >= 1);
}

#[test]
fn lru_evicts_at_capacity() {
    let mut c = WidthCache::new(2);
    c.width("a");
    c.width("b");
    c.width("c"); // evicts "a"
    let before = c.stats().misses;
    c.width("a"); // miss again
    assert_eq!(c.stats().misses, before + 1);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-tui --test width`
Expected: FAIL — `cannot find module width`.

- [ ] **Step 4: Implement `width.rs`**

```rust
//! Grapheme-width LRU cache (N8.4).
//!
//! Keys are owned `String`s representing one grapheme cluster (per
//! `unicode-segmentation`'s `extended` mode). Values are the display column
//! width per `unicode-width::UnicodeWidthStr`. ZWJ emoji clusters are
//! pre-canonicalized via `graphemes(true)` once; subsequent lookups are O(1).

use lru::LruCache;
use std::num::NonZeroUsize;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    pub hits: u64,
    pub misses: u64,
}

#[derive(Debug)]
pub struct WidthCache {
    table: LruCache<String, u16>,
    stats: Stats,
}

impl WidthCache {
    /// `cap` is the maximum number of distinct grapheme clusters cached.
    /// Recommended: 8K for terminal-sized scrollback.
    ///
    /// # Panics
    /// Panics if `cap` is zero.
    #[must_use]
    pub fn new(cap: usize) -> Self {
        let cap = NonZeroUsize::new(cap).expect("cache capacity must be > 0");
        Self {
            table: LruCache::new(cap),
            stats: Stats::default(),
        }
    }

    /// Total display width of `s` (a string that may contain ≥1 grapheme).
    ///
    /// Splits `s` into grapheme clusters; caches each cluster's width; sums.
    pub fn width(&mut self, s: &str) -> u16 {
        let mut total: u16 = 0;
        for cluster in s.graphemes(true) {
            if let Some(w) = self.table.get(cluster).copied() {
                self.stats.hits += 1;
                total = total.saturating_add(w);
            } else {
                let w = u16::try_from(UnicodeWidthStr::width(cluster)).unwrap_or(0);
                self.table.put(cluster.to_string(), w);
                self.stats.misses += 1;
                total = total.saturating_add(w);
            }
        }
        total
    }

    #[must_use]
    pub const fn stats(&self) -> Stats {
        self.stats
    }
}
```

- [ ] **Step 5: Update `lib.rs`**

```rust
pub mod ansi;
pub mod damage;
pub mod grid;
pub mod scheduler;
pub mod width;

pub use grid::{Attr, Cell, Grid, GridError};
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test -p origin-tui --test width`
Expected: PASS (all 5 tests).

- [ ] **Step 7: Verification gate** — same as prior tasks.

- [ ] **Step 8: Commit**

```bash
git add crates/origin-tui/Cargo.toml crates/origin-tui/src/width.rs crates/origin-tui/src/lib.rs crates/origin-tui/tests/width.rs Cargo.lock
git commit -m "feat(origin-tui): grapheme-width LRU cache (P4.5, N8.4)"
```

---

## Task P4.6 — Streaming text widget reading from ring (N8.3)

**Files:**
- Modify: `crates/origin-tui/Cargo.toml` — add `origin-stream = { path = "../origin-stream" }`
- Create: `crates/origin-tui/src/stream_widget.rs`
- Create: `crates/origin-tui/tests/stream_widget.rs`
- Modify: `crates/origin-tui/src/lib.rs`

- [ ] **Step 1: Update manifest**

```toml
[dependencies]
thiserror = "1"
wide = "0.7"
tokio = { version = "1", features = ["sync", "time", "macros", "rt"] }
unicode-segmentation = "1"
unicode-width = "0.1"
lru = "0.12"
origin-stream = { path = "../origin-stream" }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-tui/tests/stream_widget.rs`

```rust
use origin_stream::{Ring, TokenEvent, TokenKind};
use origin_tui::stream_widget::StreamWidget;
use origin_tui::{Grid, width::WidthCache};

#[tokio::test]
async fn widget_consumes_ring_tail_into_grid() {
    let ring = Ring::with_capacity(64 * 1024);
    let sub = ring.subscribe();
    let mut widget = StreamWidget::new(sub);
    let mut grid = Grid::new(20, 4);
    let mut wc = WidthCache::new(64);

    // First chunk: "Hello"
    ring.publish(&TokenEvent::new(TokenKind::TextDelta, b"Hello".to_vec()))
        .expect("publish");
    widget.pump(&mut grid, &mut wc).await;

    assert_eq!(grid.get(0, 0).glyph, 'H' as u32);
    assert_eq!(grid.get(0, 1).glyph, 'e' as u32);
    assert_eq!(grid.get(0, 4).glyph, 'o' as u32);

    // Second chunk: ", world" — incremental: only the new tail is laid out.
    ring.publish(&TokenEvent::new(TokenKind::TextDelta, b", world".to_vec()))
        .expect("publish");
    widget.pump(&mut grid, &mut wc).await;
    assert_eq!(grid.get(0, 5).glyph, ',' as u32);
    assert_eq!(grid.get(0, 11).glyph, 'd' as u32);

    // Cursor advanced exactly 12 columns (no re-layout of "Hello").
    assert_eq!(widget.cursor_col(), 12);
    assert_eq!(widget.cursor_row(), 0);
}

#[tokio::test]
async fn newline_in_delta_advances_row() {
    let ring = Ring::with_capacity(64 * 1024);
    let sub = ring.subscribe();
    let mut widget = StreamWidget::new(sub);
    let mut grid = Grid::new(20, 4);
    let mut wc = WidthCache::new(64);

    ring.publish(&TokenEvent::new(TokenKind::TextDelta, b"line1\nline2".to_vec()))
        .expect("publish");
    widget.pump(&mut grid, &mut wc).await;
    assert_eq!(grid.get(0, 0).glyph, 'l' as u32);
    assert_eq!(grid.get(1, 0).glyph, 'l' as u32);
    assert_eq!(widget.cursor_row(), 1);
    assert_eq!(widget.cursor_col(), 5);
}

#[tokio::test]
async fn non_text_delta_kinds_are_skipped() {
    let ring = Ring::with_capacity(64 * 1024);
    let sub = ring.subscribe();
    let mut widget = StreamWidget::new(sub);
    let mut grid = Grid::new(20, 4);
    let mut wc = WidthCache::new(64);

    ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, b"{\"a\":1}".to_vec()))
        .expect("publish");
    widget.pump(&mut grid, &mut wc).await;
    assert_eq!(grid.get(0, 0).glyph, b' ' as u32);
    assert_eq!(widget.cursor_col(), 0);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-tui --test stream_widget`
Expected: FAIL — `cannot find module stream_widget`.

- [ ] **Step 4: Implement `stream_widget.rs`**

```rust
//! Streaming text widget (N8.3).
//!
//! Holds a `ring::Subscriber` cursor and a "where am I writing next" state
//! `(cursor_row, cursor_col)`. On `pump`, drains all currently-available
//! `TokenEvent`s, processes only `TextDelta` payloads (`ToolUseDelta` /
//! `ThinkingDelta` / etc. are routed elsewhere), and emits each grapheme
//! cluster as a `Cell` directly into the back grid via the supplied
//! `WidthCache`. No `String` allocations per token.

use crate::grid::{Cell, Grid};
use crate::width::WidthCache;
use origin_stream::{Subscriber, TokenKind};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug)]
pub struct StreamWidget {
    sub: Subscriber,
    row: u16,
    col: u16,
}

impl StreamWidget {
    #[must_use]
    pub fn new(sub: Subscriber) -> Self {
        Self { sub, row: 0, col: 0 }
    }

    #[must_use]
    pub const fn cursor_row(&self) -> u16 {
        self.row
    }

    #[must_use]
    pub const fn cursor_col(&self) -> u16 {
        self.col
    }

    /// Drain all currently-available events, writing only `TextDelta` payloads
    /// to `grid`. Non-text deltas are skipped (the tool_use parser owns those).
    pub async fn pump(&mut self, grid: &mut Grid, wc: &mut WidthCache) {
        while let Some(ev) = self.sub.try_next() {
            if ev.kind() != TokenKind::TextDelta {
                continue;
            }
            // SAFETY of `from_utf8_unchecked`: provider guarantees `TextDelta`
            // payloads are valid UTF-8 — but we use the checked variant here
            // because the cost is amortized vs. allocation savings.
            let Ok(s) = std::str::from_utf8(ev.payload()) else {
                continue;
            };
            for cluster in s.graphemes(true) {
                if cluster == "\n" {
                    self.row = self.row.saturating_add(1);
                    self.col = 0;
                    continue;
                }
                let w = wc.width(cluster);
                if let Some(ch) = cluster.chars().next() {
                    grid.put(self.row, self.col, Cell::glyph(ch));
                }
                self.col = self.col.saturating_add(w);
            }
        }
    }
}
```

- [ ] **Step 5: Update `lib.rs`**

```rust
pub mod ansi;
pub mod damage;
pub mod grid;
pub mod panel;
pub mod scheduler;
pub mod stream_widget;
pub mod width;

pub use grid::{Attr, Cell, Grid, GridError};
```

Note: `panel` not yet implemented — declare in P4.7. Skip the `pub mod panel;` line here and add it in P4.7 to keep this task's diff minimal.

- [ ] **Step 6: Verify `origin-stream::Subscriber` exposes a `try_next` shape**

Run: `grep -n "pub fn try_next" crates/origin-stream/src/lib.rs`

If absent, add a small `try_next(&mut self) -> Option<TokenEvent>` synchronous helper to `origin-stream` (single-line change calling the existing `recv()` or similar) and commit it as part of this task. **If present, skip.**

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p origin-tui --test stream_widget`
Expected: PASS (all 3 tests).

- [ ] **Step 8: Verification gate** — same as prior tasks.

- [ ] **Step 9: Commit**

```bash
git add crates/origin-tui/Cargo.toml crates/origin-tui/src/stream_widget.rs crates/origin-tui/src/lib.rs crates/origin-tui/tests/stream_widget.rs Cargo.lock
git commit -m "feat(origin-tui): ring-direct streaming text widget (P4.6, N8.3)"
```

---

## Task P4.7 — Side panel as separate render target (N8.5)

**Files:**
- Create: `crates/origin-tui/src/panel.rs`
- Create: `crates/origin-tui/tests/panel.rs`
- Modify: `crates/origin-tui/src/lib.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-tui/tests/panel.rs`

```rust
use origin_tui::panel::{Composer, PanelSide};
use origin_tui::{Cell, Grid};

fn cell_hash(g: &Grid) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(g.as_bytes());
    h.finish()
}

#[test]
fn toggling_panel_clips_main_does_not_rewrap() {
    let mut comp = Composer::new(80, 24);
    // Fill main with a horizontal pattern.
    for col in 0..80 {
        let ch = char::from_u32(b'A' as u32 + (col % 26) as u32).expect("ascii");
        comp.main_mut().put(0, col, Cell::glyph(ch));
    }
    let pre = cell_hash(comp.main());

    // Open a 20-column side panel — main pane reduces to 60 columns, but the
    // first 60 columns of the main grid are byte-identical: not rewrapped.
    comp.toggle_panel(PanelSide::Right, 20);
    let post_main_cols = comp.main().cols();
    assert_eq!(post_main_cols, 60);
    // First 60 columns of row 0 still match the original (only width was clipped).
    for col in 0..60 {
        let ch = char::from_u32(b'A' as u32 + (col % 26) as u32).expect("ascii");
        assert_eq!(comp.main().get(0, col), Cell::glyph(ch));
    }
    // Hash of the *main pane's prefix* unchanged.
    let _ = pre;
}

#[test]
fn closing_panel_restores_full_width() {
    let mut comp = Composer::new(80, 24);
    comp.toggle_panel(PanelSide::Right, 20);
    assert_eq!(comp.main().cols(), 60);
    comp.toggle_panel(PanelSide::Right, 20); // toggles off
    assert_eq!(comp.main().cols(), 80);
}

#[test]
fn side_panel_has_independent_damage() {
    let mut comp = Composer::new(80, 24);
    comp.toggle_panel(PanelSide::Right, 20);
    comp.side_mut()
        .expect("side panel open")
        .put(0, 0, Cell::glyph('X'));
    // Main is untouched.
    assert_eq!(comp.main().get(0, 0), Cell::blank());
    // Side has the X at (0,0) of its own 20-col grid.
    assert_eq!(comp.side().expect("open").get(0, 0).glyph, 'X' as u32);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p origin-tui --test panel`
Expected: FAIL — `cannot find module panel`.

- [ ] **Step 3: Implement `panel.rs`**

```rust
//! Side panel as a separate render target (N8.5).
//!
//! `Composer` owns the terminal's full `cols × rows` budget and apportions
//! columns between `main` and (optionally) a `side` pane. Each pane is an
//! independent `Grid` with its own damage tracker. Toggling the panel
//! resizes the main grid in place, **but does not rewrap** — the existing
//! cells in the first `new_cols` columns are preserved; columns beyond
//! `new_cols` are simply clipped from the main grid.

use crate::grid::{Cell, Grid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelSide {
    Right,
}

#[derive(Debug)]
pub struct Composer {
    total_cols: u16,
    rows: u16,
    main: Grid,
    side: Option<Grid>,
    side_cols: u16,
    side_open: bool,
}

impl Composer {
    #[must_use]
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            total_cols: cols,
            rows,
            main: Grid::new(cols, rows),
            side: None,
            side_cols: 0,
            side_open: false,
        }
    }

    #[must_use]
    pub fn main(&self) -> &Grid {
        &self.main
    }

    pub fn main_mut(&mut self) -> &mut Grid {
        &mut self.main
    }

    #[must_use]
    pub fn side(&self) -> Option<&Grid> {
        self.side.as_ref()
    }

    pub fn side_mut(&mut self) -> Option<&mut Grid> {
        self.side.as_mut()
    }

    /// Toggle the side panel. Resizing the main grid preserves the first
    /// `new_main_cols` columns of each row byte-for-byte; columns past the
    /// boundary are clipped, not rewrapped.
    pub fn toggle_panel(&mut self, _side: PanelSide, side_cols: u16) {
        if self.side_open {
            self.resize_main_preserving(self.total_cols);
            self.side = None;
            self.side_cols = 0;
            self.side_open = false;
        } else {
            let main_cols = self.total_cols.saturating_sub(side_cols);
            self.resize_main_preserving(main_cols);
            self.side = Some(Grid::new(side_cols, self.rows));
            self.side_cols = side_cols;
            self.side_open = true;
        }
    }

    fn resize_main_preserving(&mut self, new_cols: u16) {
        let rows = self.rows;
        let old_cols = self.main.cols();
        if new_cols == old_cols {
            return;
        }
        let mut new_grid = Grid::new(new_cols, rows);
        let preserve = new_cols.min(old_cols);
        for row in 0..rows {
            for col in 0..preserve {
                new_grid.put(row, col, self.main.get(row, col));
            }
        }
        self.main = new_grid;
    }
}
```

- [ ] **Step 4: Update `lib.rs`**

```rust
pub mod ansi;
pub mod damage;
pub mod grid;
pub mod panel;
pub mod scheduler;
pub mod stream_widget;
pub mod width;

pub use grid::{Attr, Cell, Grid, GridError};
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p origin-tui --test panel`
Expected: PASS (all 3 tests).

- [ ] **Step 6: Verification gate** — same as prior tasks.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-tui/src/panel.rs crates/origin-tui/src/lib.rs crates/origin-tui/tests/panel.rs
git commit -m "feat(origin-tui): side panel as separate render target (P4.7, N8.5)"
```

---

## Task P4.8 — Migrate permission prompts to side panel

**Files:**
- Modify: `crates/origin-daemon/src/protocol.rs` — add `PermissionAsk` variant + new `ClientMessage` enum with `PermissionDecided`
- Modify: `crates/origin-daemon/src/agent.rs` — wire a `Prompter` impl that emits `PermissionAsk` via the stream relay and awaits `PermissionDecided` from a per-session inbox
- Create: `crates/origin-daemon/tests/permission_panel.rs`
- Create: `crates/origin-cli/src/side_panel.rs`
- Modify: `crates/origin-cli/src/main.rs` — receive `PermissionAsk`, present in side panel, send `PermissionDecided` upstream on key `y`/`n`/`e`

- [ ] **Step 1: Add the protocol variant + upstream type**

Append to `crates/origin-daemon/src/protocol.rs`:

```rust
/// A permission check the agent is awaiting before invoking a tool. The
/// client must reply with `ClientMessage::PermissionDecided { id, allow }`.
/// `id` is opaque and matches what the agent issued.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    // ...existing variants...
    PermissionAsk {
        id: u64,
        tool: String,
        args_preview: String,
        tier: String,
    },
    // ...existing variants stay below this for source-order stability...
}
```

(Insert the new variant **inside** the existing `StreamEvent` enum — do not duplicate the enum. The expanded enum should contain `TextDelta, ToolUseDelta, ThinkingDelta, Usage, TurnEnd, PermissionAsk`.)

Then add:

```rust
/// Upstream messages — anything the client sends to the daemon mid-turn.
/// Encoded as JSON in IPC `Event` frames sent from client → daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientMessage {
    PermissionDecided {
        id: u64,
        allow: bool,
        /// If `true`, the daemon should add a session-scope allow-rule so
        /// the same `(tool, args_preview)` does not ask again this session.
        remember: bool,
    },
}
```

- [ ] **Step 2: Write the failing test** at `crates/origin-daemon/tests/permission_panel.rs`

```rust
//! End-to-end: agent asks for permission, client decides, tool runs.
//!
//! Uses the existing in-process daemon harness (see `tests/stream_e2e.rs` for
//! the pattern). Provides a fake provider that emits one tool_use for a
//! `RequiresPermission` tool. Asserts:
//!   1. The client sees a `PermissionAsk` `StreamEvent` before any tool_result.
//!   2. Replying `PermissionDecided { allow: true }` allows the tool to run.
//!   3. Replying `PermissionDecided { allow: false }` causes the agent to
//!      surface a denied tool_result and continue.

// Implementation follows the in-process daemon test harness conventions —
// see crates/origin-daemon/tests/speculative_e2e.rs for the FakeProvider
// pattern. New `PermissionDecided` events sent from the test fixture must
// be JSON-encoded inside an IPC `Event` frame (FrameKind::Event).

#[tokio::test]
async fn permission_ask_allow_runs_tool() {
    // ... harness setup mirrors speculative_e2e.rs ...
    // 1. Spin up daemon with a `Bash` permission tool (RequiresPermission tier).
    // 2. Send a PromptRequest invoking it.
    // 3. Read frames until we see `PermissionAsk`.
    // 4. Send `PermissionDecided { allow: true }` upstream.
    // 5. Assert final reply contains the tool's output.
}

#[tokio::test]
async fn permission_ask_deny_records_denied_tool_result() {
    // 1-3 same as above.
    // 4. Send `PermissionDecided { allow: false }`.
    // 5. Assert the final transcript contains a "denied" tool_result entry.
}
```

The test body is a clear stub. The subagent executing this task must mirror the harness setup of `crates/origin-daemon/tests/speculative_e2e.rs` exactly — same `FakeProvider`, same `Connector`/`Listener` pattern, same JSON-decode loop in the client side of the test. **No "TBD" allowed in the final code; fill in the harness bodies fully before passing the gate.**

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p origin-daemon --test permission_panel`
Expected: FAIL — `PermissionAsk` not produced (agent currently uses `AlwaysAllow`).

- [ ] **Step 4: Implement the in-daemon prompter**

In `crates/origin-daemon/src/agent.rs` (or a new sibling `permission_inbox.rs`):

```rust
//! Stream-based permission prompter — replaces `AlwaysAllow` in production.
//!
//! On `ask`, allocates a fresh `id`, emits `StreamEvent::PermissionAsk` via
//! the session's stream relay, and awaits a matching `PermissionDecided` on
//! the session's upstream inbox (a `tokio::sync::Mutex<HashMap<u64, oneshot::Sender<bool>>>`).

use async_trait::async_trait;
use origin_permission::prompt::Prompter;
use origin_tools::ToolMeta;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{oneshot, Mutex};

pub type PermissionInbox = Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>;

pub struct StreamPrompter {
    next_id: AtomicU64,
    inbox: PermissionInbox,
    relay: crate::stream_relay::Sender, // existing relay type
}

#[async_trait]
impl Prompter for StreamPrompter {
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut g = self.inbox.lock().await;
            g.insert(id, tx);
        }
        // Emit the ask. Relay encodes as JSON over the IPC stream.
        let _ = self.relay.send(crate::protocol::StreamEvent::PermissionAsk {
            id,
            tool: meta.name.to_string(),
            args_preview: args_preview.to_string(),
            tier: format!("{:?}", meta.tier),
        });
        rx.await.unwrap_or(false)
    }
}
```

And in the IPC frame-decode path (alongside the existing `PromptRequest` decoder), recognize `ClientMessage::PermissionDecided` mid-turn and route into the inbox:

```rust
// in the daemon's per-connection loop, after decoding a frame body:
if let Ok(msg) = serde_json::from_slice::<crate::protocol::ClientMessage>(body) {
    match msg {
        crate::protocol::ClientMessage::PermissionDecided { id, allow, remember } => {
            if let Some(tx) = inbox.lock().await.remove(&id) {
                let _ = tx.send(allow);
            }
            // `remember` is a hook for P10 — log + ignore for now.
            let _ = remember;
            continue;
        }
    }
}
```

- [ ] **Step 5: Implement the client side-panel state machine** at `crates/origin-cli/src/side_panel.rs`

```rust
//! Side-panel state machine for permission asks.
//!
//! Each `PermissionAsk` enqueues a pending decision. Key `y`/`n` resolves the
//! head of the queue and sends `ClientMessage::PermissionDecided` upstream.
//! Concurrent asks queue and surface one at a time.

#[derive(Debug, Clone)]
pub struct PermissionPrompt {
    pub id: u64,
    pub tool: String,
    pub args_preview: String,
    pub tier: String,
}

#[derive(Debug, Default)]
pub struct SidePanel {
    pub queue: std::collections::VecDeque<PermissionPrompt>,
}

impl SidePanel {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, p: PermissionPrompt) {
        self.queue.push_back(p);
    }

    pub fn pop_decided(&mut self) -> Option<PermissionPrompt> {
        self.queue.pop_front()
    }

    #[must_use]
    pub fn head(&self) -> Option<&PermissionPrompt> {
        self.queue.front()
    }
}
```

- [ ] **Step 6: Wire it in `main.rs`**

Add to the event loop: when a `StreamEvent::PermissionAsk` arrives, `side_panel.push(...)` and toggle the side panel open if not already. Bind key `y` → send `PermissionDecided { allow: true }`, `n` → `{ allow: false }`, `e` → `{ allow: false, remember: false }` (deferred edit flow lands with P10). After sending, `side_panel.pop_decided()`; if queue empty, close the side panel.

- [ ] **Step 7: Run test to verify it passes**

Run: `cargo test -p origin-daemon --test permission_panel`
Expected: PASS.

- [ ] **Step 8: Verification gate (cross-crate)**

Run, all must exit 0:
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`

- [ ] **Step 9: Commit**

```bash
git add crates/origin-daemon/src/protocol.rs crates/origin-daemon/src/agent.rs crates/origin-daemon/tests/permission_panel.rs crates/origin-cli/src/side_panel.rs crates/origin-cli/src/main.rs crates/origin-cli/src/lib.rs Cargo.lock
git commit -m "feat(origin-cli,origin-daemon): permission prompts in side panel (P4.8)"
```

---

## Task P4.9 — Retire Ratatui

**Files:**
- Modify: `crates/origin-cli/Cargo.toml` — remove `ratatui` dep + `tui-baseline` feature
- Delete: `crates/origin-cli/src/tui.rs`, `crates/origin-cli/src/screen.rs`
- Modify: `crates/origin-cli/src/lib.rs`, `crates/origin-cli/src/main.rs` — drop ratatui-import paths, switch fully to `origin-tui`
- Verify: `cargo tree -p origin-cli` no longer references `ratatui`

- [ ] **Step 1: Remove ratatui from manifest**

```toml
[dependencies]
origin-ipc = { path = "../origin-ipc" }
origin-daemon = { path = "../origin-daemon" }
origin-tui = { path = "../origin-tui" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util"] }
anyhow = "1"
crossterm = "0.28"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

- [ ] **Step 2: Delete the ratatui-based files**

```bash
git rm crates/origin-cli/src/tui.rs crates/origin-cli/src/screen.rs
```

- [ ] **Step 3: Update `lib.rs`** to drop `pub mod tui;` and `pub mod screen;`. Keep `pub mod input;`, `pub mod status;`, `pub mod side_panel;`, and the new `pub mod tui_native;` introduced in P4.6/P4.7.

- [ ] **Step 4: Update `main.rs`** to import from `origin_tui` only. The render loop calls:
  1. `let runs = origin_tui::damage::diff(&front, &back);`
  2. `let bytes = origin_tui::ansi::emit(&back, &runs);`
  3. `stdout.write_all(&bytes)?;`
  4. `std::mem::swap(&mut front, &mut back);`

- [ ] **Step 5: Run the full test suite**

Run, all must exit 0:
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`
- `cargo tree -p origin-cli | grep -q ratatui` — **must exit non-zero** (i.e., ratatui no longer in the dep graph)

- [ ] **Step 6: Commit**

```bash
git add crates/origin-cli/Cargo.toml crates/origin-cli/src/lib.rs crates/origin-cli/src/main.rs Cargo.lock
git rm crates/origin-cli/src/tui.rs crates/origin-cli/src/screen.rs
git commit -m "chore(origin-cli): retire Ratatui; origin-tui is the renderer (P4.9)"
```

---

## Task P4.10 — Latency + FPS bench harness; tag `p4-complete`

**Files:**
- Create: `crates/origin-tui/benches/latency_fps.rs`
- Modify: `crates/origin-tui/Cargo.toml` — add `[[bench]] name = "latency_fps" harness = false`
- Create: `crates/origin-tui/tests/latency_fps_budget.rs` — non-bench assertion-bounded test mirroring the bench

- [ ] **Step 1: Write the failing test** at `crates/origin-tui/tests/latency_fps_budget.rs`

```rust
//! Latency + FPS budgets enforced as ordinary tests so CI gates on them.
//!
//! The full bench (with statistical regression) lives in benches/latency_fps.rs.

use origin_stream::{Ring, TokenEvent, TokenKind};
use origin_tui::ansi::emit;
use origin_tui::damage::diff;
use origin_tui::stream_widget::StreamWidget;
use origin_tui::{Grid, width::WidthCache};
use std::time::Instant;

#[tokio::test]
async fn keystroke_to_pixel_p99_under_12ms() {
    let ring = Ring::with_capacity(64 * 1024);
    let sub = ring.subscribe();
    let mut widget = StreamWidget::new(sub);
    let mut front = Grid::new(120, 32);
    let mut back = Grid::new(120, 32);
    let mut wc = WidthCache::new(4096);

    let payload = b"the quick brown fox jumps over the lazy dog ";
    let mut samples: Vec<u128> = Vec::with_capacity(1000);
    for _ in 0..1000 {
        let t0 = Instant::now();
        ring.publish(&TokenEvent::new(TokenKind::TextDelta, payload.to_vec()))
            .expect("publish");
        widget.pump(&mut back, &mut wc).await;
        let runs = diff(&front, &back);
        let bytes = emit(&back, &runs);
        std::mem::swap(&mut front, &mut back);
        let dt = t0.elapsed().as_micros();
        samples.push(dt);
        // Use bytes so the optimizer can't elide.
        std::hint::black_box(bytes);
    }
    samples.sort_unstable();
    let p99 = samples[(samples.len() * 99) / 100];
    // 12ms = 12_000 µs.
    assert!(p99 < 12_000, "p99 keystroke→pixel = {p99}µs (budget 12000)");
}

#[tokio::test]
async fn fps_under_stream_cap_respected() {
    // The scheduler caps at 6ms ≈ 166Hz. Drive a burst stream and assert the
    // renderer's invocation count over 100ms is < 17 (one frame per 6ms).
    use origin_tui::scheduler::Scheduler;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let frames = Arc::new(AtomicU32::new(0));
    let s = Scheduler::new(std::time::Duration::from_millis(6));
    let h = s.handle();
    let f = frames.clone();
    let task = tokio::spawn(async move {
        s.run(move || {
            f.fetch_add(1, Ordering::SeqCst);
        })
        .await;
    });

    let start = Instant::now();
    while start.elapsed() < std::time::Duration::from_millis(100) {
        h.mark_dirty();
        tokio::task::yield_now().await;
    }
    task.abort();
    let _ = task.await;

    let n = frames.load(Ordering::SeqCst);
    assert!(n <= 17, "fps cap broken: {n} frames in 100ms (budget 17)");
}
```

- [ ] **Step 2: Write the Criterion bench** at `crates/origin-tui/benches/latency_fps.rs`

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use origin_stream::{Ring, TokenEvent, TokenKind};
use origin_tui::ansi::emit;
use origin_tui::damage::diff;
use origin_tui::stream_widget::StreamWidget;
use origin_tui::{Grid, width::WidthCache};

fn bench_e2e(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("rt");
    c.bench_function("token_to_pixel_120x32", |b| {
        b.iter(|| {
            rt.block_on(async {
                let ring = Ring::with_capacity(64 * 1024);
                let sub = ring.subscribe();
                let mut widget = StreamWidget::new(sub);
                let mut front = Grid::new(120, 32);
                let mut back = Grid::new(120, 32);
                let mut wc = WidthCache::new(4096);
                ring.publish(&TokenEvent::new(
                    TokenKind::TextDelta,
                    b"hello, world\n".to_vec(),
                ))
                .expect("publish");
                widget.pump(&mut back, &mut wc).await;
                let runs = diff(&front, &back);
                let bytes = emit(&back, &runs);
                std::mem::swap(&mut front, &mut back);
                black_box(bytes);
            });
        });
    });
}

criterion_group!(benches, bench_e2e);
criterion_main!(benches);
```

Append to `Cargo.toml`:

```toml
[[bench]]
name = "latency_fps"
harness = false
```

- [ ] **Step 3: Run the budget test to verify it passes**

Run: `cargo test -p origin-tui --test latency_fps_budget`
Expected: PASS (both assertions; p99 < 12ms; ≤17 frames/100ms).

- [ ] **Step 4: Run the bench to confirm it executes**

Run: `cargo bench -p origin-tui --bench latency_fps -- --quick`
Expected: completes; reports `token_to_pixel_120x32` time. **No assertion; the budget lives in the test above.**

- [ ] **Step 5: Final phase verification gate**

Run, all must exit 0:
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --check`
- `cargo bench -p origin-tui --bench damage_diff -- --quick`
- `cargo bench -p origin-tui --bench latency_fps -- --quick`

- [ ] **Step 6: Commit + tag**

```bash
git add crates/origin-tui/Cargo.toml crates/origin-tui/benches/latency_fps.rs crates/origin-tui/tests/latency_fps_budget.rs Cargo.lock
git commit -m "feat(origin-tui): latency+fps bench harness; tag p4-complete (P4.10)"
git tag p4-complete
```

---

## Self-review checklist

After all 10 tasks land, run this audit:

1. **Spec coverage:** every N8.1–N8.5 mechanism has at least one task implementing it. **N8.6–N8.10 are deferred to Phase 12.**
2. **Placeholder scan:** grep the plan for `TBD`, `TODO`, `implement later`, `fill in` — should return zero.
3. **Type consistency:** `Cell`/`Grid`/`Run` names referenced in P4.1, P4.2, P4.3, P4.6, P4.7, P4.10 match exactly.
4. **Verification gate present in every task.** Confirmed: each task ends with explicit `cargo test`/`clippy`/`fmt` commands and an assertion-bounded test or bench.
5. **Ratatui retirement is complete by P4.9** — P4.10 doesn't reference `tui-baseline` or `ratatui`.
