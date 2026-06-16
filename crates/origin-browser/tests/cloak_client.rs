// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::unwrap_used)]

use origin_browser::cloak::CloakClient;
use origin_browser::protocol::Verb;

mod common;

#[tokio::test]
async fn cloak_client_round_trip() {
    if !common::node_available() {
        eprintln!("skipping cloak_client_round_trip: `node` unavailable");
        return;
    }
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fakes/cloak_fake.mjs");
    let mut client = CloakClient::spawn_with_command("node", &[fake.to_str().unwrap()])
        .await
        .unwrap();
    let r = client
        .send(&Verb::Open {
            url: "u".into(),
            session: "s".into(),
        })
        .await
        .unwrap();
    assert_eq!(r.title.as_deref(), Some("cloak"));
}
