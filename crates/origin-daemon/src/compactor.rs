//! Summary-backed compaction (P5.4).

use origin_core::types::{Block, Message};

pub const DEFAULT_SOFT_CAP_BYTES: usize = 200 * 1024;
pub const COMPACT_OLDEST_N_TURNS: usize = 4;

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
