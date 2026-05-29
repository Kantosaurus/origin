// SPDX-License-Identifier: Apache-2.0
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use origin_tui::damage::diff;
use origin_tui::{Cell, Grid};

#[allow(
    clippy::cast_possible_truncation,
    reason = "row/col fit in u16 by construction"
)]
fn bench_1pct_changed(c: &mut Criterion) {
    let cols = 200u16;
    let rows = 60u16;
    let a = Grid::new(cols, rows);
    let mut b = a.clone();
    let total = (usize::from(cols) * usize::from(rows)) / 100;
    for i in 0..total {
        let row = (i % usize::from(rows)) as u16;
        let col = ((i * 17) % usize::from(cols)) as u16;
        b.put(row, col, Cell::glyph('x'));
    }
    c.bench_function("damage_diff_200x60_1pct", |bencher| {
        bencher.iter(|| {
            let runs = diff(black_box(&a), black_box(&b));
            black_box(runs);
        });
    });
}

criterion_group!(benches, bench_1pct_changed);
criterion_main!(benches);
