use std::path::Path;
use std::sync::Mutex;

use refinery::embed_migrations;
use rusqlite::Connection;
use thiserror::Error;

embed_migrations!("src/migrations");

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration: {0}")]
    Migration(#[from] refinery::Error),
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (or create) a `SQLite` database at `path` and run any pending migrations.
    ///
    /// # Errors
    /// Returns a `StoreError` if the database cannot be opened or migrations fail.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let mut conn = Connection::open(path)?;
        // WAL mode and synchronous must be set outside a transaction;
        // refinery wraps each migration in one, so we apply them here.
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\
             PRAGMA synchronous  = NORMAL;",
        )?;
        migrations::runner().run(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Run a closure against the underlying connection under a mutex.
    ///
    /// # Errors
    /// Propagates rusqlite errors from the closure.
    ///
    /// # Panics
    /// Panics if the connection mutex is poisoned (a prior caller panicked while holding it).
    #[allow(clippy::expect_used)]
    pub fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> rusqlite::Result<R> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        f(&conn)
    }

    /// True iff a migrated session with this content-key was already inserted.
    ///
    /// # Errors
    /// Propagates rusqlite errors from the query.
    pub fn contains_migrated_session(&self, key: &str) -> rusqlite::Result<bool> {
        self.with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM migrated_sessions WHERE key = ?",
                [key],
                |r| r.get(0),
            )?;
            Ok(n > 0)
        })
    }

    /// Insert a migrated session keyed by `key` with JSON `body`.
    ///
    /// # Errors
    /// Propagates rusqlite errors (including UNIQUE violations) from the insert.
    pub fn insert_migrated_session(&self, key: &str, body_json: &str) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO migrated_sessions(key, body) VALUES(?, ?)",
                rusqlite::params![key, body_json],
            )?;
            Ok(())
        })
    }

    /// True iff a migrated skill with this content-key was already inserted.
    ///
    /// # Errors
    /// Propagates rusqlite errors from the query.
    pub fn contains_migrated_skill(&self, key: &str) -> rusqlite::Result<bool> {
        self.with_conn(|c| {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM migrated_skills WHERE key = ?", [key], |r| {
                r.get(0)
            })?;
            Ok(n > 0)
        })
    }

    /// Insert a migrated skill keyed by `key` with JSON `body`.
    ///
    /// # Errors
    /// Propagates rusqlite errors (including UNIQUE violations) from the insert.
    pub fn insert_migrated_skill(&self, key: &str, body_json: &str) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO migrated_skills(key, body) VALUES(?, ?)",
                rusqlite::params![key, body_json],
            )?;
            Ok(())
        })
    }

    /// Run `PRAGMA wal_checkpoint(TRUNCATE)` so the WAL is folded back into
    /// the main database file and truncated. Safe to run on a quiesced
    /// store; concurrent writers will be queued by the connection mutex.
    ///
    /// # Errors
    /// Propagates rusqlite errors from the PRAGMA.
    pub fn wal_checkpoint_truncate(&self) -> rusqlite::Result<()> {
        self.with_conn(|c| {
            c.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
    }
}
