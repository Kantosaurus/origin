// SPDX-License-Identifier: Apache-2.0
//! `origin-trace` — structured tracing spans written to a per-day parquet ring.
//!
//! The crate exposes (1) a `tracing::Subscriber`-compatible layer that turns
//! every span close into a row, (2) a per-day parquet writer that rotates at
//! 64 MiB, and (3) a query layer with column-pushdown predicates.

#![allow(clippy::module_name_repetitions)]

pub mod layer;
pub mod query;
pub mod ring;
pub mod schema;

pub use layer::{init, Layer, LayerGuard};
pub use query::{QueryArgs, QueryError, QueryRow};
pub use ring::{Ring, RingError};
pub use schema::{span_schema, SpanRow};
