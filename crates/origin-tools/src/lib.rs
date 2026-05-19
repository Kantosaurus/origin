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
