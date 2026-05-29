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
        let id = s.id.to_string();
        let provider = s.provider_name.clone();
        let model = s.model.clone();
        let now = now_ms();
        self.inner.with_conn(|c| {
            // UPSERT rather than INSERT OR REPLACE: REPLACE deletes the existing
            // row before re-inserting, which resets `created_at` to now, wipes
            // any `title`, and (with foreign_keys ON) risks cascading the delete
            // to this session's messages. On conflict, update only the mutable
            // provider/model and leave created_at, title, and child rows intact.
            c.execute(
                "INSERT INTO sessions (id, created_at, title, provider, model) \
                 VALUES (?1, ?2, NULL, ?3, ?4) \
                 ON CONFLICT(id) DO UPDATE SET provider = excluded.provider, model = excluded.model",
                rusqlite::params![id, now, provider, model],
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
        Ok(messages)
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
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| {
            // Saturating cast — won't overflow in our lifetime.
            i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
        })
        .unwrap_or(0)
}
