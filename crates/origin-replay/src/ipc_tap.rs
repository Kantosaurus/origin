//! Tap point for IPC frames. The daemon/client wire reader+writer call
//! `inbound`/`outbound` after a frame is fully assembled, so the recorder
//! sees byte-identical wire payloads.

#![allow(clippy::module_name_repetitions)]

use crate::recorder::{Frame, Recorder};
use std::sync::Arc;

pub struct IpcTap {
    recorder: Arc<dyn Recorder>,
}

impl IpcTap {
    #[must_use]
    pub const fn new(recorder: Arc<dyn Recorder>) -> Self {
        Self { recorder }
    }

    pub fn inbound(&self, conn: u32, body: &[u8]) {
        self.recorder.record(Frame::IpcInbound {
            conn,
            body: body.to_vec(),
        });
    }

    pub fn outbound(&self, conn: u32, body: &[u8]) {
        self.recorder.record(Frame::IpcOutbound {
            conn,
            body: body.to_vec(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::FileRecorder;
    use tempfile::NamedTempFile;

    #[test]
    #[allow(clippy::similar_names)] // `tap` vs `tmp` is intentional in this test
    fn inbound_and_outbound_record_frames() {
        let tmp = NamedTempFile::new().expect("tempfile");
        let rec = FileRecorder::create(tmp.path()).expect("create");
        let tap = IpcTap::new(rec.clone());
        tap.inbound(0, b"hello");
        tap.outbound(0, b"world");
        rec.close();
        let body = std::fs::read_to_string(tmp.path()).expect("read");
        assert_eq!(body.lines().count(), 2);
    }
}
