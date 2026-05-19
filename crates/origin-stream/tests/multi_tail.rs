use origin_stream::{Ring, TokenEvent, TokenKind};

#[tokio::test]
async fn single_producer_three_consumers_see_same_sequence() {
    let ring = Ring::with_capacity(64 * 1024);
    let sub_a = ring.subscribe();
    let sub_b = ring.subscribe();
    let sub_c = ring.subscribe();

    let events: Vec<TokenEvent> = (0..50)
        .map(|i| TokenEvent::new(TokenKind::TextDelta, format!("tok-{i}").into_bytes()))
        .collect();

    let producer = {
        let ring = ring.clone();
        let events = events.clone();
        tokio::spawn(async move {
            for ev in events {
                ring.publish(&ev).expect("publish");
            }
            ring.close();
        })
    };

    async fn collect_all(mut sub: origin_stream::Subscriber) -> Vec<origin_stream::TokenEvent> {
        let mut out = Vec::new();
        while let Some(ev) = sub.next().await.expect("recv") {
            out.push(ev);
        }
        out
    }

    let (a, b, c) = tokio::join!(collect_all(sub_a), collect_all(sub_b), collect_all(sub_c));
    producer.await.expect("producer task");

    assert_eq!(a.len(), 50);
    assert_eq!(a, b);
    assert_eq!(b, c);
}

/// Subscribers created mid-stream see only events published AFTER their
/// subscribe() call. P2.7 (SSE → ring) and the daemon relay rely on this.
#[tokio::test]
async fn mid_stream_subscriber_sees_only_later_events() {
    let ring = Ring::with_capacity(64 * 1024);

    for i in 0..10u32 {
        ring.publish(&TokenEvent::new(TokenKind::TextDelta, vec![i as u8]))
            .expect("pre-publish");
    }

    let mut late = ring.subscribe();

    for i in 10..15u32 {
        ring.publish(&TokenEvent::new(TokenKind::TextDelta, vec![i as u8]))
            .expect("post-publish");
    }
    ring.close();

    let mut got: Vec<u8> = Vec::new();
    while let Some(ev) = late.next().await.expect("recv") {
        assert_eq!(ev.payload().len(), 1);
        got.push(ev.payload()[0]);
    }
    assert_eq!(got, vec![10, 11, 12, 13, 14]);
}
