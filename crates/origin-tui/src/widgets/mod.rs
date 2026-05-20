//! Composable side-panel widgets that emit pure-data lines for the cli
//! renderer.
//!
//! Phase 9 introduces [`plan_panel`], the plan side panel that subscribes
//! to the daemon's `PlanHandle` and renders the folded shared plan.

pub mod metrics;
pub mod plan_panel;
