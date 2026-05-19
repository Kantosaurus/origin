use criterion::{black_box, criterion_group, criterion_main, Criterion};
use origin_core::types::{Block, Message, Role};
use origin_daemon::compactor::{compact, CompactionInput, DEFAULT_SOFT_CAP_BYTES};

fn synth_turn(i: usize) -> (Message, Message) {
    let user_text = format!("Question {i}: ").repeat(50);
    let asst_text = format!("Answer {i}: ").repeat(50);
    (
        Message {
            role: Role::User,
            blocks: vec![Block::Text {
                text: user_text,
                cache_marker: None,
            }],
        },
        Message {
            role: Role::Assistant,
            blocks: vec![Block::Text {
                text: asst_text,
                cache_marker: None,
            }],
        },
    )
}

fn estimate_bytes(transcript: &[Message]) -> usize {
    transcript
        .iter()
        .flat_map(|m| m.blocks.iter())
        .map(|b| {
            if let Block::Text { text, .. } = b {
                text.len()
            } else {
                0
            }
        })
        .sum()
}

fn bench_long_session(c: &mut Criterion) {
    let mut transcript: Vec<Message> = Vec::with_capacity(1440);
    let mut summaries: Vec<Option<String>> = Vec::with_capacity(1440);
    for i in 0..720 {
        let (u, a) = synth_turn(i);
        transcript.push(u);
        summaries.push(Some(format!("user said q{i}")));
        transcript.push(a);
        summaries.push(Some(format!("asst answered a{i}")));
    }

    c.bench_function("compact_long_session", |b| {
        b.iter(|| {
            let current = estimate_bytes(&transcript);
            let out = compact(&CompactionInput {
                transcript: &transcript,
                summaries: &summaries,
                current_bytes: current,
                soft_cap_bytes: DEFAULT_SOFT_CAP_BYTES,
            });
            black_box(out);
        });
    });

    // Static assertion: compaction actually fires + shrinks bytes.
    let current = estimate_bytes(&transcript);
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: current,
        soft_cap_bytes: DEFAULT_SOFT_CAP_BYTES,
    });
    let new_bytes = estimate_bytes(&out.transcript);
    assert!(
        new_bytes < current,
        "compaction must shrink transcript bytes: was {current}, now {new_bytes}"
    );
    assert!(
        !out.compacted_indices.is_empty(),
        "compaction must replace at least one turn"
    );
}

criterion_group!(benches, bench_long_session);
criterion_main!(benches);
