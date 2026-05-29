// SPDX-License-Identifier: Apache-2.0
//! Parse `<goal-status>` tags emitted by the main model.
//!
//! Tolerant by design: case-insensitive `state=`, whitespace allowed
//! in attributes, missing `<reason>` defaults to empty, multiple tags →
//! last wins. Anything we cannot make sense of returns `TagOutcome::Missing`
//! so a forgetful main model never accidentally ends the loop.

use crate::state::TagOutcome;

/// Parse the rightmost well-formed `<goal-status>` tag in `text`.
///
/// Returns [`TagOutcome::Missing`] if no tag is found or the rightmost one
/// has an unknown `state=` value.
#[allow(clippy::module_name_repetitions)] // `parse_tag` is the public entry point of the `tag` module
#[must_use]
pub fn parse_tag(text: &str) -> TagOutcome {
    let mut last = TagOutcome::Missing;
    let mut cursor = 0;
    while let Some(open_rel) = text[cursor..].find("<goal-status") {
        let open = cursor + open_rel;
        let Some(tag_close_rel) = text[open..].find('>') else {
            break;
        };
        let attrs_end = open + tag_close_rel;
        let attrs = &text[open + "<goal-status".len()..attrs_end];
        let Some(close_rel) = text[attrs_end..].find("</goal-status>") else {
            break;
        };
        let close = attrs_end + close_rel;
        let inner = &text[attrs_end + 1..close];
        cursor = close + "</goal-status>".len();
        // The rightmost well-formed tag is authoritative. A trailing tag with an
        // unknown state must override any earlier valid outcome (yielding
        // Missing) rather than silently falling back to the stale earlier tag —
        // the model's *latest* emitted status is what counts.
        last = build_outcome(attrs, inner).unwrap_or(TagOutcome::Missing);
    }
    last
}

fn build_outcome(attrs: &str, inner: &str) -> Option<TagOutcome> {
    let state = extract_state(attrs)?.to_ascii_lowercase();
    let reason = extract_reason(inner);
    match state.as_str() {
        "met" => Some(TagOutcome::Met),
        "in_progress" => Some(TagOutcome::InProgress { what_remains: reason }),
        "blocked" => Some(TagOutcome::Blocked { why: reason }),
        _ => None,
    }
}

fn extract_state(attrs: &str) -> Option<&str> {
    // Hand-rolled to stay dependency-free. Looks for `state` (ws) `=` (ws) `"..."`.
    //
    // Boundary discipline: `state` must be a whole token. The byte before it
    // must be start-of-attrs or ASCII whitespace, AND the byte after it must
    // be `=`, ASCII whitespace, or end-of-attrs. Without the trailing check,
    // attribute names like `state-extra` or `statemachine` would also match
    // the `state` prefix; the inner parser would then fail on the `-`/`m`
    // (not `=`) and re-loop via `i += 1`, which happens to recover today but
    // is fragile to future tweaks.
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        let prefix_ok = &bytes[i..i + 5] == b"state";
        let left_boundary_ok = i == 0 || matches!(bytes[i - 1], b' ' | b'\t' | b'\r' | b'\n');
        let right_boundary_ok =
            i + 5 == bytes.len() || matches!(bytes[i + 5], b'=' | b' ' | b'\t' | b'\r' | b'\n');
        if prefix_ok && left_boundary_ok && right_boundary_ok {
            let mut j = i + 5;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b'=' {
                i += 1;
                continue;
            }
            j += 1;
            while j < bytes.len() && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            if j >= bytes.len() || bytes[j] != b'"' {
                i += 1;
                continue;
            }
            let val_start = j + 1;
            let val_end = val_start + attrs[val_start..].find('"')?;
            return Some(&attrs[val_start..val_end]);
        }
        i += 1;
    }
    None
}

fn extract_reason(inner: &str) -> String {
    let Some(open) = inner.find("<reason>") else {
        return String::new();
    };
    let after_open = open + "<reason>".len();
    let Some(close_rel) = inner[after_open..].find("</reason>") else {
        return String::new();
    };
    inner[after_open..after_open + close_rel].trim().to_string()
}
