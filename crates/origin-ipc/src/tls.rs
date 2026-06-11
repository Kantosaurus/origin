// SPDX-License-Identifier: Apache-2.0
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub struct CertBundle {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub ca_der: Vec<u8>,
}

/// Generate a self-signed Ed25519 cert.
///
/// # Errors
/// Returns an `rcgen::Error` if cert serialization fails.
pub fn generate_self_signed(cn: &str) -> Result<CertBundle, rcgen::Error> {
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(DnType::CommonName, cn);
    params.subject_alt_names = vec![SanType::DnsName(cn.to_string().try_into()?)];
    let cert = params.self_signed(&key_pair)?;
    let cert_der = cert.der().to_vec();
    let key_der = key_pair.serialize_der();
    Ok(CertBundle {
        ca_der: cert_der.clone(),
        cert_der,
        key_der,
    })
}

/// A certificate's SHA-256 fingerprint — the zero-trust pinning anchor.
///
/// This 32-byte hash, not a CA chain, is what the QUIC transport pins. Because
/// it is a hash (not a public-key signature), it stays sound against a quantum
/// adversary: even one able to forge the classical Ed25519 cert signature
/// cannot produce a different certificate with the same SHA-256 fingerprint.
/// See [`crate::quic`] for how it gates the handshake.
pub type CertFingerprint = [u8; 32];

#[must_use]
pub fn sha256_fingerprint_hex(cert_der: &[u8]) -> String {
    hex::encode(sha256_fingerprint(cert_der))
}

/// Raw 32-byte SHA-256 fingerprint of a DER-encoded certificate.
#[must_use]
pub fn sha256_fingerprint(cert_der: &[u8]) -> CertFingerprint {
    let digest = Sha256::digest(cert_der);
    let mut out = [0_u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Parse a hex-encoded SHA-256 fingerprint (64 hex chars, case-insensitive,
/// optional `:`/whitespace separators) into 32 bytes.
///
/// Returns `None` when the input does not decode to exactly 32 bytes — callers
/// must treat that as "no trustworthy pin" and fail closed rather than fall
/// back to unauthenticated trust.
#[must_use]
pub fn parse_fingerprint_hex(s: &str) -> Option<CertFingerprint> {
    let cleaned: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | ' ' | '\t' | '\n' | '\r'))
        .collect();
    let bytes = hex::decode(cleaned).ok()?;
    let arr: CertFingerprint = bytes.try_into().ok()?;
    Some(arr)
}

/// Constant-time equality for two fingerprints, so a pin check cannot be turned
/// into a timing oracle that leaks how many leading bytes matched.
#[must_use]
pub fn fingerprints_eq(a: &CertFingerprint, b: &CertFingerprint) -> bool {
    let mut diff = 0_u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
