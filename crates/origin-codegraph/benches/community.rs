// SPDX-License-Identifier: Apache-2.0
use criterion::{criterion_group, criterion_main, Criterion};
use origin_codegraph::community::{communities, GraphInput, PageRankOpts};
use origin_codegraph::extract::EdgeKind;
use origin_codegraph::record::Confidence;

fn build_1k() -> GraphInput {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for c in 0..10u64 {
        for i in 0..100u64 {
            let n = c * 100 + i;
            nodes.push(n);
            for j in 0..100u64 {
                if i != j {
                    edges.push((n, c * 100 + j, EdgeKind::Calls, Confidence::Extracted));
                }
            }
        }
    }
    for c in 0..9u64 {
        edges.push((c * 100, (c + 1) * 100, EdgeKind::Mentions, Confidence::Inferred));
    }
    GraphInput { nodes, edges }
}

fn bench(c: &mut Criterion) {
    let g = build_1k();
    c.bench_function("communities_1k", |b| {
        b.iter(|| {
            let r = communities(g.clone(), PageRankOpts::default());
            assert!(r.modularity > 0.6, "modularity {}", r.modularity);
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
