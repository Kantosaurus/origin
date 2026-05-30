// SPDX-License-Identifier: Apache-2.0
//! Mid-execution steering for the origin agent.
//!
//! Typed text becomes a hint queued and injected into the next turn
//! without stopping the running agent. Pure queue plus merge, no I/O.
#![forbid(unsafe_code)]

use std::collections::VecDeque;

/// Opening delimiter that marks the start of an injected steering block.
pub const STEER_OPEN: &str = "<steering>";

/// Closing delimiter that marks the end of an injected steering block.
pub const STEER_CLOSE: &str = "</steering>";

/// Errors that can occur while working with steering hints.
///
/// Reserved for future fallible operations; the queue itself is infallible.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SteeringError {
    /// A hint was rejected because it contained no usable content.
    #[error("steering hint was empty")]
    EmptyHint,
}

/// A first-in, first-out queue of steering hints awaiting injection.
///
/// Hints accumulate while a turn is in flight and are merged into a
/// single steering block when the next turn is assembled.
#[derive(Debug, Default, Clone)]
pub struct SteeringQueue {
    hints: VecDeque<String>,
}

impl SteeringQueue {
    /// Creates an empty steering queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a steering hint for injection into the next turn.
    pub fn push(&mut self, hint: impl Into<String>) {
        self.hints.push_back(hint.into());
    }

    /// Returns `true` when no hints are queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }

    /// Returns the number of queued hints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hints.len()
    }

    /// Drains every queued hint into one steering block and clears the queue.
    ///
    /// Hints are joined in insertion order, one per line. Returns `None`
    /// when the queue is empty, leaving it untouched.
    pub fn drain_block(&mut self) -> Option<String> {
        if self.hints.is_empty() {
            return None;
        }
        let block = self
            .hints
            .drain(..)
            .collect::<Vec<String>>()
            .join("\n");
        Some(block)
    }
}

/// Prepends a steering block ahead of the base user text when present.
///
/// When `steering_block` is `Some`, the block is wrapped in the
/// [`STEER_OPEN`] and [`STEER_CLOSE`] markers and placed before
/// `base_user_text`, separated by a blank line. When `None`, the base
/// text is returned unchanged.
#[must_use]
pub fn merge_into_prompt(base_user_text: &str, steering_block: Option<&str>) -> String {
    steering_block.map_or_else(
        || base_user_text.to_string(),
        |block| format!("{STEER_OPEN}\n{block}\n{STEER_CLOSE}\n\n{base_user_text}"),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn push_then_drain_block_joins_both_hints_and_empties() {
        let mut q = SteeringQueue::new();
        q.push("focus on tests");
        q.push("avoid touching siblings");
        let block = q.drain_block().unwrap();
        assert!(block.contains("focus on tests"));
        assert!(block.contains("avoid touching siblings"));
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn drain_block_none_when_empty() {
        let mut q = SteeringQueue::new();
        assert!(q.drain_block().is_none());
    }

    #[test]
    fn drain_block_returns_none_again_after_draining() {
        let mut q = SteeringQueue::new();
        q.push("one");
        assert!(q.drain_block().is_some());
        assert!(q.drain_block().is_none());
    }

    #[test]
    fn merge_with_none_equals_base() {
        let base = "implement the feature";
        assert_eq!(merge_into_prompt(base, None), base);
    }

    #[test]
    fn merge_with_some_wraps_in_markers_before_base() {
        let merged = merge_into_prompt("base text", Some("hint"));
        assert!(merged.contains(STEER_OPEN));
        assert!(merged.contains(STEER_CLOSE));
        assert!(merged.contains("hint"));
        let open_pos = merged.find(STEER_OPEN).unwrap();
        let base_pos = merged.find("base text").unwrap();
        assert!(open_pos < base_pos, "steering must precede base text");
    }

    #[test]
    fn len_tracks_pushes() {
        let mut q = SteeringQueue::new();
        assert_eq!(q.len(), 0);
        q.push("a");
        assert_eq!(q.len(), 1);
        q.push("b");
        assert_eq!(q.len(), 2);
        assert!(!q.is_empty());
    }

    #[test]
    fn drain_block_preserves_insertion_order() {
        let mut q = SteeringQueue::new();
        q.push("first");
        q.push("second");
        let block = q.drain_block().unwrap();
        let first_pos = block.find("first").unwrap();
        let second_pos = block.find("second").unwrap();
        assert!(first_pos < second_pos);
    }

    #[test]
    fn merge_then_drain_roundtrip() {
        let mut q = SteeringQueue::new();
        q.push("steer one");
        q.push("steer two");
        let block = q.drain_block();
        let merged = merge_into_prompt("original prompt", block.as_deref());
        assert!(merged.contains("steer one"));
        assert!(merged.contains("steer two"));
        assert!(merged.contains("original prompt"));
        assert!(merged.starts_with(STEER_OPEN));
    }
}
