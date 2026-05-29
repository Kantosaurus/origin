// SPDX-License-Identifier: Apache-2.0
//! Virtual clock — replay mode reads timestamps from a recorded stream so
//! `now()` is byte-deterministic.

#![allow(clippy::module_name_repetitions)]

use parking_lot::Mutex;
use std::sync::Arc;

pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    #[allow(clippy::cast_possible_truncation)]
    fn now_unix_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

pub struct VirtualClock {
    samples: Mutex<std::vec::IntoIter<u64>>,
}

impl VirtualClock {
    #[must_use]
    pub fn from_samples(samples: Vec<u64>) -> Arc<Self> {
        Arc::new(Self {
            samples: Mutex::new(samples.into_iter()),
        })
    }
}

impl Clock for VirtualClock {
    fn now_unix_ms(&self) -> u64 {
        self.samples.lock().next().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_clock_replays_samples_in_order() {
        let c = VirtualClock::from_samples(vec![1, 2, 3]);
        assert_eq!(c.now_unix_ms(), 1);
        assert_eq!(c.now_unix_ms(), 2);
        assert_eq!(c.now_unix_ms(), 3);
        assert_eq!(c.now_unix_ms(), 0); // exhausted → 0
    }
}
