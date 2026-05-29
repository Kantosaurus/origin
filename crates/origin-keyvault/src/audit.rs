// SPDX-License-Identifier: Apache-2.0
//! `KeyVault` audit log: 30-day rotating ring, 8 MiB pages, JSON-Lines on disk.
//!
//! Records **what** key was touched (provider + account + action + timestamp),
//! never the secret bytes. The ring is independent of the parquet trace
//! pipeline (N10.16) so a parquet failure cannot drop audit records.

#![allow(
    clippy::module_name_repetitions,
    clippy::future_not_send,
    clippy::option_if_let_else
)]

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt as _, BufReader};
use tokio::sync::Mutex;

/// Errors raised by the audit ring.
#[derive(Debug, Error)]
pub enum AuditError {
    /// Underlying I/O failure on the audit page.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialization failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

/// Action recorded for an audit event. Mirrors the `KeyVault` public API
/// surface so each call-site can be traced without leaking the secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    /// `KeyVault::set` was invoked.
    Set,
    /// `KeyVault::get` was invoked.
    Get,
    /// `KeyVault::delete` was invoked.
    Delete,
    /// `KeyVault::list` was invoked.
    List,
}

/// One audit-ring record. Never contains the secret bytes — only the
/// (provider, account, action, timestamp) tuple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unix epoch milliseconds at the time of the event.
    pub ts_ms: i64,
    /// Action that was performed.
    pub action: AuditAction,
    /// Provider namespace (e.g. `"anthropic"`).
    pub provider: String,
    /// Account identifier (e.g. `"default"`).
    pub account: String,
}

/// 30-day rotating audit ring. Pages roll over at 8 MiB by default;
/// callers can override the page size via [`AuditRing::open_with_page_size`]
/// for tests.
pub struct AuditRing {
    dir: PathBuf,
    page_bytes: usize,
    current: Mutex<RingState>,
}

struct RingState {
    file: File,
    current_path: PathBuf,
    bytes: usize,
}

impl AuditRing {
    /// Open (or create) an audit ring rooted at `dir`. Uses the default 8 MiB
    /// page size.
    ///
    /// # Errors
    /// Returns [`AuditError::Io`] if the directory cannot be created or the
    /// initial page cannot be opened.
    pub async fn open<P: AsRef<Path>>(dir: P) -> Result<Self, AuditError> {
        Self::open_with_page_size(dir, 8 * 1024 * 1024).await
    }

    /// Same as [`Self::open`] but with a custom page byte-budget — used by
    /// integration tests to force rotation without writing megabytes.
    ///
    /// # Errors
    /// Returns [`AuditError::Io`] if the directory cannot be created or the
    /// initial page cannot be opened.
    pub async fn open_with_page_size<P: AsRef<Path>>(dir: P, page_bytes: usize) -> Result<Self, AuditError> {
        let dir = dir.as_ref().to_path_buf();
        tokio::fs::create_dir_all(&dir).await?;
        let (file, path, bytes) = Self::open_current_page(&dir).await?;
        Ok(Self {
            dir,
            page_bytes,
            current: Mutex::new(RingState {
                file,
                current_path: path,
                bytes,
            }),
        })
    }

    async fn open_current_page(dir: &Path) -> Result<(File, PathBuf, usize), AuditError> {
        let today = Utc::now().format("%Y-%m-%d");
        let path = dir.join(format!("audit-{today}.jsonl"));
        let bytes = match tokio::fs::metadata(&path).await {
            Ok(m) => usize::try_from(m.len()).unwrap_or(0),
            Err(_) => 0,
        };
        let file = OpenOptions::new().create(true).append(true).open(&path).await?;
        Ok((file, path, bytes))
    }

    /// Record an event. Never blocks on disk-rotation lock contention beyond
    /// the per-process mutex (one ring per daemon).
    ///
    /// # Errors
    /// Returns [`AuditError`] on I/O or serialization failure.
    pub async fn record(&self, action: AuditAction, provider: &str, account: &str) -> Result<(), AuditError> {
        let ev = AuditEvent {
            ts_ms: Utc::now().timestamp_millis(),
            action,
            provider: provider.into(),
            account: account.into(),
        };
        let mut line = serde_json::to_string(&ev)?;
        line.push('\n');
        let buf = line.as_bytes();

        {
            let mut g = self.current.lock().await;
            if g.bytes + buf.len() > self.page_bytes {
                g.file.flush().await?;
                let (file, path, _) = Self::open_next_page(&self.dir).await?;
                g.file = file;
                g.current_path = path;
                g.bytes = 0;
            }
            g.file.write_all(buf).await?;
            g.bytes += buf.len();
        }
        // Best-effort GC: remove any audit page older than 30 days.
        let _ = Self::gc_old_pages(&self.dir).await;
        Ok(())
    }

    async fn open_next_page(dir: &Path) -> Result<(File, PathBuf, usize), AuditError> {
        let stamp = Utc::now().format("%Y-%m-%d-%H%M%S%f");
        let path = dir.join(format!("audit-{stamp}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await?;
        Ok((file, path, 0))
    }

    async fn gc_old_pages(dir: &Path) -> Result<(), AuditError> {
        let cutoff = Utc::now() - chrono::Duration::days(30);
        let mut rd = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let mtime: chrono::DateTime<Utc> = meta
                .modified()
                .ok()
                .map_or_else(Utc::now, chrono::DateTime::<Utc>::from);
            if mtime < cutoff {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
        Ok(())
    }

    /// Read every page in chronological order and return all events. Used by
    /// integration tests and the future `origin keyring audit` CLI.
    ///
    /// # Errors
    /// Returns [`AuditError`] on I/O / parse failure.
    pub async fn replay(&self) -> Result<Vec<AuditEvent>, AuditError> {
        let mut entries: Vec<PathBuf> = Vec::new();
        let mut rd = tokio::fs::read_dir(&self.dir).await?;
        while let Some(e) = rd.next_entry().await? {
            if e.path().extension().and_then(|s| s.to_str()) == Some("jsonl") {
                entries.push(e.path());
            }
        }
        entries.sort();
        let mut out = Vec::new();
        for p in entries {
            let f = tokio::fs::File::open(&p).await?;
            let mut lines = BufReader::new(f).lines();
            while let Some(line) = lines.next_line().await? {
                if line.is_empty() {
                    continue;
                }
                out.push(serde_json::from_str(&line)?);
            }
        }
        // Filenames are sorted lexicographically above, which misorders
        // intra-day rotated pages (e.g. `…​.10` before `…​.2`). Sort the merged
        // events by their own timestamp so the result is truly chronological
        // regardless of the page-file naming/rotation scheme. Stable sort keeps
        // same-millisecond events in their original (append) order.
        out.sort_by_key(|e: &AuditEvent| e.ts_ms);
        Ok(out)
    }
}
