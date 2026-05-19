use origin_daemon::tool_use_parser::{ToolUseDelta, ToolUseParser};

#[test]
fn emits_field_event_before_closing_brace() {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("Read");
    let events = p.feed(b"{\"file_path\":\"/etc/passwd\"");
    let names: Vec<_> = events
        .into_iter()
        .map(|e| match e {
            ToolUseDelta::Field { name, value, .. } => (name, value),
            #[allow(clippy::panic)] // test-only: unexpected variant signals a bug
            ToolUseDelta::Closed { .. } => panic!("unexpected Closed event"),
        })
        .collect();
    assert_eq!(names, vec![("file_path".into(), b"/etc/passwd".to_vec())]);
}

#[test]
fn coalesces_split_value_across_chunks() {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("Read");
    let mut all = Vec::new();
    all.extend(p.feed(b"{\"file_path\":\"/etc/"));
    all.extend(p.feed(b"passwd\"}"));
    let strings: Vec<_> = all
        .into_iter()
        .filter_map(|e| match e {
            ToolUseDelta::Field { name, value, .. } if name == "file_path" => Some(value),
            ToolUseDelta::Field { .. } | ToolUseDelta::Closed { .. } => None,
        })
        .collect();
    assert_eq!(strings, vec![b"/etc/passwd".to_vec()]);
}

#[test]
fn surfaces_close_event_on_outer_brace() {
    let mut p = ToolUseParser::new();
    p.begin_tool_use("Read");
    let events = p.feed(b"{\"file_path\":\"a\"}");
    let closed = events
        .into_iter()
        .any(|e| matches!(e, ToolUseDelta::Closed { .. }));
    assert!(closed, "expected ToolUseDelta::Closed at outer `}}`");
}

use proptest::prelude::*;

proptest! {
    /// Whatever the chunking, the set of `(name, value)` pairs the parser
    /// emits is identical.
    #[test]
    fn chunking_is_irrelevant_to_field_events(
        chunks in proptest::collection::vec(1u8..16, 1..32),
    ) {
        let input = b"{\"file_path\":\"/tmp/x\",\"recursive\":true,\"limit\":7}";
        let mut p_whole = ToolUseParser::new();
        p_whole.begin_tool_use("X");
        let whole_events = p_whole.feed(input);

        let mut p_split = ToolUseParser::new();
        p_split.begin_tool_use("X");
        let mut cursor = 0;
        let mut split_events = Vec::new();
        for c in chunks {
            let end = (cursor + c as usize).min(input.len());
            split_events.extend(p_split.feed(&input[cursor..end]));
            cursor = end;
            if cursor == input.len() { break; }
        }
        if cursor < input.len() {
            split_events.extend(p_split.feed(&input[cursor..]));
        }

        let proj = |evs: Vec<ToolUseDelta>| -> Vec<(String, Vec<u8>)> {
            evs.into_iter()
                .filter_map(|e| match e {
                    ToolUseDelta::Field { name, value, .. } => Some((name, value)),
                    ToolUseDelta::Closed { .. } => None,
                })
                .collect()
        };
        prop_assert_eq!(proj(whole_events), proj(split_events));
    }
}
