// SPDX-License-Identifier: Apache-2.0
//! `DiagnosticsHandle` impl wrapping `origin-lsp-client::LspClient` and
//! resolving rust-analyzer per the two-tier policy in the spec.
//!
//! # Known limitation — Phase 6
//! `DaemonRa` is constructed per-call in `dispatch_tool` because the
//! `dispatch_with_envelope` plumbing was deferred in Phase 2. The per-call
//! construction means the `OnceCell` / `LspClient` is NOT shared across calls,
//! so RA is re-spawned on each `Diagnostics` invocation. Phase 8 cleanup will
//! wire a shared `Arc<DaemonRa>` into `EnvelopeCtx`.
//!
//! # Known limitation — post-mutation notification
//! `notify_ra_after_mutation` is also deferred to Phase 8. Diagnostics sees
//! file state via `did_open` on first query per client instance. Not as
//! efficient but avoids the envelope plumbing.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use origin_lsp_client::LspClient;
use origin_tools::error::{ErrClass, ToolError};
use origin_tools::ra_bridge::{
    DiagnosticsHandle, NavCallItem, NavLocation, NavigationHandle, RaDiagnostic, Severity,
};
use tokio::sync::OnceCell;

/// Bounded wait for a single LSP navigation request before giving up.
const NAV_TIMEOUT: Duration = Duration::from_secs(10);

/// Daemon-side implementation of `DiagnosticsHandle` backed by a lazy
/// `LspClient` connected to a real rust-analyzer process.
pub struct DaemonRa {
    workspace_root: PathBuf,
    client: OnceCell<Option<Arc<LspClient>>>,
}

impl std::fmt::Debug for DaemonRa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonRa")
            .field("workspace_root", &self.workspace_root)
            .field("client_initialized", &self.client.initialized())
            .finish()
    }
}

impl DaemonRa {
    #[must_use]
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            client: OnceCell::new(),
        }
    }

    async fn client(&self) -> Option<&Arc<LspClient>> {
        let c = self
            .client
            .get_or_init(|| async {
                let bin = resolve_ra()?;
                LspClient::spawn(&bin, &self.workspace_root)
                    .await
                    .ok()
                    .map(Arc::new)
            })
            .await;
        c.as_ref()
    }
}

/// Two-tier RA resolution:
/// 1. `rust-analyzer` on PATH.
/// 2. `$ORIGIN_CACHE/bin/rust-analyzer[.exe]`.
fn resolve_ra() -> Option<String> {
    // Tier 1: PATH.
    if which::which("rust-analyzer").is_ok() {
        return Some("rust-analyzer".into());
    }
    // Tier 2: $ORIGIN_CACHE/bin/rust-analyzer.
    let cache = std::env::var("ORIGIN_CACHE")
        .ok()
        .or_else(|| std::env::var("LOCALAPPDATA").ok().map(|p| format!("{p}\\origin")))
        .or_else(|| {
            std::env::var("XDG_CACHE_HOME")
                .ok()
                .map(|p| format!("{p}/origin"))
        })
        .or_else(|| {
            dirs::home_dir().map(|h| h.join(".cache").join("origin").to_string_lossy().into_owned())
        })?;
    #[cfg(windows)]
    let bin = format!("{cache}\\bin\\rust-analyzer.exe");
    #[cfg(not(windows))]
    let bin = format!("{cache}/bin/rust-analyzer");
    if std::path::Path::new(&bin).exists() {
        Some(bin)
    } else {
        None
    }
}

#[async_trait]
impl DiagnosticsHandle for DaemonRa {
    async fn diagnostics(&self, path: Option<&Path>, _sev: Severity) -> Result<Vec<RaDiagnostic>, ToolError> {
        let Some(c) = self.client().await else {
            return Err(ToolError::new(
                ErrClass::Subsystem,
                "ra_unavailable",
                "rust-analyzer not found on PATH or in $ORIGIN_CACHE/bin \
                     (install with: origin daemon install-ra, then gunzip/unzip the archive)",
            )
            .hint("run `origin daemon install-ra` to fetch the binary"));
        };
        let raw = c.diagnostics(path).await;
        Ok(raw
            .into_iter()
            .map(|d| RaDiagnostic {
                file: d.file,
                line: d.line,
                col: d.col,
                severity: d.severity,
                message: d.message,
                code: d.code,
            })
            .collect())
    }

    async fn notify_file_changed(&self, path: &Path, contents: &str) {
        // Phase 6: best-effort; per-call DaemonRa instances don't share
        // LspClient state, so this is a no-op until Phase 8 shared wiring.
        if let Some(c) = self.client().await {
            let _ = c.did_change(path, contents).await;
        }
    }
}

/// `ra_unavailable` tool error shared by every navigation method.
fn nav_unavailable() -> ToolError {
    ToolError::new(
        ErrClass::Subsystem,
        "ra_unavailable",
        "rust-analyzer not found on PATH or in $ORIGIN_CACHE/bin",
    )
    .hint("run `origin daemon install-ra` to fetch the binary")
}

/// Map an `LspError` from a navigation request onto a subsystem tool error.
fn nav_request_err(e: &origin_lsp_client::LspError) -> ToolError {
    ToolError::new(ErrClass::Subsystem, "ra_request_failed", e.to_string())
}

/// Convert a 0-based LSP `Location` into a 1-based tool `NavLocation`.
fn to_nav_location(l: origin_lsp_client::Location) -> NavLocation {
    NavLocation {
        file: l.file,
        line: l.line.saturating_add(1),
        col: l.col.saturating_add(1),
    }
}

/// Convert a 0-based LSP `CallHierarchyItem` into a 1-based tool `NavCallItem`.
fn to_nav_call(i: origin_lsp_client::CallHierarchyItem) -> NavCallItem {
    NavCallItem {
        name: i.name,
        file: i.file,
        line: i.line.saturating_add(1),
        col: i.col.saturating_add(1),
    }
}

impl DaemonRa {
    /// Read the target file's text so the language server can answer a position
    /// query (servers only resolve positions in opened documents).
    fn nav_source(path: &Path) -> Result<String, ToolError> {
        std::fs::read_to_string(path).map_err(|e| {
            ToolError::new(
                ErrClass::Io,
                "read_failed",
                format!("cannot read {} for navigation: {e}", path.display()),
            )
        })
    }
}

/// Navigation via rust-analyzer. First cut: rust-analyzer-only (mirrors the
/// `Diagnostics` per-call posture); the trait's 1-based positions are converted
/// to/from the 0-based LSP wire form here.
#[async_trait]
impl NavigationHandle for DaemonRa {
    async fn definition(&self, path: &Path, line: u32, col: u32) -> Result<Vec<NavLocation>, ToolError> {
        let Some(c) = self.client().await else {
            return Err(nav_unavailable());
        };
        let text = Self::nav_source(path)?;
        let locs = c
            .definition(path, "rust", &text, line.saturating_sub(1), col.saturating_sub(1), NAV_TIMEOUT)
            .await
            .map_err(|e| nav_request_err(&e))?;
        Ok(locs.into_iter().map(to_nav_location).collect())
    }

    async fn references(
        &self,
        path: &Path,
        line: u32,
        col: u32,
        include_declaration: bool,
    ) -> Result<Vec<NavLocation>, ToolError> {
        let Some(c) = self.client().await else {
            return Err(nav_unavailable());
        };
        let text = Self::nav_source(path)?;
        let locs = c
            .references(
                path,
                "rust",
                &text,
                line.saturating_sub(1),
                col.saturating_sub(1),
                include_declaration,
                NAV_TIMEOUT,
            )
            .await
            .map_err(|e| nav_request_err(&e))?;
        Ok(locs.into_iter().map(to_nav_location).collect())
    }

    async fn incoming_calls(&self, path: &Path, line: u32, col: u32) -> Result<Vec<NavCallItem>, ToolError> {
        let Some(c) = self.client().await else {
            return Err(nav_unavailable());
        };
        let text = Self::nav_source(path)?;
        let items = c
            .incoming_calls(path, "rust", &text, line.saturating_sub(1), col.saturating_sub(1), NAV_TIMEOUT)
            .await
            .map_err(|e| nav_request_err(&e))?;
        Ok(items.into_iter().map(to_nav_call).collect())
    }

    async fn outgoing_calls(&self, path: &Path, line: u32, col: u32) -> Result<Vec<NavCallItem>, ToolError> {
        let Some(c) = self.client().await else {
            return Err(nav_unavailable());
        };
        let text = Self::nav_source(path)?;
        let items = c
            .outgoing_calls(path, "rust", &text, line.saturating_sub(1), col.saturating_sub(1), NAV_TIMEOUT)
            .await
            .map_err(|e| nav_request_err(&e))?;
        Ok(items.into_iter().map(to_nav_call).collect())
    }
}
