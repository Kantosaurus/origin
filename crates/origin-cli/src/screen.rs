//! Screen layout primitives for the new origin-tui renderer.
//!
//! Layout is largely done inside `Composer`; this module just re-exports
//! the cross-pane `Rect` for consumers and provides any layout helpers
//! origin-cli still needs.

pub use origin_tui::stream_widget::Rect;
