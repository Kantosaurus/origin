// SPDX-License-Identifier: Apache-2.0
use origin_daemon::pairing::Pairing;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use std::time::Duration;

#[test]
fn render_pair_start_event_to_stdout() {
    let pairing = Pairing::new();
    let session = pairing.start(Duration::from_secs(60));
    let ev = StreamEvent::PairCode {
        code: session.code.clone(),
        expires_in_secs: 60,
    };
    let json = serde_json::to_string(&ev).expect("serialize PairCode");
    assert!(json.contains(&session.code));
    let _: ClientMessage = ClientMessage::PairStart { ttl_secs: 60 };
}

#[test]
fn parse_origin_url_round_trips() {
    let url = "origin://127.0.0.1:7878#deadbeef";
    let parsed = origin_cli::admin_url::parse_origin_url(url).expect("parse origin url");
    assert_eq!(parsed.addr.to_string(), "127.0.0.1:7878");
    assert_eq!(parsed.fingerprint_hex, "deadbeef");
}

#[test]
fn parse_origin_url_rejects_wrong_scheme() {
    assert!(origin_cli::admin_url::parse_origin_url("http://127.0.0.1:7878#abc").is_err());
}
