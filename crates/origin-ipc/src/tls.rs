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

#[must_use]
pub fn sha256_fingerprint_hex(cert_der: &[u8]) -> String {
    let digest = Sha256::digest(cert_der);
    hex::encode(digest)
}
