//! Tap point for CAS writes. The CAS layer calls `on_write(handle_hex, size)`
//! after a blob is durably stored so the recorder can fingerprint the run.

#![allow(clippy::module_name_repetitions)]

use crate::recorder::{Frame, Recorder};
use std::sync::Arc;

pub struct CasTap {
    recorder: Arc<dyn Recorder>,
}

impl CasTap {
    #[must_use]
    pub const fn new(recorder: Arc<dyn Recorder>) -> Self {
        Self { recorder }
    }

    pub fn on_write(&self, handle_hex: &str, size: u64) {
        self.recorder.record(Frame::CasWrite {
            handle_hex: handle_hex.to_string(),
            size,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::NullRecorder;
    use std::sync::Arc;

    #[test]
    fn on_write_does_not_panic_with_null_recorder() {
        let tap = CasTap::new(Arc::new(NullRecorder));
        tap.on_write("deadbeef", 1024);
    }
}
