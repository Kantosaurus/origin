//! P6.7 — round-trip test: daemon emits `MemoryProposed` after a turn and
//! accepts a `ClientMessage::MemoryDecision` over the same connection.

#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::match_wildcard_for_single_variants,
    clippy::too_many_lines
)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use origin_core::types::{Block, Message, Role};
use origin_daemon::agent::{run_loop, LoopOptions};
use origin_daemon::protocol::{ClientMessage, MemoryAction, StreamEvent};
use origin_daemon::session::Session;
use origin_ipc::frame::{encode, FrameKind};
use origin_ipc::transport::{Connector, Listener, SharedConnection};
use origin_mem::Proposer;
use origin_permission::prompt::AlwaysAllow;
use origin_provider::{ChatRequest, ChatResponse, Provider, ProviderError, Usage};
use tokio::sync::{mpsc, Mutex as AsyncMutex};

/// Stub provider that returns a canned assistant reply. Bypasses any network.
struct StubProvider {
    text: String,
}

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &'static str {
        "stub"
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatResponse, ProviderError> {
        Ok(ChatResponse {
            assistant: Message::new(Role::Assistant).with_block(Block::text(&self.text)),
            usage: Usage::default(),
        })
    }
}

#[tokio::test]
async fn memory_proposed_round_trip_via_stub_provider() {
    // ── 1. Listener + connection ────────────────────────────────────────────
    let path = unique_path("memproposal");
    let listener = Listener::bind(&path).await.expect("bind");

    // ── 2. Server task ──────────────────────────────────────────────────────
    // Emulates the daemon main loop's per-connection handler:
    //   - read one ClientMessage::Prompt
    //   - run_loop with proposer enabled
    //   - send StreamEvent::MemoryProposed for each proposal
    //   - send Response
    //   - read one ClientMessage::MemoryDecision and reply with a confirmation
    //     Event frame so the client knows the daemon accepted it
    let server_path = path.clone();
    let server_task = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        let shared: SharedConnection = Arc::new(AsyncMutex::new(conn));

        // ── First frame: Prompt ────────────────────────────────────────────
        let body = shared.lock().await.read_frame_body().await.expect("read prompt");
        let msg: ClientMessage = serde_json::from_slice(&body).expect("decode ClientMessage");
        let prompt = match msg {
            ClientMessage::Prompt(p) => p,
            other => panic!("expected Prompt, got {other:?}"),
        };

        let mut session = Session::new("stub", &prompt.model);
        let provider = StubProvider {
            text: "ok, noted".into(),
        };

        // Side-band StreamEvent channel that the proposer drains MemoryProposed
        // events into. We forward to the client as Event frames here.
        let (event_tx, mut event_rx) = mpsc::channel::<StreamEvent>(16);
        let conn_for_relay = Arc::clone(&shared);
        let relay_handle = tokio::spawn(async move {
            while let Some(ev) = event_rx.recv().await {
                let body = serde_json::to_vec(&ev).expect("encode event");
                conn_for_relay
                    .lock()
                    .await
                    .write_frame(FrameKind::Event, &body)
                    .await
                    .expect("write event");
            }
        });

        let proposer = Arc::new(Proposer::new());
        let opts = LoopOptions {
            max_turns: 5,
            cas: None,
            relay_tx: None,
            streaming_disabled: true,
            proposer: Some(Arc::clone(&proposer)),
            event_tx: Some(event_tx.clone()),
            injector: None,
            sidecar: None,
            session_store: None,
            proposal_registry: None,
            skills: None,
            skill_catalog: None,
            memory_handle: None,
        };
        let summary = run_loop(&mut session, &prompt.user_text, &provider, &AlwaysAllow, &opts)
            .await
            .expect("run_loop ok");
        // `opts` holds a clone of `event_tx` (via `LoopOptions::event_tx`).
        // Drop both the local handle AND `opts` before awaiting the relay so
        // the channel actually closes — otherwise the relay loop's
        // `event_rx.recv().await` never returns `None` and we deadlock here.
        drop(event_tx);
        drop(opts);
        relay_handle.await.expect("relay join");

        // We expect at least one proposal pending.
        assert!(
            !session.pending_proposals.is_empty(),
            "expected at least one pending proposal"
        );

        // Write Response so client knows the turn is finished.
        let reply = origin_daemon::protocol::PromptReply {
            assistant_text: summary.assistant_text,
            turns: summary.turns,
        };
        let bytes = serde_json::to_vec(&reply).expect("serialize reply");
        shared
            .lock()
            .await
            .write_frame(FrameKind::Response, &bytes)
            .await
            .expect("write response");

        // ── Second frame: MemoryDecision ───────────────────────────────────
        let body = shared
            .lock()
            .await
            .read_frame_body()
            .await
            .expect("read decision");
        let decision: ClientMessage =
            serde_json::from_slice(&body).expect("decode ClientMessage::MemoryDecision");
        match decision {
            ClientMessage::MemoryDecision { proposal_id, action } => {
                // For P6.7 the handler is a stub — it just drops/keeps the proposal
                // and acknowledges receipt with a Response frame.
                assert_eq!(proposal_id, 1, "first proposal id is 1");
                assert!(matches!(action, MemoryAction::Accept));
                // Acknowledge.
                shared
                    .lock()
                    .await
                    .write_frame(FrameKind::Response, b"{\"ok\":true}")
                    .await
                    .expect("write ack");
            }
            other => panic!("expected MemoryDecision, got {other:?}"),
        }
    });

    // ── 3. Client side ──────────────────────────────────────────────────────
    let mut client = Connector::connect(&server_path).await.expect("connect");

    // Send ClientMessage::Prompt.
    let prompt = ClientMessage::Prompt(origin_daemon::protocol::PromptRequest {
        system: String::new(),
        model: "claude-opus-4-7".into(),
        user_text: "remember: x is the variable name we use".into(),
    });
    let body = serde_json::to_vec(&prompt).expect("encode prompt");
    let frame = encode(1, FrameKind::Request, &body);
    client.write_raw(&frame).await.expect("write prompt");

    // Read frames until we see Response. Collect StreamEvents.
    let proposals: Arc<Mutex<Vec<(u32, String)>>> = Arc::new(Mutex::new(Vec::new()));
    loop {
        let (kind, body) = client.read_frame().await.expect("read frame");
        match kind {
            FrameKind::Event => {
                let ev: StreamEvent = serde_json::from_slice(&body).expect("decode StreamEvent");
                if let StreamEvent::MemoryProposed {
                    proposal_id,
                    body: pbody,
                    ..
                } = ev
                {
                    proposals.lock().expect("lock").push((proposal_id, pbody));
                }
            }
            FrameKind::Response => break,
            FrameKind::ErrorFrame => panic!("got error frame"),
            FrameKind::Request => panic!("unexpected request frame"),
        }
    }

    // Assert we saw at least one MemoryProposed event whose body contains "x".
    let snap = proposals.lock().expect("lock").clone();
    assert!(!snap.is_empty(), "expected at least one MemoryProposed event");
    assert!(
        snap.iter().any(|(_, b)| b.contains('x')),
        "expected a proposal body containing 'x', got: {snap:?}"
    );

    // Send a MemoryDecision::Accept for proposal_id 1.
    let decision = ClientMessage::MemoryDecision {
        proposal_id: 1,
        action: MemoryAction::Accept,
    };
    let body = serde_json::to_vec(&decision).expect("encode decision");
    let frame = encode(2, FrameKind::Request, &body);
    client.write_raw(&frame).await.expect("write decision");

    // Expect an ack.
    let (kind, _body) = client.read_frame().await.expect("read ack");
    assert_eq!(kind, FrameKind::Response, "expected Response ack");

    server_task.await.expect("server task");
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
