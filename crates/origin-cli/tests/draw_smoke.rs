// SPDX-License-Identifier: Apache-2.0
use origin_cli::autocomplete::CompletionSources;
use origin_cli::tui::App;
use origin_tui::composer::Composer;
use origin_tui::stream_widget::{Rect, StreamWidget};

#[test]
fn empty_app_draws_status_only() {
    let mut app = App::new(
        "anthropic",
        "claude-opus-4-7".to_string(),
        CompletionSources::default(),
    );
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
    let mut app = App::new(
        "anthropic",
        "claude-opus-4-7".to_string(),
        CompletionSources::default(),
    );
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

/// Regression: the input card is drawn over a band of rows near the bottom
/// of the message area. The scrollback viewport must stop at the card's top
/// edge, otherwise the lines that would land on the card's rows are
/// permanently invisible — and worse, the most recent few lines vanish while
/// older ones remain on screen.
///
/// Concretely: with 50 lines pushed and a tall-enough screen, the rendered
/// rows must be contiguous (line-N then line-N+1 then line-N+2 ...). A gap
/// in the sequence of visible line numbers means lines were eaten by the
/// card.
#[test]
fn scrollback_does_not_lose_lines_behind_input_card() {
    let mut app = App::new(
        "anthropic",
        "claude-opus-4-7".to_string(),
        CompletionSources::default(),
    );
    for i in 0..50 {
        app.add_line("", &format!("line-{i:02}"));
    }

    let cols: u16 = 80;
    let rows: u16 = 20;
    let mut composer = Composer::new(cols, rows);
    let mut widget = StreamWidget::new(Rect {
        row: 0,
        col: 0,
        cols,
        rows: rows - 1,
    });
    eprintln!("scrollback.len() = {}", app.scrollback.len());
    app.draw(&mut composer, &mut widget);

    let grid = composer.main_grid();
    let gcols = grid.cols();
    let grows = grid.rows();
    let mut rows_dump: Vec<String> = Vec::with_capacity(grows as usize);
    let mut visible_line_nums: Vec<u32> = Vec::new();
    for r in 0..grows {
        let mut row = String::new();
        for c in 0..gcols {
            let g = grid.get(r, c).glyph;
            if let Some(ch) = char::from_u32(g) {
                row.push(ch);
            }
        }
        let trimmed = row.trim_start();
        if let Some(rest) = trimmed.strip_prefix("line-") {
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let rest_after = &rest[digits.len()..];
            // Reject partial matches like "line-4shift+enter…" produced when
            // a row is partially overpainted by the cap-line hint text.
            if !digits.is_empty() && (rest_after.is_empty() || rest_after.starts_with(' ')) {
                if let Ok(n) = digits.parse::<u32>() {
                    visible_line_nums.push(n);
                }
            }
        }
        rows_dump.push(row);
    }

    let dump = rows_dump
        .iter()
        .enumerate()
        .map(|(i, r)| format!("{i:02}: |{r}|"))
        .collect::<Vec<_>>()
        .join("\n");
    eprintln!("Grid dump (rows={grows}, cols={gcols}):\n{dump}");
    eprintln!("visible line numbers: {visible_line_nums:?}");

    assert!(
        visible_line_nums.len() >= 2,
        "expected at least two visible line numbers; dump:\n{dump}"
    );
    for w in visible_line_nums.windows(2) {
        assert_eq!(
            w[1],
            w[0] + 1,
            "scrollback has a gap between line-{:02} and line-{:02} — lines were eaten by the input card.\n\nDump:\n{dump}",
            w[0],
            w[1],
        );
    }
    // The latest visible line should be the very last one pushed (line-49).
    // Non-empty: the `len() >= 2` assert above guarantees a last element.
    let last = *visible_line_nums
        .last()
        .expect("visible_line_nums is non-empty (asserted len >= 2 above)");
    assert_eq!(
        last, 49,
        "expected the latest scrollback line (line-49) to be visible, got line-{last:02}.\n\nDump:\n{dump}"
    );
}
