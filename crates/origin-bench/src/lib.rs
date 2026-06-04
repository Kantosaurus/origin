// SPDX-License-Identifier: Apache-2.0
//! Benchmark harness comparing origin against other coding-agent CLIs.
//!
//! A fixed [`task_set`] is driven through either the in-process origin
//! [`runner_origin`] or a generic [`runner_subprocess`], collecting [`metrics`]
//! and rendering Markdown/JSON [`report`]s. Repeated runs feed the
//! multi-sample pass@k / pass^k / flakiness metrics in [`reliability`].

pub mod leaderboard;
pub mod metrics;
pub mod reliability;
pub mod report;
pub mod runner_origin;
pub mod runner_subprocess;
pub mod task_set;
