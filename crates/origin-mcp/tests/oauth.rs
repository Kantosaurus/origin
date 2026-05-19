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
    assert!(transport.current_bearer().is_none(), "no bearer before attach");

    attach_bearer(&kv, "mcp-server-x", "default", &transport)
        .await
        .expect("attach");

    // Assert the bearer was sourced from the vault.
    assert_eq!(
        transport.current_bearer().as_deref(),
        Some("tok-abc"),
        "attach_bearer must copy the vault-stored token onto the transport"
    );
}
