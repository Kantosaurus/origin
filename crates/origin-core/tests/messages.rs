use origin_core::types::{Block, CacheBoundary, Message, Role};

#[test]
fn message_with_text_block_roundtrips() {
    let m = Message::new(Role::User).with_block(Block::text("hello"));
    let bytes = rkyv::to_bytes::<_, 256>(&m).expect("rkyv serialization should not fail for a Message");
    let arch = rkyv::check_archived_root::<Message>(&bytes)
        .expect("rkyv validation should pass for a freshly serialized Message");
    assert_eq!(Role::from_archived(&arch.role), Role::User);
    assert_eq!(arch.blocks.len(), 1);
}

#[test]
fn block_text_carries_no_cache_marker_by_default() {
    let b = Block::text("x");
    assert!(matches!(
        b,
        Block::Text {
            cache_marker: None,
            ..
        }
    ));
}

#[test]
fn cache_boundary_variants_round_trip() {
    for v in [
        CacheBoundary::Frozen,
        CacheBoundary::Sticky,
        CacheBoundary::Sliding,
    ] {
        let bytes =
            rkyv::to_bytes::<_, 32>(&v).expect("rkyv serialization should not fail for CacheBoundary");
        let _ = rkyv::check_archived_root::<CacheBoundary>(&bytes)
            .expect("rkyv validation should pass for CacheBoundary");
    }
}
