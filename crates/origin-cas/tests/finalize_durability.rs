//! Regression test for the `PackBuilder::finalize` durability fix.
//!
//! The fix adds a `sync_all()` after the buffered-writer flush in
//! `finalize()` so that the kernel pushes the index + footer out to disk
//! before we tell the caller the pack is durable. We cannot reasonably
//! simulate a host crash inside a unit test, so this test instead asserts
//! the *observable* contract that the fix must preserve:
//!
//!   * `finalize()` returns `Ok(path)`.
//!   * The returned path opens cleanly with `PackReader::open` — i.e. the
//!     header, payload region, index, and footer all round-trip.
//!   * Every appended payload reads back byte-for-byte.
//!
//! `sync_all()` is a no-op semantically (it does not change file bytes),
//! so a passing test here both before and after the fix is expected. The
//! purpose is to make sure the new fsync call did not regress the happy
//! path (e.g. by being placed before the buffered flush, or by swallowing
//! an error).

use origin_cas::{Hash, PackBuilder, PackReader};
use tempfile::tempdir;

#[test]
fn finalize_produces_a_readable_pack() {
    let dir = tempdir().expect("tempdir");
    let pack = dir.path().join("durable.pack");

    let payloads: [&[u8]; 3] = [
        b"alpha-payload",
        b"beta-payload-with-longer-bytes",
        &[0xABu8; 1024],
    ];
    let hashes: Vec<Hash> = payloads.iter().map(|b| Hash::of(b)).collect();

    let mut builder = PackBuilder::create(&pack).expect("create");
    for (h, p) in hashes.iter().zip(payloads.iter()) {
        builder.append(*h, p).expect("append");
    }
    let written = builder.finalize().expect("finalize must succeed post-fsync");
    assert_eq!(written, pack);

    // Open from a fresh handle: this exercises the on-disk bytes the
    // kernel was just asked to durably persist. If `sync_all` were placed
    // before the buffered flush, the footer would still be in the
    // `BufWriter` and this open would observe a truncated file.
    let reader = PackReader::open(&pack).expect("reader open");
    for (h, p) in hashes.iter().zip(payloads.iter()) {
        let slice = reader.read(*h).expect("hash present in index");
        assert_eq!(slice.as_ref(), *p, "payload round-trip mismatch");
    }
}
