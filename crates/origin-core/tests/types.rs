#![allow(clippy::unwrap_used)]

use origin_core::types::{MessageId, Role, TurnIndex};

#[test]
fn role_round_trips_rkyv() {
    for r in [Role::User, Role::Assistant, Role::Tool, Role::System] {
        let bytes = rkyv::to_bytes::<_, 64>(&r).unwrap();
        let archived = rkyv::check_archived_root::<Role>(&bytes).unwrap();
        assert_eq!(Role::from_archived(archived), r);
    }
}

#[test]
fn message_id_is_ulid() {
    let id = MessageId::new();
    assert_eq!(id.to_string().len(), 26);
}

#[test]
fn turn_index_is_monotonic() {
    let a = TurnIndex(0);
    let b = a.next();
    assert!(b.0 > a.0);
}
