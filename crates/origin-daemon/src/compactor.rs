// SPDX-License-Identifier: Apache-2.0
//! Summary-backed compaction (P5.4).

use origin_cas::{Hash, Store};
use origin_core::types::{Block, Message, Role};

pub const DEFAULT_SOFT_CAP_BYTES: usize = 200 * 1024;
pub const COMPACT_OLDEST_N_TURNS: usize = 4;

/// Fallback charge for a handle-backed tool result when the CAS is unavailable
/// or the handle missed: better to over-estimate than to charge the ~16-byte
/// structural floor and let compaction never fire while the real request grows.
const CAS_RESULT_FLOOR: usize = 8 * 1024;

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
pub fn estimate_transcript_bytes(transcript: &[Message], cas: Option<&Store>) -> usize {
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
                    tool_use_id,
                    handle,
                    inline,
                    ..
                } => tool_use_id.len().saturating_add(tool_result_payload_bytes(
                    handle.as_ref(),
                    inline.as_ref(),
                    cas,
                )),
                Block::Thinking { tokens, signature } => tokens
                    .len()
                    .saturating_add(signature.as_ref().map_or(0, String::len)),
            };
            total = total.saturating_add(block_bytes);
        }
    }
    total
}

/// Bytes a tool result contributes to the in-flight request. Inline bytes are
/// charged directly; a handle-backed result (`inline: None`) is charged its
/// REAL CAS payload size because `expand_messages_for_wire` inlines that full
/// payload at send time — charging the ~16-byte structural floor instead let a
/// session of large reads/greps/build logs stay under the soft cap while the
/// real request grew until it blew the model window (a hard 400).
fn tool_result_payload_bytes(
    handle: Option<&[u8; 32]>,
    inline: Option<&Vec<u8>>,
    cas: Option<&Store>,
) -> usize {
    if let Some(bytes) = inline {
        return bytes.len();
    }
    let Some(h) = handle else { return 0 };
    cas.and_then(|c| c.get(Hash::from_bytes(*h)).ok().flatten())
        .map_or(CAS_RESULT_FLOOR, |b| b.len())
}

/// A7: assistant turns whose tool results survive a high-water elision.
pub const ELIDE_KEEP_TURNS: usize = 10;
/// A7: the tighter window used after a provider context-length error.
pub const ELIDE_KEEP_TURNS_AGGRESSIVE: usize = 3;
/// A7: percent of the model window a high-water elision aims to get under.
pub const ELIDE_LOW_WATER_PCT: usize = 50;
/// A7: a firing that cannot reach low water must free at least this percent
/// of the model window, or it is skipped.
pub const ELIDE_MIN_FREE_PCT: usize = 10;
const ELIDED_PREFIX: &[u8] = b"[tool result elided:";

type ToolNames = std::collections::HashMap<String, String>;

/// Index of the oldest of the last `keep_turns` assistant messages (everything
/// before it is eligible), plus the tool names of the `tool_use`s before it.
fn elision_scope(transcript: &[Message], keep_turns: usize) -> Option<(usize, ToolNames)> {
    let cutoff = transcript
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, m)| m.role == Role::Assistant)
        .nth(keep_turns.saturating_sub(1))
        .map(|(i, _)| i)?;
    let names = transcript[..cutoff]
        .iter()
        .flat_map(|m| &m.blocks)
        .filter_map(|b| match b {
            Block::ToolUse { id, name, .. } => Some((id.clone(), name.clone())),
            _ => None,
        })
        .collect();
    Some((cutoff, names))
}

/// The placeholder for `block` and the bytes swapping it in frees, or `None`
/// when `block` is not a tool result, is already elided, or is no larger than
/// its placeholder.
fn elision_of(block: &Block, names: &ToolNames, cas: Option<&Store>) -> Option<(Vec<u8>, usize)> {
    let Block::ToolResult {
        tool_use_id,
        handle,
        inline,
        ..
    } = block
    else {
        return None;
    };
    if inline.as_ref().is_some_and(|b| b.starts_with(ELIDED_PREFIX)) {
        return None;
    }
    let bytes = tool_result_payload_bytes(handle.as_ref(), inline.as_ref(), cas);
    let tool = names.get(tool_use_id.as_str()).map_or("unknown", String::as_str);
    let placeholder = format!("[tool result elided: {tool} {bytes} bytes]").into_bytes();
    let freed = bytes.checked_sub(placeholder.len()).filter(|&f| f > 0)?;
    Some((placeholder, freed))
}

/// Bytes [`elide_old_tool_results`] with `keep_turns` would free, without
/// touching the transcript.
#[must_use]
pub fn elision_savings(transcript: &[Message], keep_turns: usize, cas: Option<&Store>) -> usize {
    elision_scope(transcript, keep_turns).map_or(0, |(cutoff, names)| {
        transcript[..cutoff]
            .iter()
            .flat_map(|m| &m.blocks)
            .filter_map(|b| elision_of(b, &names, cas))
            .map(|(_, freed)| freed)
            .sum()
    })
}

/// One high-water elision firing (A7); sizes are in transcript bytes.
///
/// Elides once with the LARGEST window from [`ELIDE_KEEP_TURNS`] down to
/// [`ELIDE_KEEP_TURNS_AGGRESSIVE`] that brings `current_bytes` under the
/// [`ELIDE_LOW_WATER_PCT`] mark, so the next firing is far away. When no
/// window gets there (e.g. a system prompt elision can never shrink), the
/// 3-turn window fires only if it frees at least [`ELIDE_MIN_FREE_PCT`] of the
/// window; otherwise nothing is touched — eliding a sliver every turn would
/// break the prompt cache each turn without relieving the pressure. Returns
/// the number of results elided.
pub fn elide_to_low_water(
    transcript: &mut [Message],
    cas: Option<&Store>,
    current_bytes: usize,
    window_bytes: usize,
) -> usize {
    let low_water = window_bytes.saturating_mul(ELIDE_LOW_WATER_PCT) / 100;
    let min_free = window_bytes.saturating_mul(ELIDE_MIN_FREE_PCT) / 100;
    let keep = (ELIDE_KEEP_TURNS_AGGRESSIVE..=ELIDE_KEEP_TURNS)
        .rev()
        .find(|&k| current_bytes.saturating_sub(elision_savings(transcript, k, cas)) < low_water)
        .or_else(|| {
            (elision_savings(transcript, ELIDE_KEEP_TURNS_AGGRESSIVE, cas) >= min_free)
                .then_some(ELIDE_KEEP_TURNS_AGGRESSIVE)
        });
    keep.map_or(0, |k| elide_old_tool_results(transcript, k, cas))
}

/// Summary-free elision (A7): stub out tool results older than the last `keep_turns` assistant turns.
///
/// Each body becomes the one-line placeholder `[tool result elided: <tool> <N> bytes]`,
/// all in ONE batch — each rewrite invalidates the prompt cache from that point
/// on, so everything eligible goes at once rather than one result per turn.
///
/// The block stays a `ToolResult` with its `tool_use_id`, so `tool_use` /
/// `tool_result` pairing is untouched for every provider; the CAS handle is
/// dropped so no provider re-inflates the old payload. Results already elided,
/// or no larger than their placeholder, are skipped, so a repeat call returns 0
/// and rewrites nothing. Returns the number of results elided.
pub fn elide_old_tool_results(transcript: &mut [Message], keep_turns: usize, cas: Option<&Store>) -> usize {
    let Some((cutoff, names)) = elision_scope(transcript, keep_turns) else {
        return 0;
    };
    let mut elided = 0;
    for block in transcript[..cutoff].iter_mut().flat_map(|m| &mut m.blocks) {
        let Some((placeholder, _)) = elision_of(block, &names, cas) else {
            continue;
        };
        if let Block::ToolResult { handle, inline, .. } = block {
            *handle = None;
            *inline = Some(placeholder);
            elided += 1;
        }
    }
    elided
}

/// Whether a provider `Api` error body says the request overflowed the model's
/// context window (Anthropic, OpenAI-compatible, Gemini, llama.cpp/Ollama).
#[must_use]
pub fn is_context_length_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    [
        "prompt is too long",
        "context_length_exceeded",
        "context length",
        "context window",
        "context size",
        "maximum number of tokens",
    ]
    .iter()
    .any(|needle| m.contains(needle))
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

    // Pick the oldest summarizable turns — but NEVER turn 0. Turn 0 carries the
    // user's original task statement / pinned goal; folding it into a summary
    // makes the model drift from the objective over a long session. (Turn 0 is
    // the first user prompt, never a tool_use, so the pairing closure below can't
    // pull it back in.)
    let mut selected: Vec<usize> = Vec::with_capacity(COMPACT_OLDEST_N_TURNS);
    for (i, sum) in input.summaries.iter().enumerate().take(transcript.len()) {
        if selected.len() >= COMPACT_OLDEST_N_TURNS {
            break;
        }
        if i == 0 {
            continue;
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
    let has_tool_use = |i: usize| {
        transcript[i]
            .blocks
            .iter()
            .any(|b| matches!(b, Block::ToolUse { .. }))
    };
    let has_tool_result = |i: usize| {
        transcript[i]
            .blocks
            .iter()
            .any(|b| matches!(b, Block::ToolResult { .. }))
    };
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
    cas: Option<&Store>,
) -> Option<Vec<Message>> {
    maybe_compact_transcript_indexed(transcript, summaries, soft_cap_bytes, cas)
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
    cas: Option<&Store>,
) -> Option<CompactionOutput> {
    let current_bytes = estimate_transcript_bytes(transcript, cas);
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

    /// The `Role::Tool` message carrying the matching `tool_result` (13 bytes).
    fn tool_result(id: &str) -> Message {
        Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: id.into(),
                handle: None,
                inline: Some(b"file contents".to_vec()),
                cache_marker: None,
                is_error: false,
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

        let out = maybe_compact_transcript(&transcript, &summaries, 1, None)
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
        let summaries: Vec<Option<String>> = (0..transcript.len()).map(|i| Some(format!("s{i}"))).collect();

        let out = maybe_compact_transcript(&transcript, &summaries, 1, None)
            .await
            .expect("over-cap must compact");
        assert_eq!(
            first_orphan(&out),
            None,
            "the COMPACT_OLDEST_N_TURNS boundary must not split a tool_use/tool_result pair",
        );
    }

    #[test]
    fn handle_backed_tool_results_are_charged_their_real_cas_size() {
        // Regression: a handle-backed tool result (inline: None) carries its
        // payload in CAS; `expand_messages_for_wire` inlines that full payload at
        // send time. Charging the ~16-byte structural floor let big reads/greps
        // stay under the soft cap while the real request blew the window. The
        // estimate must reflect the real CAS size.
        let tmp = tempfile::tempdir().expect("tmp");
        let cas = Store::open(origin_cas::StoreConfig {
            root: tmp.path().join("cas"),
            hot_capacity: 16,
            warm_pack_target_bytes: 64 * 1024,
            cold_zstd_level: 1,
        })
        .expect("open cas");
        let big = vec![b'x'; 50_000];
        let h = cas.put(&big).expect("put");

        let transcript = vec![Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: "A".into(),
                handle: Some(*h.as_bytes()),
                inline: None,
                cache_marker: None,
                is_error: false,
            }],
        }];

        let est = estimate_transcript_bytes(&transcript, Some(&cas));
        assert!(
            est >= 50_000,
            "handle-backed result must be charged its real CAS size (~50k), got {est}"
        );
    }

    /// A tool turn whose result carries `body_len` bytes.
    fn big_tool_result(id: &str, body_len: usize) -> Message {
        Message {
            role: Role::Tool,
            blocks: vec![Block::ToolResult {
                tool_use_id: id.into(),
                handle: Some([7; 32]),
                inline: Some(vec![b'x'; body_len]),
                cache_marker: None,
                is_error: false,
            }],
        }
    }

    #[test]
    fn elision_rewrites_every_old_result_in_one_batch_and_keeps_pairing() {
        // 14 tool turns; with keep=10 the 5 oldest results are eligible and must
        // all go in ONE call (each rewrite invalidates the prompt cache).
        let mut transcript = vec![user("task")];
        for k in 0..14 {
            transcript.push(assistant_tool_use(&format!("T{k}")));
            transcript.push(big_tool_result(&format!("T{k}"), 5_000));
        }
        transcript.push(Message {
            role: Role::Assistant,
            blocks: vec![Block::text("done")],
        });
        let before = transcript.clone();

        let elided = elide_old_tool_results(&mut transcript, ELIDE_KEEP_TURNS, None);

        // The 10 most recent assistant turns are "done" + T13..T5, so the results
        // of T0..T4 (indices 2,4,..,10) are older and all go at once.
        assert_eq!(elided, 5, "every eligible result is elided in a single batch");
        for k in 0..5 {
            let Block::ToolResult {
                tool_use_id,
                handle,
                inline,
                ..
            } = &transcript[2 + 2 * k].blocks[0]
            else {
                panic!("still a tool_result block");
            };
            assert_eq!(tool_use_id, &format!("T{k}"), "pairing id is preserved");
            assert!(handle.is_none(), "handle dropped so no provider re-inflates it");
            assert_eq!(
                inline.as_deref(),
                Some(&b"[tool result elided: read 5000 bytes]"[..])
            );
        }
        // Recent turns (and every tool_use, and turn 0) are byte-identical.
        assert_eq!(transcript[0], before[0]);
        for i in 11..transcript.len() {
            assert_eq!(transcript[i], before[i], "recent message {i} untouched");
        }
        assert_eq!(
            first_orphan(&transcript),
            None,
            "tool_use/tool_result pairing stays valid"
        );
        // Idempotent: a second pass finds nothing new, so no extra cache rewrite.
        assert_eq!(elide_old_tool_results(&mut transcript, ELIDE_KEEP_TURNS, None), 0);
        // The aggressive window reaches further back (T5..T11).
        assert_eq!(
            elide_old_tool_results(&mut transcript, ELIDE_KEEP_TURNS_AGGRESSIVE, None),
            7
        );
    }

    /// 14 tool turns of 30 KB results (~421 KB) against a 400 KB window
    /// (low water 200 KB, minimum useful firing 40 KB).
    fn pressured() -> Vec<Message> {
        let mut transcript = vec![user("task")];
        for k in 0..14 {
            transcript.push(assistant_tool_use(&format!("T{k}")));
            transcript.push(big_tool_result(&format!("T{k}"), 30_000));
        }
        transcript
    }

    #[test]
    fn low_water_elision_steps_the_window_down_or_skips() {
        const WINDOW: usize = 400_000;
        // Keeping 10 or 7 leaves the context above low water; keeping 6 is
        // the largest window that gets under it, so 8 results go in one batch.
        let mut t = pressured();
        let current = estimate_transcript_bytes(&t, None);
        assert_eq!(elide_to_low_water(&mut t, None, current, WINDOW), 8);
        assert!(estimate_transcript_bytes(&t, None) < WINDOW / 2);
        assert_eq!(
            elision_savings(&t, 6, None),
            0,
            "everything older than 6 turns went"
        );

        // A measured prompt so large (system prompt + schemas) that no window
        // gets under low water: the 3-turn window still frees >= 10 % of the
        // window, so it fires once with everything it can reach.
        let mut t = pressured();
        assert_eq!(elide_to_low_water(&mut t, None, WINDOW * 2, WINDOW), 11);

        // ...but when even that frees < 10 % of the window, nothing is touched.
        let mut t = pressured();
        let before = t.clone();
        assert_eq!(elide_to_low_water(&mut t, None, WINDOW * 10, WINDOW * 10), 0);
        assert_eq!(t, before);
    }

    #[test]
    fn elision_skips_results_smaller_than_the_placeholder() {
        let mut transcript = vec![user("task")];
        for k in 0..4 {
            transcript.push(assistant_tool_use(&format!("T{k}")));
            transcript.push(tool_result(&format!("T{k}"))); // 13 bytes
        }
        let before = transcript.clone();
        assert_eq!(elide_old_tool_results(&mut transcript, 1, None), 0);
        assert_eq!(transcript, before);
    }

    #[test]
    fn context_length_errors_are_recognised_across_providers() {
        for msg in [
            "status 400 Bad Request: {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"prompt is too long: 210000 tokens > 200000 maximum\"}}",
            "status 400 (https://api.openai.com/v1): {\"error\":{\"code\":\"context_length_exceeded\"}}",
            "status 400: This model's maximum context length is 128000 tokens.",
            "status 400: The input token count (1200000) exceeds the maximum number of tokens allowed (1048576).",
            "status 400: the request exceeds the available context size",
        ] {
            assert!(is_context_length_error(msg), "{msg}");
        }
        assert!(!is_context_length_error("status 500: internal error"));
        assert!(!is_context_length_error("status 400: invalid tool schema"));
    }

    #[test]
    fn estimate_grows_monotonically_with_transcript() {
        let small = vec![user("hi")];
        let big = vec![user(&"x".repeat(10_000)), user("more")];
        assert!(estimate_transcript_bytes(&big, None) > estimate_transcript_bytes(&small, None));
        // Every block carries at least the fixed overhead.
        assert!(estimate_transcript_bytes(&small, None) >= "hi".len());
    }

    #[tokio::test]
    async fn under_cap_returns_none_no_alloc() {
        let transcript: Vec<Message> = (0..3).map(|i| user(&format!("turn {i}"))).collect();
        let summaries: Vec<Option<String>> = transcript.iter().map(|_| Some("s".into())).collect();
        // Soft cap far above the tiny transcript ⇒ no compaction.
        let out = maybe_compact_transcript(&transcript, &summaries, 1_000_000, None).await;
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
        let out = maybe_compact_transcript(&transcript, &summaries, 1, None)
            .await
            .expect("over-cap must compact");
        assert_eq!(out.len(), transcript.len());
        // Turn 0 (the task statement) is NEVER compacted.
        assert_eq!(out[0], transcript[0], "turn 0 must be preserved verbatim");
        // The next COMPACT_OLDEST_N_TURNS turns (1..=N) are folded into summaries.
        for msg in out.iter().skip(1).take(COMPACT_OLDEST_N_TURNS) {
            let Block::Text { text, .. } = &msg.blocks[0] else {
                panic!("compacted block must be text");
            };
            assert!(text.starts_with("[compacted turn"));
        }
        // Turns after the compacted window are untouched.
        for (i, original) in transcript.iter().enumerate().skip(COMPACT_OLDEST_N_TURNS + 1) {
            assert_eq!(out[i], *original);
        }
    }
}
