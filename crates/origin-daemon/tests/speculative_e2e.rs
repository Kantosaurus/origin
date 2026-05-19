use origin_daemon::tool_use_parser::ToolUseParser;
use origin_stream::{Ring, TokenEvent, TokenKind};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Verifies that a speculative task fires *before* the tool_use block closes.
/// Doesn't go through `run_loop`; instead exercises the parser-driven
/// spawn shape directly so the unit captures the precise timing invariant.
#[tokio::test]
async fn speculative_task_fires_before_close() {
    let counter = Arc::new(AtomicU32::new(0));
    let c = counter.clone();
    let ring = Ring::with_capacity(64 * 1024);
    let sub = ring.subscribe();

    let parse_handle = tokio::spawn(async move {
        let mut sub = sub;
        let mut parser = ToolUseParser::new();
        parser.begin_tool_use("Read");
        while let Some(ev) = sub.next().await.expect("next") {
            if ev.kind() == TokenKind::ToolUseDelta {
                let events = parser.feed(ev.payload());
                for e in events {
                    if let origin_daemon::tool_use_parser::ToolUseDelta::Field { .. } = e {
                        c.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
        }
    });

    ring.publish(&TokenEvent::new(
        TokenKind::ToolUseDelta,
        b"{\"file_path\":\"/etc/passwd\"".to_vec(),
    ))
    .expect("publish field");

    let started = Instant::now();
    while counter.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() <= Duration::from_secs(2),
            "speculative dispatch never fired before close"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    ring.publish(&TokenEvent::new(TokenKind::ToolUseDelta, b"}".to_vec()))
        .expect("publish close");
    ring.close();
    parse_handle.await.expect("join");
}
