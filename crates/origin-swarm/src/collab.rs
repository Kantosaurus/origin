// SPDX-License-Identifier: Apache-2.0
//! Real-time swarm collaboration primitives (WS-L, jcode L238).
//!
//! Two cooperating pieces, both pure and crate-local:
//!
//! - [`FileRegistry`] — a mutex-guarded map `path → {WorkerId that READ it}`.
//!   When worker A edits a file that workers B and C had previously read,
//!   [`FileRegistry::record_edit`] returns `{B, C}` (never A itself): the set
//!   of workers who should be told to re-check their view of that path. This
//!   closes jcode L238 ("when worker A edits a file worker B has read, notify
//!   B to re-check").
//! - [`Mailbox`] / [`Message`] — a per-worker inbox plus a small message type
//!   with a [`MsgScope`] (`Direct`, `Repo`, `Broadcast`) so workers can DM each
//!   other, post to a shared repo channel, or broadcast to the whole swarm.
//!
//! Both types are deliberately self-contained: no IO, no provider, no plan
//! coupling. The daemon wires them in best-effort behind a default-off gate
//! (`ORIGIN_SWARM_COLLAB`); when the gate is unset nothing here is touched and
//! the daemon's behavior is byte-identical.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::coordinator::WorkerId;

/// Inner map shape for [`FileRegistry`]: `path → set of workers that read it`.
type ReaderMap = HashMap<PathBuf, HashSet<WorkerId>>;

/// A file-shift notice produced by [`FileRegistry::record_edit`].
///
/// Carries the edited `path`, the `editor` who changed it, and the `readers`
/// (the other workers who had read the path and should re-check it). The
/// daemon turns each notice into a [`Message`] with [`MsgScope::Direct`] per
/// reader, or logs it when per-worker mailbox plumbing is not reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileShiftNotice {
    /// The path that was just edited.
    pub path: PathBuf,
    /// The worker that performed the edit.
    pub editor: WorkerId,
    /// Other workers who had read `path` and should re-check it. Never
    /// contains `editor`.
    pub readers: Vec<WorkerId>,
}

/// Tracks which workers read which paths so an edit can notify them (jcode L238).
///
/// Internally a single `Mutex<HashMap<PathBuf, HashSet<WorkerId>>>`. All public
/// methods take `&self` and lock for the minimum span — no guard is ever held
/// across an `.await` (there are none here) nor returned to the caller.
#[derive(Debug, Default)]
pub struct FileRegistry {
    /// `path → set of workers that have read it`.
    readers: Mutex<ReaderMap>,
}

impl FileRegistry {
    /// Construct an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `worker` has read `path`.
    ///
    /// Idempotent: reading the same path twice from the same worker is a no-op
    /// on the second call. A poisoned lock is treated as empty-and-recovered
    /// (the registry is advisory, so a panic in another thread must not break
    /// the turn) — see [`Self::guard`].
    pub fn record_read(&self, worker: WorkerId, path: impl AsRef<Path>) {
        let mut map = Self::guard(&self.readers);
        map.entry(path.as_ref().to_path_buf()).or_default().insert(worker);
    }

    /// Record that `worker` edited `path`; return the OTHER workers who had
    /// read it (the ones to notify).
    ///
    /// The editor is removed from the reader set for `path` (an editor that had
    /// also read the file does not notify itself, and after an edit its own
    /// view is current). Readers other than the editor are retained so a later
    /// edit by a third worker still notifies them.
    ///
    /// Returns an empty `Vec` when no other worker had read the path.
    #[must_use = "the returned workers are the ones to notify of the file shift"]
    #[allow(clippy::significant_drop_tightening)] // the guard is needed for the whole body (get_mut → mutate → collect); it drops at fn end
    pub fn record_edit(&self, worker: WorkerId, path: impl AsRef<Path>) -> Vec<WorkerId> {
        let path = path.as_ref();
        let mut map = Self::guard(&self.readers);
        let Some(set) = map.get_mut(path) else {
            return Vec::new();
        };
        // The editor's own prior read no longer counts: it just rewrote the
        // file, so its view is current and it must not self-notify.
        set.remove(&worker);
        set.iter().copied().collect()
    }

    /// Convenience wrapper: [`Self::record_edit`] plus packaging the result
    /// into a [`FileShiftNotice`]. Returns `None` when there are no readers to
    /// notify, so callers can `if let Some(notice) = …` without a length check.
    #[must_use = "the returned notice names the readers to inform of the file shift"]
    pub fn record_edit_notice(&self, worker: WorkerId, path: impl AsRef<Path>) -> Option<FileShiftNotice> {
        let path = path.as_ref();
        let readers = self.record_edit(worker, path);
        if readers.is_empty() {
            return None;
        }
        Some(FileShiftNotice {
            path: path.to_path_buf(),
            editor: worker,
            readers,
        })
    }

    /// Drop all read-tracking for `worker` (e.g. on worker exit) so a finished
    /// worker is never notified and never leaks into another's notice set.
    pub fn forget_worker(&self, worker: WorkerId) {
        let mut map = Self::guard(&self.readers);
        for set in map.values_mut() {
            set.remove(&worker);
        }
        map.retain(|_, set| !set.is_empty());
    }

    /// Number of distinct paths currently tracked (diagnostic).
    #[must_use]
    pub fn tracked_paths(&self) -> usize {
        Self::guard(&self.readers).len()
    }

    /// Lock helper that recovers from a poisoned mutex.
    ///
    /// The registry is advisory bookkeeping; if a thread panicked while holding
    /// the lock we still want a usable (if possibly slightly stale) map rather
    /// than propagating the panic into an unrelated worker's turn.
    fn guard(m: &Mutex<ReaderMap>) -> MutexGuard<'_, ReaderMap> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Delivery scope for a [`Message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgScope {
    /// Deliver to exactly one worker.
    Direct(WorkerId),
    /// Post to the shared repo channel (every worker in the room can drain it).
    Repo,
    /// Broadcast to every worker in the swarm.
    Broadcast,
}

impl MsgScope {
    /// `true` iff a message with this scope should be delivered to `target`.
    ///
    /// `Repo` and `Broadcast` are addressed to everyone; `Direct(w)` only to
    /// `w`. (The distinction between `Repo` and `Broadcast` is intent, not
    /// routing — both fan out to all workers — so the daemon can render them
    /// differently.)
    #[must_use]
    pub fn delivers_to(self, target: WorkerId) -> bool {
        match self {
            Self::Direct(w) => w == target,
            Self::Repo | Self::Broadcast => true,
        }
    }
}

/// A swarm message: who sent it, where it goes, and the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The worker that authored the message.
    pub from: WorkerId,
    /// Delivery scope.
    pub scope: MsgScope,
    /// Free-form message body.
    pub body: String,
}

impl Message {
    /// Construct a message.
    #[must_use]
    pub fn new(from: WorkerId, scope: MsgScope, body: impl Into<String>) -> Self {
        Self {
            from,
            scope,
            body: body.into(),
        }
    }
}

/// A single worker's inbox: push at the back, drain in FIFO order.
///
/// Internally a `Mutex<Vec<Message>>`. [`Self::push`] appends; [`Self::drain`]
/// returns everything queued so far (in order) and leaves the inbox empty.
/// Like [`FileRegistry`], a poisoned lock recovers rather than panicking.
#[derive(Debug, Default)]
pub struct Mailbox {
    queue: Mutex<Vec<Message>>,
}

impl Mailbox {
    /// Construct an empty mailbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a message at the back of the inbox.
    pub fn push(&self, msg: Message) {
        Self::guard(&self.queue).push(msg);
    }

    /// Remove and return every queued message in FIFO order, leaving the inbox
    /// empty. Returns an empty `Vec` when nothing is queued.
    #[must_use]
    pub fn drain(&self) -> Vec<Message> {
        std::mem::take(&mut *Self::guard(&self.queue))
    }

    /// Number of messages currently queued (diagnostic).
    #[must_use]
    pub fn len(&self) -> usize {
        Self::guard(&self.queue).len()
    }

    /// `true` iff the inbox is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        Self::guard(&self.queue).is_empty()
    }

    /// Lock helper that recovers from a poisoned mutex (see
    /// [`FileRegistry::guard`]).
    fn guard(m: &Mutex<Vec<Message>>) -> MutexGuard<'_, Vec<Message>> {
        m.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
#[allow(clippy::panic)] // assertion macros + test invariants may panic/unreachable.
mod tests {
    use super::*;
    use crate::coordinator::WorkerId;

    // ── FileRegistry ──────────────────────────────────────────────────────

    #[test]
    fn read_then_edit_by_another_notifies_the_reader() {
        let reg = FileRegistry::new();
        let reader = WorkerId::generate();
        let editor = WorkerId::generate();

        reg.record_read(reader, "src/lib.rs");
        let notify = reg.record_edit(editor, "src/lib.rs");

        assert_eq!(
            notify,
            vec![reader],
            "the reader (not the editor) must be notified"
        );
    }

    #[test]
    fn edit_by_self_does_not_self_notify() {
        let reg = FileRegistry::new();
        let w = WorkerId::generate();

        reg.record_read(w, "src/lib.rs");
        let notify = reg.record_edit(w, "src/lib.rs");

        assert!(
            notify.is_empty(),
            "a worker editing a file it read must not notify itself"
        );
    }

    #[test]
    fn edit_with_no_readers_is_empty() {
        let reg = FileRegistry::new();
        let editor = WorkerId::generate();

        // Never recorded as read by anyone.
        let notify = reg.record_edit(editor, "untouched.rs");

        assert!(notify.is_empty());
    }

    #[test]
    fn multiple_readers_all_notified_except_editor() {
        let reg = FileRegistry::new();
        let a = WorkerId::generate();
        let b = WorkerId::generate();
        let c = WorkerId::generate();

        reg.record_read(a, "shared.rs");
        reg.record_read(b, "shared.rs");
        reg.record_read(c, "shared.rs"); // c is also the editor below

        let notify: HashSet<WorkerId> = reg.record_edit(c, "shared.rs").into_iter().collect();
        let expected: HashSet<WorkerId> = [a, b].into_iter().collect();

        assert_eq!(notify, expected, "all readers except the editor are notified");
    }

    #[test]
    fn second_edit_by_third_worker_still_notifies_remaining_readers() {
        let reg = FileRegistry::new();
        let a = WorkerId::generate();
        let b = WorkerId::generate();
        let c = WorkerId::generate();

        reg.record_read(a, "f.rs");
        reg.record_read(b, "f.rs");

        // First edit by a third party notifies both a and b; the set retains
        // a and b for any later edit.
        let first = reg.record_edit(c, "f.rs");
        assert_eq!(first.len(), 2);

        // A subsequent edit by a still notifies b (a removed itself).
        let second = reg.record_edit(a, "f.rs");
        assert_eq!(second, vec![b]);
    }

    #[test]
    fn record_read_is_idempotent_per_worker() {
        let reg = FileRegistry::new();
        let reader = WorkerId::generate();
        let editor = WorkerId::generate();

        reg.record_read(reader, "x.rs");
        reg.record_read(reader, "x.rs"); // duplicate
        let notify = reg.record_edit(editor, "x.rs");

        assert_eq!(
            notify,
            vec![reader],
            "duplicate reads collapse to one notify entry"
        );
    }

    #[test]
    fn record_edit_notice_packages_readers() {
        let reg = FileRegistry::new();
        let reader = WorkerId::generate();
        let editor = WorkerId::generate();

        reg.record_read(reader, "pkg/mod.rs");
        let notice = reg
            .record_edit_notice(editor, "pkg/mod.rs")
            .expect("readers exist");

        assert_eq!(notice.editor, editor);
        assert_eq!(notice.readers, vec![reader]);
        assert_eq!(notice.path, PathBuf::from("pkg/mod.rs"));
    }

    #[test]
    fn record_edit_notice_is_none_without_readers() {
        let reg = FileRegistry::new();
        let editor = WorkerId::generate();
        assert!(reg.record_edit_notice(editor, "lonely.rs").is_none());
    }

    #[test]
    fn forget_worker_drops_its_reads() {
        let reg = FileRegistry::new();
        let gone = WorkerId::generate();
        let editor = WorkerId::generate();

        reg.record_read(gone, "y.rs");
        assert_eq!(reg.tracked_paths(), 1);

        reg.forget_worker(gone);
        assert_eq!(
            reg.tracked_paths(),
            0,
            "path is pruned once its last reader leaves"
        );

        let notify = reg.record_edit(editor, "y.rs");
        assert!(notify.is_empty(), "a forgotten worker is never notified");
    }

    #[test]
    fn distinct_paths_are_independent() {
        let reg = FileRegistry::new();
        let reader = WorkerId::generate();
        let editor = WorkerId::generate();

        reg.record_read(reader, "a.rs");
        // Editing a different path notifies no one.
        assert!(reg.record_edit(editor, "b.rs").is_empty());
        // Editing the read path still notifies.
        assert_eq!(reg.record_edit(editor, "a.rs"), vec![reader]);
    }

    // ── Mailbox ───────────────────────────────────────────────────────────

    #[test]
    fn mailbox_push_drain_preserves_fifo_order() {
        let mbox = Mailbox::new();
        let from = WorkerId::generate();

        mbox.push(Message::new(from, MsgScope::Broadcast, "first"));
        mbox.push(Message::new(from, MsgScope::Broadcast, "second"));
        mbox.push(Message::new(from, MsgScope::Broadcast, "third"));

        let drained = mbox.drain();
        let bodies: Vec<&str> = drained.iter().map(|m| m.body.as_str()).collect();
        assert_eq!(bodies, vec!["first", "second", "third"]);
    }

    #[test]
    fn mailbox_drain_empties_the_inbox() {
        let mbox = Mailbox::new();
        let from = WorkerId::generate();

        mbox.push(Message::new(from, MsgScope::Repo, "hi"));
        assert_eq!(mbox.len(), 1);
        assert!(!mbox.is_empty());

        let first = mbox.drain();
        assert_eq!(first.len(), 1);
        assert!(mbox.is_empty(), "drain leaves the inbox empty");

        let second = mbox.drain();
        assert!(second.is_empty(), "draining an empty inbox yields nothing");
    }

    #[test]
    fn mailbox_drain_on_empty_is_empty() {
        let mbox = Mailbox::new();
        assert!(mbox.drain().is_empty());
        assert!(mbox.is_empty());
    }

    // ── MsgScope ──────────────────────────────────────────────────────────

    #[test]
    fn direct_scope_delivers_only_to_target() {
        let target = WorkerId::generate();
        let other = WorkerId::generate();
        let scope = MsgScope::Direct(target);

        assert!(scope.delivers_to(target));
        assert!(!scope.delivers_to(other));
    }

    #[test]
    fn repo_and_broadcast_deliver_to_everyone() {
        let anyone = WorkerId::generate();
        assert!(MsgScope::Repo.delivers_to(anyone));
        assert!(MsgScope::Broadcast.delivers_to(anyone));
    }

    #[test]
    fn message_new_sets_fields() {
        let from = WorkerId::generate();
        let to = WorkerId::generate();
        let msg = Message::new(from, MsgScope::Direct(to), "ping");

        assert_eq!(msg.from, from);
        assert_eq!(msg.scope, MsgScope::Direct(to));
        assert_eq!(msg.body, "ping");
    }
}
