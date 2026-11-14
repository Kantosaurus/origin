// SPDX-License-Identifier: Apache-2.0
//! C9: `redact::collect` picks credential-named env vars and vault entries only.
//! Own test binary: it mutates the process environment, so it must not race
//! sibling tests that spawn child processes.
#![allow(clippy::unwrap_used)]

/// Finding 1: only credential-shaped env names (and vault entries) are
/// collected; vendor config like `AWS_REGION` stays readable text.
#[tokio::test]
async fn collect_redacts_credentials_not_vendor_config() {
    for (k, v) in [
        ("AWS_REGION", "us-east-1"),
        ("OPENAI_BASE_URL", "https://api.example.test/v1"),
        ("ANTHROPIC_MODEL", "claude-c9-test-model"),
        ("C9_INDENT_TOKEN", "          "),
        ("OPENAI_API_KEY", "sk-openai-c9-value-0123"),
        ("AWS_SECRET_ACCESS_KEY", "aws-secret-c9-value-0123"),
    ] {
        std::env::set_var(k, v);
    }
    let vault = origin_keyvault::KeyVault::in_memory();
    vault
        .set(
            "anthropic",
            "default",
            origin_keyvault::Secret::new("sk-vault-c9-0123456789".to_string()),
        )
        .await
        .unwrap();
    let r = origin_daemon::redact::collect(&vault, &origin_provider::catalog::Catalog::builtin(), "default")
        .await;

    let text = "region=us-east-1 url=https://api.example.test/v1 model=claude-c9-test-model \
                indent=[          ] openai=sk-openai-c9-value-0123 aws=aws-secret-c9-value-0123 \
                vault=sk-vault-c9-0123456789";
    let out = r.redact(text);
    for kept in [
        "us-east-1",
        "https://api.example.test/v1",
        "claude-c9-test-model",
        "[          ]",
    ] {
        assert!(out.contains(kept), "{kept} must not be redacted: {out}");
    }
    for gone in [
        "[REDACTED:OPENAI_API_KEY]",
        "[REDACTED:AWS_SECRET_ACCESS_KEY]",
        "[REDACTED:anthropic]",
    ] {
        assert!(out.contains(gone), "{gone} missing: {out}");
    }
}
