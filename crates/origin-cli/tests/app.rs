// SPDX-License-Identifier: Apache-2.0
use origin_cli::tui::App;

#[test]
fn assistant_turn_lifecycle() {
    let mut app = App::new("anthropic", "claude-opus-4-7", Default::default());
    app.start_assistant_turn();
    app.append_to_current_assistant("Hel");
    app.append_to_current_assistant("lo");
    app.finalize_assistant_turn(2);
    assert!(app.current_assistant.is_none());
    assert!(app.scrollback.iter().any(|l| l.text == "  Hello"));
}

// /model slash command parser is reachable from the CLI surface and
// returns the requested model name. Wired into `handle_submit` in
// main.rs; this test pins the parser contract so a future refactor
// can't accidentally break the slash routing.
#[test]
fn model_command_parser_is_exported() {
    use origin_cli::input::parse_model_command;
    let name = parse_model_command("/model claude-haiku-4-5").expect("parse");
    assert_eq!(name, "claude-haiku-4-5");
}

#[test]
fn model_command_rejects_bare_verb() {
    use origin_cli::input::parse_model_command;
    assert!(parse_model_command("/model").is_none());
}
