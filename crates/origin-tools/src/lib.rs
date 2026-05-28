//! Tool registry + macros + builtin tools.

/// Default per-tool token budget for serialised results. Tools may override
/// via the `token_budget:` arm of `origin_tool!`.
pub const DEFAULT_TOKEN_BUDGET: u32 = 25_000;

pub mod budget_writer;
pub mod builtins;
pub mod proc_supervisor;
pub mod result_cas;
pub mod dispatch;
pub mod error;
pub mod macros;
pub mod registry;
pub mod text_fmt;
pub mod tool_envelope;

pub use error::{ErrClass, ToolError};

pub use dispatch::{Cache, CacheHit, NormalizedInput, MEMOIZATION_SKIPLIST};
// Re-export so downstream tests + callers can construct `ToolMeta` literals
// without taking a direct dep on `origin-sandbox` (P11.5).
pub use origin_sandbox::{ProfileOrdinal, SandboxProfile};
pub use registry::{registry_iter, ToolMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    AutoAllowed,
    RequiresPermission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Urgency {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SideEffects {
    Pure,
    Mutating,
}

/// Runtime tool object — what dispatch actually calls when a tool has no
/// compile-time inventory entry (MCP-discovered tools live here).
#[async_trait::async_trait]
pub trait DynTool: Send + Sync + std::fmt::Debug {
    fn meta(&self) -> &ToolMeta;
    /// `args` is JSON; the returned `Value` is the tool's structured result.
    async fn invoke(&self, args: serde_json::Value) -> Result<serde_json::Value, String>;
}
