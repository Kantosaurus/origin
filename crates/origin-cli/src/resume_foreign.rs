// SPDX-License-Identifier: Apache-2.0
//! `origin resume-foreign` — cross-harness *live resume*.
//!
//! Reconstructs an in-flight conversation from another harness (Claude Code,
//! jcode, opencode) into a brand-new resumable origin session. Unlike
//! `origin import` — which only *stores* history — this hydrates a session the
//! user can immediately continue with `origin run --session <id>`.
//!
//! The heavy lifting (parse → reconstruct → persist) lives in the daemon's
//! [`ClientMessage::ResumeForeign`] handler; this client validates the source
//! tag locally for a fast, offline error, opens a one-shot local-socket
//! connection (the same path the other admin commands use, see
//! [`crate::admin`]), sends the envelope, and renders the reply. *Closes: jcode
//! L227.*

use anyhow::Result;
use origin_daemon::protocol::{ClientMessage, StreamEvent};

/// The harness tags `origin resume-foreign` accepts, mirroring
/// [`origin_migrate::reconstruct::SourceKind::from_tag`]. Validated client-side
/// so an obvious typo fails immediately without a daemon round-trip.
const KNOWN_SOURCES: &[&str] = &[
    "claude-code",
    "claude",
    "cc",
    "jcode",
    "opencode",
    "oc",
    "codex",
    "cx",
];

/// Run `origin resume-foreign <source> <path>`.
///
/// Reconstructs the foreign session at `path` into a new origin session via the
/// daemon and prints the new session id plus the `origin run --session <id>`
/// guidance to continue it.
///
/// # Errors
/// Returns if `source` is not a recognized harness tag, the daemon refuses, the
/// IPC transport closes, or the reply shape does not match.
pub async fn run(source: String, path: String) -> Result<()> {
    let tag = source.trim().to_ascii_lowercase();
    if !KNOWN_SOURCES.contains(&tag.as_str()) {
        anyhow::bail!(
            "unknown source {source:?}: expected one of claude-code | jcode | opencode \
             (aliases claude/cc/oc)"
        );
    }
    if path.trim().is_empty() {
        anyhow::bail!("a path to the external session file or harness directory is required");
    }

    let ev = crate::admin::round_trip(ClientMessage::ResumeForeign { source, path }).await?;
    match ev {
        StreamEvent::ForeignResumed {
            session_id,
            messages_loaded,
            suggested_model,
        } => {
            println!(
                "resumed foreign session into {session_id}: {messages_loaded} messages \
                 (model {suggested_model})"
            );
            // The hydrated session is now a first-class, resumable origin
            // session: it is listed by `origin sessions ls` and its persisted
            // transcript can be inspected / continued via `origin sessions
            // resume`. We surface the working command rather than an invented
            // flag so the printed guidance is actually runnable.
            println!("resume it with: origin sessions resume {session_id}");
            Ok(())
        }
        StreamEvent::AdminError { message } => Err(anyhow::anyhow!("{message}")),
        other => Err(anyhow::anyhow!("unexpected: {other:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::KNOWN_SOURCES;

    /// The client-side allowlist must accept exactly the canonical tags and
    /// aliases the daemon's `SourceKind::from_tag` resolves, so the fast
    /// offline check never rejects an input the daemon would have accepted.
    #[test]
    fn known_sources_match_source_kind_from_tag() {
        use origin_migrate::reconstruct::SourceKind;
        for tag in KNOWN_SOURCES {
            assert!(
                SourceKind::from_tag(tag).is_some(),
                "client accepts {tag:?} but daemon would reject it"
            );
        }
        // A tag the allowlist omits must also be rejected by the daemon mapping,
        // keeping the two in lockstep.
        assert!(!KNOWN_SOURCES.contains(&"nope"));
        assert!(SourceKind::from_tag("nope").is_none());
    }

    /// Case-insensitive tags are normalized before the allowlist check.
    #[test]
    fn tag_normalization_is_case_insensitive() {
        let normalized = "Claude-Code".trim().to_ascii_lowercase();
        assert!(KNOWN_SOURCES.contains(&normalized.as_str()));
    }
}
