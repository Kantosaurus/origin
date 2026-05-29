// SPDX-License-Identifier: Apache-2.0
//! Seeded RNG hooked through the recorder.

#![allow(clippy::module_name_repetitions)]

use parking_lot::Mutex;
use std::sync::Arc;

pub trait Rng: Send + Sync {
    fn fill(&self, out: &mut [u8]);
}

pub struct SeededRng {
    state: Mutex<u64>,
}

impl SeededRng {
    #[must_use]
    pub fn new(seed: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(seed),
        })
    }
}

impl Rng for SeededRng {
    #[allow(clippy::cast_possible_truncation, clippy::significant_drop_tightening)]
    fn fill(&self, out: &mut [u8]) {
        let mut s = self.state.lock();
        for b in out.iter_mut() {
            // SplitMix64 — deterministic, fast.
            *s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            *b = z as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_rng_is_deterministic() {
        let a = SeededRng::new(42);
        let b = SeededRng::new(42);
        let mut ba = [0u8; 16];
        let mut bb = [0u8; 16];
        a.fill(&mut ba);
        b.fill(&mut bb);
        assert_eq!(ba, bb);
    }

    #[test]
    fn different_seeds_diverge() {
        let a = SeededRng::new(1);
        let b = SeededRng::new(2);
        let mut ba = [0u8; 16];
        let mut bb = [0u8; 16];
        a.fill(&mut ba);
        b.fill(&mut bb);
        assert_ne!(ba, bb);
    }
}
