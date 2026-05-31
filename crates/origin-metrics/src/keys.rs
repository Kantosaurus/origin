// SPDX-License-Identifier: Apache-2.0
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

/// OpenTelemetry `GenAI` semantic-convention attribute and metric keys.
///
/// These are the standard `gen_ai.*` names from the OpenTelemetry semantic
/// conventions for Generative-AI systems. Origin's internal Prometheus families
/// (`origin_tokens_in_total`, …) keep their established names for the
/// `/metrics` text endpoint; this module provides the parallel `gen_ai.*`
/// vocabulary that the OTLP export attaches so dashboards built against the
/// convention work unchanged. See [`gen_ai_for_internal`] for the mapping from
/// an internal family name to its convention counterpart.
pub mod genai {
    /// `gen_ai.system` attribute: the `GenAI` product/provider (`anthropic`, …).
    pub const SYSTEM: &str = "gen_ai.system";

    /// `gen_ai.operation.name` attribute: the operation (`chat`, `text_completion`, …).
    pub const OPERATION_NAME: &str = "gen_ai.operation.name";

    /// `gen_ai.request.model` attribute: the model named in the request.
    pub const REQUEST_MODEL: &str = "gen_ai.request.model";

    /// `gen_ai.response.model` attribute: the model that produced the response.
    pub const RESPONSE_MODEL: &str = "gen_ai.response.model";

    /// `gen_ai.usage.input_tokens` metric/attribute: prompt (input) tokens.
    pub const USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";

    /// `gen_ai.usage.output_tokens` metric/attribute: completion (output) tokens.
    pub const USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";

    /// `gen_ai.client.operation.duration` metric: end-to-end client latency.
    pub const OPERATION_DURATION: &str = "gen_ai.client.operation.duration";

    /// `gen_ai.server.time_to_first_token` metric: latency to the first token.
    pub const TIME_TO_FIRST_TOKEN: &str = "gen_ai.server.time_to_first_token";

    /// `gen_ai.tool.call.count` metric: number of model-issued tool calls.
    pub const TOOL_CALL_COUNT: &str = "gen_ai.tool.call.count";

    /// `gen_ai.tool.name` attribute: the invoked tool's name.
    pub const TOOL_NAME: &str = "gen_ai.tool.name";
}

/// Convention-mapped name for one of origin's internal metric families.
///
/// Maps an `origin_*` Prometheus family name to the OpenTelemetry `GenAI`
/// semantic-convention name the OTLP pipeline should export it under. Returns
/// `None` for families that have no convention counterpart (e.g.
/// `origin_sandbox_violation_total`, which is origin-specific). The mapping is
/// pure and total over the inputs it recognizes, so it is unit-testable without
/// constructing an exporter.
#[must_use]
pub fn gen_ai_for_internal(internal: &str) -> Option<&'static str> {
    match internal {
        "origin_tokens_in_total" => Some(genai::USAGE_INPUT_TOKENS),
        "origin_tokens_out_total" => Some(genai::USAGE_OUTPUT_TOKENS),
        "origin_tool_call_total" => Some(genai::TOOL_CALL_COUNT),
        _ => None,
    }
}

/// Convention-mapped attribute name for one of origin's internal label keys.
///
/// Maps an internal Prometheus label name (`provider`, `model`, `tool`, …) to
/// the OpenTelemetry `GenAI` semantic-convention attribute key the OTLP pipeline
/// should attach. Returns `None` for labels with no convention counterpart
/// (e.g. `result`, `profile`, `kind`).
#[must_use]
pub fn gen_ai_attr_for_label(label: &str) -> Option<&'static str> {
    match label {
        "provider" => Some(genai::SYSTEM),
        "model" => Some(genai::REQUEST_MODEL),
        "tool" => Some(genai::TOOL_NAME),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{gen_ai_attr_for_label, gen_ai_for_internal, genai};

    #[test]
    fn internal_families_map_to_convention_names() {
        assert_eq!(
            gen_ai_for_internal("origin_tokens_in_total"),
            Some("gen_ai.usage.input_tokens")
        );
        assert_eq!(
            gen_ai_for_internal("origin_tokens_out_total"),
            Some("gen_ai.usage.output_tokens")
        );
        assert_eq!(
            gen_ai_for_internal("origin_tool_call_total"),
            Some("gen_ai.tool.call.count")
        );
    }

    #[test]
    fn origin_specific_families_have_no_convention_name() {
        assert_eq!(gen_ai_for_internal("origin_sandbox_violation_total"), None);
        assert_eq!(gen_ai_for_internal("origin_cache_hit_total"), None);
        assert_eq!(gen_ai_for_internal("not_a_family"), None);
    }

    #[test]
    fn labels_map_to_convention_attributes() {
        assert_eq!(gen_ai_attr_for_label("provider"), Some("gen_ai.system"));
        assert_eq!(gen_ai_attr_for_label("model"), Some("gen_ai.request.model"));
        assert_eq!(gen_ai_attr_for_label("tool"), Some("gen_ai.tool.name"));
        assert_eq!(gen_ai_attr_for_label("result"), None);
        assert_eq!(gen_ai_attr_for_label("profile"), None);
    }

    #[test]
    fn convention_constants_use_the_gen_ai_namespace() {
        for k in [
            genai::SYSTEM,
            genai::OPERATION_NAME,
            genai::REQUEST_MODEL,
            genai::RESPONSE_MODEL,
            genai::USAGE_INPUT_TOKENS,
            genai::USAGE_OUTPUT_TOKENS,
            genai::OPERATION_DURATION,
            genai::TIME_TO_FIRST_TOKEN,
            genai::TOOL_CALL_COUNT,
            genai::TOOL_NAME,
        ] {
            assert!(k.starts_with("gen_ai."), "{k} must be a gen_ai.* key");
        }
    }
}
