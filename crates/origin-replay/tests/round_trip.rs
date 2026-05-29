// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]
#![allow(clippy::redundant_clone)]

use origin_replay::bundle::{Bundle, BundleWriter, Manifest};
use tempfile::tempdir;

#[test]
fn writer_then_reader_round_trip_three_entries() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("test.origin-replay");

    let manifest = Manifest {
        version: 1,
        session_id: "s-1".into(),
        recorded_at_unix_ms: 1_700_000_000_000,
        origin_version: "0.0.1".into(),
    };

    {
        let mut w = BundleWriter::create(&path, manifest.clone()).expect("create");
        w.write_entry("provider/000.bin", b"alpha").unwrap();
        w.write_entry("ipc/000.bin", b"beta").unwrap();
        w.write_entry("clock.csv", b"0,1700000000000\n").unwrap();
        w.finish().unwrap();
    }

    let b = Bundle::open(&path).expect("open");
    assert_eq!(b.manifest().session_id, "s-1");
    assert_eq!(b.read_entry("provider/000.bin").unwrap(), b"alpha");
    assert_eq!(b.read_entry("ipc/000.bin").unwrap(), b"beta");
    assert_eq!(b.read_entry("clock.csv").unwrap(), b"0,1700000000000\n");
}

#[test]
fn corrupt_bundle_is_rejected() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("bad.origin-replay");
    std::fs::write(&path, b"not-a-bundle").unwrap();
    assert!(Bundle::open(&path).is_err());
}
