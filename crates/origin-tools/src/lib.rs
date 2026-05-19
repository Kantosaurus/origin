//! Tool registry + macros + builtin tools.

pub mod builtins;
pub mod dispatch;
pub mod macros;
pub mod registry;

pub use dispatch::{Cache, CacheHit, NormalizedInput, MEMOIZATION_SKIPLIST};
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
