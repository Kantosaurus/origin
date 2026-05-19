//! `SidePanelPrompter` — implements `origin_permission::prompt::Prompter` by
//! routing permission asks through the side-panel event queue.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use origin_permission::prompt::Prompter;
use origin_tools::ToolMeta;

use crate::panel::{PanelEvent, PermissionOutcome};

/// Bridges the async permission-check pathway to the side panel UI.
///
/// When `ask` is called it:
/// 1. Allocates a fresh unique `id`.
/// 2. Registers a oneshot sender in `pending`.
/// 3. Sends a `PanelEvent::PermissionAsk` over `tx`.
/// 4. Awaits the oneshot; the TUI runtime calls `resolve` when the user keys
///    in a decision.
pub struct SidePanelPrompter {
    tx: mpsc::Sender<PanelEvent>,
    pending: Mutex<HashMap<u64, oneshot::Sender<PermissionOutcome>>>,
    next_id: AtomicU64,
}

impl SidePanelPrompter {
    /// Create a new prompter that dispatches events over `tx`.
    #[must_use]
    pub fn new(tx: mpsc::Sender<PanelEvent>) -> Self {
        Self {
            tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Deliver a previously-requested decision to the waiting `ask` future.
    ///
    /// Silently ignores unknown `id`s (e.g. if the caller timed out).
    pub fn resolve(&self, id: u64, outcome: PermissionOutcome) {
        let maybe_tx = self.pending.lock().remove(&id);
        if let Some(tx) = maybe_tx {
            // If the receiver has been dropped we don't care.
            let _ = tx.send(outcome);
        }
    }
}

#[async_trait]
impl Prompter for SidePanelPrompter {
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (outcome_tx, outcome_rx) = oneshot::channel::<PermissionOutcome>();

        self.pending.lock().insert(id, outcome_tx);

        let ev = PanelEvent::PermissionAsk {
            id,
            tool: meta.name.to_string(),
            tier: meta.tier,
            args_preview: args_preview.to_string(),
        };

        // If the channel is closed, default to Deny (safe fallback).
        if self.tx.send(ev).await.is_err() {
            self.pending.lock().remove(&id);
            return false;
        }

        (outcome_rx.await).map_or(false, |outcome| matches!(outcome, PermissionOutcome::Allow))
    }
}
