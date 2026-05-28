//! Eager turn summarization (N2.5.a).

use origin_core::types::{Block, Message};
use origin_provider::{ChatRequest, Provider};
use std::sync::Arc;

use crate::job::SummaryDeliverer;

const SYS_PROMPT: &str = "You are a summarizer. Reply with exactly one 1-3 sentence summary of the \
     conversation turn. No prelude, no formatting.";

pub async fn run(
    provider: &Arc<dyn Provider>,
    model: &str,
    session_id: &str,
    turn_index: u32,
    transcript: &[Message],
    deliver_to: &dyn SummaryDeliverer,
) {
    let req = ChatRequest {
        system: SYS_PROMPT.to_string(),
        messages: transcript.to_vec(),
        model: model.to_string(),
        tools: Vec::new(),
    };
    let summary = match provider.chat(req).await {
        Ok(resp) => first_text(&resp.assistant).unwrap_or_else(|| fallback(transcript)),
        Err(_) => fallback(transcript),
    };
    deliver_to.deliver(session_id, turn_index, &summary).await;
}

fn first_text(m: &Message) -> Option<String> {
    m.blocks.iter().find_map(|b| match b {
        Block::Text { text, .. } => Some(text.clone()),
        _ => None,
    })
}

fn fallback(transcript: &[Message]) -> String {
    transcript.last().and_then(first_text).map_or_else(
        || "(empty turn)".to_string(),
        |s| {
            let trimmed = s.trim();
            if trimmed.len() <= 120 {
                trimmed.to_string()
            } else {
                // 120 is a byte budget; walk char_indices to find the largest
                // byte index <= 120 so we never slice mid-codepoint.
                let mut end = 0;
                for (i, _) in trimmed.char_indices() {
                    if i > 120 {
                        break;
                    }
                    end = i;
                }
                format!("{}...", &trimmed[..end])
            }
        },
    )
}
