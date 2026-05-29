// SPDX-License-Identifier: Apache-2.0
//! Parser for `origin://host:port#fingerprint` URLs used by the
//! remote-IPC pairing flow (P13.2.4).
//!
//! The fragment is a hex-encoded SHA-256 cert fingerprint that the
//! daemon prints alongside its 6-digit pair code. Production builds
//! will fetch the matching CA DER blob over a metadata channel; for
//! now the test rig sets `ORIGIN_REMOTE_CA_DER_FILE` to the path of
//! a pre-shared DER blob.

use std::net::SocketAddr;

pub struct OriginUrl {
    pub addr: SocketAddr,
    pub fingerprint_hex: String,
}

impl OriginUrl {
    /// Load CA DER from `ORIGIN_REMOTE_CA_DER_FILE` (env). Production
    /// will fetch via an out-of-band metadata channel; for now the
    /// test fixture writes the daemon's own cert to a tempfile and
    /// passes the path via env.
    #[must_use]
    pub fn fingerprint_to_ca_placeholder(&self) -> Vec<u8> {
        std::env::var("ORIGIN_REMOTE_CA_DER_FILE")
            .ok()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default()
    }
}

/// Parse `origin://host:port#fingerprint` into addr + fingerprint.
///
/// # Errors
/// Returns an error if the URL is malformed, the scheme is not
/// `origin`, or host/port are missing.
pub fn parse_origin_url(url: &str) -> anyhow::Result<OriginUrl> {
    let parsed = url::Url::parse(url)?;
    if parsed.scheme() != "origin" {
        anyhow::bail!("expected origin:// URL, got {url}");
    }
    let host = parsed.host_str().ok_or_else(|| anyhow::anyhow!("missing host"))?;
    let port = parsed.port().ok_or_else(|| anyhow::anyhow!("missing port"))?;
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    let fingerprint_hex = parsed.fragment().unwrap_or("").to_string();
    Ok(OriginUrl {
        addr,
        fingerprint_hex,
    })
}
