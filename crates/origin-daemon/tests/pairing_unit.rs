use origin_daemon::pairing::{Pairing, PairingError, RedeemResult};
use std::time::Duration;

#[test]
fn start_returns_6_digit_code() {
    let p = Pairing::new();
    let session = p.start(Duration::from_secs(60));
    assert_eq!(session.code.len(), 6);
    assert!(session.code.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn correct_code_redeems_once() {
    let p = Pairing::new();
    let session = p.start(Duration::from_secs(60));
    let token = match p
        .redeem(&session.code, "device-A")
        .expect("redeem succeeds for fresh code")
    {
        RedeemResult::Issued { bearer, .. } => bearer,
    };
    assert!(token.starts_with("orb_"));
    assert!(matches!(
        p.redeem(&session.code, "device-B"),
        Err(PairingError::UnknownCode)
    ));
}

#[test]
fn wrong_code_errors() {
    let p = Pairing::new();
    let _ = p.start(Duration::from_secs(60));
    assert!(matches!(
        p.redeem("000000", "device-X"),
        Err(PairingError::UnknownCode)
    ));
}

#[test]
fn expired_code_errors() {
    let p = Pairing::new();
    let session = p.start(Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(10));
    assert!(matches!(
        p.redeem(&session.code, "device-Y"),
        Err(PairingError::Expired)
    ));
}
