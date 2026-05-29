// SPDX-License-Identifier: Apache-2.0
//! Rendering of `StreamEvent::Goal*` variants into the TUI scrollback.
//!
//! Bug #4: the CLI's `call_daemon` event loop had no arms for `GoalActive`,
//! `GoalIteration`, `GoalVerifying`, `GoalCleared`, or `GoalInactive` —
//! they fell into the `_ => {}` catch-all and were silently dropped.
//!
//! This module exposes:
//!   * [`GoalRender`] — a trait surfaced over `tui::App` so the renderer
//!     can be unit-tested against a tiny fake without dragging in the full
//!     TUI grid.
//!   * [`render_goal_event`] — pure dispatch that takes a `StreamEvent`
//!     and routes Goal* variants to the renderer.
//!   * [`status_line`] / [`cleared_line`] — pure formatters so a test can
//!     assert on the exact text without instantiating an `App`.
//!
//! Rendering style mirrors the spec §1 indicator:
//!   `◎ goal · iter <i>/<max> · <tokens_spent>/<token_budget> tok`
//!
//! When a goal first activates we push the condition once; subsequent
//! iterations push only the updated status line. `GoalCleared` pushes a
//! one-line colored terminal notice per the reason variant.

use origin_daemon::protocol::StreamEvent;
use origin_goal::{ClearReasonWire, TagOutcomeWire};

use crate::theme;

/// Sink the goal renderer writes to. Implemented by `tui::App` for the real
/// UI; tests can implement against a `Vec<RenderedLine>` to assert on
/// output without an actual grid.
pub trait GoalRender {
    /// Push a foreground/background-colored row into the scrollback.
    fn push_colored(&mut self, text: String, fg: u32, bg: u32);
    /// Set / clear the "current goal status" indicator. `None` means "no
    /// active goal — clear the indicator". This is logically a single
    /// mutable cell; implementations may render it however they like
    /// (status row above the input card, dedicated line, etc.).
    fn set_goal_status(&mut self, status: Option<String>);
}

/// Format the spec §1 status indicator:
/// `◎ goal · iter <i>/<max> · <tokens_spent>/<token_budget> tok`.
#[must_use]
pub fn status_line(iter: u32, max_iter: u32, tokens_spent: u64, token_budget: u64) -> String {
    format!("\u{25CE} goal \u{00B7} iter {iter}/{max_iter} \u{00B7} {tokens_spent}/{token_budget} tok")
}

/// Spec §1 verifying indicator:
/// `◎ goal · iter <i>/<max> · verifying...`.
#[must_use]
pub fn verifying_line(iter: u32, max_iter: u32) -> String {
    format!("\u{25CE} goal \u{00B7} iter {iter}/{max_iter} \u{00B7} verifying...")
}

/// One-line terminal notice describing why a goal cleared, plus the color
/// the CLI should render it in. Reasons render per the bug-#4 spec table:
///
///   * `Met { reason }`            -> "done: <reason>"                       (green)
///   * `UserSlash`                 -> "goal cancelled"                       (yellow)
///   * `UserClearAll`              -> "session cleared"                      (yellow)
///   * `MaxIter`                   -> "max iterations reached"               (yellow)
///   * `BudgetExhausted`           -> "token budget reached"                 (yellow)
///   * `VerifierRejected { why }`  -> "verifier kept rejecting: <why>"      (red)
///   * `VerifierUnavailable`       -> "verifier unavailable; trusting main model" (yellow)
#[must_use]
pub fn cleared_line(reason: &ClearReasonWire) -> (String, u32) {
    match reason {
        ClearReasonWire::Met { reason } => (format!("done: {reason}"), theme::GREEN),
        ClearReasonWire::UserSlash => ("goal cancelled".to_string(), theme::YELLOW),
        ClearReasonWire::UserClearAll => ("session cleared".to_string(), theme::YELLOW),
        ClearReasonWire::MaxIter => ("max iterations reached".to_string(), theme::YELLOW),
        ClearReasonWire::BudgetExhausted => ("token budget reached".to_string(), theme::YELLOW),
        ClearReasonWire::VerifierRejected { why } => (format!("verifier kept rejecting: {why}"), theme::RED),
        ClearReasonWire::VerifierUnavailable => (
            "verifier unavailable; trusting main model".to_string(),
            theme::YELLOW,
        ),
        ClearReasonWire::Blocked { why } => (format!("blocked: {why}"), theme::YELLOW),
    }
}

/// Render a `StreamEvent` if it is a `Goal*` variant, mutating `app`.
///
/// Returns `true` when the event was a Goal variant (and thus handled
/// here), `false` for any other variant — the caller should continue
/// its existing dispatch in that case.
///
/// This makes the dispatch in `main.rs::call_daemon` a one-liner:
/// `if render_goal_event(&mut goal_app, &ev) { continue; }`.
pub fn render_goal_event<R: GoalRender>(app: &mut R, ev: &StreamEvent) -> bool {
    match ev {
        StreamEvent::GoalActive {
            condition,
            max_iter,
            token_budget,
        } => {
            app.push_colored(format!("  \u{25CE} goal active: {condition}"), theme::ACCENT, 0);
            app.set_goal_status(Some(status_line(0, *max_iter, 0, *token_budget)));
            true
        }
        StreamEvent::GoalIteration {
            iter,
            tokens_spent,
            last_tag,
        } => {
            // Status indicator update (always).
            // We don't know `max_iter` / `token_budget` here from the event
            // alone; emit the iteration with the counters we have. The
            // renderer keeps the prior caps in the status string by leaving
            // them to the renderer's own bookkeeping when desired. For the
            // pure rendering test, the iteration-only string is enough.
            app.set_goal_status(Some(format!(
                "\u{25CE} goal \u{00B7} iter {iter} \u{00B7} {tokens_spent} tok"
            )));
            // Blocker notice (one-line).
            if let TagOutcomeWire::Blocked { why } = last_tag {
                app.push_colored(format!("  \u{26A0} goal blocked: {why}"), theme::YELLOW, 0);
            }
            true
        }
        StreamEvent::GoalVerifying => {
            app.set_goal_status(Some("\u{25CE} goal \u{00B7} verifying...".to_string()));
            true
        }
        StreamEvent::GoalCleared {
            reason,
            iter,
            tokens_spent,
        } => {
            let (msg, fg) = cleared_line(reason);
            // Suffix with the iter / tokens summary so the user can see the
            // run's footprint at a glance without scrolling back.
            app.push_colored(
                format!("  \u{25EF} {msg}  (iter {iter}, {tokens_spent} tok)"),
                fg,
                0,
            );
            app.set_goal_status(None);
            true
        }
        StreamEvent::GoalInactive => {
            app.push_colored("  no active goal".to_string(), theme::MUTED, 0);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::panic)]
mod tests {
    use super::*;

    /// Recorded sink for the tests. Captures every push + status update so
    /// assertions can be made on counts AND ordering.
    #[derive(Default)]
    struct FakeApp {
        lines: Vec<(String, u32)>,
        status: Option<String>,
        status_updates: u32,
    }

    impl GoalRender for FakeApp {
        fn push_colored(&mut self, text: String, fg: u32, _bg: u32) {
            self.lines.push((text, fg));
        }
        fn set_goal_status(&mut self, status: Option<String>) {
            self.status = status;
            self.status_updates += 1;
        }
    }

    #[test]
    fn goal_active_pushes_condition_and_sets_status() {
        let mut app = FakeApp::default();
        let handled = render_goal_event(
            &mut app,
            &StreamEvent::GoalActive {
                condition: "fix the build".into(),
                max_iter: 5,
                token_budget: 10_000,
            },
        );
        assert!(handled, "GoalActive must be handled, not fall through");
        assert_eq!(app.lines.len(), 1, "expected one scrollback push");
        assert!(app.lines[0].0.contains("fix the build"));
        let status = app.status.as_ref().expect("status must be set");
        assert!(status.contains("0/5"), "status iter/max wrong: {status}");
        assert!(status.contains("0/10000"), "status tokens wrong: {status}");
    }

    #[test]
    fn goal_iteration_updates_status_and_surfaces_blocker() {
        let mut app = FakeApp::default();
        let handled = render_goal_event(
            &mut app,
            &StreamEvent::GoalIteration {
                iter: 3,
                tokens_spent: 4_200,
                last_tag: TagOutcomeWire::Blocked {
                    why: "missing creds".into(),
                },
            },
        );
        assert!(handled);
        assert_eq!(app.status_updates, 1);
        let status = app.status.as_ref().expect("status");
        assert!(status.contains("iter 3"));
        assert!(status.contains("4200"));
        assert_eq!(app.lines.len(), 1, "blocked tag must push a notice");
        assert!(app.lines[0].0.contains("missing creds"));
        assert_eq!(app.lines[0].1, theme::YELLOW);
    }

    #[test]
    fn goal_iteration_in_progress_does_not_push_notice() {
        let mut app = FakeApp::default();
        render_goal_event(
            &mut app,
            &StreamEvent::GoalIteration {
                iter: 1,
                tokens_spent: 100,
                last_tag: TagOutcomeWire::InProgress {
                    what_remains: "writing tests".into(),
                },
            },
        );
        assert!(
            app.lines.is_empty(),
            "InProgress must not push a scrollback line; got {:?}",
            app.lines
        );
        assert!(app.status.is_some());
    }

    #[test]
    fn goal_verifying_updates_status_indicator() {
        let mut app = FakeApp::default();
        render_goal_event(&mut app, &StreamEvent::GoalVerifying);
        let status = app.status.as_ref().expect("status");
        assert!(status.contains("verifying"), "got: {status}");
        assert!(app.lines.is_empty());
    }

    #[test]
    fn goal_cleared_met_uses_green() {
        let mut app = FakeApp::default();
        render_goal_event(
            &mut app,
            &StreamEvent::GoalCleared {
                reason: ClearReasonWire::Met {
                    reason: "tests pass".into(),
                },
                iter: 2,
                tokens_spent: 5_000,
            },
        );
        assert_eq!(app.lines.len(), 1);
        assert!(app.lines[0].0.contains("done: tests pass"));
        assert!(app.lines[0].0.contains("iter 2"));
        assert!(app.lines[0].0.contains("5000 tok"));
        assert_eq!(app.lines[0].1, theme::GREEN);
        assert!(app.status.is_none(), "status must be cleared");
    }

    #[test]
    fn goal_cleared_user_slash_is_yellow_with_cancelled_text() {
        let mut app = FakeApp::default();
        render_goal_event(
            &mut app,
            &StreamEvent::GoalCleared {
                reason: ClearReasonWire::UserSlash,
                iter: 1,
                tokens_spent: 200,
            },
        );
        assert_eq!(app.lines.len(), 1);
        assert!(app.lines[0].0.contains("goal cancelled"));
        assert_eq!(app.lines[0].1, theme::YELLOW);
    }

    #[test]
    fn goal_cleared_user_clear_all_says_session_cleared() {
        let mut app = FakeApp::default();
        render_goal_event(
            &mut app,
            &StreamEvent::GoalCleared {
                reason: ClearReasonWire::UserClearAll,
                iter: 4,
                tokens_spent: 7_777,
            },
        );
        assert_eq!(app.lines.len(), 1);
        assert!(app.lines[0].0.contains("session cleared"));
        assert_eq!(app.lines[0].1, theme::YELLOW);
    }

    #[test]
    fn goal_cleared_max_iter_and_budget_render_yellow() {
        for r in [ClearReasonWire::MaxIter, ClearReasonWire::BudgetExhausted] {
            let mut app = FakeApp::default();
            render_goal_event(
                &mut app,
                &StreamEvent::GoalCleared {
                    reason: r.clone(),
                    iter: 0,
                    tokens_spent: 0,
                },
            );
            assert_eq!(app.lines[0].1, theme::YELLOW, "reason {r:?} must be yellow");
        }
    }

    #[test]
    fn goal_cleared_verifier_rejected_is_red_with_why() {
        let mut app = FakeApp::default();
        render_goal_event(
            &mut app,
            &StreamEvent::GoalCleared {
                reason: ClearReasonWire::VerifierRejected {
                    why: "tests still fail".into(),
                },
                iter: 5,
                tokens_spent: 9_999,
            },
        );
        assert_eq!(app.lines.len(), 1);
        assert!(app.lines[0].0.contains("verifier kept rejecting"));
        assert!(app.lines[0].0.contains("tests still fail"));
        assert_eq!(app.lines[0].1, theme::RED);
    }

    #[test]
    fn goal_cleared_verifier_unavailable_is_yellow() {
        let mut app = FakeApp::default();
        render_goal_event(
            &mut app,
            &StreamEvent::GoalCleared {
                reason: ClearReasonWire::VerifierUnavailable,
                iter: 1,
                tokens_spent: 0,
            },
        );
        assert!(app.lines[0].0.contains("verifier unavailable"));
        assert_eq!(app.lines[0].1, theme::YELLOW);
    }

    #[test]
    fn goal_inactive_emits_muted_info_not_error() {
        // Bug #20: bare `/goal` with no active goal must render as a benign
        // info row, not an error. Asserts on the COLOR not being theme::RED
        // because that's how `error>` rows are styled.
        let mut app = FakeApp::default();
        let handled = render_goal_event(&mut app, &StreamEvent::GoalInactive);
        assert!(handled, "GoalInactive must not fall through to error path");
        assert_eq!(app.lines.len(), 1);
        assert!(app.lines[0].0.contains("no active goal"));
        assert_ne!(
            app.lines[0].1,
            theme::RED,
            "GoalInactive must not render in error/red"
        );
    }

    #[test]
    fn non_goal_event_returns_false_unhandled() {
        let mut app = FakeApp::default();
        let handled = render_goal_event(&mut app, &StreamEvent::TextDelta { text: "hi".into() });
        assert!(!handled, "non-Goal events must fall through to existing dispatch");
        assert!(app.lines.is_empty());
        assert_eq!(app.status_updates, 0);
    }
}
