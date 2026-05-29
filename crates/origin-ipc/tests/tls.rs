// SPDX-License-Identifier: Apache-2.0
use origin_ipc::tls::{generate_self_signed, sha256_fingerprint_hex};

#[test]
fn self_signed_cert_has_stable_fingerprint() {
    let bundle = generate_self_signed("origin-daemon").expect("generate");
    let fp = sha256_fingerprint_hex(&bundle.cert_der);
    assert_eq!(fp.len(), 64);
    assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn cert_der_and_key_der_are_nonempty() {
    let bundle = generate_self_signed("origin-daemon").expect("generate");
    assert!(!bundle.cert_der.is_empty());
    assert!(!bundle.key_der.is_empty());
    assert_eq!(bundle.ca_der, bundle.cert_der);
}
