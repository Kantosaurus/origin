// SPDX-License-Identifier: Apache-2.0
//! `WireDecision` — per-block inline-vs-reference rule for handle substitution
//! in the message-to-wire encoder (N2.4 step 2).

use crate::Band;

/// Inline byte budget for non-Frozen, non-Sticky bands. Bodies larger than
/// this are emitted as `<result handle:… — N bytes>` references; the model
/// can inflate via `Recall` if it needs the body.
pub const INLINE_BYTE_BUDGET: usize = 2048;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDecision {
    /// Expand the CAS handle into wire bytes.
    Inline,
    /// Emit a short `<result handle:… — N bytes>` reference.
    Reference,
}

impl WireDecision {
    /// Decide for one tool-result block parked in `band` with `byte_len` body.
    #[must_use]
    pub const fn for_block(band: Band, byte_len: usize) -> Self {
        match band {
            // Frozen + Sticky: always inline. These sections hit cache; the
            // bytes are amortized across many turns.
            Band::Frozen | Band::Sticky => Self::Inline,
            // Sliding + Volatile: inline only if small enough.
            Band::Sliding | Band::Volatile => {
                if byte_len <= INLINE_BYTE_BUDGET {
                    Self::Inline
                } else {
                    Self::Reference
                }
            }
        }
    }
}
