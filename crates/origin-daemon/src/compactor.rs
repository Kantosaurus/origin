// SPDX-License-Identifier: Apache-2.0
//! Summary-backed compaction (P5.4).

use origin_core::types::{Block, Message};

pub const DEFAULT_SOFT_CAP_BYTES: usize = 200 * 1024;
pub const COMPACT_OLDEST_N_TURNS: usize = 4;

/// Estimate the byte footprint of an in-flight transcript.
///
/// This is the cheap, allocation-free heuristic the live agent loop uses to
/// decide whether the accumulated context has crossed the compaction soft cap.
/// It sums the textual/structural payload of every block (text bodies, tool
/// `input_json`, inline tool-result bytes, thinking tokens) plus a small,
/// fixed per-block overhead so empty marker blocks still count. It deliberately
/// ignores wire framing and prompt-cache markers — it only needs to be
/// monotonic in transcript growth, not an exact token count.
#[must_use]
pub fn estimate_transcript_bytes(transcript: &[Message]) -> usize {
    /// Fixed overhead charged per block so structural growth is never free.
    const PER_BLOCK_OVERHEAD: usize = 16;
    let mut total: usize = 0;
    for msg in transcript {
        for block in &msg.blocks {
            total = total.saturating_add(PER_BLOCK_OVERHEAD);
            let block_bytes = match block {
                Block::Text { text, .. } => text.len(),
                Block::ToolUse {
                    id, name, input_json, ..
                } => id
                    .len()
                    .saturating_add(name.len())
                    .saturating_add(input_json.len()),
                Block::ToolResult {
                    tool_use_id, inline, ..
                } => tool_use_id
                    .len()
                    .saturating_add(inline.as_ref().map_or(0, Vec::len)),
                Block::Thinking { tokens, signature } => tokens
                    .len()
                    .saturating_add(signature.as_ref().map_or(0, String::len)),
            };
            total = total.saturating_add(block_bytes);
        }
    }
    total
}

pub struct CompactionInput<'a> {
    pub transcript: &'a [Message],
    pub summaries: &'a [Option<String>],
    pub current_bytes: usize,
    pub soft_cap_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutput {
    pub transcript: Vec<Message>,
    pub compacted_indices: Vec<usize>,
}

/// Compact the oldest turns whose summaries are available, until either
/// `COMPACT_OLDEST_N_TURNS` replacements have been made or no more
/// summarizable turns remain.
///
/// Pass-through when `current_bytes <= soft_cap_bytes`.
#[must_use]
pub fn compact(input: &CompactionInput<'_>) -> CompactionOutput {
    if input.current_bytes <= input.soft_cap_bytes {
        return CompactionOutput {
            transcript: input.transcript.to_vec(),
            compacted_indices: Vec::new(),
        };
    }
    let mut out = input.transcript.to_vec();
    let mut compacted = Vec::with_capacity(COMPACT_OLDEST_N_TURNS);
    for (i, sum) in input.summaries.iter().enumerate().take(input.transcript.len()) {
        if compacted.len() >= COMPACT_OLDEST_N_TURNS {
            break;
        }
        let Some(summary) = sum.as_ref() else {
            continue;
        };
        let role = input.transcript[i].role;
        out[i] = Message {
            role,
            blocks: vec![Block::Text {
                text: format!("[compacted turn {i}] {summary}"),
                cache_marker: None,
            }],
        };
        compacted.push(i);
    }
    CompactionOutput {
        transcript: out,
        compacted_indices: compacted,
    }
}

/// Compact `input`, firing a `PreCompress` lifecycle hook first.
///
/// Wraps the pure [`compact`] with a best-effort `PreCompress` fire (gemini /
/// claude `PreCompact`): when `~/.origin/hooks.json` configures a hook, it is
/// notified of the transcript size just before compaction; with no hooks the
/// fire is a no-op and this is byte-identical to calling [`compact`] directly.
///
/// The hook is informational — its override is ignored — so compaction always
/// proceeds. This is the firing site any runtime compaction call should adopt.
pub async fn compact_with_hooks(input: &CompactionInput<'_>) -> CompactionOutput {
    if let Some(h) = crate::hooks_runtime::global().await {
        let current_bytes = u64::try_from(input.current_bytes).unwrap_or(u64::MAX);
        let _ = h
            .fire(&origin_hooks::LifecycleEvent::PreCompress { current_bytes })
            .await;
    }
    compact(input)
}

/// Live, in-loop compaction guard for the agent's working transcript.
///
/// This is the runtime call-site for [`compact_with_hooks`]: the agent loop
/// invokes it between turns so that, once the accumulated context crosses
/// `soft_cap_bytes`, the oldest summarizable turns are folded into their
/// summaries before the next provider call — and the `PreCompress` lifecycle
/// hook fires for any configured listener.
///
/// **Default-off / byte-identical for short sessions:** when the estimated
/// transcript size (via [`estimate_transcript_bytes`]) is at or under the soft
/// cap, this returns `None` immediately, performing no hook fire and no
/// allocation of a replacement transcript — so a session that never grows past
/// the cap behaves exactly as before this wiring existed.
///
/// `summaries[i]` is the eager turn summary for `transcript[i]` (or `None` when
/// unavailable). Only turns with a summary are replaced; with no summaries the
/// returned transcript is structurally unchanged even past the cap, but the
/// `PreCompress` hook still fires so listeners observe the pressure event.
///
/// Returns `Some(new_transcript)` only when compaction ran (over the cap),
/// `None` otherwise.
pub async fn maybe_compact_transcript(
    transcript: &[Message],
    summaries: &[Option<String>],
    soft_cap_bytes: usize,
) -> Option<Vec<Message>> {
    maybe_compact_transcript_indexed(transcript, summaries, soft_cap_bytes)
        .await
        .map(|o| o.transcript)
}

/// Same as [`maybe_compact_transcript`] but returns the full [`CompactionOutput`]
/// so the caller also learns WHICH turn indices were collapsed.
///
/// The agent loop uses this to snapshot each compacted turn's pre-compaction
/// body (via `SessionStore::snapshot_original`) before replacing the transcript,
/// making compaction reversible by a later rewind. Returns `None` (no hook fire,
/// no allocation) when under the cap, exactly like [`maybe_compact_transcript`].
pub async fn maybe_compact_transcript_indexed(
    transcript: &[Message],
    summaries: &[Option<String>],
    soft_cap_bytes: usize,
) -> Option<CompactionOutput> {
    let current_bytes = estimate_transcript_bytes(transcript);
    if current_bytes <= soft_cap_bytes {
        return None;
    }
    Some(
        compact_with_hooks(&CompactionInput {
            transcript,
            summaries,
            current_bytes,
            soft_cap_bytes,
        })
        .await,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use origin_core::types::Role;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: text.into(),
                cache_marker: None,
            }],
        }
    }

    #[test]
    fn estimate_grows_monotonically_with_transcript() {
        let small = vec![user("hi")];
        let big = vec![user(&"x".repeat(10_000)), user("more")];
        assert!(estimate_transcript_bytes(&big) > estimate_transcript_bytes(&small));
        // Every block carries at least the fixed overhead.
        assert!(estimate_transcript_bytes(&small) >= "hi".len());
    }

    #[tokio::test]
    async fn under_cap_returns_none_no_alloc() {
        let transcript: Vec<Message> = (0..3).map(|i| user(&format!("turn {i}"))).collect();
        let summaries: Vec<Option<String>> = transcript.iter().map(|_| Some("s".into())).collect();
        // Soft cap far above the tiny transcript ⇒ no compaction.
        let out = maybe_compact_transcript(&transcript, &summaries, 1_000_000).await;
        assert!(
            out.is_none(),
            "short session must be byte-identical (no compaction)"
        );
    }

    #[tokio::test]
    async fn over_cap_compacts_oldest_summarized_turns() {
        let transcript: Vec<Message> = (0..10).map(|i| user(&format!("turn {i} body"))).collect();
        let summaries: Vec<Option<String>> = transcript
            .iter()
            .enumerate()
            .map(|(i, _)| Some(format!("sum{i}")))
            .collect();
        // Force the over-cap branch with a tiny cap.
        let out = maybe_compact_transcript(&transcript, &summaries, 1)
            .await
            .expect("over-cap must compact");
        assert_eq!(out.len(), transcript.len());
        // The oldest COMPACT_OLDEST_N_TURNS turns are folded into summaries.
        for msg in out.iter().take(COMPACT_OLDEST_N_TURNS) {
            let Block::Text { text, .. } = &msg.blocks[0] else {
                panic!("compacted block must be text");
            };
            assert!(text.starts_with("[compacted turn"));
        }
        // Later turns are untouched.
        for (i, original) in transcript.iter().enumerate().skip(COMPACT_OLDEST_N_TURNS) {
            assert_eq!(out[i], *original);
        }
    }
}
