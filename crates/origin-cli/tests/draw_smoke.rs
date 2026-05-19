use origin_cli::tui::App;
use origin_tui::composer::Composer;
use origin_tui::stream_widget::{Rect, StreamWidget};

#[test]
fn empty_app_draws_status_only() {
    let mut app = App::new("anthropic", "claude-opus-4-7".to_string());
    app.add_line("", "hello");
    let mut composer = Composer::new(40, 10);
    let mut widget = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 40,
        rows: 6,
    });
    app.draw(&mut composer, &mut widget);
    let bytes = composer.frame();
    let s = String::from_utf8(bytes).expect("utf-8");
    assert!(s.contains("hello"), "scrollback line must render; got {s:?}");
}

#[test]
fn live_assistant_buffer_renders_in_main() {
    let mut app = App::new("anthropic", "claude-opus-4-7".to_string());
    app.start_assistant_turn();
    app.append_to_current_assistant("hello world");
    let mut composer = Composer::new(40, 10);
    let mut widget = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols: 40,
        rows: 6,
    });
    app.draw(&mut composer, &mut widget);
    let bytes = composer.frame();
    let s = String::from_utf8(bytes).expect("utf-8");
    assert!(s.contains("hello world"));
}
