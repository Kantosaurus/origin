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

    /// C2: `false` when no person can answer this prompter (a headless run with
    /// no attached client, or a sub-agent whose `ask` only checks the grants the
    /// user approved). An ask that needs a person is then denied without calling
    /// [`Prompter::ask`].
    fn can_ask(&self) -> bool {
        true
    }

    /// C2: [`Prompter::ask`], saying whether the person may answer "always
    /// allow" (`false` for a `Task` delegation or an ask that needs a person
    /// every time). Prompters without an "always" answer ignore the flag.
    async fn ask_offering(&self, meta: &ToolMeta, args_preview: &str, offer_always: bool) -> bool {
        let _ = offer_always;
        self.ask(meta, args_preview).await
    }

    /// C2: whether the person answered "always allow" for `tool` earlier in
    /// this session. Consulted only for an ordinary ask, never for `Task` or an
    /// ask that needs a person.
    fn standing_allow(&self, tool: &str) -> bool {
        let _ = tool;
        false
    }
}

pub struct AlwaysAllow;
pub struct AlwaysDeny;
/// C2: nobody can be asked (headless run, no attached client). Every ask is
/// denied and [`Prompter::can_ask`] says so, so the denial can say why.
pub struct NoPrompter;

#[async_trait]
impl Prompter for NoPrompter {
    #[allow(clippy::unused_async)] // trait signature requires async
    async fn ask(&self, _meta: &ToolMeta, _args_preview: &str) -> bool {
        false
    }

    fn can_ask(&self) -> bool {
        false
    }
}

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
