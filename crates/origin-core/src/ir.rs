#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheKind {
    None,
    Implicit,
    Explicit,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy)]
pub struct ProviderCaps {
    pub prompt_cache: CacheKind,
    pub thinking: bool,
    pub parallel_tools: bool,
    pub vision: bool,
    pub audio: bool,
}
