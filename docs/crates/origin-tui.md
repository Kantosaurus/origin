# origin-tui

> Custom cell-grid terminal renderer with SIMD damage diffing and ANSI emit.

## Purpose

`origin-tui` is `origin`'s hand-rolled terminal renderer — the Phase 4
replacement for Ratatui. It models the screen as a packed cell grid, computes the
minimal set of changed spans between two frames with a SIMD damage diff, and
emits only those spans as ANSI. A frame scheduler coalesces redraws to a budget,
a grapheme-width LRU keeps Unicode width lookups cheap, and higher-level widgets
(streaming text, side panel, composer) render onto separate targets.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `Cell` | struct | Packed 16-byte `#[repr(C)]` cell: `glyph`, `fg`, `bg`, `attr`. |
| `Attr` | struct | `#[repr(transparent)]` style bitflags (`BOLD`, `ITALIC`, …). |
| `Grid` | struct | Row-major `Vec<Cell>` with `put`/`get`/`resize`/`as_bytes`. |
| `GridError` | enum | `Overflow(cols, rows)`. |
| `damage::diff` | fn | `(&Grid, &Grid) → Vec<Run>` two-pass SIMD damage diff. |
| `Run` | struct | `{ row, col, len }` — one contiguous changed span. |
| `ansis::emit` | fn | Encode damage runs to ANSI escape bytes. |
| `Scheduler` / `Handle` | struct | Frame-budget coalescer + `mark_dirty` handle. |
| `WidthCache` | struct | LRU grapheme-width cache. |
| `StreamWidget` / `Rect` | struct | Incremental streaming-text render target. |
| `Composer` | struct | Side-panel render target. |
| `Panel`, `PanelState`, `PanelEvent`, `PermissionOutcome` | type | Permission/side-panel widgets. |
| `LayoutCache`, `LayoutSpan` | struct | Cached layout spans. |
| `SidePanelPrompter` | struct | CLI-side prompter over the panel. |

## Key types

```rust
#[repr(C)]
pub struct Cell {
    pub glyph: u32,  // Unicode scalar; BLANK = ASCII space
    pub fg: u32,     // 0x00RRGGBB, 0 = terminal default
    pub bg: u32,
    pub attr: u32,   // Attr bits
}

// damage.rs — the layout contract the SIMD coarse pass depends on:
const CELL_BYTES: usize = std::mem::size_of::<Cell>();
const _: () = assert!(CELL_BYTES == 16, "SIMD coarse pass assumes Cell is 16 bytes");
```

The compile-time `const _: () = assert!(CELL_BYTES == 16, …)` locks the cell
layout: `Cell` is `#[repr(C)]` with four `u32` fields and no padding, so a
`&[Cell]` aliases a `&[u8]` of exactly `len * 16` bytes (the `SAFETY` rationale
behind `Grid::as_bytes`), and the diff can scan it byte-for-byte.

## How it works

`damage::diff` is a two-pass-per-row scan over the raw cell bytes:

```
for each row (row_bytes = cols * 16):
  ┌─ coarse pass: stride 32 bytes (two cells) as u8x32 SIMD compare
  │   prev != next  ──► row_changed, break to fine pass
  └─ fine pass: per-16-byte-cell compare → emit Run { row, col, len }
                for each maximal contiguous span of differing cells
```

The coarse SIMD pass (`wide::u8x32`) skips unchanged rows cheaply; only rows that
differ fall through to the per-cell fine pass that builds the `Run` spans
`ansis::emit` turns into escape sequences. The runs respect wide-glyph
continuation cells (`Cell::CONTINUATION_GLYPH = 0xFFFF_FFFF`, an invalid scalar
that emits no character because the preceding wide glyph already advanced the
cursor two columns). The `Scheduler` coalesces `mark_dirty` signals to a frame
budget so a burst of stream deltas produces one repaint, and `WidthCache` LRU-caches
grapheme widths so layout does not recompute Unicode width per draw.

## Dependencies & features

The crate **overrides** the workspace `unsafe_code = "forbid"` to `allow` — P4.2
needs `wide::u8x32` intrinsic dispatch — and every `unsafe` block carries a
`SAFETY:` comment (`undocumented_unsafe_blocks = "deny"`). Key deps: `wide` (SIMD),
`unicode-segmentation` + `unicode-width`, `lru` + `fxhash` (width cache), `tokio`
(sync/time for the scheduler), `parking_lot`, `blake3`, `rkyv`, plus workspace
crates `origin-metrics`, `origin-stream`, `origin-tools`, `origin-permission`,
`origin-cas`, `origin-plan`. No Cargo features. A `damage_diff` Criterion bench
guards the diff's performance.

## Used by

`Grep "origin-tui" glob "crates/*/Cargo.toml"`:

- `crates/origin-cli/Cargo.toml`
- `crates/origin-tui/Cargo.toml` (self)

## Testing

`proptest` drives randomized grids through `diff` to check the runs reproduce the
target frame, plus inline unit tests for `Grid` put/get/resize, `Cell` packing,
wide-glyph continuation handling, and ANSI emit. The `damage_diff` Criterion
bench (`harness = false`) tracks diff throughput; `tokio` test-util time control
exercises the `Scheduler`'s frame coalescing.

## See also

- [tui-and-cli subsystem](../subsystems/tui-and-cli.md)
- [crates index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
