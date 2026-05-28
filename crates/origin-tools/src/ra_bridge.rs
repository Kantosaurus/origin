//! Trait the envelope passes into the `Diagnostics` tool. Implemented daemon-side
//! by `origin-daemon::ra_impl::DaemonRa` (wraps `origin-lsp-client::LspClient`).
//!
//! # Design note — Phase 6
//! `DaemonRa` is constructed per-call (not shared via `EnvelopeCtx`) because the
//! `dispatch_with_envelope` plumbing was deferred in Phase 2. Phase 8 cleanup
//! will wire the shared handle; until then, per-call construction is suboptimal
//! but unblocking.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;

/// Filter level for [`DiagnosticsHandle::diagnostics`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// All diagnostics regardless of level.
    Any,
    /// Only errors (severity code 1).
    Error,
    /// Errors + warnings (severity code ≤ 2).
    Warning,
    /// Errors + warnings + info + hints (severity code ≤ 4).
    Hint,
}

impl Severity {
    /// Returns `true` if `sev_code` passes this filter.
    #[must_use]
    pub const fn allows(self, sev_code: u8) -> bool {
        match self {
            Self::Any => true,
            Self::Error => sev_code == 1,
            Self::Warning => sev_code <= 2,
            Self::Hint => sev_code <= 4,
        }
    }
}

/// A single LSP diagnostic, normalised out of the raw wire format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaDiagnostic {
    pub file: std::path::PathBuf,
    pub line: u32,
    pub col: u32,
    /// LSP severity code: 1=error, 2=warning, 3=information, 4=hint.
    pub severity: u8,
    pub message: String,
    pub code: Option<String>,
}

/// Object-safe trait mirroring `MemoryHandle` (from `origin_tools::dispatch`).
/// Implemented in the daemon by `DaemonRa`; tests use `FakeRa`.
#[async_trait]
pub trait DiagnosticsHandle: Send + Sync + std::fmt::Debug {
    /// Fetch diagnostics, optionally filtered to `path` and by `severity`.
    ///
    /// # Errors
    /// Returns `subsystem.ra_unavailable` if the language server is down.
    async fn diagnostics(
        &self,
        path: Option<&Path>,
        severity: Severity,
    ) -> Result<Vec<RaDiagnostic>, ToolError>;

    /// Inform the server that `path` now has `contents` (full-sync).
    /// Best-effort — callers must not rely on this completing before the
    /// next `diagnostics` call returns updated results.
    ///
    /// # Note — Phase 6 deferral
    /// Post-mutation notification (`notify_ra_after_mutation`) is deferred
    /// to Phase 8. For now, Diagnostics sees file state via `did_open` on
    /// first query. Not as efficient but avoids the envelope plumbing.
    async fn notify_file_changed(&self, path: &Path, contents: &str);
}
