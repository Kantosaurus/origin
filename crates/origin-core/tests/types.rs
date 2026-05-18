use origin_core::types::{MessageId, Role, TurnIndex};

#[test]
fn role_round_trips_rkyv() {
    for r in [Role::User, Role::Assistant, Role::Tool, Role::System] {
        let bytes =
            rkyv::to_bytes::<_, 64>(&r).expect("rkyv serialization should not fail for a Role variant");
        let archived = rkyv::check_archived_root::<Role>(&bytes)
            .expect("rkyv validation should pass for a freshly serialized Role");
        assert_eq!(Role::from_archived(archived), r);
    }
}

#[test]
fn message_id_is_ulid() {
    let id = MessageId::new();
    assert_eq!(id.to_string().len(), 26);
}

#[test]
fn message_ids_are_unique() {
    let a = MessageId::new();
    let b = MessageId::new();
    assert_ne!(
        a, b,
        "two consecutive MessageId::new() calls should produce distinct ULIDs"
    );
}

#[test]
fn turn_index_is_monotonic() {
    let a = TurnIndex(0);
    let b = a.next().expect("TurnIndex(0).next() should not overflow");
    assert!(b.0 > a.0);
}

#[test]
fn turn_index_saturates_at_max() {
    assert!(TurnIndex(u32::MAX).next().is_none());
}
