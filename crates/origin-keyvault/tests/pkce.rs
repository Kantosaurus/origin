// SPDX-License-Identifier: Apache-2.0
//! PKCE verifier/challenge property tests (RFC 7636).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use origin_keyvault::Pkce;
use sha2::{Digest, Sha256};

#[test]
fn verifier_is_within_rfc7636_length_and_charset() {
    let pkce = Pkce::new();
    let verifier = pkce.verifier();

    let len = verifier.len();
    assert!(
        (43..=128).contains(&len),
        "verifier must be 43..=128 chars (RFC 7636), got {len}"
    );

    assert!(
        verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "verifier must only contain unreserved [A-Za-z0-9_-]: {verifier}"
    );
}

#[test]
fn challenge_is_sha256_base64url_of_verifier() {
    let pkce = Pkce::new();
    let verifier = pkce.verifier();
    let challenge = pkce.challenge();

    let digest = Sha256::digest(verifier.as_bytes());
    let expected = URL_SAFE_NO_PAD.encode(digest);

    assert_eq!(
        challenge, expected,
        "challenge must be base64url-no-pad(sha256(verifier))"
    );
}
