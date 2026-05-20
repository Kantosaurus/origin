use origin_mcp::limits::{enforce_cap, MAX_RESPONSE_BYTES};
use origin_mcp::TransportError;

#[test]
fn under_cap_passes() {
    let just_under = vec![b'x'; MAX_RESPONSE_BYTES - 1];
    assert!(enforce_cap(just_under.len()).is_ok());
}

#[test]
fn at_cap_passes() {
    assert!(enforce_cap(MAX_RESPONSE_BYTES).is_ok());
}

#[test]
fn over_cap_fails() {
    let result = enforce_cap(MAX_RESPONSE_BYTES + 1);
    assert!(matches!(result, Err(TransportError::TooLarge { observed, cap })
        if observed == MAX_RESPONSE_BYTES + 1 && cap == MAX_RESPONSE_BYTES));
}
