// SPDX-License-Identifier: Apache-2.0
//! Degenerate-loop detector.
//!
//! Catches the model repeating a failing or no-progress action — a failing
//! `Edit` re-issued verbatim, a `Bash` command rerun to the same error, two
//! actions ping-ponging forever. Nothing else bounds this: `max_turns` is the
//! `u32::MAX` sentinel, and the memoization cache deny-lists Bash/Edit/Write
//! (the exact flail tools), so it can't catch them.
//!
//! Two tiers, both deterministic and O(1)/record over a small ring — no model
//! call: a Tier-1 `<origin-stuck>` nudge fed back via the volatile context, then
//! a Tier-2 hard halt once the loop is confirmed degenerate.

use std::collections::hash_map::DefaultHasher;
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

const RING: usize = 20;
const NUDGE_N: usize = 3;
const HALT_N: usize = 5;

/// A semantic fingerprint of one tool outcome: which tool, its
/// volatility-stripped args, the observation it produced, and whether it errored.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    tool: u64,
    args: u64,
    obs: u64,
    is_error: bool,
}

/// Ring of the last [`RING`] tool outcomes this turn-loop saw.
#[derive(Default)]
pub struct StuckDetector {
    ring: VecDeque<Fingerprint>,
}

/// The verdict from [`StuckDetector::assess`].
pub enum StuckLevel {
    /// Making progress (or too little history) — do nothing.
    Ok,
    /// Tier-1: append this `<origin-stuck>` nudge to the next turn's context.
    Nudge(String),
    /// Tier-2: the loop is confirmed degenerate — terminate the turn.
    Halt(String),
}

impl StuckDetector {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one tool outcome.
    pub fn record(&mut self, tool: &str, args: &str, is_error: bool, observation: &[u8]) {
        let fp = Fingerprint {
            tool: hash_one(tool),
            args: hash_one(normalize_args(args).as_str()),
            obs: hash_one(observation),
            is_error,
        };
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back(fp);
    }

    /// Classify the current tail of the ring.
    #[must_use]
    pub fn assess(&self) -> StuckLevel {
        let Some(last) = self.ring.back().copied() else {
            return StuckLevel::Ok;
        };
        // Consecutive identical (tool+args+obs) actions, or consecutive
        // same-action errors — whichever run is longer.
        let same = self
            .ring
            .iter()
            .rev()
            .take_while(|f| f.tool == last.tool && f.args == last.args && f.obs == last.obs)
            .count();
        let errs = self
            .ring
            .iter()
            .rev()
            .take_while(|f| f.tool == last.tool && f.args == last.args && f.is_error)
            .count();
        let run = same.max(errs);
        if run >= HALT_N {
            return StuckLevel::Halt(format!(
                "the same action repeated {run}× with no change in result — halting to avoid an infinite loop"
            ));
        }
        if run >= NUDGE_N {
            return StuckLevel::Nudge(STUCK_NUDGE.to_string());
        }
        // A-B-A-B ping-pong: the last four alternate between two distinct actions.
        if self.ring.len() >= 4 {
            let v: Vec<&Fingerprint> = self.ring.iter().rev().take(4).collect();
            let act = |f: &Fingerprint| (f.tool, f.args);
            if act(v[0]) == act(v[2]) && act(v[1]) == act(v[3]) && act(v[0]) != act(v[1]) {
                return StuckLevel::Nudge(STUCK_NUDGE.to_string());
            }
        }
        StuckLevel::Ok
    }
}

const STUCK_NUDGE: &str = "<origin-stuck>\nYou appear to be repeating the same action(s) without making progress. \
Stop and change your approach: re-read the relevant file, question your assumption about why the last attempt \
failed, or try a different tool. Do NOT repeat the previous action verbatim.\n</origin-stuck>";

fn hash_one<T: Hash + ?Sized>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// Strip volatile detail — digits (line numbers / timestamps / ids) and
/// surrounding whitespace — so "the same action" is recognised across cosmetic
/// variation the model might introduce between otherwise-identical retries.
fn normalize_args(args: &str) -> String {
    args.chars()
        .filter(|c| !c.is_ascii_digit())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{StuckDetector, StuckLevel};

    #[test]
    fn repeated_failing_action_nudges_then_halts() {
        let mut d = StuckDetector::new();
        d.record("Edit", "{file: a.rs}", true, b"no_match");
        d.record("Edit", "{file: a.rs}", true, b"no_match");
        assert!(matches!(d.assess(), StuckLevel::Ok), "2 repeats is not yet stuck");
        d.record("Edit", "{file: a.rs}", true, b"no_match");
        assert!(matches!(d.assess(), StuckLevel::Nudge(_)), "3 repeats ⇒ nudge");
        d.record("Edit", "{file: a.rs}", true, b"no_match");
        d.record("Edit", "{file: a.rs}", true, b"no_match");
        assert!(matches!(d.assess(), StuckLevel::Halt(_)), "5 repeats ⇒ halt");
    }

    #[test]
    fn distinct_progress_is_not_stuck() {
        let mut d = StuckDetector::new();
        d.record("Read", "a.rs", false, b"...");
        d.record("Edit", "a.rs", false, b"ok-1");
        d.record("Edit", "b.rs", false, b"ok-2");
        assert!(matches!(d.assess(), StuckLevel::Ok));
    }

    #[test]
    fn detects_ping_pong_between_two_actions() {
        let mut d = StuckDetector::new();
        d.record("Edit", "A", false, b"1");
        d.record("Edit", "B", false, b"2");
        d.record("Edit", "A", false, b"3");
        d.record("Edit", "B", false, b"4");
        assert!(matches!(d.assess(), StuckLevel::Nudge(_)));
    }

    #[test]
    fn digit_only_variation_still_counts_as_same_action() {
        // The model retries the same edit but the error cites a different line
        // number each time — normalization must still see it as one action.
        let mut d = StuckDetector::new();
        d.record("Edit", "line 10 of a.rs", true, b"x");
        d.record("Edit", "line 11 of a.rs", true, b"x");
        d.record("Edit", "line 12 of a.rs", true, b"x");
        assert!(matches!(d.assess(), StuckLevel::Nudge(_)));
    }
}
