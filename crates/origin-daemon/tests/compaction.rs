use origin_core::types::{Block, Message, Role};
use origin_daemon::compactor::{compact, CompactionInput, COMPACT_OLDEST_N_TURNS, DEFAULT_SOFT_CAP_BYTES};

fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        blocks: vec![Block::Text {
            text: text.into(),
            cache_marker: None,
        }],
    }
}

#[test]
fn under_cap_is_passthrough() {
    let transcript: Vec<Message> = (0..6).map(|i| user(&format!("turn {i}"))).collect();
    let summaries: Vec<Option<String>> = transcript.iter().map(|_| Some("s".into())).collect();
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: 1_000,
        soft_cap_bytes: 100_000,
    });
    assert_eq!(out.transcript, transcript);
    assert!(out.compacted_indices.is_empty());
}

#[test]
#[allow(clippy::panic)]
fn over_cap_replaces_oldest_n_turns_with_summaries() {
    let transcript: Vec<Message> = (0..10).map(|i| user(&format!("turn {i} body"))).collect();
    let summaries: Vec<Option<String>> = transcript
        .iter()
        .map(|m| {
            let Block::Text { text, .. } = &m.blocks[0] else {
                unreachable!()
            };
            Some(format!("sum-of-{text}"))
        })
        .collect();
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: 1_000_000,
        soft_cap_bytes: 100_000,
    });
    assert_eq!(
        out.compacted_indices,
        (0..COMPACT_OLDEST_N_TURNS).collect::<Vec<_>>()
    );
    for &i in &out.compacted_indices {
        let Block::Text { text, .. } = &out.transcript[i].blocks[0] else {
            panic!()
        };
        assert!(text.contains("sum-of-"));
        assert!(text.starts_with("[compacted turn"));
    }
    for (i, original) in transcript.iter().enumerate().skip(COMPACT_OLDEST_N_TURNS) {
        assert_eq!(out.transcript[i], *original);
    }
}

#[test]
fn missing_summary_is_skipped_but_others_still_compact() {
    let transcript: Vec<Message> = (0..6).map(|i| user(&format!("t{i}"))).collect();
    let summaries: Vec<Option<String>> = vec![
        None,
        Some("s1".into()),
        Some("s2".into()),
        Some("s3".into()),
        Some("s4".into()),
        Some("s5".into()),
    ];
    let out = compact(&CompactionInput {
        transcript: &transcript,
        summaries: &summaries,
        current_bytes: 1_000_000,
        soft_cap_bytes: 100,
    });
    assert!(!out.compacted_indices.contains(&0));
    assert_eq!(out.compacted_indices, vec![1, 2, 3, 4]);
}

#[test]
fn default_constants_are_stable() {
    assert_eq!(COMPACT_OLDEST_N_TURNS, 4);
    assert_eq!(DEFAULT_SOFT_CAP_BYTES, 200 * 1024);
}
