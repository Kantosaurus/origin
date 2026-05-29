// SPDX-License-Identifier: Apache-2.0
//! `origin-keyvault` — cross-platform secret storage with a uniform async
//! façade.
//!
//! The crate exposes a single entry point, [`KeyVault`], which dispatches to
//! a platform-specific backend (Linux Secret Service, macOS Keychain,
//! Windows Credential Manager) or to an in-process [`MemoryBackend`] when
//! the `ORIGIN_KEYVAULT=memory` env var is set. Secrets enter and leave
//! through [`Secret<T>`] so the inner value is zeroized on drop and never
//! appears in `Debug` output.
//!
//! Phase 8.1 shipped the façade, `Secret<T>`, and the per-OS backends.
//! Phase 8.2 adds the OAuth driver (`Pkce` + `OAuthClient`) re-exported
//! from [`oauth`].

pub mod audit;
mod backend;
mod backend_memory;
mod oauth;
mod secret;

pub use crate::oauth::{AuthCodeRequest, ExchangedTokens, OAuthClient, Pkce, RefreshOutcome};

use crate::audit::{AuditAction, AuditRing};

#[cfg(target_os = "linux")]
mod backend_linux;
#[cfg(target_os = "macos")]
mod backend_macos;
#[cfg(target_os = "windows")]
mod backend_windows;

use std::sync::Arc;

use thiserror::Error;
use zeroize::Zeroize;

use crate::backend::Backend;
use crate::backend_memory::MemoryBackend;

pub use crate::secret::Secret;

/// Errors surfaced by the keyvault façade and its backends.
#[derive(Debug, Error)]
pub enum Error {
    /// No secret exists for the given (provider, account) pair.
    #[error("no secret for provider `{provider}` account `{account}`")]
    NotFound {
        /// Provider namespace (e.g. `"anthropic"`).
        provider: String,
        /// Account identifier within the provider (e.g. `"default"`).
        account: String,
    },
    /// Wrapped backend failure (D-Bus, Keychain, Cred Manager, …).
    #[error("keyvault backend error: {0}")]
    Backend(String),
    /// Stored value was not valid UTF-8; `KeyVault::get` returns
    /// `Secret<String>` and rejects non-UTF-8 blobs here.
    #[error("stored value is not valid UTF-8")]
    Utf8,
    /// Serialization failure (reserved for P8.2 OAuth token blobs).
    #[error("serde error: {0}")]
    Serde(String),
}

/// Cross-platform secret store.
#[derive(Clone)]
pub struct KeyVault {
    inner: Arc<dyn Backend>,
    audit: Option<Arc<AuditRing>>,
}

impl KeyVault {
    /// Picks a backend based on the running OS and the `ORIGIN_KEYVAULT`
    /// env var. Setting `ORIGIN_KEYVAULT=memory` forces the in-process
    /// backend on every platform (used by tests and ephemeral CI runs).
    ///
    /// # Errors
    /// Currently never fails on any supported platform — the platform
    /// backends defer their connection setup until the first call. Returns
    /// `Result` so future backends (e.g. a remote vault) can fail eagerly.
    pub fn detect() -> Result<Self, Error> {
        if std::env::var("ORIGIN_KEYVAULT").as_deref() == Ok("memory") {
            return Ok(Self::in_memory());
        }

        #[cfg(target_os = "linux")]
        let backend: Arc<dyn Backend> = Arc::new(crate::backend_linux::LinuxBackend::new());
        #[cfg(target_os = "macos")]
        let backend: Arc<dyn Backend> = Arc::new(crate::backend_macos::MacBackend::new());
        #[cfg(target_os = "windows")]
        let backend: Arc<dyn Backend> = Arc::new(crate::backend_windows::WindowsBackend::new());
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let backend: Arc<dyn Backend> = Arc::new(MemoryBackend::new());

        Ok(Self {
            inner: backend,
            audit: None,
        })
    }

    /// Returns a vault backed by an in-process map. Values do not survive
    /// the process and are not encrypted on disk.
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            inner: Arc::new(MemoryBackend::new()),
            audit: None,
        }
    }

    /// Open the platform-detected vault with an attached audit ring rooted
    /// at `audit_dir`. Every public method on the returned [`KeyVault`]
    /// records a best-effort (provider, account, action, timestamp) tuple
    /// into the ring after the backend call returns.
    ///
    /// # Errors
    /// Forwards [`audit::AuditError`] as [`Error::Backend`] and any error
    /// returned by [`KeyVault::detect`].
    #[allow(clippy::future_not_send)]
    pub async fn detect_with_audit<P: AsRef<std::path::Path>>(audit_dir: P) -> Result<Self, Error> {
        let mut vault = Self::detect()?;
        let ring = AuditRing::open(audit_dir)
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;
        vault.audit = Some(Arc::new(ring));
        Ok(vault)
    }

    async fn audit(&self, action: AuditAction, provider: &str, account: &str) {
        if let Some(ring) = &self.audit {
            let _ = ring.record(action, provider, account).await;
        }
    }

    /// Stores `value` under the given (provider, account) pair.
    ///
    /// # Errors
    /// Forwards backend-specific failures via [`Error::Backend`].
    pub async fn set<T>(&self, provider: &str, account: &str, value: Secret<T>) -> Result<(), Error>
    where
        T: Zeroize + AsRef<[u8]> + Send,
    {
        // Materialize a `Vec<u8>` copy and zeroize it after the backend
        // call so the intermediate buffer is short-lived. The caller's
        // `Secret<T>` itself zeroizes on drop at function exit.
        let mut bytes = value.expose().as_ref().to_vec();
        let result = self.inner.set(provider, account, &bytes).await;
        bytes.zeroize();
        self.audit(AuditAction::Set, provider, account).await;
        result
    }

    /// Retrieves a previously stored secret as a `Secret<String>`. Returns
    /// [`Error::NotFound`] if no value exists; [`Error::Utf8`] if the
    /// stored bytes are not valid UTF-8.
    ///
    /// # Errors
    /// See [`Error`].
    pub async fn get(&self, provider: &str, account: &str) -> Result<Secret<String>, Error> {
        let mut bytes = self.inner.get(provider, account).await?;
        let s = String::from_utf8(bytes.clone()).map_err(|_| Error::Utf8);
        // Wipe the intermediate buffer regardless of UTF-8 outcome.
        bytes.zeroize();
        self.audit(AuditAction::Get, provider, account).await;
        Ok(Secret::new(s?))
    }

    /// Deletes the stored secret. Returns [`Error::NotFound`] if no entry
    /// exists.
    ///
    /// # Errors
    /// Forwards backend-specific failures via [`Error::Backend`].
    pub async fn delete(&self, provider: &str, account: &str) -> Result<(), Error> {
        let result = self.inner.delete(provider, account).await;
        self.audit(AuditAction::Delete, provider, account).await;
        result
    }

    /// Lists every account stored under `provider`. macOS and Windows
    /// return an empty `Vec` until P11 wires enumeration; Linux and the
    /// in-memory backend return real entries today.
    ///
    /// # Errors
    /// Forwards backend-specific failures via [`Error::Backend`].
    pub async fn list(&self, provider: &str) -> Result<Vec<String>, Error> {
        let result = self.inner.list(provider).await;
        self.audit(AuditAction::List, provider, "").await;
        result
    }
}

impl core::fmt::Debug for KeyVault {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyVault").finish_non_exhaustive()
    }
}
