// SPDX-License-Identifier: Apache-2.0
//! `SidePanelPrompter` — implements `origin_permission::prompt::Prompter` by
//! routing permission asks through the side-panel event queue.

use std::collections::HashMap;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot, Mutex as AsyncMutex};

use origin_permission::prompt::Prompter;
use origin_tools::ToolMeta;

use crate::panel::{PanelEvent, PermissionOutcome};

/// Bridges the async permission-check pathway to the side panel UI.
///
/// When `ask` is called it:
/// 1. Holds the submit lock to atomically allocate an id, register a oneshot
///    sender in `pending`, and send `PanelEvent::PermissionAsk` over `tx` —
///    in one uninterrupted sequence.  This guarantees that events arrive in
///    the mpsc channel in strict call-submission order.
/// 2. Releases the lock *after* the channel send completes, so the next
///    concurrent `ask` only enqueues after this one has been delivered.
/// 3. Awaits the oneshot without holding the lock, so the wait phase is
///    fully concurrent across all in-flight `ask` callers.
pub struct SidePanelPrompter {
    tx: mpsc::Sender<PanelEvent>,
    pending: Mutex<HashMap<u64, oneshot::Sender<PermissionOutcome>>>,
    /// Serialises the id-allocation + channel-send pair so that the mpsc
    /// delivery order matches submission order.  Released after `tx.send`
    /// returns but before the oneshot `.await`.
    submit_lock: AsyncMutex<u64>,
}

impl SidePanelPrompter {
    /// Create a new prompter that dispatches events over `tx`.
    #[must_use]
    pub fn new(tx: mpsc::Sender<PanelEvent>) -> Self {
        Self {
            tx,
            pending: Mutex::new(HashMap::new()),
            // Start ids at 1 so a "valid id" is distinguishable from `0` in
            // logs / test fixtures; the value is just a counter, the only
            // semantic constraint is "strictly increasing".
            submit_lock: AsyncMutex::new(1),
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
    // RATIONALE: submit_lock is held intentionally across `tx.send` to
    // guarantee that the mpsc channel receives events in the same order that
    // callers entered `ask`.  Dropping the guard early would lose that
    // ordering guarantee.
    #[allow(clippy::significant_drop_tightening)]
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool {
        let outcome_rx = {
            // Hold submit_lock across id-allocation + channel send so that
            // mpsc delivery order matches the order callers entered `ask`.
            let mut next_id = self.submit_lock.lock().await;
            let id = *next_id;
            *next_id += 1;

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
                // submit_lock guard (`next_id`) drops here, releasing the lock.
                return false;
            }

            // submit_lock guard (`next_id`) drops here after the send, so the
            // next concurrent `ask` can proceed through its enqueue phase.
            outcome_rx
        };

        (outcome_rx.await).is_ok_and(|outcome| matches!(outcome, PermissionOutcome::Allow))
    }
}
