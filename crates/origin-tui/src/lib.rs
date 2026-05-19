//! `origin-tui` — custom cell-grid renderer (replaces Ratatui in Phase 4).
//!
//! Phase 4 deliverables: `Cell`, `Grid`, SIMD damage diff (`damage::diff`),
//! ANSI emit (`ansi::emit`), frame coalescing (`Scheduler`), grapheme-width
//! LRU (`WidthCache`), streaming text widget (`StreamWidget`), and a side
//! panel as a separate render target (`Composer`).

pub mod damage;
pub mod grid;

pub use damage::Run;
pub use grid::{Attr, Cell, Grid, GridError};
