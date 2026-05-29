// SPDX-License-Identifier: Apache-2.0
use origin_cli::tutorial::{steps, Step};

#[test]
fn tutorial_has_seven_steps_in_order() {
    let s = steps();
    assert_eq!(s.len(), 7);
    let ids: Vec<&str> = s.iter().map(|x| x.id).collect();
    assert_eq!(
        ids,
        vec![
            "welcome",
            "agent-loop",
            "code-graph",
            "memory",
            "skills",
            "swarm",
            "done"
        ]
    );
}

#[test]
fn each_step_has_a_title_and_body() {
    for st in steps() {
        let _: &Step = st;
        assert!(!st.title.is_empty());
        assert!(!st.body.is_empty());
    }
}

#[test]
fn run_writes_to_provided_writer() {
    use std::io::Cursor;
    // Provide enough newlines for the 7 prompts to consume.
    let input = b"\n\n\n\n\n\n\n";
    let mut out: Vec<u8> = Vec::new();
    origin_cli::tutorial::run(Cursor::new(input), &mut out).expect("run");
    let s = String::from_utf8(out).expect("utf8");
    assert!(s.contains("Welcome to origin"));
    assert!(s.contains("You're set"));
}
