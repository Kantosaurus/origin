use origin_ipc::frame::{encode, validate, FrameKind};
use proptest::prelude::*;

proptest! {
    #[test]
    fn any_body_round_trips(
        body in proptest::collection::vec(any::<u8>(), 0..4096),
        id: u64,
    ) {
        let bytes = encode(id, FrameKind::Request, &body);
        let frame = validate(&bytes).expect("frame should validate");
        prop_assert_eq!(frame.request_id, id);
        prop_assert_eq!(frame.body, body.as_slice());
    }
}
