// SPDX-License-Identifier: Apache-2.0
//! Parser for `origin://host:port#fingerprint` URLs used by the
//! remote-IPC pairing flow (P13.2.4).
//!
//! The fragment is a hex-encoded SHA-256 cert fingerprint that the
//! daemon prints alongside its 6-digit pair code. The client pins the
//! daemon's certificate to this fingerprint — a hash, so the trust
//! anchor is both zero-trust (only the exact paired daemon is trusted,
//! no CA chain to subvert) and post-quantum-safe (a quantum adversary
//! who forges the classical cert signature still cannot match the
//! SHA-256 hash). The previous `ORIGIN_REMOTE_CA_DER_FILE` placeholder
//! (which loaded a CA blob and never checked the fingerprint) is gone.

use std::net::SocketAddr;

use origin_ipc::tls::{CertBundle, CertFingerprint};

pub struct OriginUrl {
    pub addr: SocketAddr,
    pub fingerprint_hex: String,
}

impl OriginUrl {
    /// The pinned server certificate fingerprint from the URL fragment.
    ///
    /// # Errors
    /// Fails closed when the fragment is missing or is not a valid 32-byte
    /// SHA-256 fingerprint — we never connect to a remote daemon without a pin.
    pub fn server_fingerprint(&self) -> anyhow::Result<CertFingerprint> {
        origin_ipc::tls::parse_fingerprint_hex(&self.fingerprint_hex).ok_or_else(|| {
            anyhow::anyhow!(
                "remote daemon URL is missing a valid #<sha256-fingerprint>; \
                 refusing to connect without a certificate pin"
            )
        })
    }
}

/// Resolve the client identity presented for mutual TLS to the remote daemon.
///
/// Zero-trust requires the client to authenticate too. When both
/// `ORIGIN_REMOTE_CLIENT_CERT_FILE` and `ORIGIN_REMOTE_CLIENT_KEY_FILE` (DER)
/// are set, the pinned identity provisioned at pairing time is loaded;
/// otherwise an ephemeral self-signed identity is minted. (Binding the client
/// identity to the redeemed pairing bearer is future work.)
///
/// # Errors
/// Returns an error if a configured cert/key file cannot be read or an
/// ephemeral identity cannot be generated.
pub fn resolve_client_bundle() -> anyhow::Result<CertBundle> {
    let cert = std::env::var("ORIGIN_REMOTE_CLIENT_CERT_FILE")
        .ok()
        .map(std::fs::read)
        .transpose()?;
    let key = std::env::var("ORIGIN_REMOTE_CLIENT_KEY_FILE")
        .ok()
        .map(std::fs::read)
        .transpose()?;
    if let (Some(cert_der), Some(key_der)) = (cert, key) {
        return Ok(CertBundle {
            ca_der: cert_der.clone(),
            cert_der,
            key_der,
        });
    }
    origin_ipc::tls::generate_self_signed("origin-client")
        .map_err(|e| anyhow::anyhow!("generating client identity: {e}"))
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
