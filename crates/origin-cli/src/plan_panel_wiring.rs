// SPDX-License-Identifier: Apache-2.0
//! Plan side-panel wiring for the cli.
//!
//! Wire shape (P9.9 → P10):
//!
//! 1. The daemon hosts a process-wide
//!    [`PlanBus`](origin_daemon::plan_bus::PlanBus); swarm coordinators
//!    publish each successful `PlanHandle::apply` op to it.
//! 2. The cli sends [`ClientMessage::SubscribePlan`] over the IPC socket;
//!    the daemon spawns a relay task that forwards every envelope as a
//!    [`StreamEvent::PlanOp`] frame.
//! 3. The cli's IPC loop deserialises the frame and calls
//!    [`Wiring::ingest`]; the panel re-folds the log; the cli re-renders.
//!
//! [`Wiring::ingest`] is the single seam the cli's IPC loop drives. The
//! widget unit tests in `crates/origin-tui/tests/plan_panel.rs` cover the
//! fold semantics; [`tests`] below covers the in-process feed path.

use origin_plan::OpEnvelope;
use origin_tui::widgets::plan_panel::{PlanLine, PlanPanel};

/// In-process wiring for the plan side panel.
///
/// Wraps [`PlanPanel`] so the cli has a single seam to feed plan ops into.
/// The daemon-driven [`StreamEvent::PlanOp`] receive loop calls [`Self::ingest`].
#[derive(Debug, Default)]
pub struct Wiring {
    panel: PlanPanel,
}

impl Wiring {
    /// Construct a fresh, empty wiring.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply a plan op to the underlying panel. Called by the cli's IPC
    /// receive loop for every [`StreamEvent::PlanOp`] frame.
    pub fn ingest(&mut self, op: OpEnvelope) {
        self.panel.apply_op(op);
    }

    /// Borrow the underlying panel for rendering.
    #[must_use]
    pub const fn panel(&self) -> &PlanPanel {
        &self.panel
    }

    /// Convenience: render the panel into a flat `Vec<PlanLine>`.
    #[must_use]
    pub fn render(&self) -> Vec<PlanLine> {
        self.panel.render()
    }
}

#[cfg(test)]
mod tests {
    use super::Wiring;
    use origin_plan::{ActorId, AddStep, Lamport, LogootKey, Op, OpEnvelope, StepId};

    #[test]
    fn ingest_then_render_round_trip() {
        let actor = ActorId::new(1);
        let id = StepId::from_u128(99);
        let key = LogootKey::between(None, None, actor, 1);

        let mut wiring = Wiring::new();
        wiring.ingest(OpEnvelope::new(
            actor,
            Lamport::new(1),
            Op::AddStep(AddStep {
                id,
                parent: None,
                body: "wired".into(),
                key,
            }),
        ));

        let lines = wiring.render();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].content, "wired");
    }
}
