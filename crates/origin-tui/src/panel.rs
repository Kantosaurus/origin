//! Side panel event queue + permission decision UI.

use std::collections::VecDeque;

use origin_tools::Tier;

use crate::{Cell, Grid};

/// The user's decision for a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOutcome {
    Allow,
    Deny,
    Edit,
}

/// Events that flow through the side panel.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub enum PanelEvent {
    /// A tool is requesting permission.
    PermissionAsk {
        id: u64,
        tool: String,
        tier: Tier,
        args_preview: String,
    },
    /// A permission decision has been made.
    PermissionDecided { id: u64, outcome: PermissionOutcome },
    /// User pressed `?` — toggle the metrics view.
    ShowMetrics,
}

/// Active sub-view of the side panel.
///
/// The permission queue (default) renders [`PanelEvent::PermissionAsk`]
/// rows. The metrics view replaces the queue display until dismissed.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PanelState {
    /// Permission ask queue (default).
    #[default]
    PermissionQueue,
    /// `?metrics` panel.
    Metrics,
}

/// Side panel event queue with keyboard-driven permission handling.
#[derive(Debug, Default)]
pub struct Panel {
    items: VecDeque<PanelEvent>,
    state: PanelState,
}

impl Panel {
    /// Create a new empty `Panel`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append an event to the queue.
    ///
    /// [`PanelEvent::ShowMetrics`] is a control event: instead of being
    /// enqueued for rendering, it toggles the panel into [`PanelState::Metrics`]
    /// mode without disturbing the pending permission asks.
    pub fn push(&mut self, ev: PanelEvent) {
        if matches!(ev, PanelEvent::ShowMetrics) {
            self.state = match self.state {
                PanelState::PermissionQueue => PanelState::Metrics,
                PanelState::Metrics => PanelState::PermissionQueue,
            };
            return;
        }
        self.items.push_back(ev);
    }

    /// Current sub-view.
    #[must_use]
    pub const fn state(&self) -> PanelState {
        self.state
    }

    /// If the front of the queue is a `PermissionAsk`, interpret key `k` as a
    /// permission decision and pop the event, returning the `PermissionOutcome`.
    ///
    /// `'y'`/`'Y'` → `Allow`, `'n'`/`'N'` → `Deny`, `'e'`/`'E'` → `Edit`.
    /// `'?'` toggles the metrics panel and returns `None`.
    /// Any other key returns `None` without consuming the event.
    pub fn handle_key(&mut self, k: char) -> Option<PermissionOutcome> {
        if k == '?' {
            self.state = match self.state {
                PanelState::PermissionQueue => PanelState::Metrics,
                PanelState::Metrics => PanelState::PermissionQueue,
            };
            return None;
        }
        let outcome = match k {
            'y' | 'Y' => PermissionOutcome::Allow,
            'n' | 'N' => PermissionOutcome::Deny,
            'e' | 'E' => PermissionOutcome::Edit,
            _ => return None,
        };
        match self.items.front() {
            Some(PanelEvent::PermissionAsk { .. }) => {
                self.items.pop_front();
                Some(outcome)
            }
            _ => None,
        }
    }

    /// Render queued events into `side`, one event per row.
    ///
    /// Text is truncated to the grid's column width. Only ASCII labels are
    /// written — `Ask: <tool> [<tier>]`.
    pub fn render(&self, side: &mut Grid) {
        let width = side.cols();
        for (row_idx, ev) in self.items.iter().enumerate() {
            let row = u16::try_from(row_idx).unwrap_or(u16::MAX);
            if row >= side.rows() {
                break;
            }
            let label = match ev {
                PanelEvent::PermissionAsk { tool, tier, .. } => {
                    let tier_str = match tier {
                        Tier::AutoAllowed => "AutoAllowed",
                        Tier::RequiresPermission => "RequiresPermission",
                    };
                    format!("Ask: {tool} [{tier_str}]")
                }
                PanelEvent::PermissionDecided { id, outcome } => {
                    let outcome_str = match outcome {
                        PermissionOutcome::Allow => "Allow",
                        PermissionOutcome::Deny => "Deny",
                        PermissionOutcome::Edit => "Edit",
                    };
                    format!("Done: {id} {outcome_str}")
                }
                // `ShowMetrics` is a control event handled by `push`; it never
                // sits in the queue. Skip if it somehow appears.
                PanelEvent::ShowMetrics => continue,
            };
            for (col_idx, ch) in label.chars().enumerate() {
                let col = u16::try_from(col_idx).unwrap_or(u16::MAX);
                if col >= width {
                    break;
                }
                side.put(row, col, Cell::glyph(ch));
            }
        }
    }
}
