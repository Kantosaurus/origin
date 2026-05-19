//! `origin-planner` — predictive prompt-cache prefix planner.
//!
//! Phase 3 deliverables: `Band`, `PrefixLedger`, `CachePlanner`, `WireDecision`.

pub mod band;
pub mod ledger;
pub mod planner;

pub use band::Band;
pub use ledger::{LedgerError, PrefixLedger, SectionId, Stability};
pub use planner::{CachePlanner, Plan, Section};
