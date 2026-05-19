//! SQLite-backed session persistence (inline blobs for P1; CAS handles arrive in P2).

use std::path::Path;

use origin_core::types::{Message, Role};
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
}

impl SessionStore {
    /// Open or create the `SQLite` database at `path` and run migrations.
    ///
    /// # Errors
    /// Propagates `StoreError` for open/migration failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionStoreError> {
        Ok(Self {
            inner: Store::open(path)?,
        })
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
            c.execute(
                "INSERT OR REPLACE INTO sessions (id, created_at, title, provider, model) \
                 VALUES (?1, ?2, NULL, ?3, ?4)",
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
