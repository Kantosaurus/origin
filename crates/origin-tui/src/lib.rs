//! `origin-tui` — custom cell-grid renderer (replaces Ratatui in Phase 4).
//!
//! Phase 4 deliverables: `Cell`, `Grid`, SIMD damage diff (`damage::diff`),
//! ANSI emit (`ansi::emit`), frame coalescing (`Scheduler`), grapheme-width
//! LRU (`WidthCache`), streaming text widget (`StreamWidget`), and a side
//! panel as a separate render target (`Composer`).

pub mod ansi;
pub mod cli_prompter;
pub mod composer;
pub mod damage;
pub mod grid;
pub mod layout_cache;
pub mod panel;
pub mod scheduler;
pub mod stream_widget;
pub mod widgets;
pub mod width;

pub use cli_prompter::SidePanelPrompter;
pub use composer::Composer;
pub use damage::Run;
pub use grid::{Attr, Cell, Grid, GridError};
pub use layout_cache::{LayoutCache, LayoutCacheError, LayoutSpan};
pub use panel::{Panel, PanelEvent, PermissionOutcome};
pub use scheduler::{Handle, Scheduler};
pub use stream_widget::{Rect, StreamWidget};
pub use width::WidthCache;
