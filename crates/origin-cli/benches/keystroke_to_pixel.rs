use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use origin_cli::tui::App;
use origin_stream::{TokenEvent, TokenKind};
use origin_tui::composer::Composer;
use origin_tui::stream_widget::{Rect, StreamWidget};

fn bench_keystroke_to_pixel(c: &mut Criterion) {
    let mut group = c.benchmark_group("keystroke_to_pixel");
    group.throughput(Throughput::Elements(1));
    group.bench_function("type_then_render_one_frame", |b| {
        b.iter_batched(
            || {
                let app = App::new("anthropic", "claude-opus-4-7".to_string(), Default::default());
                let composer = Composer::new(200, 60);
                let widget = StreamWidget::new(Rect {
                    row: 0,
                    col: 0,
                    cols: 200,
                    rows: 56,
                });
                (app, composer, widget)
            },
            |(mut app, mut composer, mut widget)| {
                app.input.push('x');
                app.draw(&mut composer, &mut widget);
                black_box(composer.frame());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

fn bench_stream_under_load(c: &mut Criterion) {
    let mut group = c.benchmark_group("stream_under_load");
    group.bench_function("1k_deltas_8b_one_frame_per_tick", |b| {
        b.iter_batched(
            || {
                let composer = Composer::new(200, 60);
                let widget = StreamWidget::new(Rect {
                    row: 0,
                    col: 0,
                    cols: 200,
                    rows: 56,
                });
                (composer, widget)
            },
            |(mut composer, mut widget)| {
                for i in 0..1_000u32 {
                    let payload = format!("delta{i:03}").into_bytes();
                    let ev = TokenEvent::new(TokenKind::TextDelta, payload);
                    widget.apply(&ev, composer.main_grid());
                }
                black_box(composer.frame());
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_keystroke_to_pixel, bench_stream_under_load);
criterion_main!(benches);
