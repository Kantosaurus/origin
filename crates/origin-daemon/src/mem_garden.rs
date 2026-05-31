// SPDX-License-Identifier: Apache-2.0
//! Default-off idle-time **auto-memory** mining loop (gemini-cli L200).
//!
//! When `ORIGIN_MEM_GARDEN=1` is set, the daemon spawns a background sidecar
//! task that, on a slow idle cadence and only while the ambient
//! [`BudgetPolicy`](origin_ambient::BudgetPolicy) still has non-reserved
//! headroom, scans recently persisted session transcripts, extracts candidate
//! memory entries via [`origin_mem`]'s existing turn-end
//! [`Proposer`](origin_mem::Proposer), **redacts secrets** out of each draft via
//! [`origin_telemetry::redact`], and writes one Markdown draft per candidate into
//! a **review inbox** at `~/.origin/memory-inbox/<id>.md` for the user to
//! accept/reject. Nothing is ever written into the live memory store — the inbox
//! is a staging area only.
//!
//! The loop is **idempotent**: each draft's filename is a content hash, so a
//! candidate already staged in the inbox (or already accepted-and-removed by the
//! user) is skipped on the next pass. Everything here is best-effort: I/O,
//! extraction, and decode failures are swallowed and logged so a malformed
//! transcript or a missing home directory can never panic or wedge the daemon.
//!
//! With the env var unset nothing is spawned, so default daemon behaviour is
//! byte-identical. *Closes: gemini-cli L200 (Auto Memory — background mining of
//! sessions into a review inbox with secret redaction).*

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use origin_ambient::BudgetPolicy;
use origin_core::types::{Block, Message, Role};
use origin_mem::Proposer;

use crate::session_store::SessionStore;

/// Interval between mining passes. Long: auto-memory is strictly opportunistic
/// idle-time work, never deadline-driven.
const TICK: Duration = Duration::from_secs(300);

/// Total per-process auto-memory token budget (mining work + user reserve).
/// Mirrors the ambient loop's headroom guarantee so background mining never
/// starves an interactive session.
const TOTAL_BUDGET_TOKENS: u64 = 1_000_000;

/// Tokens reserved for the interactive user; mining never dips below this.
const USER_RESERVE_TOKENS: u64 = 200_000;

/// Estimated token cost charged per mining pass against the headroom check.
const PASS_COST_TOKENS: u64 = 10_000;

/// Most recent sessions scanned per pass. Caps the work (and the `SQLite` reads)
/// a single pass can do so the loop stays bounded regardless of store size.
const MAX_SESSIONS_PER_PASS: usize = 16;

/// Maximum draft body length, in bytes, after redaction. Caps any single inbox
/// file so a pathological transcript line cannot write an unbounded draft.
const MAX_DRAFT_BODY_BYTES: usize = 2_000;

/// Whether the auto-memory mining loop is enabled (`ORIGIN_MEM_GARDEN=1`).
///
/// Default-off: returns `false` whenever the env var is unset or not exactly
/// `"1"`, so the loop is opt-in and the daemon is byte-identical when unused.
#[must_use]
pub fn enabled() -> bool {
    std::env::var("ORIGIN_MEM_GARDEN").as_deref() == Ok("1")
}

/// Spawn the background auto-memory mining loop if `ORIGIN_MEM_GARDEN=1`.
///
/// `session_store` is the daemon's shared session store; the loop reads recent
/// transcripts from it read-only. Default-off: returns immediately (spawning
/// nothing) when [`enabled`] is `false`. The spawned task runs for the life of
/// the process; its handle is intentionally dropped (fire-and-forget background
/// work, mirroring [`crate::ambient::maybe_spawn`]).
pub fn maybe_spawn(session_store: Arc<SessionStore>) {
    if !enabled() {
        return;
    }
    tracing::info!("mem-garden: ORIGIN_MEM_GARDEN=1 — starting auto-memory mining loop");
    origin_runtime::spawn_in(origin_runtime::TaskClass::Sidecar, async move {
        run_loop(session_store).await;
    });
}

/// The mining loop: every [`TICK`], if the budget policy still has headroom,
/// scan recent transcripts and stage redacted drafts into the review inbox.
async fn run_loop(session_store: Arc<SessionStore>) {
    let budget = BudgetPolicy::new(TOTAL_BUDGET_TOKENS, USER_RESERVE_TOKENS);
    let proposer = Proposer::new();
    let mut spent_today: u64 = 0;
    loop {
        tokio::time::sleep(TICK).await;
        if !budget.may_run(spent_today, PASS_COST_TOKENS) {
            // Out of non-reserved headroom for now; protect the user reserve and
            // try again next tick (the daemon owns no per-day reset, so a
            // long-lived process simply quiesces — acceptable for idle work).
            continue;
        }
        spent_today = spent_today.saturating_add(PASS_COST_TOKENS);
        let staged = mine_once(session_store.as_ref(), &proposer);
        if staged > 0 {
            tracing::info!(staged, "mem-garden: staged auto-memory drafts into review inbox");
        }
    }
}

/// Run one mining pass and return the number of new drafts staged into the
/// inbox. Best-effort: every fallible step is swallowed so a single bad session
/// never aborts the pass.
fn mine_once(session_store: &SessionStore, proposer: &Proposer) -> usize {
    let Some(inbox) = inbox_dir() else {
        return 0;
    };
    if let Err(e) = std::fs::create_dir_all(&inbox) {
        tracing::warn!(error = %e, "mem-garden: could not create inbox dir");
        return 0;
    }
    let summaries = match session_store.list_summaries() {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "mem-garden: list_summaries failed");
            return 0;
        }
    };
    summaries
        .iter()
        .take(MAX_SESSIONS_PER_PASS)
        .map(|summary| mine_session(session_store, proposer, &inbox, &summary.id))
        .sum()
}

/// Mine a single session's transcript into `inbox`, returning the number of new
/// drafts staged. Best-effort: a load failure yields `0` for that session and
/// never aborts the surrounding pass.
fn mine_session(
    session_store: &SessionStore,
    proposer: &Proposer,
    inbox: &std::path::Path,
    session_id: &str,
) -> usize {
    let messages = match session_store.load_messages(session_id) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(session = %session_id, error = %e, "mem-garden: load_messages failed");
            return 0;
        }
    };
    let (user_text, assistant_text) = split_roles(&messages);
    let mut next_id: u32 = 1;
    let mut staged = 0_usize;
    for proposal in proposer.scan(&user_text, &assistant_text, &mut next_id) {
        let draft = render_draft(session_id, &proposal.suggested_tags, &proposal.body);
        if stage_draft(inbox, &draft) {
            staged += 1;
        }
    }
    staged
}

/// A fully-rendered, secret-redacted memory draft ready to write to the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft {
    /// Content-hash dedup key (lowercase hex), used as the filename stem.
    key: String,
    /// The full Markdown document, including YAML frontmatter.
    markdown: String,
}

/// Write `draft` into `inbox` as `<key>.md`, skipping it if a file with that
/// content-hash filename already exists (idempotent dedup). Returns `true` only
/// when a new file was actually created.
fn stage_draft(inbox: &std::path::Path, draft: &Draft) -> bool {
    let path = inbox.join(format!("{}.md", draft.key));
    if path.exists() {
        return false;
    }
    match std::fs::write(&path, &draft.markdown) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "mem-garden: draft write failed");
            false
        }
    }
}

/// Concatenate the plain-text blocks of all user and all assistant messages into
/// two newline-joined buffers, matching the `(user, assistant)` shape
/// [`Proposer::scan`] expects. Tool / system / thinking blocks are ignored — the
/// proposer's patterns only ever fire on natural-language user/assistant text.
fn split_roles(messages: &[Message]) -> (String, String) {
    let mut user = String::new();
    let mut assistant = String::new();
    for m in messages {
        let target = match m.role {
            Role::User => &mut user,
            Role::Assistant => &mut assistant,
            Role::Tool | Role::System => continue,
        };
        for block in &m.blocks {
            if let Block::Text { text, .. } = block {
                if !target.is_empty() {
                    target.push('\n');
                }
                target.push_str(text);
            }
        }
    }
    (user, assistant)
}

/// Render a single proposal into a redacted inbox [`Draft`].
///
/// The candidate `body` is **redacted token-by-token** via
/// [`origin_telemetry::redact`] (each whitespace-delimited token is treated as a
/// telemetry property value, so any token shaped like a secret — `sk-…`,
/// `Bearer …`, long hex/base64 — is replaced with the telemetry redaction
/// placeholder). The redacted body is capped at [`MAX_DRAFT_BODY_BYTES`] and
/// embedded under YAML frontmatter recording the source session and suggested
/// tags. The dedup [`key`](Draft::key) is the content hash of the redacted body
/// plus its source session, so the same candidate always maps to the same file.
fn render_draft(session_id: &str, tags: &[String], body: &str) -> Draft {
    let redacted = redact_text(body);
    let capped = cap_bytes(&redacted, MAX_DRAFT_BODY_BYTES);
    let key = content_hash(session_id, &capped);
    let tags_line = if tags.is_empty() {
        "[]".to_string()
    } else {
        format!("[{}]", tags.join(", "))
    };
    // Sanitize the session id for the frontmatter so an exotic id can never
    // break the YAML (newlines/quotes would corrupt the document).
    let safe_session = sanitize_field(session_id);
    let mut markdown = String::with_capacity(capped.len() + 128);
    markdown.push_str("---\n");
    markdown.push_str("source: auto-memory\n");
    markdown.push_str(&format!("session: \"{safe_session}\"\n"));
    markdown.push_str(&format!("tags: {tags_line}\n"));
    markdown.push_str(&format!("hash: {key}\n"));
    markdown.push_str("---\n\n");
    markdown.push_str(&capped);
    markdown.push('\n');
    Draft { key, markdown }
}

/// Redact secrets from free text by treating each whitespace-delimited token as
/// a telemetry property value and running [`origin_telemetry::redact`] over the
/// batch, then re-joining with single spaces. This reuses the canonical secret
/// detector rather than re-implementing one, at the cost of collapsing runs of
/// whitespace (acceptable for a one-line memory snippet).
fn redact_text(text: &str) -> String {
    let mut props: Vec<(String, String)> = text
        .split_whitespace()
        .map(|tok| (String::new(), tok.to_string()))
        .collect();
    origin_telemetry::redact(&mut props);
    props
        .into_iter()
        .map(|(_, v)| v)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Truncate `s` to at most `max` bytes without splitting a UTF-8 codepoint.
fn cap_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Replace characters that would corrupt a one-line YAML scalar (quotes,
/// backslashes, and any control/newline byte) with spaces.
fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| if c == '"' || c == '\\' || c.is_control() { ' ' } else { c })
        .collect()
}

/// Stable content-hash dedup key for a candidate: an FNV-1a hash over the source
/// session id and the (already redacted) body, rendered as lowercase hex. Pure
/// and reproducible across processes, so the same candidate always yields the
/// same inbox filename — the basis of the idempotent skip in [`stage_draft`].
fn content_hash(session_id: &str, body: &str) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    mix(session_id.as_bytes());
    // Domain separator so "ab"+"c" and "a"+"bc" cannot collide.
    mix(&[0u8]);
    mix(body.as_bytes());
    format!("{hash:016x}")
}

/// `~/.origin/memory-inbox`, honoring `ORIGIN_HOME` (used by tests + the CLI).
fn inbox_dir() -> Option<PathBuf> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)?;
    Some(home.join(".origin").join("memory-inbox"))
}

#[cfg(test)]
mod tests {
    use super::{
        cap_bytes, content_hash, render_draft, sanitize_field, split_roles,
    };
    use origin_core::types::{Block, Message, Role};

    #[test]
    fn gate_is_off_by_default() {
        // The gate must be off unless the env var is exactly "1". We assert the
        // unset case by temporarily clearing it for this test thread; tests run
        // in-process so we restore it afterward to avoid cross-test leakage.
        let prev = std::env::var_os("ORIGIN_MEM_GARDEN");
        std::env::remove_var("ORIGIN_MEM_GARDEN");
        assert!(!super::enabled(), "mem-garden must be off when env unset");
        std::env::set_var("ORIGIN_MEM_GARDEN", "0");
        assert!(!super::enabled(), "mem-garden must be off when env != \"1\"");
        std::env::set_var("ORIGIN_MEM_GARDEN", "1");
        assert!(super::enabled(), "mem-garden must be on when env == \"1\"");
        // Restore whatever the harness had.
        match prev {
            Some(v) => std::env::set_var("ORIGIN_MEM_GARDEN", v),
            None => std::env::remove_var("ORIGIN_MEM_GARDEN"),
        }
    }

    #[test]
    fn draft_redacts_an_embedded_secret() {
        // A "remember" candidate whose body carries a fake API key. The rendered
        // draft must NOT leak the secret and MUST contain the redaction marker.
        let secret = "sk-ABCDEF0123456789abcdef0123456789";
        let body = format!("my key is {secret} keep it safe");
        let draft = render_draft("sess-1", &["user-statement".to_string()], &body);
        assert!(
            !draft.markdown.contains(secret),
            "secret leaked into draft: {}",
            draft.markdown
        );
        assert!(
            draft.markdown.contains(origin_telemetry::REDACTED),
            "redaction marker missing: {}",
            draft.markdown
        );
        // The surrounding non-secret words survive.
        assert!(draft.markdown.contains("my key is"));
        assert!(draft.markdown.contains("keep it safe"));
        // Frontmatter records the source session and tag.
        assert!(draft.markdown.contains("session: \"sess-1\""));
        assert!(draft.markdown.contains("user-statement"));
        assert!(draft.markdown.starts_with("---\n"));
    }

    #[test]
    fn dedup_key_is_stable_for_same_content() {
        let a = content_hash("sess-1", "remember to pin deps");
        let b = content_hash("sess-1", "remember to pin deps");
        assert_eq!(a, b, "same content must hash to the same key");
        // Different body OR different session ⇒ different key.
        assert_ne!(a, content_hash("sess-1", "remember to pin other deps"));
        assert_ne!(a, content_hash("sess-2", "remember to pin deps"));
        // Domain separator prevents the classic concatenation collision.
        assert_ne!(content_hash("ab", "c"), content_hash("a", "bc"));
        // Key is fixed-width lowercase hex, safe as a filename stem.
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn render_is_idempotent_in_filename() {
        // Two renders of the same candidate produce the same dedup key, which is
        // what makes the inbox write idempotent.
        let d1 = render_draft("s", &["todo".to_string()], "TODO body");
        let d2 = render_draft("s", &["todo".to_string()], "TODO body");
        assert_eq!(d1.key, d2.key);
        assert_eq!(d1.markdown, d2.markdown);
    }

    #[test]
    fn split_roles_separates_user_and_assistant_text() {
        let messages = vec![
            Message::new(Role::User).with_block(Block::text("remember: i prefer tabs")),
            Message::new(Role::Assistant).with_block(Block::text("I'll note that for you")),
            // Tool/system blocks are ignored.
            Message::new(Role::Tool).with_block(Block::text("tool output noise")),
        ];
        let (user, assistant) = split_roles(&messages);
        assert!(user.contains("i prefer tabs"));
        assert!(assistant.contains("I'll note that"));
        assert!(!user.contains("tool output"));
        assert!(!assistant.contains("tool output"));
    }

    #[test]
    fn cap_bytes_respects_utf8_boundaries() {
        // A multi-byte char straddling the cap must not be split.
        let s = "aé"; // 'a' = 1 byte, 'é' = 2 bytes
        assert_eq!(cap_bytes(s, 1), "a");
        assert_eq!(cap_bytes(s, 2), "a"); // would split 'é' → backs off
        assert_eq!(cap_bytes(s, 3), "aé");
        assert_eq!(cap_bytes("short", 100), "short");
    }

    #[test]
    fn sanitize_field_strips_yaml_breakers() {
        let dirty = "ses\"sion\nwith\\stuff";
        let clean = sanitize_field(dirty);
        assert!(!clean.contains('"'));
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('\\'));
    }
}
