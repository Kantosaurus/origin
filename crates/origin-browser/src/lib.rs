//! origin-browser: dual-backend browser routing + WebFetch + WebSearch.
//!
//! Public surface is the three top-level entry points:
//!  - `BrowserRouter::run(verb)` for stateful browsing
//!  - `web_fetch::fetch(url)` for one-shot reader-mode fetches
//!  - `web_search::search(query)` for Tavily search
pub mod protocol;
pub mod detectors;
pub mod agent_browser;
pub mod cloak;
pub mod router;
pub mod web_fetch;
pub mod web_search;

pub use protocol::{Verb, SnapshotResp};
pub use router::{BrowserRouter, RouterError};
