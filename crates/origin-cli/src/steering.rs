// SPDX-License-Identifier: Apache-2.0
//! Mid-turn steering for the interactive CLI.
//!
//! While a turn is in flight, text the user types is queued as a steering hint
//! (via [`origin_steering::SteeringQueue`]) instead of being sent as a fresh
//! prompt. On the next turn the queued hints are drained into a single block and
//! merged ahead of the base prompt with [`origin_steering::merge_into_prompt`].
//!
//! This lands the queue plus a unit-tested merge helper. Capturing keystrokes
//! from the live TUI event loop into the queue is deferred to keep the wiring
//! additive and green.
// TODO(wire): push typed text into `SteeringQueue` from the interactive event
// loop while a turn is in flight, then call `next_turn_prompt` when assembling
// the next `ChatRequest`.

use origin_steering::{merge_into_prompt, SteeringQueue};

/// Assemble the next turn's prompt by draining any queued steering hints and
/// merging them ahead of `base_prompt`.
///
/// When the queue is empty the base prompt is returned unchanged (so the
/// default, no-steering path is byte-identical); otherwise the drained hints are
/// wrapped in steering markers and placed before the base prompt. The queue is
/// emptied as a side effect.
#[must_use]
pub fn next_turn_prompt(queue: &mut SteeringQueue, base_prompt: &str) -> String {
    let block = queue.drain_block();
    merge_into_prompt(base_prompt, block.as_deref())
}

#[cfg(test)]
mod tests {
    use super::next_turn_prompt;
    use origin_steering::{SteeringQueue, STEER_OPEN};

    #[test]
    fn no_hints_leaves_base_prompt_unchanged() {
        let mut q = SteeringQueue::new();
        let out = next_turn_prompt(&mut q, "implement the feature");
        assert_eq!(out, "implement the feature");
    }

    #[test]
    fn queued_hints_merge_ahead_of_base_and_drain() {
        let mut q = SteeringQueue::new();
        q.push("focus on tests");
        q.push("avoid touching siblings");
        let out = next_turn_prompt(&mut q, "implement the feature");
        assert!(out.starts_with(STEER_OPEN));
        assert!(out.contains("focus on tests"));
        assert!(out.contains("avoid touching siblings"));
        assert!(out.contains("implement the feature"));
        // The queue is drained after assembling the next turn.
        assert!(q.is_empty());
        // A subsequent turn with no new hints is unchanged again.
        assert_eq!(next_turn_prompt(&mut q, "next"), "next");
    }
}
