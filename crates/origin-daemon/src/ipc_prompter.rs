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

use std::collections::{HashMap, HashSet};
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
/// C2: the `(session id, tool)` an "always allow" answer to an ask covers.
type AlwaysKey = (String, String);
/// An in-flight ask: its decision sender and the key an lways answer covers.
type Pending = (oneshot::Sender<bool>, Option<AlwaysKey>);

#[derive(Debug, Default)]
pub struct PermissionRegistry {
    /// Each in-flight ask: its decision sender and, when the person may answer
    /// "always allow", the `(session id, tool)` that answer covers (C2).
    pending: Mutex<HashMap<u64, Pending>>,
    /// Monotonic correlation-id source. Starts at 1 so `0` is never valid.
    next_id: AtomicU64,
    /// C2: tools the person answered "always allow" for, per session id.
    // ponytail: in memory only (lost on daemon restart) and never evicted;
    // persist or key on session lifetime if that ever matters.
    standing: Mutex<HashMap<String, HashSet<String>>>,
}

impl PermissionRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            standing: Mutex::new(HashMap::new()),
        }
    }

    /// Reserve a correlation id and a receiver for its decision. `always_key`
    /// is the `(session id, tool)` an "always allow" answer may cover.
    fn register(&self, always_key: Option<AlwaysKey>) -> (u64, oneshot::Receiver<bool>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, (tx, always_key));
        (id, rx)
    }

    /// C2: whether `tool` has a standing "always allow" in `session_id`.
    #[must_use]
    pub fn is_standing(&self, session_id: &str, tool: &str) -> bool {
        self.standing
            .lock()
            .get(session_id)
            .is_some_and(|tools| tools.contains(tool))
    }

    /// Drop a reservation whose ask could not be emitted.
    fn cancel(&self, id: u64) {
        self.pending.lock().remove(&id);
    }

    /// Number of in-flight reservations (test-only leak check).
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    /// Deliver a client decision, correlated by `id`. Unknown ids (a stray or
    /// late decision, or one whose `ask` already gave up) are ignored, so a
    /// misbehaving client can never panic the daemon. C2: `allow && always`
    /// records a standing allow for the ask's session and tool, but only for an
    /// ask that offered "always" (never a `Task` or an ask that needs a person).
    pub fn resolve(&self, id: u64, allow: bool, always: bool) {
        let entry = self.pending.lock().remove(&id);
        if let Some((tx, always_key)) = entry {
            if let (true, true, Some((session, tool))) = (allow, always, always_key) {
                self.standing.lock().entry(session).or_default().insert(tool);
            }
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

/// Removes a [`PermissionRegistry`] reservation on drop — so a turn cancelled
/// mid-ask (the `ask` future dropped while parked on `rx`) cannot leak its
/// `pending` entry for the daemon's lifetime. On the happy path `resolve`
/// already removed it, so the drop-time `cancel` is a no-op.
struct PermissionCancelGuard<'a> {
    reg: &'a PermissionRegistry,
    id: u64,
}
impl Drop for PermissionCancelGuard<'_> {
    fn drop(&mut self) {
        self.reg.cancel(self.id);
    }
}

/// [`ChoiceRegistry`] analogue of [`PermissionCancelGuard`].
struct ChoiceCancelGuard<'a> {
    reg: &'a ChoiceRegistry,
    id: &'a str,
}
impl Drop for ChoiceCancelGuard<'_> {
    fn drop(&mut self) {
        self.reg.cancel(self.id);
    }
}

/// Per-turn prompter: emits asks over `event_tx` and parks on the shared
/// [`PermissionRegistry`] until the decision is resolved from any connection.
pub struct IpcPrompter {
    /// The turn's event channel (what `run_loop` streams over). Closed ⇒ deny.
    event_tx: mpsc::Sender<StreamEvent>,
    /// Shared decision table.
    registry: std::sync::Arc<PermissionRegistry>,
    /// C2: the session whose standing "always allow" answers this prompter uses.
    session_id: String,
    /// C2: set once the client is gone (channel closed, decision dropped); from
    /// then on [`Prompter::can_ask`] is `false`, so denials say nobody can be asked.
    gone: std::sync::atomic::AtomicBool,
}

impl IpcPrompter {
    /// Create a prompter that emits asks over `event_tx`, parks on `registry`
    /// and keeps "always allow" answers for `session_id`.
    #[must_use]
    pub const fn new(
        event_tx: mpsc::Sender<StreamEvent>,
        registry: std::sync::Arc<PermissionRegistry>,
        session_id: String,
    ) -> Self {
        Self {
            event_tx,
            registry,
            session_id,
            gone: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Core ask by tool name + preview (the trait's [`ToolMeta`]-free seam, so
    /// the round-trip is unit-testable without constructing a full `ToolMeta`).
    async fn ask_named(&self, tool: &str, args_preview: &str, offer_always: bool) -> bool {
        let always_key = offer_always.then(|| (self.session_id.clone(), tool.to_string()));
        let (id, rx) = self.registry.register(always_key);
        let _guard = PermissionCancelGuard {
            reg: &self.registry,
            id,
        };
        let ask = StreamEvent::PermissionAsk {
            id,
            tool: tool.to_string(),
            args_preview: args_preview.to_string(),
            no_always: !offer_always,
        };
        if self.event_tx.send(ask).await.is_err() {
            // Client/relay gone ⇒ cannot ask ⇒ secure default deny (guard cleans up).
            self.gone.store(true, Ordering::Relaxed);
            return false;
        }
        // Block until the client answers; a dropped sender resolves to deny.
        rx.await.unwrap_or_else(|_| {
            self.gone.store(true, Ordering::Relaxed);
            false
        })
    }
}

#[async_trait]
impl Prompter for IpcPrompter {
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool {
        self.ask_named(meta.name, args_preview, false).await
    }

    async fn ask_offering(&self, meta: &ToolMeta, args_preview: &str, offer_always: bool) -> bool {
        self.ask_named(meta.name, args_preview, offer_always).await
    }

    fn can_ask(&self) -> bool {
        !self.gone.load(Ordering::Relaxed)
    }

    fn standing_allow(&self, tool: &str) -> bool {
        self.registry.is_standing(&self.session_id, tool)
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
    pub const fn new(event_tx: mpsc::Sender<StreamEvent>, registry: std::sync::Arc<ChoiceRegistry>) -> Self {
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
        let _guard = ChoiceCancelGuard {
            reg: &self.registry,
            id: &id,
        };
        let ask = StreamEvent::ChoiceAsk {
            id: id.clone(),
            question,
            options,
            multi_select,
            allow_custom,
        };
        if self.event_tx.send(ask).await.is_err() {
            // Client/relay gone ⇒ cannot ask ⇒ secure default skip (guard cleans up).
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
                ..
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
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry), "s".into());
        let handle = tokio::spawn(async move { prompter.ask_named("Bash", "rm -rf x", false).await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Bash", "rm -rf x");
        registry.resolve(id, true, false);
        assert!(handle.await.expect("task joins"), "allow decision returns true");
    }

    #[tokio::test]
    async fn resolve_false_denies_the_waiting_ask() {
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry), "s".into());
        let handle = tokio::spawn(async move { prompter.ask_named("Write", "/etc/passwd", false).await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Write", "/etc/passwd");
        registry.resolve(id, false, false);
        assert!(!handle.await.expect("task joins"), "deny decision returns false");
    }

    #[tokio::test]
    async fn cross_connection_resolve_unblocks_ask() {
        // The decision is delivered through the SHARED registry (the cross-
        // connection path), not the prompter that emitted the ask.
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry), "s".into());
        let handle = tokio::spawn(async move { prompter.ask_named("Edit", "src/x.rs", false).await });
        let id = expect_ask(rx.recv().await.expect("ask emitted"), "Edit", "src/x.rs");
        // A *different* holder of the Arc resolves it.
        let other = Arc::clone(&registry);
        other.resolve(id, true, false);
        assert!(handle.await.expect("task joins"));
    }

    #[tokio::test]
    async fn ask_denies_when_client_channel_closed() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx); // no client listening
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, registry, "s".into());
        assert!(
            !prompter.ask_named("Bash", "x", false).await,
            "a vanished client must deny, never silently allow"
        );
    }

    /// C2 (fix 8): a vanished client makes the prompter report it cannot ask,
    /// so the denial says nobody could be asked rather than "user-denied".
    #[tokio::test]
    async fn closed_channel_turns_can_ask_off() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let prompter = IpcPrompter::new(tx, Arc::new(PermissionRegistry::new()), "s".into());
        assert!(prompter.can_ask());
        assert!(!prompter.ask_named("Bash", "x", true).await);
        assert!(!prompter.can_ask());
    }

    /// C2 (fix 3): "always allow" sticks for the ask's session and tool only
    /// when the ask offered it; the ask carries `no_always` otherwise.
    #[tokio::test]
    async fn always_answer_sticks_only_when_offered() {
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = Arc::new(IpcPrompter::new(tx, Arc::clone(&registry), "s1".into()));
        for (offer, tool) in [(false, "Task"), (true, "Bash")] {
            let p = Arc::clone(&prompter);
            let handle = tokio::spawn(async move { p.ask_named(tool, "x", offer).await });
            let (id, no_always) = match rx.recv().await.expect("ask emitted") {
                StreamEvent::PermissionAsk { id, no_always, .. } => (id, no_always),
                other => panic!("expected PermissionAsk, got {other:?}"),
            };
            assert_eq!(no_always, !offer, "{tool}");
            registry.resolve(id, true, true);
            assert!(handle.await.expect("joins"));
        }
        assert!(!prompter.standing_allow("Task"), "an unoffered always is ignored");
        assert!(prompter.standing_allow("Bash"));
        assert!(!registry.is_standing("other-session", "Bash"), "per session");
    }

    #[test]
    fn resolve_unknown_id_is_noop() {
        let registry = PermissionRegistry::new();
        registry.resolve(999, true, false); // must not panic
    }

    #[tokio::test]
    async fn dropped_ask_does_not_leak_its_reservation() {
        // A turn cancelled mid-ask (Ctrl+C drops the `ask` future while it parks
        // on `rx`) must not leak the registry reservation for the daemon's life.
        let (tx, mut rx) = mpsc::channel(4);
        let registry = Arc::new(PermissionRegistry::new());
        let prompter = IpcPrompter::new(tx, Arc::clone(&registry), "s".into());
        {
            let fut = prompter.ask_named("Bash", "x", false);
            tokio::pin!(fut);
            // Poll the ask far enough to register + emit, then DROP it (the rx
            // branch wins, the never-resolved `fut` is dropped at scope end).
            tokio::select! {
                _ = &mut fut => panic!("ask must not resolve without a decision"),
                ev = rx.recv() => { let _ = ev.expect("ask emitted"); }
            }
        }
        assert_eq!(
            registry.pending_len(),
            0,
            "a dropped (cancelled) ask must clean up its reservation"
        );
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
        let handle =
            tokio::spawn(async move { prompter.ask("Pick".to_string(), Vec::new(), true, true).await });
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
        let handle =
            tokio::spawn(async move { prompter.ask("Pick".to_string(), Vec::new(), false, false).await });
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
        let (sel, custom) = prompter.ask("Pick".to_string(), Vec::new(), false, false).await;
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
