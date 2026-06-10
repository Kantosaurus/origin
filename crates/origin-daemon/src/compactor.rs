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
    let transcript = input.transcript;

    // Pick the oldest summarizable turns, as before.
    let mut selected: Vec<usize> = Vec::with_capacity(COMPACT_OLDEST_N_TURNS);
    for (i, sum) in input.summaries.iter().enumerate().take(transcript.len()) {
        if selected.len() >= COMPACT_OLDEST_N_TURNS {
            break;
        }
        if sum.is_some() {
            selected.push(i);
        }
    }

    // Close the selection under tool_use/tool_result pairing. A tool turn spans
    // TWO adjacent messages — an `Assistant` message carrying `tool_use` blocks
    // followed by a `Role::Tool` message carrying the matching `tool_result`s.
    // Folding one half into a text summary without the other leaves a dangling
    // `tool_use` or an orphaned `tool_result`, which the Anthropic Messages API
    // rejects with "unexpected tool_use_id found in tool_result blocks ... Each
    // tool_result block must have a corresponding tool_use block in the previous
    // message." So whenever we compact one half, compact its partner too — even
    // if the partner has no summary of its own (it gets a bare marker) and even
    // if that pushes us past `COMPACT_OLDEST_N_TURNS` (the cap is a soft
    // heuristic; correctness wins).
    let has_tool_use = |i: usize| transcript[i].blocks.iter().any(|b| matches!(b, Block::ToolUse { .. }));
    let has_tool_result =
        |i: usize| transcript[i].blocks.iter().any(|b| matches!(b, Block::ToolResult { .. }));
    let mut to_compact: std::collections::BTreeSet<usize> = selected.iter().copied().collect();
    for &i in &selected {
        if has_tool_use(i) && i + 1 < transcript.len() && has_tool_result(i + 1) {
            to_compact.insert(i + 1);
        }
        if has_tool_result(i) && i > 0 && has_tool_use(i - 1) {
            to_compact.insert(i - 1);
        }
    }

    let mut out = transcript.to_vec();
    let mut compacted = Vec::with_capacity(to_compact.len());
    for i in to_compact {
        let role = transcript[i].role;
        // Prefer the turn's own summary; a partner folded in only for pairing
        // (e.g. the tool half) may have none, so fall back to a bare marker.
        let text = input.summaries.get(i).and_then(Option::as_ref).map_or_else(
            || format!("[compacted turn {i}]"),
            |summary| format!("[compacted turn {i}] {summary}"),
        );
        out[i] = Message {
            role,
            blocks: vec![Block::Text {
                text,
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

    /// An assistant turn that issued one `tool_use`.
    fn assistant_tool_use(id: &str) -> Message {
        Message {
            role: Role::Assistant,
            blocks: vec![
                Block::Text {
                    text: "calling a tool".into(),
                    cache_marker: None,
                },
                Block::ToolUse {
                    id: id.into(),
                    name: "read".into(),
                    input_json: b"{}".to_vec(),
                    cache_marker: None,
                },
            ],
        }
    }

    /// The `Role::Tool` message carrying the matching `tool_result`.
    fn tool_result(id: &str) -> Message {
        Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: id.into(),
                handle: None,
                inline: Some(b"file contents".to_vec()),
                cache_marker: None,
            }],
        }
    }

    /// The Anthropic Messages API invariant: every `tool_result` must have a
    /// matching `tool_use` in the immediately-preceding message. Returns the
    /// first orphaned `tool_use_id`, or `None` if the transcript is well-formed.
    fn first_orphan(transcript: &[Message]) -> Option<String> {
        for (i, msg) in transcript.iter().enumerate() {
            for block in &msg.blocks {
                if let Block::ToolResult { tool_use_id, .. } = block {
                    let prev_has_match = i > 0
                        && transcript[i - 1]
                            .blocks
                            .iter()
                            .any(|b| matches!(b, Block::ToolUse { id, .. } if id == tool_use_id));
                    if !prev_has_match {
                        return Some(tool_use_id.clone());
                    }
                }
            }
        }
        None
    }

    #[tokio::test]
    async fn compaction_never_orphans_a_tool_result_with_sparse_summaries() {
        // Regression: compacting the assistant half of a tool turn (folding away
        // its `tool_use`) while leaving the paired `Role::Tool` message intact
        // produces a `tool_result` with no preceding `tool_use` — which the
        // Anthropic API rejects with "unexpected tool_use_id found in tool_result
        // blocks ...". Summaries are sparse in production (loaded per-turn from
        // the store), so the assistant turn can have a summary while its tool turn
        // does not.
        let mut transcript = vec![user("do a thing"), assistant_tool_use("A"), tool_result("A")];
        for k in 0..6 {
            transcript.push(user(&format!("later turn {k}")));
        }
        let mut summaries: Vec<Option<String>> = vec![None; transcript.len()];
        summaries[1] = Some("read a file".into()); // assistant turn only — tool turn (2) has none

        let out = maybe_compact_transcript(&transcript, &summaries, 1)
            .await
            .expect("over-cap must compact");
        assert_eq!(
            first_orphan(&out),
            None,
            "compaction must not orphan a tool_result from its tool_use",
        );
    }

    #[tokio::test]
    async fn compaction_does_not_split_a_tool_pair_at_the_n_boundary() {
        // Even with every turn summarizable, COMPACT_OLDEST_N_TURNS can fall
        // BETWEEN an assistant(tool_use) and its tool(result), folding the
        // assistant but not the result. Layout: [0]=user, then pairs at
        // (1,2),(3,4),(5,6),(7,8) — the N=4 cut lands inside pair (3,4).
        let mut transcript = vec![user("start")];
        for k in 0..4 {
            transcript.push(assistant_tool_use(&format!("T{k}")));
            transcript.push(tool_result(&format!("T{k}")));
        }
        let summaries: Vec<Option<String>> =
            (0..transcript.len()).map(|i| Some(format!("s{i}"))).collect();

        let out = maybe_compact_transcript(&transcript, &summaries, 1)
            .await
            .expect("over-cap must compact");
        assert_eq!(
            first_orphan(&out),
            None,
            "the COMPACT_OLDEST_N_TURNS boundary must not split a tool_use/tool_result pair",
        );
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
