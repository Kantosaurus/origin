//! Plan side-panel wiring for the cli (P9.9).
//!
//! Phase 9 introduced the `origin-plan` CRDT, the `origin-swarm` coordinator,
//! and the `origin-tui::widgets::plan_panel::PlanPanel` widget. The intended
//! end-to-end shape for the cli is:
//!
//! 1. The daemon owns a `PlanHandle` (op-log + broadcast sender).
//! 2. The cli, on connect, subscribes to `PlanHandle::watch()` and receives a
//!    `tokio::sync::broadcast::Receiver<OpEnvelope>`.
//! 3. Each received op flows through [`PlanPanel::apply_op`]; the panel
//!    re-folds the log; the cli re-renders.
//!
//! The cross-process plumbing for step 1+2 (broadcast over the existing
//! `origin-ipc` framing) is out of scope for P9.9 — the daemon does not yet
//! expose a `PlanHandle` over IPC. P9.9's hard test gate is the widget unit
//! tests in `crates/origin-tui/tests/plan_panel.rs`.
//!
//! This module pins the in-process shape so a future phase has a single
//! wiring seam to plug daemon-side broadcast into. [`Wiring::new`] gives us a
//! ready-to-render panel; [`Wiring::ingest`] is the call site that will be
//! driven by the broadcast receiver loop. The cli renderer can then call
//! `wiring.panel().render()` and write the resulting [`PlanLine`]s into a
//! `Grid` column the same way `Panel::render` does.
//!
//! TODO(p10): replace [`Wiring::ingest`]'s manual feed with a
//! `tokio::sync::broadcast::Receiver<OpEnvelope>` subscription point. The
//! daemon already broadcasts `StreamEvent`s; extending the protocol to carry
//! a `PlanOp` frame and threading `PlanHandle::subscribe()` through
//! `origin-daemon` is the remaining work.

use origin_plan::OpEnvelope;
use origin_tui::widgets::plan_panel::{PlanLine, PlanPanel};

/// In-process wiring for the plan side panel.
///
/// Wraps [`PlanPanel`] so the cli has a single seam to feed plan ops into.
/// When daemon-side broadcast lands, the `ingest` call site becomes the
/// receiver-loop branch.
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

    /// Apply a plan op to the underlying panel.
    ///
    /// The daemon-driven broadcast loop will call this from its
    /// `tokio::select!` branch when P10 wires it up.
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
