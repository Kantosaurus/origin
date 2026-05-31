// SPDX-License-Identifier: Apache-2.0
//! Migrate sessions/skills/memories from other harnesses into `origin`.
//! See spec §11 Phase 14 — "Migration tools".

#![forbid(unsafe_code)]

pub mod claude_code;
pub mod jcode;
pub mod opencode;
pub mod reconstruct;
pub mod sink;
pub mod source;
