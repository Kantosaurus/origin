// SPDX-License-Identifier: Apache-2.0
use criterion::{criterion_group, criterion_main, Criterion};
use origin_codegraph::{chunker, Language};
use std::fmt::Write as _;

fn synth(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let _ = writeln!(s, "fn fn_{i}() {{ let _x = {i}; }}");
    }
    s
}

fn bench(c: &mut Criterion) {
    let src = synth(5_000);
    let bytes = src.as_bytes();
    c.bench_function("chunk_5kloc_rust", |b| {
        b.iter(|| chunker::chunks_ast_biased(Language::Rust, bytes).expect("chunks"));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
