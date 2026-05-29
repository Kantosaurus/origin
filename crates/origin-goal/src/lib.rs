// SPDX-License-Identifier: Apache-2.0
//! Goal driver: persistent completion conditions with inline self-tag protocol.

#![forbid(unsafe_code)]

pub mod flags;
pub mod state;
pub mod tag;
pub mod verifier;
pub mod wire;

pub use flags::{parse_goal_args, FlagParseError, GoalArgs};
pub use state::{ClearReason, GoalState, GoalStatus, TagOutcome};
pub use tag::parse_tag;
pub use wire::{ClearReasonWire, GoalSnapshot, GoalStatusWire, TagOutcomeWire};
