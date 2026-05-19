//! `origin-sidecar` — always-on small-model worker (N2.5).

pub mod extract;
pub mod job;
pub mod runtime;
pub mod summarize;

pub use job::{ExtractDeliverer, SidecarJob, SummaryDeliverer};
pub use runtime::{Sidecar, SidecarConfig, SidecarError};
