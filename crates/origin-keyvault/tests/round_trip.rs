// SPDX-License-Identifier: Apache-2.0
use origin_keyvault::{Error, KeyVault, Secret};

#[tokio::test]
async fn write_read_delete_round_trip() {
    let vault = KeyVault::in_memory();
    vault
        .set("anthropic", "default", Secret::new("sk-ant-xxx".to_string()))
        .await
        .expect("set should succeed");

    let got = vault
        .get("anthropic", "default")
        .await
        .expect("get should succeed");
    assert_eq!(got.expose(), "sk-ant-xxx");

    let listed = vault.list("anthropic").await.expect("list should succeed");
    assert_eq!(listed, vec!["default".to_string()]);

    vault
        .delete("anthropic", "default")
        .await
        .expect("delete should succeed");

    let err = vault
        .get("anthropic", "default")
        .await
        .expect_err("get after delete must fail");
    assert!(matches!(err, Error::NotFound { .. }));
}

#[test]
fn secret_debug_is_redacted() {
    let s = Secret::new("supersecret".to_string());
    let printed = format!("{s:?}");
    assert!(
        !printed.contains("supersecret"),
        "Secret::Debug must not contain the value, got {printed}"
    );
    assert!(
        printed.contains("redacted"),
        "Secret::Debug should mention redacted, got {printed}"
    );
}
