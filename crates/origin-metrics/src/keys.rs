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

    // -- Request attributes (sampling / generation parameters) --------------

    /// `gen_ai.request.temperature` attribute: requested sampling temperature.
    pub const REQUEST_TEMPERATURE: &str = "gen_ai.request.temperature";

    /// `gen_ai.request.top_p` attribute: requested nucleus-sampling `top_p`.
    pub const REQUEST_TOP_P: &str = "gen_ai.request.top_p";

    /// `gen_ai.request.top_k` attribute: requested top-k sampling value.
    pub const REQUEST_TOP_K: &str = "gen_ai.request.top_k";

    /// `gen_ai.request.max_tokens` attribute: requested max output tokens.
    pub const REQUEST_MAX_TOKENS: &str = "gen_ai.request.max_tokens";

    /// `gen_ai.request.frequency_penalty` attribute: requested frequency penalty.
    pub const REQUEST_FREQUENCY_PENALTY: &str = "gen_ai.request.frequency_penalty";

    /// `gen_ai.request.presence_penalty` attribute: requested presence penalty.
    pub const REQUEST_PRESENCE_PENALTY: &str = "gen_ai.request.presence_penalty";

    /// `gen_ai.request.stop_sequences` attribute: requested stop sequences.
    pub const REQUEST_STOP_SEQUENCES: &str = "gen_ai.request.stop_sequences";

    /// `gen_ai.request.seed` attribute: requested deterministic sampling seed.
    pub const REQUEST_SEED: &str = "gen_ai.request.seed";

    /// `gen_ai.request.encoding_formats` attribute: requested embedding encodings.
    pub const REQUEST_ENCODING_FORMATS: &str = "gen_ai.request.encoding_formats";

    /// `gen_ai.request.choice.count` attribute: number of completion choices requested.
    pub const REQUEST_CHOICE_COUNT: &str = "gen_ai.request.choice.count";

    // -- Response attributes -------------------------------------------------

    /// `gen_ai.response.id` attribute: provider-assigned response identifier.
    pub const RESPONSE_ID: &str = "gen_ai.response.id";

    /// `gen_ai.response.finish_reasons` attribute: per-choice finish reasons.
    pub const RESPONSE_FINISH_REASONS: &str = "gen_ai.response.finish_reasons";

    // -- Usage ---------------------------------------------------------------

    /// `gen_ai.usage.cached_input_tokens` attribute: prompt tokens served from cache.
    pub const USAGE_CACHED_INPUT_TOKENS: &str = "gen_ai.usage.cached_input_tokens";

    // -- Operation / agent / tool / conversation -----------------------------

    /// `gen_ai.agent.id` attribute: the unique agent identifier.
    pub const AGENT_ID: &str = "gen_ai.agent.id";

    /// `gen_ai.agent.name` attribute: the human-readable agent name.
    pub const AGENT_NAME: &str = "gen_ai.agent.name";

    /// `gen_ai.agent.description` attribute: free-form agent description.
    pub const AGENT_DESCRIPTION: &str = "gen_ai.agent.description";

    /// `gen_ai.tool.call.id` attribute: the tool-call identifier.
    pub const TOOL_CALL_ID: &str = "gen_ai.tool.call.id";

    /// `gen_ai.tool.description` attribute: free-form tool description.
    pub const TOOL_DESCRIPTION: &str = "gen_ai.tool.description";

    /// `gen_ai.tool.type` attribute: the tool kind (`function`, `extension`, …).
    pub const TOOL_TYPE: &str = "gen_ai.tool.type";

    /// `gen_ai.conversation.id` attribute: the conversation/thread identifier.
    pub const CONVERSATION_ID: &str = "gen_ai.conversation.id";

    /// `gen_ai.data_source.id` attribute: the retrieval data-source identifier.
    pub const DATA_SOURCE_ID: &str = "gen_ai.data_source.id";

    // -- Provider / system ---------------------------------------------------

    /// `gen_ai.provider.name` attribute: the provider (newer alias of `system`).
    pub const PROVIDER_NAME: &str = "gen_ai.provider.name";

    /// `server.address` attribute: the `GenAI` server host or socket path.
    pub const SERVER_ADDRESS: &str = "server.address";

    /// `server.port` attribute: the `GenAI` server port.
    pub const SERVER_PORT: &str = "server.port";

    /// `error.type` attribute: the error class for a failed operation.
    pub const ERROR_TYPE: &str = "error.type";

    // -- Metric instrument names ---------------------------------------------

    /// `gen_ai.client.token.usage` metric: client-side token-usage histogram.
    pub const CLIENT_TOKEN_USAGE: &str = "gen_ai.client.token.usage";

    /// `gen_ai.server.request.duration` metric: server-side request duration.
    pub const SERVER_REQUEST_DURATION: &str = "gen_ai.server.request.duration";

    /// `gen_ai.server.time_per_output_token` metric: per-output-token latency.
    pub const TIME_PER_OUTPUT_TOKEN: &str = "gen_ai.server.time_per_output_token";

    // -- Standard `gen_ai.operation.name` values -----------------------------

    /// `gen_ai.operation.name` value: a chat completion.
    pub const OPERATION_CHAT: &str = "chat";

    /// `gen_ai.operation.name` value: a content-generation call.
    pub const OPERATION_GENERATE_CONTENT: &str = "generate_content";

    /// `gen_ai.operation.name` value: a text-completion call.
    pub const OPERATION_TEXT_COMPLETION: &str = "text_completion";

    /// `gen_ai.operation.name` value: an embeddings call.
    pub const OPERATION_EMBEDDINGS: &str = "embeddings";

    /// `gen_ai.operation.name` value: a tool execution.
    pub const OPERATION_EXECUTE_TOOL: &str = "execute_tool";

    /// `gen_ai.operation.name` value: agent creation.
    pub const OPERATION_CREATE_AGENT: &str = "create_agent";

    /// `gen_ai.operation.name` value: agent invocation.
    pub const OPERATION_INVOKE_AGENT: &str = "invoke_agent";

    // -- Standard `gen_ai.response.finish_reasons` values --------------------

    /// `gen_ai.response.finish_reasons` value: the model stopped naturally.
    pub const FINISH_STOP: &str = "stop";

    /// `gen_ai.response.finish_reasons` value: the model issued tool calls.
    pub const FINISH_TOOL_CALLS: &str = "tool_calls";

    // -- Event names (GenAI semantic-convention message/choice events) -------

    /// Event name for a system-role message turned into a span event.
    pub const EVENT_SYSTEM_MESSAGE: &str = "gen_ai.system.message";

    /// Event name for a user-role message turned into a span event.
    pub const EVENT_USER_MESSAGE: &str = "gen_ai.user.message";

    /// Event name for an assistant-role message turned into a span event.
    pub const EVENT_ASSISTANT_MESSAGE: &str = "gen_ai.assistant.message";

    /// Event name for a tool-result message turned into a span event.
    pub const EVENT_TOOL_MESSAGE: &str = "gen_ai.tool.message";

    /// Event name for a single generated choice.
    pub const EVENT_CHOICE: &str = "gen_ai.choice";

    // -- Event body fields ---------------------------------------------------

    /// `gen_ai.message.role` attribute on a message event: the message role.
    pub const MESSAGE_ROLE: &str = "gen_ai.message.role";

    /// `gen_ai.message.content` attribute on a message event: the message body.
    pub const MESSAGE_CONTENT: &str = "gen_ai.message.content";

    /// `gen_ai.choice.index` attribute on a choice event: the 0-based index.
    pub const CHOICE_INDEX: &str = "gen_ai.choice.index";
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

    #[test]
    fn newly_added_request_attributes_pin_convention_strings() {
        assert_eq!(genai::REQUEST_TEMPERATURE, "gen_ai.request.temperature");
        assert_eq!(genai::REQUEST_TOP_P, "gen_ai.request.top_p");
        assert_eq!(genai::REQUEST_TOP_K, "gen_ai.request.top_k");
        assert_eq!(genai::REQUEST_MAX_TOKENS, "gen_ai.request.max_tokens");
        assert_eq!(
            genai::REQUEST_FREQUENCY_PENALTY,
            "gen_ai.request.frequency_penalty"
        );
        assert_eq!(
            genai::REQUEST_PRESENCE_PENALTY,
            "gen_ai.request.presence_penalty"
        );
        assert_eq!(genai::REQUEST_STOP_SEQUENCES, "gen_ai.request.stop_sequences");
        assert_eq!(genai::REQUEST_SEED, "gen_ai.request.seed");
        assert_eq!(
            genai::REQUEST_ENCODING_FORMATS,
            "gen_ai.request.encoding_formats"
        );
        assert_eq!(genai::REQUEST_CHOICE_COUNT, "gen_ai.request.choice.count");
    }

    #[test]
    fn newly_added_response_and_usage_attributes_pin_convention_strings() {
        assert_eq!(genai::RESPONSE_ID, "gen_ai.response.id");
        assert_eq!(genai::RESPONSE_FINISH_REASONS, "gen_ai.response.finish_reasons");
        assert_eq!(
            genai::USAGE_CACHED_INPUT_TOKENS,
            "gen_ai.usage.cached_input_tokens"
        );
    }

    #[test]
    fn newly_added_agent_tool_conversation_attributes_pin_convention_strings() {
        assert_eq!(genai::AGENT_ID, "gen_ai.agent.id");
        assert_eq!(genai::AGENT_NAME, "gen_ai.agent.name");
        assert_eq!(genai::AGENT_DESCRIPTION, "gen_ai.agent.description");
        assert_eq!(genai::TOOL_CALL_ID, "gen_ai.tool.call.id");
        assert_eq!(genai::TOOL_DESCRIPTION, "gen_ai.tool.description");
        assert_eq!(genai::TOOL_TYPE, "gen_ai.tool.type");
        assert_eq!(genai::CONVERSATION_ID, "gen_ai.conversation.id");
        assert_eq!(genai::DATA_SOURCE_ID, "gen_ai.data_source.id");
    }

    #[test]
    fn newly_added_provider_and_metric_names_pin_convention_strings() {
        assert_eq!(genai::PROVIDER_NAME, "gen_ai.provider.name");
        assert_eq!(genai::SERVER_ADDRESS, "server.address");
        assert_eq!(genai::SERVER_PORT, "server.port");
        assert_eq!(genai::ERROR_TYPE, "error.type");
        assert_eq!(genai::CLIENT_TOKEN_USAGE, "gen_ai.client.token.usage");
        assert_eq!(
            genai::SERVER_REQUEST_DURATION,
            "gen_ai.server.request.duration"
        );
        assert_eq!(
            genai::TIME_PER_OUTPUT_TOKEN,
            "gen_ai.server.time_per_output_token"
        );
    }

    #[test]
    fn newly_added_operation_name_values_pin_convention_strings() {
        assert_eq!(genai::OPERATION_CHAT, "chat");
        assert_eq!(genai::OPERATION_GENERATE_CONTENT, "generate_content");
        assert_eq!(genai::OPERATION_TEXT_COMPLETION, "text_completion");
        assert_eq!(genai::OPERATION_EMBEDDINGS, "embeddings");
        assert_eq!(genai::OPERATION_EXECUTE_TOOL, "execute_tool");
        assert_eq!(genai::OPERATION_CREATE_AGENT, "create_agent");
        assert_eq!(genai::OPERATION_INVOKE_AGENT, "invoke_agent");
    }
}
