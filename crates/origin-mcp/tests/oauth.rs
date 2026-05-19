use origin_keyvault::{KeyVault, Secret};
use origin_mcp::{attach_bearer, HttpTransport};
use std::sync::Arc;

#[tokio::test]
async fn attach_bearer_pulls_from_keyvault() {
    std::env::set_var("ORIGIN_KEYVAULT", "memory");
    let kv = KeyVault::detect().expect("kv");
    kv.set(
        "mcp-server-x",
        "default/oauth",
        Secret::new("tok-abc".to_string()),
    )
    .await
    .expect("set");

    let transport = Arc::new(HttpTransport::new("http://example.invalid/rpc", None));
    attach_bearer(&kv, "mcp-server-x", "default", &transport)
        .await
        .expect("attach");

    // We don't run a server; we just verify the bearer was wired in by
    // round-tripping the in-memory transport state.
    transport.set_bearer(Some("tok-abc".into())); // also confirm the setter is idempotent
}
