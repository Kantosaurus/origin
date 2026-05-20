//! Wrap an `origin-provider` HTTP layer so every request and streamed chunk
//! is fed into a Recorder; in replay mode the same layer serves chunks from
//! a Bundle instead of the network.

#![allow(clippy::module_name_repetitions)]

use crate::bundle::Bundle;
use crate::recorder::{Frame, Recorder};
use parking_lot::Mutex;
use std::sync::Arc;

pub struct ProviderTap {
    recorder: Arc<dyn Recorder>,
    next_id: Mutex<u64>,
}

impl ProviderTap {
    #[must_use]
    pub fn new(recorder: Arc<dyn Recorder>) -> Self {
        Self {
            recorder,
            next_id: Mutex::new(0),
        }
    }

    pub fn start_request(&self, body: &[u8]) -> u64 {
        let id = {
            let mut g = self.next_id.lock();
            let v = *g;
            *g += 1;
            v
        };
        let body_blake3 = *blake3::hash(body).as_bytes();
        self.recorder.record(Frame::ProviderRequest { id, body_blake3 });
        id
    }

    pub fn chunk(&self, id: u64, seq: u32, body: Vec<u8>) {
        self.recorder
            .record(Frame::ProviderResponseChunk { id, seq, body });
    }

    pub fn end(&self, id: u64) {
        self.recorder.record(Frame::ProviderResponseEnd { id });
    }
}

pub struct ReplayProvider {
    bundle: Arc<Bundle>,
}

impl ReplayProvider {
    #[must_use]
    pub const fn new(bundle: Arc<Bundle>) -> Self {
        Self { bundle }
    }

    /// Replay all chunks for request `id` as a single concatenated body.
    #[must_use]
    #[allow(clippy::case_sensitive_file_extension_comparisons)] // synthetic bundle paths are always lowercase
    pub fn body_for(&self, id: u64) -> Vec<u8> {
        let prefix = format!("provider/{id:08}/");
        let mut chunks: Vec<(u32, Vec<u8>)> = self
            .bundle
            .entry_names()
            .filter(|n| n.starts_with(&prefix) && n.ends_with(".bin"))
            .filter_map(|n| {
                let seq_str = n.trim_start_matches(&prefix).trim_end_matches(".bin");
                seq_str.parse::<u32>().ok().map(|seq| (seq, n.to_string()))
            })
            .map(|(seq, n)| {
                let body = self.bundle.read_entry(&n).unwrap_or(&[]).to_vec();
                (seq, body)
            })
            .collect();
        chunks.sort_by_key(|(s, _)| *s);
        chunks.into_iter().flat_map(|(_, b)| b).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::FileRecorder;
    use tempfile::NamedTempFile;

    #[test]
    #[allow(clippy::similar_names)] // `tap` vs `tmp` is intentional in this test
    fn start_request_increments_id() {
        let tmp = NamedTempFile::new().expect("tempfile");
        let rec = FileRecorder::create(tmp.path()).expect("create");
        let tap = ProviderTap::new(rec);
        assert_eq!(tap.start_request(b"a"), 0);
        assert_eq!(tap.start_request(b"b"), 1);
        assert_eq!(tap.start_request(b"c"), 2);
    }
}
