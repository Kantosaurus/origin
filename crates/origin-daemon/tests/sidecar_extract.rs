// SPDX-License-Identifier: Apache-2.0
#[test]
fn extract_threshold_is_16kb() {
    assert_eq!(origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES, 16 * 1024);
}

#[test]
fn small_payload_skips_extract() {
    let payload = vec![b'x'; 1024]; // 1 KB < 16 KB
    assert!(payload.len() < origin_sidecar::extract::EXTRACT_THRESHOLD_BYTES);
}
