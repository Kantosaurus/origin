// SPDX-License-Identifier: Apache-2.0
//! `Prompter` trait + test prompters.
//!
//! Production prompter lives in the TUI client (introduced in P1.11) and is
//! upgraded to side-panel-based asks in P4. Headless prompter (auto-deny
//! default) arrives in P13.

use async_trait::async_trait;
use origin_tools::ToolMeta;

#[async_trait]
pub trait Prompter: Send + Sync {
    /// Ask the user to approve a tool invocation. Returns `true` for allow.
    async fn ask(&self, meta: &ToolMeta, args_preview: &str) -> bool;
}

pub struct AlwaysAllow;
pub struct AlwaysDeny;

#[async_trait]
impl Prompter for AlwaysAllow {
    #[allow(clippy::unused_async)] // trait signature requires async
    async fn ask(&self, _meta: &ToolMeta, _args_preview: &str) -> bool {
        true
    }
}

#[async_trait]
impl Prompter for AlwaysDeny {
    #[allow(clippy::unused_async)] // trait signature requires async
    async fn ask(&self, _meta: &ToolMeta, _args_preview: &str) -> bool {
        false
    }
}
