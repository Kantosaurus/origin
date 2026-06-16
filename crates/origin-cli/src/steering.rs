// SPDX-License-Identifier: Apache-2.0
//! Mid-turn steering for the interactive CLI.
//!
//! Steering hints are queued via [`origin_steering::SteeringQueue`] instead of
//! being sent as fresh prompts. On the next turn the queued hints are drained
//! into a single block and appended AFTER the base prompt with
//! [`origin_steering::merge_into_prompt`], so the cached prefix (system + prior
//! turns + base user text) stays byte-identical for the provider's prefix cache
//! (gap 8: KV-cache-safe interleaving).
//!
//! This is wired end-to-end: the `/steer <text>` composer command (handled in
//! `main.rs`'s `handle_submit`) pushes a hint onto `App.steering`, and
//! [`next_turn_prompt`] is called from `handle_prompt_turn` when assembling the
//! next turn's prompt, draining the queue into the trailing steering block.
//!
//! Deferred (the only remaining sub-feature): auto-capturing free-typed text
//! *mid-turn* into the steering queue from the live TUI event loop. Today such
//! text is enqueued as a follow-up message (`App.input.queue_message`) rather
//! than as a steering hint; only the explicit `/steer` form reaches this queue.

use origin_steering::{merge_into_prompt, SteeringQueue};

/// Assemble the next turn's prompt by draining any queued steering hints and
/// appending them as a trailing block after `base_prompt`.
///
/// When the queue is empty the base prompt is returned unchanged (so the
/// default, no-steering path is byte-identical); otherwise the drained hints are
/// wrapped in steering markers and placed AFTER the base prompt, keeping the
/// stable prefix intact for prefix caching. The queue is emptied as a side effect.
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
    fn queued_hints_append_after_base_and_drain() {
        let mut q = SteeringQueue::new();
        q.push("focus on tests");
        q.push("avoid touching siblings");
        let out = next_turn_prompt(&mut q, "implement the feature");
        // Cache-safe: the base prompt stays a byte-identical prefix; the steering
        // block is a trailing suffix, not a prepend.
        assert!(out.starts_with("implement the feature"));
        assert!(!out.starts_with(STEER_OPEN));
        let base_pos = out.find("implement the feature").expect("base present");
        let steer_pos = out.find(STEER_OPEN).expect("steering present");
        assert!(base_pos < steer_pos, "base text must precede the steering block");
        assert!(out.contains("focus on tests"));
        assert!(out.contains("avoid touching siblings"));
        // The queue is drained after assembling the next turn.
        assert!(q.is_empty());
        // A subsequent turn with no new hints is unchanged again.
        assert_eq!(next_turn_prompt(&mut q, "next"), "next");
    }
}
