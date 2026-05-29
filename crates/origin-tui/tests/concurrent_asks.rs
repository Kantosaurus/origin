// SPDX-License-Identifier: Apache-2.0
use origin_permission::prompt::Prompter;
use origin_tools::{SandboxProfile, SideEffects, Tier, ToolMeta, Urgency};
use origin_tui::{PanelEvent, PermissionOutcome, SidePanelPrompter};
use std::sync::Arc;
use tokio::sync::mpsc;

const META_READ: ToolMeta = ToolMeta {
    name: "Read",
    description: "read",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: "{}",
    sandbox_profile: SandboxProfile::ReadFs,
    token_budget: origin_tools::DEFAULT_TOKEN_BUDGET,
    hot: true,
};

const META_BASH: ToolMeta = ToolMeta {
    name: "Bash",
    description: "bash",
    tier: Tier::RequiresPermission,
    urgency: Urgency::High,
    side_effects: SideEffects::Mutating,
    input_schema: "{}",
    sandbox_profile: SandboxProfile::Shell,
    token_budget: origin_tools::DEFAULT_TOKEN_BUDGET,
    hot: true,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::panic)]
async fn two_concurrent_asks_both_deliver_via_queue() {
    let (tx, mut rx) = mpsc::channel::<PanelEvent>(8);
    let prompter = Arc::new(SidePanelPrompter::new(tx));

    // Spawn the first ask, then wait for its event to land in the mpsc queue
    // before spawning the second. The first event's arrival is a causal proof
    // that ask #1 took the submit lock, sent its event, released the lock, and
    // is now parked on its oneshot — so ask #2 will follow with id+1.
    let p1 = prompter.clone();
    let h1 = tokio::spawn(async move { p1.ask(&META_READ, "/etc/hosts").await });
    let ev1 = rx.recv().await.expect("ev1");

    let p2 = prompter.clone();
    let h2 = tokio::spawn(async move { p2.ask(&META_BASH, "ls -la").await });
    let ev2 = rx.recv().await.expect("ev2");
    let (id1, id2) = match (ev1, ev2) {
        (
            PanelEvent::PermissionAsk { id: i1, tool: t1, .. },
            PanelEvent::PermissionAsk { id: i2, tool: t2, .. },
        ) => {
            assert_eq!(t1, "Read");
            assert_eq!(t2, "Bash");
            (i1, i2)
        }
        other => panic!("expected two PermissionAsks, got {other:?}"),
    };

    // Resolve in reverse order to prove the queue dispatches by id, not FIFO.
    prompter.resolve(id2, PermissionOutcome::Allow);
    prompter.resolve(id1, PermissionOutcome::Deny);

    let allowed_read = h1.await.expect("join h1");
    let allowed_bash = h2.await.expect("join h2");
    assert!(!allowed_read, "Read was denied");
    assert!(allowed_bash, "Bash was allowed");
}
