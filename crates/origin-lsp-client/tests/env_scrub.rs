// SPDX-License-Identifier: Apache-2.0
//! A6: a language-server child gets the allow-listed environment, not the
//! daemon's. One test fn in its own binary because it mutates the process
//! environment.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::time::Duration;

use origin_lsp_client::LspClient;

/// What the fake server reports for env var `name`.
async fn server_sees(name: &str) -> String {
    let client = LspClient::spawn_with_args(env!("CARGO_BIN_EXE_fake-lsp-server"), &[], Path::new("."))
        .await
        .unwrap();
    client
        .did_open(Path::new("env.txt"), "plaintext", &format!("ENV:{name}"))
        .await
        .unwrap();
    let published = async {
        loop {
            if let Some(d) = client.diagnostics(None).await.pop() {
                return d.message;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), published)
        .await
        .unwrap()
}

#[tokio::test]
async fn language_server_env_drops_secrets_keeps_toolchain_vars() {
    std::env::set_var("ANTHROPIC_API_KEY", "sk-parent-secret");
    std::env::remove_var("ORIGIN_ENV_SCRUB");
    std::env::remove_var("ORIGIN_ENV_PASSTHROUGH");
    // rust-analyzer needs these to find the toolchain; they are allow-listed.
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        if std::env::var_os(name).is_none() {
            std::env::set_var(name, format!("probe-{name}"));
        }
    }

    assert_eq!(server_sees("ANTHROPIC_API_KEY").await, "<unset>", "secret leaked");
    assert_ne!(server_sees("PATH").await, "<unset>", "PATH missing");
    for name in ["CARGO_HOME", "RUSTUP_HOME"] {
        assert_eq!(server_sees(name).await, std::env::var(name).unwrap(), "{name}");
    }

    // Escape hatch: ORIGIN_ENV_SCRUB=0 restores full inheritance.
    std::env::set_var("ORIGIN_ENV_SCRUB", "0");
    assert_eq!(server_sees("ANTHROPIC_API_KEY").await, "sk-parent-secret");
}
