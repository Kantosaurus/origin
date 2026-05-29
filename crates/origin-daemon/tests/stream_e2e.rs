// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::panic)]

use origin_daemon::protocol::StreamEvent;
use origin_daemon::stream_relay::relay_to_connection;
use origin_ipc::transport::{Connector, Listener};
use origin_stream::{Ring, TokenEvent, TokenKind};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::test]
async fn relay_publishes_token_events_as_event_frames() {
    let path = unique_path("relay");
    let listener = Listener::bind(&path).await.expect("bind");
    let path_clone = path.clone();

    let server_task = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        let shared = Arc::new(Mutex::new(conn));
        let ring = Ring::with_capacity(64 * 1024);
        // Subscribe BEFORE spawning the producer so the relay sees every event.
        let sub = ring.subscribe();
        let p = tokio::spawn(async move {
            ring.publish(&TokenEvent::new(TokenKind::TextDelta, b"Hel".to_vec()))
                .expect("p1");
            ring.publish(&TokenEvent::new(TokenKind::TextDelta, b"lo".to_vec()))
                .expect("p2");
            ring.publish(&TokenEvent::new(TokenKind::TurnEnd, Vec::new()))
                .expect("p3");
            ring.close();
        });
        relay_to_connection(sub, shared).await.expect("relay");
        p.await.expect("producer");
    });

    let mut client = Connector::connect(&path_clone).await.expect("connect");
    let mut got_text = String::new();
    let mut saw_turn_end = false;
    while !saw_turn_end {
        let body = client.read_frame_body().await.expect("read");
        let ev: StreamEvent = serde_json::from_slice(&body).expect("decode event");
        match ev {
            StreamEvent::TextDelta { text } => got_text.push_str(&text),
            StreamEvent::TurnEnd => {
                saw_turn_end = true;
            }
            _ => {}
        }
    }
    assert_eq!(got_text, "Hello");
    assert!(saw_turn_end);
    server_task.await.expect("server");
}

fn unique_path(label: &str) -> String {
    let pid = std::process::id();
    let nano = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    #[cfg(windows)]
    {
        format!(r"\\.\pipe\origin-test-{label}-{pid}-{nano}")
    }
    #[cfg(unix)]
    {
        format!(
            "{}/origin-test-{label}-{pid}-{nano}.sock",
            std::env::temp_dir().display()
        )
    }
}
