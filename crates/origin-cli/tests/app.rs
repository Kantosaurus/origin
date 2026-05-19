use origin_cli::tui::App;

#[test]
fn assistant_turn_lifecycle() {
    let mut app = App::new();
    app.start_assistant_turn();
    app.append_to_current_assistant("Hel");
    app.append_to_current_assistant("lo");
    app.finalize_assistant_turn(2);
    assert!(app.current_assistant.is_none());
    assert!(app.scrollback.iter().any(|l| l == "origin (2 turns)> Hello"));
}
