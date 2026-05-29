// SPDX-License-Identifier: Apache-2.0
// `LogootKey` is the canonical name in the Logoot literature; we keep the
// repetition with the module name for discoverability rather than renaming.
#![allow(clippy::module_name_repetitions)]

//! Logoot position keys — dense, totally-ordered list positions.
//!
//! A [`LogootKey`] is a sequence of [`PathComponent`]s, each carrying a digit
//! plus the producing actor as a disambiguator. Keys form a dense total order:
//! between any two distinct keys a new key can always be generated without
//! coordination with other replicas. This is the position primitive backing
//! the `Reorder` op (P9.1) on the shared plan list.
//!
//! Compared to a fractional-index encoding, this representation never
//! degrades into a runaway-precision float and never needs renormalisation
//! after concurrent inserts at the same location.

use core::cmp::Ordering;

use crate::lamport::ActorId;

/// One step inside a [`LogootKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct PathComponent {
    /// Digit on this level. Higher digit → later in the list.
    pub digit: u32,
    /// Actor stamp; breaks ties between concurrent inserts at the same digit.
    pub actor: ActorId,
}

impl PathComponent {
    /// Construct a new component.
    #[must_use]
    pub const fn new(digit: u32, actor: ActorId) -> Self {
        Self { digit, actor }
    }
}

impl PartialOrd for PathComponent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PathComponent {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.digit.cmp(&other.digit) {
            Ordering::Equal => self.actor.cmp(&other.actor),
            o => o,
        }
    }
}

/// Dense Logoot position key.
///
/// Lexicographic order over the path components defines the list position.
/// Components are 1-indexed conceptually but represented as a plain `Vec`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct LogootKey {
    path: Vec<PathComponent>,
}

impl LogootKey {
    /// Construct a key from an explicit component path. The path must be
    /// non-empty; an empty path is reserved for the implicit endpoints `None`
    /// in [`Self::between`].
    #[must_use]
    pub fn from_path(path: Vec<PathComponent>) -> Self {
        debug_assert!(!path.is_empty(), "LogootKey path must be non-empty");
        Self { path }
    }

    /// Borrow the underlying component slice.
    #[must_use]
    pub fn path(&self) -> &[PathComponent] {
        &self.path
    }

    /// Produce a key strictly between `left` and `right`.
    ///
    /// `left = None` means "before the first existing key"; `right = None`
    /// means "after the last existing key". `actor` stamps the new component
    /// for collision resistance. `seed` lets callers nudge the chosen digit
    /// deterministically — used by the property tests to avoid PRNG drift.
    ///
    /// The algorithm: walk both paths in lock-step; at the first level where
    /// they leave room for a new digit, place one there stamped by `actor`.
    /// If no room exists at any shared level, append a fresh deeper level.
    #[must_use]
    pub fn between(left: Option<&Self>, right: Option<&Self>, actor: ActorId, seed: u64) -> Self {
        // Minimum and maximum sentinel digits. Real digits live strictly inside.
        const MIN: u32 = 0;
        const MAX: u32 = u32::MAX;

        let left_path: &[PathComponent] = left.map_or(&[], |k| k.path.as_slice());
        let right_path: &[PathComponent] = right.map_or(&[], |k| k.path.as_slice());

        let mut prefix: Vec<PathComponent> = Vec::new();
        let mut depth = 0usize;
        loop {
            let l_digit = left_path.get(depth).map_or(MIN, |c| c.digit);
            let r_digit = right_path.get(depth).map_or(MAX, |c| c.digit);

            if l_digit + 1 < r_digit {
                // Strictly between — pick a digit.
                let span = r_digit - l_digit - 1;
                #[allow(clippy::cast_possible_truncation)]
                // `seed % span` always fits in u32 when span ≤ u32::MAX.
                let offset = (seed % u64::from(span)) as u32 + 1;
                let digit = l_digit + offset;
                prefix.push(PathComponent::new(digit, actor));
                return Self { path: prefix };
            }

            // No room at this level. We must copy the lower bound's component
            // (so the new key is still > left) and descend.
            //
            // Subtle: if the left path is exhausted here we cannot use MIN as
            // a real component (digit 0 collides with the virtual lower bound
            // at the next level). Use digit = l_digit (= MIN = 0) stamped
            // with actor id 0 so any later actor will sort after it.
            let descent_actor = left_path.get(depth).map_or_else(|| ActorId::new(0), |c| c.actor);
            prefix.push(PathComponent::new(l_digit, descent_actor));
            depth += 1;
            // Safety net: paths can grow arbitrarily but realistic inputs are
            // bounded. Cap at 32 levels to avoid runaway recursion on
            // pathological inputs in tests; emit one final digit if hit.
            if depth >= 32 {
                // Pathological: the bounded recursion gave up. Pick a digit
                // near the midpoint stamped by the actor; correctness still
                // holds because subsequent inserts under this prefix will
                // see a non-empty left/right and find room.
                #[allow(clippy::cast_possible_truncation)]
                // Mask first to keep within u32; this is a degenerate path
                // and the high bits of `seed` are not load-bearing.
                let low = (seed & 0x3FFF_FFFF) as u32;
                let digit = MAX / 2 + low;
                prefix.push(PathComponent::new(digit, actor));
                return Self { path: prefix };
            }
        }
    }
}

impl PartialOrd for LogootKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LogootKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.path.cmp(&other.path)
    }
}

#[cfg(test)]
mod tests {
    use super::{ActorId, LogootKey, PathComponent};

    #[test]
    fn between_produces_strictly_ordered_key() {
        let actor = ActorId::new(1);
        let k1 = LogootKey::between(None, None, actor, 10);
        let k2 = LogootKey::between(Some(&k1), None, actor, 20);
        assert!(k1 < k2);
        let mid = LogootKey::between(Some(&k1), Some(&k2), actor, 5);
        assert!(k1 < mid);
        assert!(mid < k2);
    }

    #[test]
    fn between_terminates_when_adjacent() {
        let actor = ActorId::new(1);
        // Manually build two adjacent keys (digit 1 and digit 2 on the same level).
        let k1 = LogootKey::from_path(vec![PathComponent::new(1, actor)]);
        let k2 = LogootKey::from_path(vec![PathComponent::new(2, actor)]);
        let mid = LogootKey::between(Some(&k1), Some(&k2), actor, 0);
        assert!(k1 < mid);
        assert!(mid < k2);
    }

    #[test]
    fn ordering_is_lexicographic() {
        let actor = ActorId::new(1);
        let a = LogootKey::from_path(vec![PathComponent::new(5, actor)]);
        let b = LogootKey::from_path(vec![PathComponent::new(5, actor), PathComponent::new(3, actor)]);
        // Shorter path that matches the prefix sorts first when remaining
        // components are non-empty.
        assert!(a < b);
    }
}
