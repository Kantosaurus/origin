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
