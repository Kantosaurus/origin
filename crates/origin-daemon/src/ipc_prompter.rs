// SPDX-License-Identifier: Apache-2.0
//! `IpcPrompter` — the opt-in interactive permission prompter.
//!
//! When a turn sets [`PromptRequest::permission_ask`](crate::protocol::PromptRequest),
//! the daemon routes `RequiresPermission` tools through this prompter instead of
//! `AlwaysAllow`. [`IpcPrompter::ask`] emits a
//! [`StreamEvent::PermissionAsk`] to the client and blocks until the matching
//! [`ClientMessage::PermissionDecision`](crate::protocol::ClientMessage) is
//! delivered via [`IpcPrompter::resolve`] (called by the connection's frame
//! reader). If the event channel is closed or the decision sender is dropped,
//! `ask` denies — the secure default, so a vanished client can never silently
//! auto-approve.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use origin_permission::prompt::Prompter;
use origin_tools::ToolMeta;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::StreamEvent;

/// Routes tool-permission asks over the IPC connection and awaits decisions.
///
/// Cloneable handle pattern: wrap in `Arc` so the agent loop (as `&dyn
/// Prompter`) and the connection reader (calling [`resolve`](Self::resolve))
/// share one instance.
pub struct IpcPrompter {
    /// Side-band channel to the client (the same one `run_loop` streams events
    /// over). Closed ⇒ no client ⇒ deny.
    event_tx: mpsc::Sender<StreamEvent>,
    /// In-flight asks awaiting a decision, keyed by correlation id.
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    /// Monotonic correlation-id source. Starts at 1 so `0` is never a valid id.
    next_id: AtomicU64,
}

impl IpcPrompter {
    /// Create a prompter that emits asks over `event_tx`.
    #[must_use]
    pub fn new(event_tx: mpsc::Sender<StreamEvent>) -> Self {
        Self {
            event_tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Deliver a client decision to the waiting `ask`, correlated by `id`.
    ///
    /// Unknown ids (a stray or late decision, or one whose `ask` already gave
    /// up) are ignored, so a misbehaving client can never panic the daemon.
    pub fn resolve(&self, id: u64, allow: bool) {
        let sender = self.pending.lock().remove(&id);
        if let Some(tx) = sender {
            // The receiver may have been dropped (turn ended); ignore the error.
            let _ = tx.send(allow);
        }
    }

    /// Core ask by tool name + preview (the trait's [`ToolMeta`]-free seam, so
    /// the round-trip is unit-testable without constructing a full `ToolMeta`).
    async fn ask_named(&self, tool: &str, args_preview: &str) -> bool {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        // Register before emitting so a fast decision can't race the insert.
        self.pending.lock().insert(id, tx);
        let ask = StreamEvent::PermissionAsk {
            id,
            tool: tool.to_string(),
            args_preview: args_preview.to_string(),
        };
        if self.event_tx.send(ask).await.is_err() {
            // Client/relay gone ⇒ cannot ask ⇒ secure default deny. Clean up.
            self.pending.lock().remove(&id);
            return false;
        }
        // Block until the client answers; a dropped sender resolves to deny.
        rx.await.unwrap_or(false)
    }
}

#[async_trait]
impl Prompter for IpcPrompter {
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool {
        self.ask_named(meta.name, args_preview).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Receive the next emitted ask and return its correlation id, asserting the
    /// tool/preview round-tripped.
    fn expect_ask(ev: StreamEvent, tool: &str, preview: &str) -> u64 {
        match ev {
            StreamEvent::PermissionAsk {
                id,
                tool: t,
                args_preview,
            } => {
                assert_eq!(t, tool);
                assert_eq!(args_preview, preview);
                id
            }
            other => panic!("expected PermissionAsk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_true_allows_the_waiting_ask() {
        let (tx, mut rx) = mpsc::channel(4);
        let prompter = Arc::new(IpcPrompter::new(tx));
        let p2 = Arc::clone(&prompter);
        let handle = tokio::spawn(async move { p2.ask_named("Bash", "rm -rf x").await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Bash", "rm -rf x");
        prompter.resolve(id, true);
        assert!(handle.await.expect("task joins"), "allow decision returns true");
    }

    #[tokio::test]
    async fn resolve_false_denies_the_waiting_ask() {
        let (tx, mut rx) = mpsc::channel(4);
        let prompter = Arc::new(IpcPrompter::new(tx));
        let p2 = Arc::clone(&prompter);
        let handle = tokio::spawn(async move { p2.ask_named("Write", "/etc/passwd").await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Write", "/etc/passwd");
        prompter.resolve(id, false);
        assert!(!handle.await.expect("task joins"), "deny decision returns false");
    }

    #[tokio::test]
    async fn ask_denies_when_client_channel_closed() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // no client listening
        let prompter = IpcPrompter::new(tx);
        assert!(
            !prompter.ask_named("Bash", "x").await,
            "a vanished client must deny, never silently allow"
        );
    }

    #[test]
    fn resolve_unknown_id_is_noop() {
        let (tx, _rx) = mpsc::channel(1);
        let prompter = IpcPrompter::new(tx);
        prompter.resolve(999, true); // must not panic
    }
}
