// SPDX-License-Identifier: Apache-2.0
//! Core message types and the canonical Intermediate Representation (IR) for origin.
//!
//! [`ir`] defines the load-bearing types that flow through the whole system —
//! `Message`, `Block`, and `ToolCall` — and [`types`] holds the shared
//! provider/capability types. All are `rkyv`-archivable, so a single byte buffer
//! flows through IPC, `SQLite` blobs, and in-memory ring buffers without
//! re-encoding on the hot path.

pub mod ir;
pub mod types;
