// SPDX-License-Identifier: Apache-2.0
//! End-to-end check that an active `/goal` actually causes `run_loop` to
//! emit the `<origin-goal>` block in the assembled system prompt of every
//! provider call. This is the regression net the Wave-4 harness couldn't
//! cast — that harness short-circuited `run_loop` entirely and built its
//! own minimal `ChatRequest`, so a missing-block bug in
//! `agent.rs::run_loop` would have slipped by every existing T11-T16 test.
//!
//! The Wave-9 follow-up harness now routes through real `run_loop`. This
//! test:
//!   1. Activates a goal whose condition contains a distinctive literal
//!      ("fix the tests") so we can grep the captured system prompt for
//!      it.
//!   2. Runs one iteration with a scripted `in_progress` reply.
//!   3. Asserts the captured main-loop `ChatRequest.system` contains the
//!      opening `<origin-goal>` tag AND the literal condition text. Both
//!      assertions matter: the tag check pins the wire shape, the
//!      condition check pins the substitution path (i.e. confirms the
//!      block isn't a static placeholder).

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use origin_goal::GoalState;

mod goal_helpers;
use goal_helpers::{run_driver_loop_with_state, ScriptedProvider};

#[tokio::test]
async fn active_goal_renders_origin_goal_block_in_system_prompt() {
    // Single in_progress reply — we only need one main-loop call to
    // capture the system prompt. The cap (max_iter=1) ensures the driver
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
    let sys = &main_calls[0].system;

    // 1) Wire-shape check: the `<origin-goal>` tag must appear in the
    //    assembled system prompt. The closing tag check is paired so a
    //    half-rendered block (open tag only) still trips this assertion.
    assert!(
        sys.contains("<origin-goal>"),
        "system prompt missing <origin-goal> opening tag; got:\n{sys}"
    );
    assert!(
        sys.contains("</origin-goal>"),
        "system prompt missing </origin-goal> closing tag; got:\n{sys}"
    );

    // 2) Substitution check: the literal goal condition must be inside
    //    the block. This guards against a regression that hard-coded a
    //    placeholder condition into the block.
    assert!(
        sys.contains("fix the tests"),
        "system prompt missing literal goal condition; got:\n{sys}"
    );

    // 3) Ordering check: `<origin-goal>` must come AFTER the identity
    //    block. The agent.rs assembly places the goal block last so the
    //    prompt-cache breakpoints sit on the static prefix; this is the
    //    perf-relevant invariant.
    let id_pos = sys
        .find("<origin-identity>")
        .or_else(|| sys.find("identity"))
        .unwrap_or(0);
    let goal_pos = sys.find("<origin-goal>").expect("checked above");
    assert!(
        goal_pos > id_pos,
        "<origin-goal> block must come after identity material so the cache prefix stays stable; \
         goal_pos={goal_pos}, id_pos={id_pos}"
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
    assert!(
        !sys.contains("<origin-goal>"),
        "no goal active → must not render the goal block; got:\n{sys}"
    );
}
