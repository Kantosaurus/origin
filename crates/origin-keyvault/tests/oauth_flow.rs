//! End-to-end OAuth flow: auth-code exchange + refresh rotation against a
//! mocked token endpoint (`wiremock`).

use origin_keyvault::{AuthCodeRequest, KeyVault, OAuthClient, RefreshOutcome};
use std::time::Duration;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Read the persisted OAuth blob back out of the vault and pull
/// `(access, refresh)` out of it. The wire shape is internal to
/// `origin-keyvault`, but the JSON keys are stable contract for the test.
async fn read_stored(vault: &KeyVault, provider: &str, account: &str) -> (String, Option<String>) {
    let secret = vault
        .get(provider, &format!("{account}/oauth"))
        .await
        .expect("vault must contain persisted OAuth tokens");
    let v: serde_json::Value =
        serde_json::from_str(secret.expose()).expect("persisted OAuth blob must be valid JSON");
    let access = v
        .get("access")
        .and_then(serde_json::Value::as_str)
        .expect("persisted blob must have `access`")
        .to_owned();
    let refresh = v
        .get("refresh")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    (access, refresh)
}

#[tokio::test]
async fn exchange_then_refresh_rotates_tokens() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=auth-code"))
        .and(body_string_contains("code_verifier=verifier"))
        .and(body_string_contains("redirect_uri=https"))
        .and(body_string_contains("client_id=client-id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=refresh-1"))
        .and(body_string_contains("client_id=client-id"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-2",
            "refresh_token": "refresh-2",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let vault = KeyVault::in_memory();
    let client = OAuthClient::new("github", format!("{}/token", server.uri()), "client-id");

    let exchanged = client
        .exchange(
            &vault,
            "default",
            AuthCodeRequest::new("auth-code", "verifier", "https://example.invalid/callback"),
        )
        .await
        .expect("exchange should succeed");
    assert_eq!(exchanged.access.expose(), "access-1");

    // Vault must hold exactly what the mock returned after `exchange`.
    let (stored_access, stored_refresh) = read_stored(&vault, "github", "default").await;
    assert_eq!(stored_access, "access-1");
    assert_eq!(stored_refresh.as_deref(), Some("refresh-1"));

    let outcome = client
        .refresh(&vault, "default")
        .await
        .expect("refresh should succeed");
    let RefreshOutcome::Rotated { access } = outcome else {
        unreachable!("expected Rotated, got NotDue");
    };
    assert_eq!(access.expose(), "access-2");

    // Vault must hold the rotated tokens after `refresh`.
    let (stored_access, stored_refresh) = read_stored(&vault, "github", "default").await;
    assert_eq!(stored_access, "access-2");
    assert_eq!(stored_refresh.as_deref(), Some("refresh-2"));
}

#[tokio::test]
async fn refresh_if_due_skips_when_not_due() {
    let server = MockServer::start().await;

    // Only one /token POST (the initial exchange). If refresh_if_due fires
    // a request, wiremock's `expect(1)` will trip on shutdown.
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "access-1",
            "refresh_token": "refresh-1",
            "expires_in": 3600
        })))
        .expect(1)
        .mount(&server)
        .await;

    let vault = KeyVault::in_memory();
    let client = OAuthClient::new("github", format!("{}/token", server.uri()), "client-id");
    client
        .exchange(
            &vault,
            "default",
            AuthCodeRequest::new("auth-code", "verifier", "https://example.invalid/callback"),
        )
        .await
        .expect("exchange should succeed");

    // Token has ~3600s left; 60s safety window must not trigger refresh.
    let outcome = client
        .refresh_if_due(&vault, "default", Duration::from_secs(60))
        .await
        .expect("refresh_if_due should succeed");
    assert!(matches!(outcome, RefreshOutcome::NotDue { .. }));
}
