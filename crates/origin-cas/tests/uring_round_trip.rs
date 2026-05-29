// SPDX-License-Identifier: Apache-2.0
//! Linux-only — write a pack with the std writer and read entries back with
//! the uring reader.

#![cfg(all(target_os = "linux", feature = "uring"))]

use origin_cas::{packfile_uring::read_at_uring, Hash, PackBuilder, PackReader};
use tempfile::TempDir;

#[test]
fn uring_round_trip() {
    let dir = TempDir::new().expect("tmpdir");
    let path = dir.path().join("test.pack");

    // Build a small pack via the std writer.
    let mut b = PackBuilder::create(&path).expect("create");
    let payloads: Vec<(Hash, Vec<u8>)> = (0..16u8)
        .map(|i| {
            let bytes = vec![i; 4096];
            let h = Hash::of(&bytes);
            (h, bytes)
        })
        .collect();
    for (h, bytes) in &payloads {
        b.append(*h, bytes).expect("append");
    }
    b.finalize().expect("finalize");

    // Read it back through the uring path.
    tokio_uring::start(async {
        let reader = PackReader::open(&path).expect("open");
        for (h, expected) in &payloads {
            let bytes = read_at_uring(&reader, *h).await.expect("read_at_uring");
            assert_eq!(bytes, *expected, "uring read must match writer payload");
        }
    });
}
