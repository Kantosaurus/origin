//! End-to-end OAuth flow: auth-code exchange + refresh rotation against a
//! mocked token endpoint (`wiremock`).

use origin_keyvault::{AuthCodeRequest, KeyVault, OAuthClient, RefreshOutcome};
use std::time::Duration;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn exchange_then_refresh_rotates_tokens() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
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
            AuthCodeRequest {
                code: "auth-code".into(),
                code_verifier: "verifier".into(),
                redirect_uri: "https://example.invalid/callback".into(),
            },
        )
        .await
        .expect("exchange should succeed");
    assert_eq!(exchanged.access.expose(), "access-1");

    let outcome = client
        .refresh(&vault, "default")
        .await
        .expect("refresh should succeed");
    let RefreshOutcome::Rotated { access } = outcome else {
        unreachable!("expected Rotated, got NotDue");
    };
    assert_eq!(access.expose(), "access-2");
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
            AuthCodeRequest {
                code: "auth-code".into(),
                code_verifier: "verifier".into(),
                redirect_uri: "https://example.invalid/callback".into(),
            },
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
