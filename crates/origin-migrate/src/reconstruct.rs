// SPDX-License-Identifier: Apache-2.0
//! Cross-harness *live-resume* transcript reconstruction.
//!
//! origin already imports external sessions idempotently (see [`crate::sink`]),
//! but importing only *stores* a session — it cannot *continue* one. Closing the
//! jcode-import-core gap "Cross-harness session import AND resume" means taking an
//! in-flight conversation from another harness (Claude Code, jcode, opencode) and
//! reconstructing it as origin's native message model so a fresh origin session can
//! be seeded with that history and the turn can simply continue.
//!
//! This module is the reconstruction *logic*. It deliberately does **not** wire the
//! daemon IPC (a follow-up); it reuses the existing [`Source`](crate::source::Source)
//! parsers — which already extract an ordered [`ImportedSession`] of role/body pairs —
//! and adapts each [`ImportedMessage`] into origin's real
//! [`Message`](origin_core::types::Message) / [`Block`](origin_core::types::Block) /
//! [`Role`](origin_core::types::Role) types. Alongside the messages it returns a
//! provider/model **remap suggestion**: the external session's model id mapped onto a
//! sensible origin-catalog model, so a resumed session can pick a provider.

use std::path::Path;

use origin_core::types::{Block, Message, Role};

use crate::claude_code::ClaudeCodeSource;
use crate::codex::CodexSource;
use crate::jcode::JcodeSource;
use crate::opencode::OpencodeSource;
use crate::source::{ImportedMessage, ImportedSession, Source, SourceError};

/// Origin catalog fallback used when an external model id cannot be mapped to a
/// more specific entry. Matches the daemon's default (`claude-sonnet-4-6`).
pub const DEFAULT_SUGGESTED_MODEL: &str = "claude-sonnet-4-6";

/// External-model-id -> origin-catalog-model remap table, ordered most-specific
/// first so e.g. `claude-3-opus` resolves to an opus entry before the generic
/// `claude` catch-all. Matching is case-insensitive substring containment (see
/// [`suggest_model`]) so versioned ids still resolve to the right family.
const MODEL_REMAP_TABLE: &[(&str, &str)] = &[
    ("opus", "claude-opus-4-6"),
    ("haiku", "claude-haiku-4-6"),
    ("sonnet", "claude-sonnet-4-6"),
    ("claude", "claude-sonnet-4-6"),
    ("gpt-5", "gpt-5-codex"),
    ("codex", "gpt-5-codex"),
    ("gpt-4o", "gpt-4o"),
    ("gpt-4", "gpt-4o"),
    ("o3", "gpt-5-codex"),
    ("o1", "gpt-5-codex"),
    ("gpt", "gpt-4o"),
    ("gemini", "gemini-2.5-pro"),
];

/// Which external harness a reconstructed session originated from.
///
/// Mirrors the `name()` of each [`Source`](crate::source::Source) implementation
/// so a resumed session can be attributed and re-exported to the same harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Anthropic Claude Code (`~/.claude`, JSONL transcripts).
    ClaudeCode,
    /// jcode (`sessions.sqlite`).
    Jcode,
    /// opencode (`storage/*.json`).
    Opencode,
    /// Codex CLI (`sessions/**/rollout-*.jsonl`).
    Codex,
}

impl SourceKind {
    /// Stable string tag, identical to the originating
    /// [`Source::name`](crate::source::Source::name).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Jcode => "jcode",
            Self::Opencode => "opencode",
            Self::Codex => "codex",
        }
    }

    /// Parse a harness tag back into a [`SourceKind`]. Accepts the canonical
    /// [`Self::as_str`] tags plus common aliases (`claude`, `cc`, `oc`, `cx`).
    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "cc" => Some(Self::ClaudeCode),
            "jcode" => Some(Self::Jcode),
            "opencode" | "oc" => Some(Self::Opencode),
            "codex" | "cx" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// A reconstructed in-flight conversation ready to seed a live origin session.
///
/// The `messages` preserve the external transcript's order and roles, expressed in
/// origin's native [`Message`] model; `suggested_model` is the origin-catalog model a
/// resumed session should pick (never empty — falls back to
/// [`DEFAULT_SUGGESTED_MODEL`]); `source_kind` and `original_id` retain provenance so
/// the resumed session can be attributed back to the originating harness.
#[derive(Debug, Clone)]
pub struct ResumedSession {
    /// Ordered transcript in origin's native message model.
    pub messages: Vec<Message>,
    /// Origin-catalog model id a resumed session should adopt.
    pub suggested_model: String,
    /// The harness this transcript came from.
    pub source_kind: SourceKind,
    /// The originating harness's session identifier (its `source_id`).
    pub original_id: String,
}

/// Map an external harness role string onto origin's [`Role`].
///
/// External harnesses are inconsistent (`"user"`, `"human"`, `"assistant"`,
/// `"model"`, `"tool"`, `"system"`); matching is case-insensitive and trims
/// surrounding whitespace. Anything unrecognized is treated as
/// [`Role::User`] — the safe default for resuming, since an unknown
/// speaker is more useful continued as user input than silently dropped.
#[must_use]
fn map_role(role: &str) -> Role {
    match role.trim().to_ascii_lowercase().as_str() {
        "assistant" | "model" | "ai" => Role::Assistant,
        "tool" | "tool_result" | "function" => Role::Tool,
        "system" | "developer" => Role::System,
        _ => Role::User,
    }
}

/// Adapt one [`ImportedMessage`] into an origin [`Message`].
///
/// The existing parsers already flatten each external message to a single text
/// `body`, so the reconstructed message carries exactly one [`Block::Text`]. Empty
/// bodies still yield a (empty-text) block so the turn boundary — and the role — is
/// preserved for the resumed conversation.
#[must_use]
fn adapt_message(m: &ImportedMessage) -> Message {
    Message::new(map_role(&m.role)).with_block(Block::text(m.body.clone()))
}

/// Suggest an origin-catalog model id for an external session's model id.
///
/// A small substring-matching table covers the model families origin's catalog
/// ships (Anthropic Claude, the GPT/o-series, Google Gemini). Matching is
/// case-insensitive and substring-based so versioned ids
/// (`claude-3-5-sonnet-20241022`, `gpt-4o-2024-08-06`) still resolve to the right
/// family. Anything unrecognized — including `None` — falls back to
/// [`DEFAULT_SUGGESTED_MODEL`]. The result is never empty.
#[must_use]
pub fn suggest_model(external_model_id: Option<&str>) -> String {
    let Some(raw) = external_model_id else {
        return DEFAULT_SUGGESTED_MODEL.to_string();
    };
    let id = raw.trim().to_ascii_lowercase();
    if id.is_empty() {
        return DEFAULT_SUGGESTED_MODEL.to_string();
    }

    for (needle, mapped) in MODEL_REMAP_TABLE {
        if id.contains(needle) {
            return (*mapped).to_string();
        }
    }
    DEFAULT_SUGGESTED_MODEL.to_string()
}

/// Reconstruct an already-parsed [`ImportedSession`] into a [`ResumedSession`].
///
/// This is the shared core: every per-source entry point ([`from_claude_code`],
/// [`from_jcode`], [`from_opencode`]) funnels through here so role/block adaptation
/// and ordering are identical across harnesses. The transcript order from the
/// importer is preserved verbatim.
///
/// `external_model_id` is the model the external session was running, when known
/// (the existing importer does not yet capture it, so callers that have it out of
/// band may pass it through); see [`suggest_model`] for the mapping.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn reconstruct_session(
    session: &ImportedSession,
    kind: SourceKind,
    external_model_id: Option<&str>,
) -> ResumedSession {
    ResumedSession {
        messages: session.messages.iter().map(adapt_message).collect(),
        suggested_model: suggest_model(external_model_id),
        source_kind: kind,
        original_id: session.source_id.clone(),
    }
}

/// Reconstruct a Claude Code session for live resume.
#[must_use]
pub fn from_claude_code(session: &ImportedSession, external_model_id: Option<&str>) -> ResumedSession {
    reconstruct_session(session, SourceKind::ClaudeCode, external_model_id)
}

/// Reconstruct a jcode session for live resume.
#[must_use]
pub fn from_jcode(session: &ImportedSession, external_model_id: Option<&str>) -> ResumedSession {
    reconstruct_session(session, SourceKind::Jcode, external_model_id)
}

/// Reconstruct an opencode session for live resume.
#[must_use]
pub fn from_opencode(session: &ImportedSession, external_model_id: Option<&str>) -> ResumedSession {
    reconstruct_session(session, SourceKind::Opencode, external_model_id)
}

/// Reconstruct a Codex session for live resume.
#[must_use]
pub fn from_codex(session: &ImportedSession, external_model_id: Option<&str>) -> ResumedSession {
    reconstruct_session(session, SourceKind::Codex, external_model_id)
}

/// Unified entry point: reconstruct a session given its declared [`SourceKind`].
///
/// Dispatches on `kind`; the per-source functions currently share one core, but
/// keeping a single typed dispatch point lets the daemon-IPC follow-up route a
/// detected/declared format without re-deriving the match.
#[must_use]
pub fn reconstruct(session: &ImportedSession, kind: SourceKind, external_model_id: Option<&str>) -> ResumedSession {
    reconstruct_session(session, kind, external_model_id)
}

impl SourceKind {
    /// Scan `root` with the matching [`Source`] adapter and return its
    /// importable bundle. Centralizes the `kind -> Source` dispatch so callers
    /// (the daemon `ResumeForeign` handler) do not re-derive the match.
    ///
    /// # Errors
    /// Propagates the [`SourceError`] raised by the underlying adapter when the
    /// directory is unreadable or its contents fail to parse.
    fn scan_root(self, root: &Path) -> Result<crate::source::MigrateBundle, SourceError> {
        match self {
            Self::ClaudeCode => ClaudeCodeSource.scan(root),
            Self::Jcode => JcodeSource.scan(root),
            Self::Opencode => OpencodeSource.scan(root),
            Self::Codex => CodexSource.scan(root),
        }
    }
}

/// Resolve the candidate scan roots to try for a user-supplied `path`.
///
/// The existing [`Source`] adapters expect a *harness root* whose subtree holds
/// the transcripts (`<root>/projects/*.jsonl` for Claude Code,
/// `<root>/sessions.sqlite` for jcode, `<root>/storage/*.json` for opencode).
/// A user resuming a *single* session, however, naturally points at the
/// transcript file itself (e.g. `~/.claude/projects/demo/abc.jsonl`). To accept
/// both forms without duplicating any parse logic, this returns the path itself
/// (when it is a directory) followed by up to three ancestor directories — so a
/// `<root>/projects/<proj>/<id>.jsonl` file (three levels below its harness root)
/// still resolves. Order is most-specific first; the first root that yields a
/// session wins.
fn candidate_roots(path: &Path) -> Vec<&Path> {
    let mut roots: Vec<&Path> = Vec::new();
    if path.is_dir() {
        roots.push(path);
    }
    // Walk up to three ancestors so a `<root>/projects/<proj>/<id>.jsonl` (Claude
    // Code) or `<root>/storage/<id>.json` (opencode) file resolves back to its
    // harness root.
    let mut cur = path.parent();
    for _ in 0..3 {
        let Some(p) = cur else { break };
        if !roots.contains(&p) {
            roots.push(p);
        }
        cur = p.parent();
    }
    roots
}

/// Reconstruct a single foreign session for live resume directly from a
/// filesystem `path`, validating the input before any read.
///
/// `path` may be either the originating harness's *root directory* or a single
/// transcript *file* inside it (see [`candidate_roots`] for the resolution
/// order). The matching [`Source`] adapter is run over each candidate root until
/// one yields at least one session; the first session of the first non-empty
/// scan is reconstructed via [`reconstruct`]. `external_model_id` is threaded
/// through to [`suggest_model`] for the provider/model remap.
///
/// This is the shared entry point the daemon's `ResumeForeign` IPC handler calls
/// so the reconstruct + persist path lives behind one typed, fallible function
/// rather than re-deriving the scan/dispatch in the daemon.
///
/// # Errors
/// Returns [`SourceError::NotFound`] when `path` does not exist or no candidate
/// root yields a session, and propagates any [`SourceError`] raised while
/// scanning (unreadable directory, malformed transcript).
// Mirrors the `reconstruct`/`reconstruct_session` naming already established in
// this module; the `reconstruct_` prefix names the operation, not the module.
#[allow(clippy::module_name_repetitions)]
pub fn reconstruct_from_path(
    kind: SourceKind,
    path: &Path,
    external_model_id: Option<&str>,
) -> Result<ResumedSession, SourceError> {
    if !path.exists() {
        return Err(SourceError::NotFound(path.display().to_string()));
    }

    let mut last_err: Option<SourceError> = None;
    for root in candidate_roots(path) {
        match kind.scan_root(root) {
            Ok(bundle) => {
                if let Some(session) = bundle.sessions.into_iter().next() {
                    return Ok(reconstruct(&session, kind, external_model_id));
                }
            }
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| {
        SourceError::NotFound(format!("no {} session found under {}", kind.as_str(), path.display()))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::ImportedMessage;

    fn msg(role: &str, body: &str) -> ImportedMessage {
        ImportedMessage {
            role: role.to_string(),
            body: body.to_string(),
        }
    }

    fn text_of(m: &Message) -> &str {
        match m.blocks.first() {
            Some(Block::Text { text, .. }) => text.as_str(),
            _ => "",
        }
    }

    /// Claude Code transcripts use `type: "human"|"assistant"` (see
    /// [`crate::claude_code`] `CcLine`). Reconstruct must map those onto
    /// origin roles in order and carry the text through.
    #[test]
    fn claude_code_roundtrip_orders_roles_and_text() {
        let session = ImportedSession {
            source_id: "projects/demo/abc.jsonl".to_string(),
            title: None,
            created_at_unix_ms: 0,
            messages: vec![
                msg("human", "fix the build"),
                msg("assistant", "patching Cargo.toml"),
                msg("human", "now run tests"),
            ],
        };

        let resumed = from_claude_code(&session, Some("claude-3-5-sonnet-20241022"));

        assert_eq!(resumed.source_kind, SourceKind::ClaudeCode);
        assert_eq!(resumed.original_id, "projects/demo/abc.jsonl");
        assert_eq!(resumed.messages.len(), 3);
        assert_eq!(resumed.messages[0].role, Role::User);
        assert_eq!(text_of(&resumed.messages[0]), "fix the build");
        assert_eq!(resumed.messages[1].role, Role::Assistant);
        assert_eq!(text_of(&resumed.messages[1]), "patching Cargo.toml");
        assert_eq!(resumed.messages[2].role, Role::User);
        assert_eq!(text_of(&resumed.messages[2]), "now run tests");
        assert_eq!(resumed.suggested_model, "claude-sonnet-4-6");
        assert!(!resumed.suggested_model.is_empty());
    }

    /// jcode stores `role` strings verbatim from `messages.role` (see
    /// [`crate::jcode`]). Verify ordering plus tool/system role mapping.
    #[test]
    fn jcode_roundtrip_maps_tool_and_system_roles() {
        let session = ImportedSession {
            source_id: "sess_01HZX".to_string(),
            title: Some("debug run".to_string()),
            created_at_unix_ms: 1_700_000_000_000,
            messages: vec![
                msg("system", "you are origin"),
                msg("user", "list files"),
                msg("assistant", "running ls"),
                msg("tool", "Cargo.toml\nsrc/"),
            ],
        };

        let resumed = from_jcode(&session, Some("gpt-4o-2024-08-06"));

        assert_eq!(resumed.source_kind, SourceKind::Jcode);
        assert_eq!(resumed.original_id, "sess_01HZX");
        let roles: Vec<Role> = resumed.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::System, Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(text_of(&resumed.messages[3]), "Cargo.toml\nsrc/");
        assert_eq!(resumed.suggested_model, "gpt-4o");
        assert!(!resumed.suggested_model.is_empty());
    }

    /// opencode flattens `parts[].text` into one body and uses `role`
    /// (see [`crate::opencode`]). Use `"model"` to exercise the assistant alias.
    #[test]
    fn opencode_roundtrip_orders_and_falls_back_model() {
        let session = ImportedSession {
            source_id: "ses_xyz".to_string(),
            title: None,
            created_at_unix_ms: 1,
            messages: vec![
                msg("user", "explain this repo"),
                msg("model", "it is a Rust harness"),
            ],
        };

        // No external model id known -> fallback, but never empty.
        let resumed = from_opencode(&session, None);

        assert_eq!(resumed.source_kind, SourceKind::Opencode);
        assert_eq!(resumed.original_id, "ses_xyz");
        assert_eq!(resumed.messages.len(), 2);
        assert_eq!(resumed.messages[0].role, Role::User);
        assert_eq!(resumed.messages[1].role, Role::Assistant); // "model" alias
        assert_eq!(text_of(&resumed.messages[1]), "it is a Rust harness");
        assert_eq!(resumed.suggested_model, DEFAULT_SUGGESTED_MODEL);
        assert!(!resumed.suggested_model.is_empty());
    }

    #[test]
    fn unified_dispatch_matches_per_source_helpers() {
        let session = ImportedSession {
            source_id: "id1".to_string(),
            title: None,
            created_at_unix_ms: 0,
            messages: vec![msg("user", "hi")],
        };
        let via_dispatch = reconstruct(&session, SourceKind::Opencode, Some("gemini-1.5-pro"));
        let via_helper = from_opencode(&session, Some("gemini-1.5-pro"));
        assert_eq!(via_dispatch.suggested_model, via_helper.suggested_model);
        assert_eq!(via_dispatch.source_kind, via_helper.source_kind);
        assert_eq!(via_dispatch.suggested_model, "gemini-2.5-pro");
    }

    #[test]
    fn suggest_model_table_and_fallback() {
        assert_eq!(suggest_model(Some("claude-3-opus-20240229")), "claude-opus-4-6");
        assert_eq!(suggest_model(Some("claude-3-5-haiku-latest")), "claude-haiku-4-6");
        assert_eq!(suggest_model(Some("gpt-5-codex")), "gpt-5-codex");
        assert_eq!(suggest_model(Some("o3-mini")), "gpt-5-codex");
        assert_eq!(suggest_model(Some("totally-unknown-model")), DEFAULT_SUGGESTED_MODEL);
        assert_eq!(suggest_model(None), DEFAULT_SUGGESTED_MODEL);
        assert_eq!(suggest_model(Some("   ")), DEFAULT_SUGGESTED_MODEL);
        assert!(!suggest_model(None).is_empty());
    }

    #[test]
    fn empty_body_preserves_turn_and_role() {
        let session = ImportedSession {
            source_id: "id".to_string(),
            title: None,
            created_at_unix_ms: 0,
            messages: vec![msg("assistant", "")],
        };
        let resumed = from_claude_code(&session, None);
        assert_eq!(resumed.messages.len(), 1);
        assert_eq!(resumed.messages[0].role, Role::Assistant);
        assert_eq!(text_of(&resumed.messages[0]), "");
    }

    #[test]
    fn source_kind_tag_roundtrip() {
        for k in [
            SourceKind::ClaudeCode,
            SourceKind::Jcode,
            SourceKind::Opencode,
            SourceKind::Codex,
        ] {
            assert_eq!(SourceKind::from_tag(k.as_str()), Some(k));
        }
        assert_eq!(SourceKind::from_tag("CLAUDE"), Some(SourceKind::ClaudeCode));
        assert_eq!(SourceKind::from_tag("oc"), Some(SourceKind::Opencode));
        assert_eq!(SourceKind::from_tag("CODEX"), Some(SourceKind::Codex));
        assert_eq!(SourceKind::from_tag("cx"), Some(SourceKind::Codex));
        assert_eq!(SourceKind::from_tag("nope"), None);
    }

    /// Codex transcripts flatten role/body pairs just like the other adapters
    /// (see [`crate::codex`]); verify ordering, tool-role mapping, and that the
    /// `gpt-5-codex` model id remaps to the codex catalog entry.
    #[test]
    fn codex_roundtrip_orders_roles_and_suggests_codex_model() {
        let session = ImportedSession {
            source_id: "sessions/2026/rollout-test.jsonl".to_string(),
            title: None,
            created_at_unix_ms: 1_700_000_000_000,
            messages: vec![
                msg("user", "refactor the parser"),
                msg("assistant", "splitting the lexer out"),
                msg("tool", "src/lexer.rs\nsrc/parser.rs"),
            ],
        };

        let resumed = from_codex(&session, Some("gpt-5-codex"));

        assert_eq!(resumed.source_kind, SourceKind::Codex);
        assert_eq!(resumed.original_id, "sessions/2026/rollout-test.jsonl");
        let roles: Vec<Role> = resumed.messages.iter().map(|m| m.role).collect();
        assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
        assert_eq!(text_of(&resumed.messages[1]), "splitting the lexer out");
        assert_eq!(resumed.suggested_model, "gpt-5-codex");
        assert!(!resumed.suggested_model.is_empty());
    }

    /// A non-existent path must fail before any read, with `NotFound`.
    #[test]
    fn reconstruct_from_path_missing_is_not_found() {
        let err = reconstruct_from_path(
            SourceKind::ClaudeCode,
            std::path::Path::new("/no/such/origin/transcript.jsonl"),
            None,
        )
        .expect_err("missing path must error");
        assert!(matches!(err, crate::source::SourceError::NotFound(_)), "got {err:?}");
    }

    /// Resolve a Claude Code transcript when the user points at the harness
    /// *root* directory (the `scan`-native form).
    #[test]
    fn reconstruct_from_path_claude_code_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let proj = dir.path().join("projects").join("demo");
        std::fs::create_dir_all(&proj).expect("mkdir projects/demo");
        let jsonl = "{\"type\":\"human\",\"content\":\"fix the build\"}\n\
                     {\"type\":\"assistant\",\"content\":\"patching Cargo.toml\"}\n";
        std::fs::write(proj.join("abc.jsonl"), jsonl).expect("write transcript");

        let resumed = reconstruct_from_path(SourceKind::ClaudeCode, dir.path(), Some("claude-3-opus"))
            .expect("reconstruct from root");
        assert_eq!(resumed.source_kind, SourceKind::ClaudeCode);
        assert_eq!(resumed.messages.len(), 2);
        assert_eq!(resumed.messages[0].role, Role::User);
        assert_eq!(text_of(&resumed.messages[0]), "fix the build");
        assert_eq!(resumed.messages[1].role, Role::Assistant);
        assert_eq!(resumed.suggested_model, "claude-opus-4-6");
    }

    /// Resolve the same transcript when the user points at the transcript
    /// *file* itself — the helper walks up to the harness root via
    /// [`candidate_roots`].
    #[test]
    fn reconstruct_from_path_claude_code_file_walks_to_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let proj = dir.path().join("projects").join("demo");
        std::fs::create_dir_all(&proj).expect("mkdir projects/demo");
        let file = proj.join("abc.jsonl");
        std::fs::write(&file, "{\"type\":\"human\",\"content\":\"hi\"}\n").expect("write transcript");

        let resumed =
            reconstruct_from_path(SourceKind::ClaudeCode, &file, None).expect("reconstruct from file");
        assert_eq!(resumed.messages.len(), 1);
        assert_eq!(text_of(&resumed.messages[0]), "hi");
        assert_eq!(resumed.suggested_model, DEFAULT_SUGGESTED_MODEL);
    }
}
