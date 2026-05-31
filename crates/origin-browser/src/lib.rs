// SPDX-License-Identifier: Apache-2.0
//! `origin-browser`: dual-backend browser routing + `WebFetch` + `WebSearch`.
//!
//! Public surface is the top-level entry points:
//!  - `BrowserRouter::run(verb)` for stateful browsing
//!  - `web_fetch::fetch(url)` for one-shot reader-mode fetches
//!  - `web_search::search(query)` for Tavily search
//!  - `visual::VisualCapture` for the opt-in screenshot + console "visual loop"
pub mod agent_browser;
pub mod cloak;
pub mod detectors;
pub mod protocol;
pub mod router;
pub mod visual;
pub mod web_fetch;
pub mod web_search;

pub use protocol::{SnapshotResp, Verb};
pub use router::{BrowserRouter, RouterError};
pub use visual::VisualCapture;
