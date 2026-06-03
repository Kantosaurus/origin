// SPDX-License-Identifier: Apache-2.0
//! Error type for the Gmail tool.
//!
//! Every variant carries a human-readable message only — never a raw token
//! or secret byte. The `xtask lint-secrets` gate plus the `Secret<T>`
//! redaction pattern keep credentials out of `Debug`/`Display` output.

use thiserror::Error;

/// Errors surfaced by [`crate::Gmail`] and its helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// The keyvault had no stored Google credentials for the requested
    /// (provider, account) pair, or the blob was unreadable.
    #[error("gmail credentials unavailable: {0}")]
    Credentials(String),

    /// The stored credential blob did not parse as the expected JSON shape
    /// (missing `client_id` / `client_secret` / `refresh_token`).
    #[error("gmail credential blob malformed: {0}")]
    CredentialFormat(String),

    /// An HTTP transport error talking to Google's OAuth or Gmail endpoints.
    #[error("gmail http error: {0}")]
    Http(String),

    /// A non-2xx response from the OAuth token endpoint or Gmail REST API.
    /// The message includes the status and (non-secret) provider error body.
    #[error("gmail api status {status}: {body}")]
    Status {
        /// HTTP status code returned by the endpoint.
        status: u16,
        /// Provider error body (Google returns a JSON error object here; it
        /// never contains the caller's credentials).
        body: String,
    },

    /// A JSON (de)serialisation failure while parsing a Gmail API response.
    #[error("gmail parse error: {0}")]
    Parse(String),

    /// A required argument was missing or invalid in a `run_tool` call.
    #[error("gmail bad arguments: {0}")]
    BadArgs(String),
}

impl Error {
    /// Build a [`Error::Status`] from a status code and body string.
    #[must_use]
    pub fn status(status: u16, body: impl Into<String>) -> Self {
        Self::Status {
            status,
            body: body.into(),
        }
    }
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;
