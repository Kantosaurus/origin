//! Recorder trait: every non-deterministic boundary writes one frame per event.

#![allow(clippy::module_name_repetitions)]

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Frame {
    ProviderRequest { id: u64, body_blake3: [u8; 32] },
    ProviderResponseChunk { id: u64, seq: u32, body: Vec<u8> },
    ProviderResponseEnd { id: u64 },
    IpcInbound { conn: u32, body: Vec<u8> },
    IpcOutbound { conn: u32, body: Vec<u8> },
    CasWrite { handle_hex: String, size: u64 },
    Clock { seq: u64, unix_ms: u64 },
    Rng { seq: u64, bytes: Vec<u8> },
}

pub trait Recorder: Send + Sync {
    fn record(&self, frame: Frame);
    fn close(&self);
}

#[derive(Default)]
pub struct NullRecorder;

impl Recorder for NullRecorder {
    fn record(&self, _frame: Frame) {}
    fn close(&self) {}
}

pub struct FileRecorder {
    inner: Mutex<BufWriter<File>>,
}

impl FileRecorder {
    /// Open `path` for append-write.
    ///
    /// # Errors
    /// Returns the underlying [`std::io::Error`] when the file cannot be created.
    pub fn create(path: &Path) -> std::io::Result<Arc<Self>> {
        let f = File::create(path)?;
        Ok(Arc::new(Self {
            inner: Mutex::new(BufWriter::new(f)),
        }))
    }
}

impl Recorder for FileRecorder {
    fn record(&self, frame: Frame) {
        let mut g = self.inner.lock();
        let line = serde_json::to_string(&frame).unwrap_or_default();
        let _ = g.write_all(line.as_bytes());
        let _ = g.write_all(b"\n");
    }
    fn close(&self) {
        let mut g = self.inner.lock();
        let _ = g.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trips_serde() {
        let f = Frame::Clock { seq: 1, unix_ms: 42 };
        let s = serde_json::to_string(&f).expect("ser");
        let back: Frame = serde_json::from_str(&s).expect("de");
        assert_eq!(f, back);
    }

    #[test]
    fn file_recorder_writes_two_lines() {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let rec = FileRecorder::create(tmp.path()).expect("create");
        rec.record(Frame::Clock { seq: 0, unix_ms: 1 });
        rec.record(Frame::Clock { seq: 1, unix_ms: 2 });
        rec.close();
        let body = std::fs::read_to_string(tmp.path()).expect("read");
        assert_eq!(body.lines().count(), 2);
    }
}
