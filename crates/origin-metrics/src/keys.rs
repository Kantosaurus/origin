//! Bounded-cardinality label keyspace.
//!
//! We enforce a static allowlist of (provider, tool, result) tuples that count
//! against the metric label set. Unknown values fall into `_other_` so a
//! pathological MCP server can't inflate cardinality.

pub const ALLOWED_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "gemini",
    "openrouter",
    "bedrock",
    "ollama",
    "github",
];

pub const ALLOWED_TOOLS: &[&str] = &[
    "Bash",
    "Edit",
    "Read",
    "Glob",
    "Grep",
    "Write",
    "Recall",
    "WebFetch",
    "graph_query",
    "graph_path",
    "graph_summarize",
    "graph_explain",
    "graph_rebuild",
    "mem_search",
    "mem_save",
    "mem_forget",
    "Ask",
    "Task",
];

pub const ALLOWED_RESULTS: &[&str] = &["ok", "err", "denied"];

#[must_use]
pub fn canonical_provider(s: &str) -> &'static str {
    canonicalize(s, ALLOWED_PROVIDERS)
}

#[must_use]
pub fn canonical_tool(s: &str) -> &'static str {
    canonicalize(s, ALLOWED_TOOLS)
}

#[must_use]
pub fn canonical_result(s: &str) -> &'static str {
    canonicalize(s, ALLOWED_RESULTS)
}

fn canonicalize(s: &str, allow: &[&'static str]) -> &'static str {
    for a in allow {
        if *a == s {
            return a;
        }
    }
    "_other_"
}
