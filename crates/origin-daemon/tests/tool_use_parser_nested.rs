// SPDX-License-Identifier: Apache-2.0
//! When tool input JSON contains a string with `{` / `]` inside it, the
//! current parser miscounts depth. Make it string-state aware.
use origin_daemon::tool_use_parser::feed_for_test;

#[test]
fn brace_inside_string_does_not_close_nested() {
    let chunks = [
        br#"{"x":{"y":"contains } and ] chars","#.as_slice(),
        br#""z":1}}"#.as_slice(),
    ];
    let mut p = feed_for_test();
    for c in chunks {
        p.feed(c);
    }
    let done = p.finish();
    assert_eq!(done.input_json, r#"{"x":{"y":"contains } and ] chars","z":1}}"#);
}

#[test]
fn escaped_quote_inside_string_does_not_exit_string_state() {
    let chunks = [br#"{"a":"he said \"hi\"","b":2}"#.as_slice()];
    let mut p = feed_for_test();
    for c in chunks {
        p.feed(c);
    }
    let done = p.finish();
    assert!(done.complete, "parser should mark complete after balanced braces");
}
