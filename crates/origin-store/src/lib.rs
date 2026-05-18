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
}
