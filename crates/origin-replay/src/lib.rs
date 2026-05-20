//! Deterministic record-and-replay for `origin` sessions.
//!
//! See spec §10C N10.7-N10.8.

#![forbid(unsafe_code)]

pub mod bundle;
pub mod clock;
pub mod recorder;
pub mod rng;
