// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_browser::agent_browser::AgentBrowserClient;
use origin_browser::protocol::Verb;

mod common;

#[tokio::test]
async fn round_trips_a_verb_through_the_fake_cli() {
    if !common::node_available() {
        eprintln!("skipping round_trips_a_verb_through_the_fake_cli: `node` unavailable");
        return;
    }
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/agent_browser_fake.mjs");
    let mut client = AgentBrowserClient::spawn_with_command("node", &[fake.to_str().unwrap()])
        .await
        .unwrap();
    let resp = client
        .send(&Verb::Open {
            url: "https://x".into(),
            session: "s".into(),
        })
        .await
        .unwrap();
    assert!(resp.ok);
    assert_eq!(resp.snapshot.as_deref(), Some("open"));
}
