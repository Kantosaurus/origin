// SPDX-License-Identifier: Apache-2.0
//! End-to-end check that an active `/goal` actually causes `run_loop` to
//! deliver the `<origin-goal>` block to the provider on every call. This is
//! the regression net the Wave-4 harness couldn't cast — that harness
//! short-circuited `run_loop` entirely and built its own minimal
//! `ChatRequest`, so a missing-block bug in `agent.rs::run_loop` would have
//! slipped by every existing T11-T16 test.
//!
//! Since the volatile-context split, the goal block is deliberately NOT part
//! of `ChatRequest.system`: its per-iteration counters would invalidate the
//! cached system+tools prefix on every request, so `run_loop` appends it as
//! a trailing text block on the last (user-role) message instead. This test
//! pins BOTH sides of that contract:
//!   1. The block (tags + substituted condition) reaches the provider in the
//!      trailing user-message text.
//!   2. The system prompt stays goal-free, keeping the cache prefix stable.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_goal::GoalState;

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn active_goal_renders_origin_goal_block_as_trailing_message() {
    // Single in_progress reply — we only need one main-loop call to
    // capture the request. The cap (max_iter=1) ensures the driver
    // stops after this iteration via MaxIter, so the test doesn't hang
    // waiting for more scripted replies.
    let provider = ScriptedProvider::new().with_main_reply(
        "iter 1 progress\n\
         <goal-status state=\"in_progress\"><reason>still going</reason></goal-status>",
    );

    let state = GoalState::new("fix the tests".into(), Some(1), None);
    let run = run_driver_loop_with_state(state, "begin", &provider, 10).await;

    // Sanity: the iteration actually ran.
    assert_eq!(run.iterations_run, 1);
    assert_eq!(provider.main_remaining(), 0);

    // Pull the captured main-loop request.
    let main_calls: Vec<_> = provider
        .captured()
        .into_iter()
        .filter(|r| !r.is_verifier)
        .collect();
    assert_eq!(
        main_calls.len(),
        1,
        "expected exactly one main-loop call; got {main_calls:?}"
    );
    let user_text = &main_calls[0].last_user_text;
    let sys = &main_calls[0].system;

    // 1) Wire-shape check: the `<origin-goal>` block must reach the provider
    //    as trailing user-message text. The closing tag check is paired so a
    //    half-rendered block (open tag only) still trips this assertion.
    assert!(
        user_text.contains("<origin-goal>"),
        "trailing user message missing <origin-goal> opening tag; got:\n{user_text}"
    );
    assert!(
        user_text.contains("</origin-goal>"),
        "trailing user message missing </origin-goal> closing tag; got:\n{user_text}"
    );

    // 2) Substitution check: the literal goal condition must be inside
    //    the block. This guards against a regression that hard-coded a
    //    placeholder condition into the block.
    assert!(
        user_text.contains("fix the tests"),
        "trailing user message missing literal goal condition; got:\n{user_text}"
    );

    // 3) Cache-stability check: the volatile goal block must NOT leak into
    //    the system prompt — its per-iteration counters would invalidate the
    //    cached system+tools prefix on every request. The identity check
    //    confirms the real assembled system prompt (not an empty stub) was
    //    captured, so this assertion can't pass vacuously.
    assert!(
        sys.contains("<origin-identity>"),
        "captured system prompt looks like a stub; got:\n{sys}"
    );
    assert!(
        !sys.contains("<origin-goal>"),
        "goal block leaked into the system prompt (breaks the prompt-cache prefix); got:\n{sys}"
    );
}

#[tokio::test]
async fn no_active_goal_omits_origin_goal_block() {
    // The block must NOT appear when no goal is active. We can't trigger
    // this via the harness's run_driver_loop_full (which always installs
    // a goal), so we drive a single bare run_loop call against the same
    // ScriptedProvider directly and inspect the capture.
    use origin_daemon::agent::{run_loop, LoopOptions};
    use origin_daemon::session::Session;
    use origin_permission::prompt::AlwaysAllow;

    let provider = ScriptedProvider::new().with_main_reply("plain reply, no goal");
    let mut session = Session::new("test", "claude-opus-4-7");
    let opts = LoopOptions {
        streaming_disabled: true,
        ..LoopOptions::default()
    };
    let _ = run_loop(&mut session, "hi", &provider, &AlwaysAllow, &opts)
        .await
        .expect("run_loop");

    let main_calls = provider.captured();
    assert_eq!(main_calls.len(), 1);
    let sys = &main_calls[0].system;
    let user_text = &main_calls[0].last_user_text;
    assert!(
        !sys.contains("<origin-goal>") && !user_text.contains("<origin-goal>"),
        "no goal active → must not render the goal block anywhere; system:\n{sys}\nuser:\n{user_text}"
    );
}
