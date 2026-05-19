//! Plan side panel (P9.9).
//!
//! `PlanPanel` is a pure-data widget: it owns an in-memory `Vec<OpEnvelope>`,
//! re-folds it on every mutation through [`origin_plan::fold`], and emits a
//! flat `Vec<PlanLine>` for the renderer to lay into a [`crate::Grid`].
//!
//! The canonical `origin-plan` surface materialises plan state through
//! `fold(impl IntoIterator<Item = OpEnvelope>) -> Plan` — there is no
//! `Plan::apply` to call op-by-op. We therefore retain the log and re-fold;
//! the fold is `O(n log n)` in op count, which is fine for the small swarm
//! plans expected in Phase 9 (tens to low hundreds of steps).

use origin_plan::{fold, ActorId, OpEnvelope, Plan, Status, StepId};

/// One rendered row in the plan panel.
///
/// The fields are public so the cli renderer can format and write cells
/// without going through accessor methods — this keeps `origin-tui`
/// testable without a real terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLine {
    /// Stable id of the underlying step.
    pub id: StepId,
    /// Indentation level (0 = root). Phase 9 renders the flat root list;
    /// nested rendering is a Phase 10+ concern.
    pub indent: u8,
    /// Glyph derived from the step's [`Status`].
    pub status_glyph: char,
    /// Step body after LWW resolution.
    pub content: String,
    /// Current lease holder, if any. Computed with `now_ms = 0` so leases
    /// with finite `expires_at_ms` still surface in the UI; the daemon is
    /// responsible for not emitting stale leases into the broadcast.
    pub holder: Option<ActorId>,
}

/// Pure-data plan side-panel widget.
#[derive(Debug, Default)]
pub struct PlanPanel {
    log: Vec<OpEnvelope>,
    plan: Plan,
}

impl PlanPanel {
    /// Construct an empty panel.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `op` to the local log and re-fold.
    ///
    /// Folding the entire log on every op is deliberate: `origin-plan`'s
    /// fold is permutation-invariant, so the only correctness-preserving
    /// way to "apply" an op is to refold the log. For the swarm-plan
    /// sizes we expect (Phase 9), this is well below human-perceptible.
    pub fn apply_op(&mut self, op: OpEnvelope) {
        self.log.push(op);
        self.plan = fold(self.log.iter().cloned());
    }

    /// Borrow the folded plan. Used by tests and by the cli renderer
    /// when it needs more than `render()` exposes.
    #[must_use]
    pub const fn fold(&self) -> &Plan {
        &self.plan
    }

    /// Produce one [`PlanLine`] per root-level step in Logoot order.
    ///
    /// Nested children are intentionally omitted in P9.9 — Phase 9's
    /// swarm plans are flat. Rendering children is a one-line change
    /// (recurse on `iter_children(Some(step.id()))`) when needed.
    #[must_use]
    pub fn render(&self) -> Vec<PlanLine> {
        self.plan
            .iter_root()
            .map(|step| PlanLine {
                id: step.id(),
                indent: 0,
                status_glyph: status_glyph(step.status()),
                content: step.body().to_owned(),
                holder: self.plan.lease_holder(step.id(), 0),
            })
            .collect()
    }
}

/// Map an on-disk [`Status`] to its single-character glyph.
///
/// Phase 9 ships with four statuses; the `Blocked` / `Failed` glyphs in the
/// original Phase 9 draft were dropped during P9.1 API reconciliation.
const fn status_glyph(status: Status) -> char {
    match status {
        Status::Pending => '○',
        Status::InProgress => '◐',
        Status::Done => '●',
        Status::Cancelled => '✕',
    }
}

#[cfg(test)]
mod tests {
    use super::{status_glyph, PlanPanel};
    use origin_plan::Status;

    #[test]
    fn glyph_table_matches_phase_9_spec() {
        assert_eq!(status_glyph(Status::Pending), '○');
        assert_eq!(status_glyph(Status::InProgress), '◐');
        assert_eq!(status_glyph(Status::Done), '●');
        assert_eq!(status_glyph(Status::Cancelled), '✕');
    }

    #[test]
    fn empty_panel_renders_no_lines() {
        let panel = PlanPanel::new();
        assert!(panel.render().is_empty());
    }
}
