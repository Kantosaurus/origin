use origin_daemon::auth::BearerStore;
use origin_daemon::pairing::Pairing;
use origin_daemon::protocol::{ClientMessage, StreamEvent};
use std::sync::Arc;
use std::time::Duration;

fn dispatch(pairing: &Pairing, store: &BearerStore, msg: ClientMessage) -> Vec<StreamEvent> {
    match msg {
        ClientMessage::PairStart { ttl_secs } => {
            let session = pairing.start(Duration::from_secs(ttl_secs.into()));
            vec![StreamEvent::PairCode {
                code: session.code,
                expires_in_secs: ttl_secs,
            }]
        }
        ClientMessage::PairRedeem { code, device_id } => match pairing.redeem(&code, &device_id) {
            Ok(origin_daemon::pairing::RedeemResult::Issued { bearer, device_id }) => {
                store.insert(bearer.clone(), device_id.clone());
                vec![StreamEvent::PairIssued {
                    bearer,
                    device_id,
                    ttl_secs: 86_400,
                }]
            }
            Err(e) => vec![StreamEvent::PairError {
                message: e.to_string(),
            }],
        },
        _ => vec![],
    }
}

#[test]
fn pair_round_trip_then_validate() {
    let pairing = Arc::new(Pairing::new());
    let store = Arc::new(BearerStore::new());
    let evs = dispatch(&pairing, &store, ClientMessage::PairStart { ttl_secs: 60 });
    let code = match &evs[0] {
        StreamEvent::PairCode { code, .. } => code.clone(),
        other => unreachable!("expected PairCode, got {other:?}"),
    };
    let evs = dispatch(
        &pairing,
        &store,
        ClientMessage::PairRedeem {
            code,
            device_id: "laptop".into(),
        },
    );
    let bearer = match &evs[0] {
        StreamEvent::PairIssued { bearer, .. } => bearer.clone(),
        other => unreachable!("expected PairIssued, got {other:?}"),
    };
    assert_eq!(store.validate(&bearer).as_deref(), Some("laptop"));
    assert!(store.validate("orb_nope").is_none());
}

#[test]
fn redeem_unknown_code_returns_pair_error() {
    let pairing = Arc::new(Pairing::new());
    let store = Arc::new(BearerStore::new());
    let evs = dispatch(
        &pairing,
        &store,
        ClientMessage::PairRedeem {
            code: "999999".into(),
            device_id: "laptop".into(),
        },
    );
    assert!(matches!(evs[0], StreamEvent::PairError { .. }));
}
