// SPDX-License-Identifier: Apache-2.0
//! SQLite-backed session persistence (inline blobs for P1; CAS handles arrive in P2).

use std::path::{Path, PathBuf};

use origin_core::types::{Message, Role};
use origin_resume_token::ResumeToken;
use origin_store::{Store, StoreError};
use thiserror::Error;

use crate::session::Session;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum SessionStoreError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("rkyv: {0}")]
    Rkyv(String),
}

pub struct SessionStore {
    inner: Store,
    /// Directory the `SQLite` database lives in. Used to derive the
    /// `resume/` subdirectory where P12.12 persists `ResumeToken`s.
    db_dir: PathBuf,
}

/// Lightweight projection of a `sessions` row used by admin/list operations.
#[derive(Debug, Clone)]
pub struct SessionSummary {
    pub id: String,
    pub created_at: i64,
    pub title: Option<String>,
    pub model: String,
    pub message_count: u32,
}

impl SessionStore {
    /// Open or create the `SQLite` database at `path` and run migrations.
    ///
    /// # Errors
    /// Propagates `StoreError` for open/migration failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        let p = path.as_ref();
        let db_dir = p.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        Ok(Self {
            inner: Store::open(p)?,
            db_dir,
        })
    }

    /// Directory under which P12.12 persists per-session resume tokens.
    /// Created on demand by [`Self::save_resume_token`].
    #[must_use]
    pub fn resume_dir(&self) -> PathBuf {
        self.db_dir.join("resume")
    }

    /// Persist `token` to `<db_dir>/resume/<session_id>.json`. Idempotent.
    ///
    /// # Errors
    /// Propagates I/O and serialization errors from
    /// [`ResumeToken::save`].
    pub fn save_resume_token(&self, token: &ResumeToken) -> std::io::Result<()> {
        token.save(&self.resume_dir())
    }

    /// Load the resume token for `session_id` if one was previously
    /// checkpointed. Returns `Ok(None)` when no token exists.
    ///
    /// # Errors
    /// Propagates I/O and serde decode errors.
    pub fn load_resume_token(&self, session_id: &str) -> std::io::Result<Option<ResumeToken>> {
        ResumeToken::load_one(&self.resume_dir(), session_id)
    }

    /// Insert (or replace) a session metadata row.
    ///
    /// # Errors
    /// Returns a sqlite error on write failure.
    pub fn persist_session(&self, s: &Session) -> Result<(), SessionStoreError> {
        let id = s.id.clone();
        let provider = s.provider_name.clone();
        let model = s.model.clone();
        // #15: derive a short, deterministic title from the first user message so
        // the `sessions.title` column — surfaced by `origin sessions ls` — is
        // actually populated. `None` until a user turn exists (a brand-new
        // session persisted before its first prompt keeps `title = NULL`).
        let title = derive_session_title(s);
        let now = now_ms();
        self.inner.with_conn(|c| {
            // UPSERT rather than INSERT OR REPLACE: REPLACE deletes the existing
            // row before re-inserting, which resets `created_at` to now, wipes
            // any `title`, and (with foreign_keys ON) risks cascading the delete
            // to this session's messages. On conflict, update only the mutable
            // provider/model; preserve created_at and child rows.
            //
            // `title`: set it on first insert, and on conflict fill it in only
            // when still NULL (`COALESCE(existing, derived)`). This makes the
            // title stable once derived — a later persist (which may have
            // compacted/rewound the first turn out of `s.messages`) never
            // clobbers an already-set title — while still back-filling a session
            // whose row was created before its first user turn landed.
            c.execute(
                "INSERT INTO sessions (id, created_at, title, provider, model) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(id) DO UPDATE SET \
                     provider = excluded.provider, \
                     model = excluded.model, \
                     title = COALESCE(sessions.title, excluded.title)",
                rusqlite::params![id, now, title, provider, model],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Append a message row at the given turn index.
    ///
    /// # Errors
    /// Returns a sqlite error on write or an rkyv error on serialization failure.
    pub fn persist_message(
        &self,
        session_id: &str,
        turn_index: u32,
        m: &Message,
    ) -> Result<(), SessionStoreError> {
        let bytes = rkyv::to_bytes::<_, 4096>(m)
            .map_err(|e| SessionStoreError::Rkyv(e.to_string()))?
            .to_vec();
        let role: i64 = match m.role {
            Role::User => 0,
            Role::Assistant => 1,
            Role::Tool => 2,
            Role::System => 3,
        };
        let now = now_ms();
        self.inner.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO messages \
                 (session_id, turn_index, role, body_inline, handle_root, summary, created_at) \
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5)",
                rusqlite::params![session_id, turn_index, role, bytes, now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Persist `messages` as the COMPLETE transcript for `session_id`,
    /// replacing any previously-persisted transcript for that id.
    ///
    /// Each message is written at its positional 0-based `turn_index`, then any
    /// stale rows with `turn_index >= messages.len()` are deleted.
    ///
    /// That final truncation is load-bearing. [`Self::persist_message`] is an
    /// `INSERT OR REPLACE` keyed by `(session_id, turn_index)`, so re-persisting
    /// a session whose transcript got SHORTER — a reused/reset session id, or a
    /// rewind — would otherwise overwrite the prefix but leave the previous
    /// run's higher-indexed rows stranded. A later [`Self::load_messages`] then
    /// splices the new prefix onto that stale tail; if the splice boundary falls
    /// between an assistant `tool_use` and its `tool_result`, the Anthropic
    /// Messages API rejects the request with "unexpected `tool_use_id` found in
    /// `tool_result` blocks ... Each `tool_result` block must have a
    /// corresponding `tool_use` block in the previous message." Truncating the
    /// tail keeps the persisted rows exactly equal to `messages`.
    ///
    /// # Errors
    /// Propagates the first sqlite/rkyv error from the underlying writes.
    pub fn persist_transcript(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), SessionStoreError> {
        for (i, m) in messages.iter().enumerate() {
            let turn = u32::try_from(i).unwrap_or(u32::MAX);
            self.persist_message(session_id, turn, m)?;
        }
        // Delete any rows left over from a previously-persisted longer transcript
        // for this id, so the stored rows are exactly `messages` (no stranded
        // tail to splice on at the next load — see the doc comment above).
        let len = u32::try_from(messages.len()).unwrap_or(u32::MAX);
        self.truncate_after(session_id, len)?;
        Ok(())
    }

    /// Update the `summary` column for an existing message row. No-op if the
    /// row does not exist. Idempotent.
    ///
    /// # Errors
    /// Propagates sqlite errors.
    pub fn update_summary(
        &self,
        session_id: &str,
        turn_index: u32,
        summary: &str,
    ) -> Result<(), SessionStoreError> {
        self.inner.with_conn(|c| {
            c.execute(
                "UPDATE messages SET summary = ?1 WHERE session_id = ?2 AND turn_index = ?3",
                rusqlite::params![summary, session_id, turn_index],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Load all messages for a session, ordered by turn.
    ///
    /// # Errors
    /// Returns a sqlite error on read failure or an rkyv error on decode failure.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>, SessionStoreError> {
        let rows: Vec<Vec<u8>> = self.inner.with_conn(|c| {
            let mut stmt =
                c.prepare("SELECT body_inline FROM messages WHERE session_id = ?1 ORDER BY turn_index ASC")?;
            let iter = stmt.query_map([session_id], |r| {
                let b: Vec<u8> = r.get(0)?;
                Ok(b)
            })?;
            let mut out = Vec::new();
            for r in iter {
                out.push(r?);
            }
            Ok(out)
        })?;

        let mut messages = Vec::with_capacity(rows.len());
        for bytes in rows {
            let archived = rkyv::check_archived_root::<Message>(&bytes)
                .map_err(|e| SessionStoreError::Rkyv(e.to_string()))?;
            let m: Message = rkyv::Deserialize::deserialize(archived, &mut rkyv::Infallible)
                .map_err(|e| SessionStoreError::Rkyv(format!("{e:?}")))?;
            messages.push(m);
        }
        // Defense-in-depth: a transcript persisted before the `persist_transcript`
        // tail-truncation fix (or corrupted by any other upstream path) can carry
        // an orphaned `tool_result` — e.g. a stranded tail spliced on after a
        // reused session id. Repair it on the way out so resuming such a session
        // self-heals instead of hard-failing the next provider call with
        // `400 unexpected tool_use_id`.
        Ok(origin_core::types::strip_orphan_tool_results(messages))
    }
}

impl SessionStore {
    /// Return `(turn_index, summary)` for every persisted message of
    /// `session_id`, ordered ascending by `turn_index`.
    ///
    /// # Errors
    /// Propagates sqlite errors on read failure.
    pub fn load_summaries(&self, session_id: &str) -> Result<Vec<(u32, Option<String>)>, SessionStoreError> {
        let rows = self.inner.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT turn_index, summary FROM messages \
                 WHERE session_id = ?1 ORDER BY turn_index ASC",
            )?;
            let iter = stmt.query_map([session_id], |r| {
                let t: i64 = r.get(0)?;
                let s: Option<String> = r.get(1)?;
                Ok((u32::try_from(t).unwrap_or(u32::MAX), s))
            })?;
            let mut out = Vec::new();
            for r in iter {
                out.push(r?);
            }
            Ok(out)
        })?;
        Ok(rows)
    }

    /// Return one [`SessionSummary`] per row in the `sessions` table, ordered
    /// newest-first by `created_at`. The `message_count` column is computed by
    /// a correlated subquery against `messages`.
    ///
    /// # Errors
    /// Propagates sqlite errors on read failure.
    pub fn list_summaries(&self) -> Result<Vec<SessionSummary>, SessionStoreError> {
        let rows = self.inner.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT s.id, s.created_at, s.title, s.model, \
                        (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) \
                 FROM sessions s \
                 ORDER BY s.created_at DESC",
            )?;
            let iter = stmt.query_map([], |r| {
                let count: i64 = r.get(4)?;
                Ok(SessionSummary {
                    id: r.get(0)?,
                    created_at: r.get(1)?,
                    title: r.get(2)?,
                    model: r.get(3)?,
                    message_count: u32::try_from(count).unwrap_or(u32::MAX),
                })
            })?;
            let mut out = Vec::new();
            for r in iter {
                out.push(r?);
            }
            Ok(out)
        })?;
        Ok(rows)
    }

    /// Fold the WAL back into the main DB file. Wraps
    /// [`origin_store::Store::wal_checkpoint_truncate`] so callers don't
    /// need to depend on `origin-store` directly.
    ///
    /// # Errors
    /// Propagates the underlying sqlite error.
    pub fn checkpoint(&self) -> Result<(), SessionStoreError> {
        self.inner.wal_checkpoint_truncate()?;
        Ok(())
    }

    /// Delete a session row and all of its associated message rows. Idempotent:
    /// removing a non-existent session is a no-op.
    ///
    /// # Errors
    /// Propagates sqlite errors on write failure.
    pub fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        self.inner.with_conn(|c| {
            c.execute("DELETE FROM messages WHERE session_id = ?1", [session_id])?;
            c.execute("DELETE FROM sessions WHERE id = ?1", [session_id])?;
            Ok(())
        })?;
        Ok(())
    }

    /// Conversation rewind: keep the first `keep_turns` message rows of a
    /// session (those with `turn_index < keep_turns`) and delete the rest,
    /// rolling the transcript back to an earlier point. The session row itself
    /// is preserved so the trimmed history can still be `resume`d. Returns the
    /// number of message rows removed. Idempotent when `keep_turns` already
    /// covers the whole transcript (removes nothing).
    ///
    /// # Errors
    /// Propagates sqlite errors on write failure.
    pub fn truncate_after(&self, session_id: &str, keep_turns: u32) -> Result<u32, SessionStoreError> {
        let removed: usize = self.inner.with_conn(|c| {
            let n = c.execute(
                "DELETE FROM messages WHERE session_id = ?1 AND turn_index >= ?2",
                rusqlite::params![session_id, keep_turns],
            )?;
            Ok(n)
        })?;
        Ok(u32::try_from(removed).unwrap_or(u32::MAX))
    }

    /// Snapshot the pre-compaction `original` body for `turn_index` so a later
    /// rewind can reconstruct it. Write-once per `(session, turn)`: re-snapshotting
    /// an already-captured turn is a no-op (the first/original snapshot wins), so
    /// repeated compaction never clobbers the true original.
    ///
    /// # Errors
    /// Returns a sqlite error on write or an rkyv error on serialization failure.
    pub fn snapshot_original(
        &self,
        session_id: &str,
        turn_index: u32,
        original: &Message,
    ) -> Result<(), SessionStoreError> {
        let bytes = rkyv::to_bytes::<_, 4096>(original)
            .map_err(|e| SessionStoreError::Rkyv(e.to_string()))?
            .to_vec();
        let now = now_ms();
        self.inner.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO message_snapshots \
                 (session_id, turn_index, original_body, compacted_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![session_id, turn_index, bytes, now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Compaction-aware rewind. Like [`Self::truncate_after`] it deletes turns
    /// `>= keep_turns`, but FIRST restores `body_inline` (and clears `summary`)
    /// for every kept turn that has a pre-compaction snapshot — so the retained
    /// transcript is byte-identical to its pre-compaction state rather than
    /// leaving the collapsed `[compacted turn N]` placeholders in place. Consumed
    /// snapshots for kept turns are dropped so a later re-compaction re-snapshots
    /// fresh. Returns the number of message rows deleted. All three statements
    /// run in one connection closure (a single implicit transaction).
    ///
    /// # Errors
    /// Propagates sqlite errors on write failure.
    pub fn rewind_restoring(&self, session_id: &str, keep_turns: u32) -> Result<u32, SessionStoreError> {
        let removed: usize = self.inner.with_conn(|c| {
            // 1. Restore kept turns that were compacted, from their snapshots.
            //    The EXISTS guard ensures non-snapshotted turns are left untouched
            //    (never set to a NULL body by the correlated subquery).
            c.execute(
                "UPDATE messages SET body_inline = ( \
                     SELECT s.original_body FROM message_snapshots s \
                     WHERE s.session_id = messages.session_id AND s.turn_index = messages.turn_index), \
                     summary = NULL \
                 WHERE session_id = ?1 AND turn_index < ?2 \
                   AND EXISTS ( \
                     SELECT 1 FROM message_snapshots s \
                     WHERE s.session_id = ?1 AND s.turn_index = messages.turn_index)",
                rusqlite::params![session_id, keep_turns],
            )?;
            // 2. Drop the now-consumed snapshots for kept turns.
            c.execute(
                "DELETE FROM message_snapshots WHERE session_id = ?1 AND turn_index < ?2",
                rusqlite::params![session_id, keep_turns],
            )?;
            // 3. Delete the rewound-past turns.
            let n = c.execute(
                "DELETE FROM messages WHERE session_id = ?1 AND turn_index >= ?2",
                rusqlite::params![session_id, keep_turns],
            )?;
            Ok(n)
        })?;
        Ok(u32::try_from(removed).unwrap_or(u32::MAX))
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| {
        // Saturating cast — won't overflow in our lifetime.
        i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
    })
}

/// Maximum number of characters retained in a derived session title.
const SESSION_TITLE_MAX_CHARS: usize = 80;

/// Derive a short, deterministic session title from the first user message
/// (#15). Returns `None` when the session has no user turn yet (the `title`
/// column then stays `NULL`). No LLM call: the title is the first non-empty
/// line of the first user message's leading text block, whitespace-collapsed
/// and truncated to [`SESSION_TITLE_MAX_CHARS`] on a char boundary.
fn derive_session_title(s: &Session) -> Option<String> {
    let first_user = s.messages.iter().find(|m| m.role == Role::User)?;
    let text = first_user.blocks.iter().find_map(|b| match b {
        origin_core::types::Block::Text { text, .. } => Some(text.as_str()),
        _ => None,
    })?;
    // First non-blank line, with internal whitespace collapsed to single spaces.
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let truncated: String = if collapsed.chars().count() > SESSION_TITLE_MAX_CHARS {
        let head: String = collapsed.chars().take(SESSION_TITLE_MAX_CHARS - 1).collect();
        format!("{head}\u{2026}") // …
    } else {
        collapsed
    };
    Some(truncated)
}

#[cfg(test)]
mod tests {
    use super::{Session, SessionStore};
    use origin_core::types::{Block, Message, Role};

    #[test]
    fn truncate_after_keeps_first_n_turns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open");
        let sid = "sess-rewind";
        // Persist the parent session row first (messages FK → sessions.id).
        store
            .persist_session(&Session::new_with_id(sid.to_string(), "test-model".to_string()))
            .expect("persist session");
        for i in 0..5u32 {
            let m = Message::new(Role::User).with_block(Block::text(format!("turn {i}")));
            store.persist_message(sid, i, &m).expect("persist");
        }
        assert_eq!(store.load_messages(sid).expect("load").len(), 5);

        // Keep the first 3 turns; the last 2 are removed.
        let removed = store.truncate_after(sid, 3).expect("truncate");
        assert_eq!(removed, 2);
        assert_eq!(store.load_messages(sid).expect("load").len(), 3);

        // Idempotent: keeping more turns than exist removes nothing.
        assert_eq!(store.truncate_after(sid, 10).expect("truncate"), 0);
        assert_eq!(store.load_messages(sid).expect("load").len(), 3);

        // Keeping 0 turns clears the transcript.
        assert_eq!(store.truncate_after(sid, 0).expect("truncate"), 3);
        assert!(store.load_messages(sid).expect("load").is_empty());
    }

    /// #15 regression: `persist_session` must populate `sessions.title` from
    /// the first user message (first non-blank line, whitespace-collapsed,
    /// truncated). Before the fix `persist` hardcoded `title NULL`, so the
    /// column surfaced by `origin sessions ls` was always empty.
    #[test]
    fn persist_session_derives_title_from_first_user_message() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open");

        // A brand-new session with NO user turn ⇒ title stays NULL.
        let empty = Session::new_with_id("sess-empty".to_string(), "m".to_string());
        store.persist_session(&empty).expect("persist empty");
        let empty_sum = store
            .list_summaries()
            .expect("list")
            .into_iter()
            .find(|r| r.id == "sess-empty")
            .expect("row present");
        assert_eq!(empty_sum.title, None, "no user turn ⇒ NULL title");

        // A session with a user turn ⇒ derived title.
        let mut s = Session::new_with_id("sess-titled".to_string(), "m".to_string());
        s.push(
            Message::new(Role::User)
                .with_block(Block::text("  Fix the   parser bug\nmore details here  ")),
        );
        store.persist_session(&s).expect("persist titled");
        let sum = store
            .list_summaries()
            .expect("list")
            .into_iter()
            .find(|r| r.id == "sess-titled")
            .expect("row present");
        assert_eq!(
            sum.title.as_deref(),
            Some("Fix the parser bug"),
            "title is the first non-blank line, whitespace-collapsed"
        );

        // A later persist must NOT clobber the now-set title even if the first
        // user turn is no longer the leading message (compaction/rewind).
        let mut s2 = Session::new_with_id("sess-titled".to_string(), "m".to_string());
        s2.push(Message::new(Role::User).with_block(Block::text("a different leading turn")));
        store.persist_session(&s2).expect("re-persist");
        let sum2 = store
            .list_summaries()
            .expect("list")
            .into_iter()
            .find(|r| r.id == "sess-titled")
            .expect("row present");
        assert_eq!(
            sum2.title.as_deref(),
            Some("Fix the parser bug"),
            "an existing title must be preserved across re-persist"
        );
    }

    fn first_text(m: &Message) -> String {
        match m.blocks.first() {
            Some(Block::Text { text, .. }) => text.clone(),
            _ => String::new(),
        }
    }

    /// An assistant turn that issued one `tool_use`.
    fn assistant_tool_use(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            blocks: vec![
                Block::text("calling a tool"),
                Block::ToolUse {
                    id: id.into(),
                    name: "Read".into(),
                    input_json: b"{}".to_vec(),
                    cache_marker: None,
                },
            ],
        }
    }

    /// The `Role::Tool` message carrying the matching `tool_result`.
    fn tool_result_msg(id: &str) -> Message {
        Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: id.into(),
                handle: None,
                inline: Some(b"result body".to_vec()),
                cache_marker: None,
            }],
        }
    }

    /// First `tool_result` with no matching `tool_use` in the previous message,
    /// i.e. the exact malformation the Anthropic Messages API rejects.
    fn first_orphan(transcript: &[Message]) -> Option<String> {
        for (i, msg) in transcript.iter().enumerate() {
            for block in &msg.blocks {
                if let Block::ToolResult { tool_use_id, .. } = block {
                    let prev_has_match = i > 0
                        && transcript[i - 1]
                            .blocks
                            .iter()
                            .any(|b| matches!(b, Block::ToolUse { id, .. } if id == tool_use_id));
                    if !prev_has_match {
                        return Some(tool_use_id.clone());
                    }
                }
            }
        }
        None
    }

    /// Regression for the live `400 unexpected tool_use_id` crash: re-persisting
    /// a session whose transcript got SHORTER (a reused/reset session id) must
    /// not leave the previous, longer run's tail rows stranded. Otherwise the
    /// next `load_messages` splices the new prefix onto that stale tail, and when
    /// the boundary lands between an assistant `tool_use` and its `tool_result`
    /// the Anthropic API rejects the request. Mirrors the production seam: a long
    /// run ending in a tool turn, then a short run ending in a terminal text turn
    /// (a summary) with no `tool_use`.
    #[test]
    fn repersisting_a_shorter_transcript_does_not_strand_the_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open");
        let sid = "sess-reuse";
        store
            .persist_session(&Session::new_with_id(sid.to_string(), "test-model".to_string()))
            .expect("persist session");

        // Run A: a well-formed long transcript ending in a tool turn — two
        // assistant(tool_use)/tool(result) pairs.
        let long = vec![
            Message::new(Role::User).with_block(Block::text("A start")),
            assistant_tool_use("toolu_OLD1"),
            tool_result_msg("toolu_OLD1"),
            assistant_tool_use("toolu_OLD2"),
            tool_result_msg("toolu_OLD2"),
        ];
        store.persist_transcript(sid, &long).expect("persist long");
        assert_eq!(store.load_messages(sid).expect("load").len(), 5);

        // Run B reuses the SAME id but is SHORTER and ends in a terminal text
        // turn (no tool_use) — exactly the production crash (turn 129 = summary).
        let short = vec![
            Message::new(Role::User).with_block(Block::text("B start")),
            Message::new(Role::Assistant).with_block(Block::text("B: done, here's a summary")),
        ];
        store.persist_transcript(sid, &short).expect("persist short");

        let loaded = store.load_messages(sid).expect("load");
        assert_eq!(
            loaded.len(),
            2,
            "stale tail rows from the longer run must be deleted, not spliced on"
        );
        assert_eq!(
            first_orphan(&loaded),
            None,
            "a reused-and-shortened session must never load an orphaned tool_result",
        );
    }

    #[test]
    fn rewind_restoring_recovers_precompaction_bodies() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open(dir.path().join("sessions.db")).expect("open");
        let sid = "sess-restore";
        store
            .persist_session(&Session::new_with_id(sid.to_string(), "test-model".to_string()))
            .expect("persist session");
        for i in 0..6u32 {
            let m = Message::new(Role::User).with_block(Block::text(format!("original turn {i}")));
            store.persist_message(sid, i, &m).expect("persist");
        }
        // Snapshot the oldest 3 originals (what compaction would collapse), then
        // overwrite their bodies with placeholders to simulate compaction.
        for i in 0..3u32 {
            let original = Message::new(Role::User).with_block(Block::text(format!("original turn {i}")));
            store.snapshot_original(sid, i, &original).expect("snapshot");
            let compacted =
                Message::new(Role::User).with_block(Block::text(format!("[compacted turn {i}] sum")));
            store
                .persist_message(sid, i, &compacted)
                .expect("persist compacted");
        }
        assert!(first_text(&store.load_messages(sid).expect("load")[0]).starts_with("[compacted turn 0]"));

        // Rewind keeping all 6: restores the 3 compacted-but-kept bodies, deletes nothing.
        assert_eq!(store.rewind_restoring(sid, 6).expect("rewind"), 0);
        let msgs = store.load_messages(sid).expect("load");
        assert_eq!(msgs.len(), 6);
        for (i, msg) in msgs.iter().take(3).enumerate() {
            assert_eq!(first_text(msg), format!("original turn {i}"), "turn {i} restored");
        }
        // Non-snapshotted kept turn is untouched.
        assert_eq!(first_text(&msgs[4]), "original turn 4");

        // Rewind keeping 2 also deletes turns >= 2.
        assert_eq!(store.rewind_restoring(sid, 2).expect("rewind 2"), 4);
        assert_eq!(store.load_messages(sid).expect("load").len(), 2);
    }
}
