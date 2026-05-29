// SPDX-License-Identifier: Apache-2.0
//! Thin re-export of [`origin_resume_token::ResumeToken`] for ergonomic
//! `origin_supervisor::resume_token::ResumeToken` paths.
//!
//! The underlying type lives in the leaf `origin-resume-token` crate so the
//! daemon can depend on the same shape without forming a daemon ↔ supervisor
//! dependency cycle. P12 ships the loader + the daemon-side ack handler;
//! the on-disk format is JSON for tooling friendliness.

pub use origin_resume_token::ResumeToken;
