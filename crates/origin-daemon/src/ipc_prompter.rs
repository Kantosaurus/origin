// SPDX-License-Identifier: Apache-2.0
//! Opt-in interactive permission prompting over IPC.
//!
//! When a turn sets [`PromptRequest::permission_ask`](crate::protocol::PromptRequest),
//! the daemon routes `RequiresPermission` tools through an [`IpcPrompter`]
//! instead of `AlwaysAllow`. [`IpcPrompter::ask`] emits a
//! [`StreamEvent::PermissionAsk`] over the turn's event channel and blocks on a
//! oneshot held in a **daemon-wide** [`PermissionRegistry`].
//!
//! The decision does NOT come back over the same connection (that connection is
//! busy streaming the turn, and it is only read *between* goal iterations — a
//! blocking in-turn read would deadlock). Instead the client sends
//! [`ClientMessage::PermissionDecision`](crate::protocol::ClientMessage) on a
//! fresh connection; that connection's handler calls
//! [`PermissionRegistry::resolve`], unblocking the waiting `ask`. This mirrors
//! the existing cross-connection `MemoryDecision` / proposal-registry pattern.
//!
//! Secure defaults: a closed event channel or a dropped decision sender denies,
//! so a vanished client can never silently auto-approve.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use origin_permission::prompt::Prompter;
use origin_tools::ToolMeta;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};

use crate::protocol::StreamEvent;

/// Daemon-wide table of in-flight permission asks awaiting a client decision.
///
/// Shared (`Arc`) across every connection handler so a decision arriving on one
/// connection can resolve an ask emitted on another. Ids are globally unique
/// within the daemon's lifetime.
#[derive(Debug, Default)]
pub struct PermissionRegistry {
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    /// Monotonic correlation-id source. Starts at 1 so `0` is never valid.
    next_id: AtomicU64,
}

impl PermissionRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Reserve a correlation id and a receiver for its decision.
    fn register(&self) -> (u64, oneshot::Receiver<bool>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);
        (id, rx)
    }

    /// Drop a reservation whose ask could not be emitted.
    fn cancel(&self, id: u64) {
        self.pending.lock().remove(&id);
    }

    /// Deliver a client decision, correlated by `id`. Unknown ids (a stray or
    /// late decision, or one whose `ask` already gave up) are ignored, so a
    /// misbehaving client can never panic the daemon.
    pub fn resolve(&self, id: u64, allow: bool) {
        let sender = self.pending.lock().remove(&id);
        if let Some(tx) = sender {
            // The receiver may have been dropped (turn ended); ignore the error.
            let _ = tx.send(allow);
        }
    }
}

/// A resolved interactive-choice decision: the option indices the user picked
/// (empty ⇒ skipped/cancelled) plus any free-text they typed.
pub type ChoiceOutcome = (Vec<usize>, Option<String>);

/// Daemon-wide table of in-flight `ask_user` choices awaiting a client decision.
///
/// This is the [`PermissionRegistry`] sibling for the `ask_user` pause-await
/// path: an [`IpcChoicePrompter::ask`] emits a
/// [`StreamEvent::ChoiceAsk`](crate::protocol::StreamEvent) over the turn's
/// event channel and parks on a oneshot held here, and a
/// [`ClientMessage::ChoiceDecision`](crate::protocol::ClientMessage) arriving on
/// any connection resolves it. Correlation ids are `String`s (the protocol uses
/// a string id for `ChoiceAsk`); the registry mints monotonic `choice-<n>` ids.
#[derive(Debug, Default)]
pub struct ChoiceRegistry {
    pending: Mutex<HashMap<String, oneshot::Sender<ChoiceOutcome>>>,
    /// Monotonic correlation-id source. Starts at 1 so the first id is
    /// `choice-1`.
    next_id: AtomicU64,
}

impl ChoiceRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Reserve a correlation id and a receiver for its decision.
    #[must_use]
    pub fn register(&self) -> (String, oneshot::Receiver<ChoiceOutcome>) {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        let id = format!("choice-{n}");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id.clone(), tx);
        (id, rx)
    }

    /// Drop a reservation whose ask could not be emitted.
    pub fn cancel(&self, id: &str) {
        self.pending.lock().remove(id);
    }

    /// Deliver a client decision, correlated by `id`. Unknown ids (a stray or
    /// late decision, or one whose `ask` already gave up) are ignored, so a
    /// misbehaving client can never panic the daemon.
    pub fn resolve(&self, id: &str, selected: Vec<usize>, custom: Option<String>) {
        let sender = self.pending.lock().remove(id);
        if let Some(tx) = sender {
            // The receiver may have been dropped (turn ended); ignore the error.
            let _ = tx.send((selected, custom));
        }
    }
}

/// Per-turn prompter: emits asks over `event_tx` and parks on the shared
/// [`PermissionRegistry`] until the decision is resolved from any connection.
pub struct IpcPrompter {
    /// The turn's event channel (what `run_loop` streams over). Closed ⇒ deny.
    event_tx: mpsc::Sender<StreamEvent>,
    /// Shared decision table.
    registry: std::sync::Arc<PermissionRegistry>,
}

impl IpcPrompter {
    /// Create a prompter that emits asks over `event_tx` and parks on `registry`.
    #[must_use]
    pub const fn new(
        event_tx: mpsc::Sender<StreamEvent>,
        registry: std::sync::Arc<PermissionRegistry>,
    ) -> Self {
        Self { event_tx, registry }
    }

    /// Core ask by tool name + preview (the trait's [`ToolMeta`]-free seam, so
    /// the round-trip is unit-testable without constructing a full `ToolMeta`).
    async fn ask_named(&self, tool: &str, args_preview: &str) -> bool {
        let (id, rx) = self.registry.register();
        let ask = StreamEvent::PermissionAsk {
            id,
            tool: tool.to_string(),
            args_preview: args_preview.to_string(),
        };
        if self.event_tx.send(ask).await.is_err() {
            // Client/relay gone ⇒ cannot ask ⇒ secure default deny. Clean up.
            self.registry.cancel(id);
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

/// Per-turn interactive-choice prompter for the `ask_user` tool.
///
/// Emits a [`StreamEvent::ChoiceAsk`] over `event_tx` and parks on the shared
/// [`ChoiceRegistry`] until a [`ClientMessage::ChoiceDecision`] resolves it from
/// any connection — the [`IpcPrompter`] analogue for structured questions.
///
/// Secure-default symmetry with the permission path: a closed event channel or
/// a dropped decision sender resolves to the *skipped* outcome (empty selection,
/// no custom text), so a vanished client can never strand the tool.
pub struct IpcChoicePrompter {
    event_tx: mpsc::Sender<StreamEvent>,
    registry: std::sync::Arc<ChoiceRegistry>,
}

impl IpcChoicePrompter {
    /// Create a choice prompter that emits asks over `event_tx` and parks on
    /// `registry`.
    #[must_use]
    pub const fn new(
        event_tx: mpsc::Sender<StreamEvent>,
        registry: std::sync::Arc<ChoiceRegistry>,
    ) -> Self {
        Self { event_tx, registry }
    }

    /// Emit a `ChoiceAsk` and block on the matching `ChoiceDecision`. Returns
    /// the resolved `(selected indices, custom text)`; an unreachable client
    /// resolves to `(vec![], None)` (the skip outcome).
    pub async fn ask(
        &self,
        question: String,
        options: Vec<crate::protocol::ChoiceOption>,
        multi_select: bool,
        allow_custom: bool,
    ) -> ChoiceOutcome {
        let (id, rx) = self.registry.register();
        let ask = StreamEvent::ChoiceAsk {
            id: id.clone(),
            question,
            options,
            multi_select,
            allow_custom,
        };
        if self.event_tx.send(ask).await.is_err() {
            // Client/relay gone ⇒ cannot ask ⇒ secure default skip. Clean up.
            self.registry.cancel(&id);
            return (Vec::new(), None);
        }
        // Block until the client answers; a dropped sender resolves to skip.
        rx.await.unwrap_or((Vec::new(), None))
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Pull the next emitted ask and return its correlation id, asserting the
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
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry));
        let handle = tokio::spawn(async move { prompter.ask_named("Bash", "rm -rf x").await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Bash", "rm -rf x");
        registry.resolve(id, true);
        assert!(handle.await.expect("task joins"), "allow decision returns true");
    }

    #[tokio::test]
    async fn resolve_false_denies_the_waiting_ask() {
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry));
        let handle = tokio::spawn(async move { prompter.ask_named("Write", "/etc/passwd").await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Write", "/etc/passwd");
        registry.resolve(id, false);
        assert!(!handle.await.expect("task joins"), "deny decision returns false");
    }

    #[tokio::test]
    async fn cross_connection_resolve_unblocks_ask() {
        // The decision is delivered through the SHARED registry (the cross-
        // connection path), not the prompter that emitted the ask.
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry));
        let handle = tokio::spawn(async move { prompter.ask_named("Edit", "src/x.rs").await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Edit", "src/x.rs");
        // A *different* holder of the Arc resolves it.
        let other = Arc::clone(&registry);
        other.resolve(id, true);
        assert!(handle.await.expect("task joins"));
    }

    #[tokio::test]
    async fn ask_denies_when_client_channel_closed() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // no client listening
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, registry);
        assert!(
            !prompter.ask_named("Bash", "x").await,
            "a vanished client must deny, never silently allow"
        );
    }

    #[test]
    fn resolve_unknown_id_is_noop() {
        let registry = PermissionRegistry::new();
        registry.resolve(999, true); // must not panic
    }

    // ── ChoiceRegistry / IpcChoicePrompter (ask_user pause-await) ────────────

    fn expect_choice_ask(ev: StreamEvent, question: &str) -> String {
        match ev {
            StreamEvent::ChoiceAsk { id, question: q, .. } => {
                assert_eq!(q, question);
                id
            }
            other => panic!("expected ChoiceAsk, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn choice_resolve_returns_selection_and_custom() {
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(ChoiceRegistry::new());
        let prompter = IpcChoicePrompter::new(tx, Arc::clone(&registry));
        let handle = tokio::spawn(async move {
            prompter
                .ask("Pick".to_string(), Vec::new(), true, true)
                .await
        });
        let id = expect_choice_ask(rx.recv().await.expect("ask emitted"), "Pick");
        registry.resolve(&id, vec![0, 2], Some("extra".to_string()));
        let (sel, custom) = handle.await.expect("task joins");
        assert_eq!(sel, vec![0, 2]);
        assert_eq!(custom.as_deref(), Some("extra"));
    }

    #[tokio::test]
    async fn cross_connection_resolve_unblocks_choice_ask() {
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(ChoiceRegistry::new());
        let prompter = IpcChoicePrompter::new(tx, Arc::clone(&registry));
        let handle = tokio::spawn(async move {
            prompter
                .ask("Pick".to_string(), Vec::new(), false, false)
                .await
        });
        let id = expect_choice_ask(rx.recv().await.expect("ask emitted"), "Pick");
        // A *different* holder of the Arc resolves it (the cross-connection path).
        let other = Arc::clone(&registry);
        other.resolve(&id, vec![1], None);
        let (sel, custom) = handle.await.expect("task joins");
        assert_eq!(sel, vec![1]);
        assert!(custom.is_none());
    }

    #[tokio::test]
    async fn choice_skips_when_client_channel_closed() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // no client listening
        let registry = Arc::new(ChoiceRegistry::new());
        let prompter = IpcChoicePrompter::new(tx, registry);
        let (sel, custom) = prompter
            .ask("Pick".to_string(), Vec::new(), false, false)
            .await;
        assert!(sel.is_empty(), "a vanished client must skip, never auto-select");
        assert!(custom.is_none());
    }

    #[test]
    fn choice_resolve_unknown_id_is_noop() {
        let registry = ChoiceRegistry::new();
        registry.resolve("nope", vec![0], None); // must not panic
    }

    #[test]
    fn choice_ids_are_unique_and_monotonic() {
        let registry = ChoiceRegistry::new();
        let (a, _ra) = registry.register();
        let (b, _rb) = registry.register();
        assert_ne!(a, b);
        assert_eq!(a, "choice-1");
        assert_eq!(b, "choice-2");
    }
}
