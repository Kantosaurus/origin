#![allow(clippy::unwrap_used)]

use origin_browser::cloak::CloakClient;
use origin_browser::protocol::Verb;

#[tokio::test]
async fn cloak_client_round_trip() {
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fakes/cloak_fake.mjs");
    let mut client = CloakClient::spawn_with_command("node", &[fake.to_str().unwrap()]).await.unwrap();
    let r = client.send(&Verb::Open { url: "u".into(), session: "s".into() }).await.unwrap();
    assert_eq!(r.title.as_deref(), Some("cloak"));
}
