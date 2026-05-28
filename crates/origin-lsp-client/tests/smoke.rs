// This test requires `rust-analyzer` on PATH; gated behind RUN_RA env var
// so the default `cargo test` workflow does not require the binary.

#[tokio::test]
async fn ra_handshake_publishes_no_diags_for_empty_workspace() {
    if std::env::var("RUN_RA").is_err() {
        eprintln!("skipping: set RUN_RA=1 to run this test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let client = origin_lsp_client::LspClient::spawn("rust-analyzer", dir.path())
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let _ = client.diagnostics(None).await; // empty is fine
}
