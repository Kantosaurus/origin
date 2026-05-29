// SPDX-License-Identifier: Apache-2.0
//! Two opens of the same recorded bundle must produce byte-identical entry
//! payloads; the virtual clock replays samples in stable order.

use origin_replay::bundle::{Bundle, BundleWriter, Manifest};
use origin_replay::clock::{Clock, VirtualClock};
use origin_replay::recorder::{FileRecorder, Frame, Recorder};
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn two_opens_of_the_same_bundle_match_byte_for_byte() {
    let dir = tempdir().expect("tempdir");
    let bundle_path = dir.path().join("session.origin-replay");

    // 1) Record a synthetic session as a JSONL frame log.
    let log = dir.path().join("frames.jsonl");
    {
        let rec = FileRecorder::create(&log).expect("create rec");
        rec.record(Frame::IpcInbound {
            conn: 0,
            body: b"prompt".to_vec(),
        });
        rec.record(Frame::ProviderRequest {
            id: 0,
            body_blake3: [1u8; 32],
        });
        rec.record(Frame::ProviderResponseChunk {
            id: 0,
            seq: 0,
            body: b"hello ".to_vec(),
        });
        rec.record(Frame::ProviderResponseChunk {
            id: 0,
            seq: 1,
            body: b"world".to_vec(),
        });
        rec.record(Frame::ProviderResponseEnd { id: 0 });
        rec.record(Frame::IpcOutbound {
            conn: 0,
            body: b"hello world".to_vec(),
        });
        rec.close();
    }

    // 2) Pack frames into a Bundle.
    {
        let mut w = BundleWriter::create(
            &bundle_path,
            Manifest {
                version: 1,
                session_id: "det-1".into(),
                recorded_at_unix_ms: 0,
                origin_version: "0.0.1".into(),
            },
        )
        .expect("create writer");
        let body = std::fs::read(&log).expect("read log");
        w.write_entry("frames.jsonl", &body).expect("write entry");
        w.finish().expect("finish");
    }

    // 3) Open the bundle twice and compare.
    let b1 = Bundle::open(&bundle_path).expect("open 1");
    let b2 = Bundle::open(&bundle_path).expect("open 2");

    let e1 = b1.read_entry("frames.jsonl").expect("entry 1");
    let e2 = b2.read_entry("frames.jsonl").expect("entry 2");
    assert_eq!(e1, e2);
    assert_eq!(b1.manifest().session_id, b2.manifest().session_id);
}

#[test]
fn virtual_clock_is_deterministic_across_constructions() {
    let c = VirtualClock::from_samples(vec![100, 200, 300]);
    assert_eq!(
        [c.now_unix_ms(), c.now_unix_ms(), c.now_unix_ms()],
        [100, 200, 300]
    );
    let c = VirtualClock::from_samples(vec![100, 200, 300]);
    assert_eq!(
        [c.now_unix_ms(), c.now_unix_ms(), c.now_unix_ms()],
        [100, 200, 300]
    );
    // Silence unused-import lint by referencing the Arc-returning constructor:
    let _: Arc<VirtualClock> = VirtualClock::from_samples(vec![]);
}
