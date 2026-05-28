//! Goal driver: persistent completion conditions with inline self-tag protocol.
//!
//! See `docs/superpowers/specs/2026-05-28-goal-skill-design.md`.

#![forbid(unsafe_code)]

pub mod flags;
pub mod state;
pub mod tag;
pub mod verifier;
pub mod wire;

pub use state::{ClearReason, GoalState, GoalStatus, TagOutcome};
pub use tag::parse_tag;
pub use flags::{parse_goal_args, GoalArgs, FlagParseError};
pub use wire::{ClearReasonWire, GoalSnapshot, GoalStatusWire, TagOutcomeWire};
