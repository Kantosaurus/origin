use origin_ipc::frame::{encode, validate, FrameKind};

#[test]
fn frame_round_trip() {
    let body = b"payload".to_vec();
    let bytes = encode(1, FrameKind::Request, &body);
    let frame = validate(&bytes).expect("frame should validate");
    assert_eq!(frame.request_id, 1);
    assert_eq!(frame.kind, FrameKind::Request);
    assert_eq!(frame.body, body.as_slice());
}

#[test]
fn truncated_frame_rejected() {
    let bytes = encode(1, FrameKind::Request, b"hi");
    let truncated = &bytes[..bytes.len() - 1];
    assert!(validate(truncated).is_err(), "truncated frame should be rejected");
}

#[test]
fn bad_magic_rejected() {
    let mut bytes = encode(1, FrameKind::Request, b"hi");
    bytes[0] = 0;
    bytes[1] = 0;
    assert!(validate(&bytes).is_err(), "bad magic should be rejected");
}

#[test]
fn all_frame_kinds_round_trip() {
    for kind in [
        FrameKind::Request,
        FrameKind::Response,
        FrameKind::Event,
        FrameKind::ErrorFrame,
    ] {
        let bytes = encode(42, kind, b"x");
        let frame = validate(&bytes).expect("frame should validate");
        assert_eq!(frame.kind, kind);
    }
}
