use origin_stream::{Ring, TokenEvent, TokenKind};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_thousand_writes_three_tails_consistent() {
    let ring = Ring::with_capacity(4 * 1024 * 1024);
    let tails: Vec<_> = (0..3).map(|_| ring.subscribe()).collect();

    let producer = {
        let ring = ring.clone();
        tokio::spawn(async move {
            for i in 0..10_000u32 {
                let ev = TokenEvent::new(TokenKind::TextDelta, i.to_be_bytes().to_vec());
                ring.publish(&ev).expect("publish");
            }
            ring.close();
        })
    };

    let mut handles = Vec::new();
    for mut sub in tails {
        handles.push(tokio::spawn(async move {
            let mut count = 0u32;
            while let Some(ev) = sub.next().await.expect("recv") {
                let bytes = ev.payload();
                let arr: [u8; 4] = bytes.try_into().expect("4 bytes");
                assert_eq!(u32::from_be_bytes(arr), count);
                count += 1;
            }
            count
        }));
    }
    producer.await.expect("producer");
    for h in handles {
        assert_eq!(h.await.expect("tail"), 10_000);
    }
}
