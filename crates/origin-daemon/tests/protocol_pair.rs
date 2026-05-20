use origin_daemon::protocol::{ClientMessage, StreamEvent};

#[test]
fn pair_start_serializes_with_kind_tag() {
    let msg = ClientMessage::PairStart { ttl_secs: 60 };
    let json = serde_json::to_string(&msg).expect("serialize PairStart");
    assert!(json.contains("\"kind\":\"pair_start\""));
}

#[test]
fn pair_redeem_round_trips() {
    let msg = ClientMessage::PairRedeem {
        code: "123456".into(),
        device_id: "macbook-pro".into(),
    };
    let json = serde_json::to_vec(&msg).expect("serialize PairRedeem");
    let back: ClientMessage = serde_json::from_slice(&json).expect("decode PairRedeem");
    assert!(matches!(back, ClientMessage::PairRedeem { .. }));
}

#[test]
fn pair_code_event_serializes() {
    let ev = StreamEvent::PairCode {
        code: "654321".into(),
        expires_in_secs: 60,
    };
    let json = serde_json::to_string(&ev).expect("serialize PairCode");
    assert!(json.contains("\"kind\":\"pair_code\""));
    assert!(json.contains("\"code\":\"654321\""));
}

#[test]
fn pair_issued_event_serializes() {
    let ev = StreamEvent::PairIssued {
        bearer: "orb_abc".into(),
        device_id: "macbook-pro".into(),
        ttl_secs: 86_400,
    };
    let json = serde_json::to_string(&ev).expect("serialize PairIssued");
    assert!(json.contains("\"kind\":\"pair_issued\""));
}
