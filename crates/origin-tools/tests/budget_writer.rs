use origin_tools::budget_writer::{ResultWriter, approx_tokens};
use serde_json::json;

#[test]
fn approx_tokens_chars_over_four() {
    assert_eq!(approx_tokens("abcd"), 1);
    assert_eq!(approx_tokens("abcdefgh"), 2);
    assert_eq!(approx_tokens(""), 0);
}

#[test]
fn writer_under_budget_emits_unchanged() {
    let mut w = ResultWriter::new(100, "Read", json!({"file_path": "x.rs", "offset": 0}));
    w.push_str("hello world");
    let body = w.finish_string();
    assert_eq!(body, "hello world");
}

#[test]
fn writer_over_budget_emits_truncation_sentinel() {
    let mut w = ResultWriter::new(2, "Read", json!({"file_path": "x.rs", "offset": 0}));
    w.push_str("aaaaaaaaaaaaaaaaaaaaaa"); // 22 chars ~ 5 tokens
    let body = w.finish_string();
    assert!(body.contains("\"kind\":\"truncated\""), "body: {body}");
    assert!(body.contains("\"continuation\""));
}

#[test]
fn writer_records_lines_consumed_for_continuation() {
    let mut w = ResultWriter::new(2, "Read", json!({"file_path": "x.rs", "offset": 0}));
    w.note_line(0);
    w.push_str("line0\n");
    w.note_line(1);
    w.push_str("line1\n");
    w.note_line(2);
    w.push_str("line2-too-long-too-long-too-long-too-long\n");
    let body = w.finish_string();
    // Continuation handle should point to line 2 (last noted before overflow).
    assert!(body.contains("\"offset\":2"), "body: {body}");
}
