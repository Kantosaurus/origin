// SPDX-License-Identifier: Apache-2.0
//! `origin-hooks` — pre-spawned shell pool + typed lifecycle event dispatch.
//!
//! Modules land in P10.5 (`shellpool`) and P10.6 (`event` + `dispatch`).

pub mod shellpool;

pub use shellpool::{PoolError, ShellPool, ShellSpec};

pub mod config;
pub mod dispatch;
pub mod event;

pub use config::{ConfigError, HookEntry, HookEventKind, HooksConfig};
pub use dispatch::{dispatch_event, DispatchError};
pub use event::{
    parse_hook_stdout, HookOverride, HookOverrideInner, HookParseError, LifecycleEvent, ToolPhase,
};
