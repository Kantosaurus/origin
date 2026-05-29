// SPDX-License-Identifier: Apache-2.0
use origin_cas::{chunks, Hash};

/// Deterministic pseudo-random byte sequence via a Wang-hash style mixer.
/// FastCDC needs entropy across its 64-byte gear window to find content-defined
/// boundaries; a trivially periodic sequence (e.g. `i % 251`) produces only
/// max-size cuts and defeats the dedup property the test is meant to verify.
fn prng_bytes(n: usize) -> Vec<u8> {
    (0..n as u32)
        .map(|i| {
            let mut x = i.wrapping_mul(2_654_435_761);
            x ^= x >> 16;
            x = x.wrapping_mul(2_246_822_519);
            x ^= x >> 13;
            x as u8
        })
        .collect()
}

/// 200 KB of pseudo-random but deterministic bytes, plus one inserted byte at
/// the midpoint. Expect >85% of chunk hashes shared across the two streams.
#[test]
fn one_byte_inserted_dedupes_neighbors() {
    let data = prng_bytes(200_000);
    let mut edited = data.clone();
    edited.insert(50_000, 0xFF);

    let a: Vec<Hash> = chunks(&data).map(|c| c.hash).collect();
    let b: Vec<Hash> = chunks(&edited).map(|c| c.hash).collect();

    assert!(
        a.len() > 4,
        "expected ≥5 chunks on a 200KB stream, got {}",
        a.len()
    );

    let shared = a.iter().filter(|h| b.contains(h)).count();
    let ratio = shared as f64 / a.len() as f64;
    assert!(ratio > 0.85, "expected >85% chunk reuse, got {ratio:.3}");
}

#[test]
fn small_input_yields_one_chunk() {
    let data = b"short";
    let cs: Vec<_> = chunks(data).collect();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].offset, 0);
    assert_eq!(cs[0].length, data.len());
    assert_eq!(cs[0].hash, Hash::of(data));
}

#[test]
fn chunks_cover_input_exactly() {
    let data: Vec<u8> = (0..120_000u32).map(|i| (i % 199) as u8).collect();
    let cs: Vec<_> = chunks(&data).collect();
    let total: usize = cs.iter().map(|c| c.length).sum();
    assert_eq!(total, data.len());
    let mut cursor = 0;
    for c in &cs {
        assert_eq!(c.offset, cursor);
        cursor += c.length;
    }
}
