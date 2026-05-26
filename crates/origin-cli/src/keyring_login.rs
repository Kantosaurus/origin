//! `origin keyring login <provider> [account]` — drives OAuth flows from the
//! builtin provider catalog.
//!
//! Dispatches on `OAuthSpec::device_flow`:
//!   - `true`  → GitHub-style device-code flow (POST device/code, poll token).
//!   - `false` → PKCE auth-code flow; either spins a localhost TCP listener for
//!               providers with a `http://localhost:…` redirect URI, or reads
//!               the code from stdin for providers whose redirect URI is a
//!               remote HTTPS URL (e.g. Anthropic's console callback).

use anyhow::{anyhow, Result};
use origin_keyvault::{AuthCodeRequest, KeyVault, OAuthClient, Pkce, Secret};
use origin_provider::catalog::{AuthScheme, Catalog};
use serde::Deserialize;
use std::io::{self, BufRead as _, Write as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Entry point called from `main.rs` when `keyring login` is dispatched.
///
/// # Errors
/// Returns an error if the provider is not in the catalog, is not an OAuth
/// provider, the keyvault cannot be detected, the OAuth flow fails, or
/// token persistence fails.
pub async fn run(provider_id: &str, account: &str) -> Result<()> {
    let cat = Catalog::builtin();
    let entry = cat
        .lookup(provider_id)
        .ok_or_else(|| anyhow!("unknown provider: {provider_id}"))?;

    let spec = match &entry.auth {
        AuthScheme::OAuth(s) => s.clone(),
        _ => {
            return Err(anyhow!("{provider_id} is not an OAuth provider"));
        }
    };

    let vault = KeyVault::detect().map_err(|e| anyhow!("keyvault detect: {e}"))?;

    if spec.device_flow {
        run_device_flow(provider_id, account, &spec, &vault).await
    } else {
        run_auth_code_flow(provider_id, account, &spec, &vault).await
    }
}

// ---------------------------------------------------------------------------
// Device-code flow (RFC 8628) — used by github-copilot
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u64,
    expires_in: u64,
}

#[derive(Deserialize)]
struct DeviceTokenResponse {
    access_token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: Option<u64>,
    error: Option<String>,
}

async fn run_device_flow(
    provider_id: &str,
    account: &str,
    spec: &origin_provider::catalog::OAuthSpec,
    vault: &KeyVault,
) -> Result<()> {
    let client = reqwest::Client::new();
    let pkce = if spec.pkce { Some(Pkce::new()) } else { None };

    let scopes: Vec<&str> = spec.scopes.iter().map(AsRef::as_ref).collect();
    let scope_str = scopes.join(" ");

    let mut form: Vec<(&str, &str)> = vec![
        ("client_id", spec.client_id.as_ref()),
        ("scope", &scope_str),
    ];
    let challenge_str;
    if let Some(ref p) = pkce {
        challenge_str = p.challenge().to_string();
        form.push(("code_challenge", &challenge_str));
        form.push(("code_challenge_method", "S256"));
    }

    let resp = client
        .post(spec.authorize_url.as_ref())
        .header("Accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| anyhow!("device code request failed: {e}"))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| anyhow!("read body: {e}"))?;
    if !status.is_success() {
        return Err(anyhow!("device code endpoint returned {status}: {body}"));
    }

    let dc: DeviceCodeResponse = serde_json::from_str(&body)
        .map_err(|e| anyhow!("parse device code response: {e}\nraw response: {body}"))?;

    println!("Open this URL in your browser: {}", dc.verification_uri);
    println!("Enter this code:               {}", dc.user_code);
    println!("Waiting for authorization…");

    let deadline = SystemTime::now() + Duration::from_secs(dc.expires_in);
    let poll_interval = Duration::from_secs(dc.interval.max(5));

    loop {
        if SystemTime::now() > deadline {
            return Err(anyhow!("device flow timed out"));
        }

        tokio::time::sleep(poll_interval).await;

        let mut poll_form: Vec<(&str, &str)> = vec![
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", dc.device_code.as_str()),
            ("client_id", spec.client_id.as_ref()),
        ];
        let verifier_str;
        if let Some(ref p) = pkce {
            verifier_str = p.verifier().to_string();
            poll_form.push(("code_verifier", &verifier_str));
        }

        let poll = client
            .post(spec.token_url.as_ref())
            .header("Accept", "application/json")
            .form(&poll_form)
            .send()
            .await
            .map_err(|e| anyhow!("token poll request failed: {e}"))?;

        let poll_body = poll.text().await.map_err(|e| anyhow!("read poll body: {e}"))?;
        let tok: DeviceTokenResponse =
            serde_json::from_str(&poll_body).map_err(|e| anyhow!("parse token response: {e}"))?;

        if let Some(err) = &tok.error {
            match err.as_str() {
                "authorization_pending" | "slow_down" => continue,
                other => return Err(anyhow!("device flow error: {other}")),
            }
        }

        if let Some(access) = tok.access_token {
            let expires_in = tok.expires_in.unwrap_or(3600);
            let expires_at = now_epoch_secs().saturating_add(expires_in);
            persist_tokens(
                vault,
                provider_id,
                account,
                &access,
                tok.refresh_token.as_deref(),
                expires_at,
            )
            .await?;
            println!("Stored OAuth tokens for {provider_id}:{account}");
            return Ok(());
        }
    }
}

// ---------------------------------------------------------------------------
// Auth-code + PKCE flow (RFC 6749 §4.1 + RFC 7636)
// ---------------------------------------------------------------------------

async fn run_auth_code_flow(
    provider_id: &str,
    account: &str,
    spec: &origin_provider::catalog::OAuthSpec,
    vault: &KeyVault,
) -> Result<()> {
    let pkce = Pkce::new();

    // Generate a random state parameter (16 random bytes, hex-encoded).
    let state: String = {
        use rand::RngCore as _;
        use std::fmt::Write as _;
        let mut buf = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut buf);
        buf.iter().fold(String::with_capacity(32), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        })
    };

    let scopes: Vec<&str> = spec.scopes.iter().map(AsRef::as_ref).collect();
    let scope_str = scopes.join(" ");

    let authorize_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&scope={}&state={}",
        spec.authorize_url,
        urlencoding::encode(spec.client_id.as_ref()),
        urlencoding::encode(spec.redirect_uri.as_ref()),
        urlencoding::encode(pkce.challenge()),
        urlencoding::encode(&scope_str),
        urlencoding::encode(&state),
    );

    // Decide whether to use a localhost listener or stdin to receive the code.
    let redirect_uri_str = spec.redirect_uri.as_ref();
    let use_localhost = redirect_uri_str.starts_with("http://localhost:")
        || redirect_uri_str.starts_with("http://127.0.0.1:");

    if use_localhost {
        // Parse the port from the redirect URI.
        let port = parse_localhost_port(redirect_uri_str)
            .ok_or_else(|| anyhow!("could not parse port from redirect_uri: {redirect_uri_str}"))?;

        println!("Open this URL in your browser:");
        println!("{authorize_url}");
        println!("Waiting for the browser redirect on http://localhost:{port} …");

        let code = receive_code_via_listener(port).await?;
        exchange_and_persist(provider_id, account, spec, vault, &pkce, &code, &state).await
    } else {
        // Remote redirect URI (e.g. Anthropic's console callback) — user must
        // paste the code manually.
        println!("Open this URL in your browser:");
        println!("{authorize_url}");
        println!();
        print!("After authorizing, paste the code here: ");
        io::stdout().flush().ok();

        let stdin = io::stdin();
        let pasted = stdin
            .lock()
            .lines()
            .next()
            .ok_or_else(|| anyhow!("no input"))?
            .map_err(|e| anyhow!("read stdin: {e}"))?;
        let pasted = pasted.trim();
        if pasted.is_empty() {
            return Err(anyhow!("empty code"));
        }

        // Claude.ai's manual-paste flow returns "<code>#<state>" — the console
        // redirect page concatenates the URL fragment into the copyable string.
        // Strip the state suffix (and verify it round-tripped, when present) so
        // the bare `code` is what reaches the /token endpoint.
        let (code, returned_state) = pasted
            .split_once('#')
            .map_or((pasted, None), |(c, s)| (c, Some(s)));
        if let Some(returned) = returned_state {
            if returned != state {
                return Err(anyhow!(
                    "OAuth state mismatch: authorization response did not echo \
                     the expected state — aborting to prevent CSRF"
                ));
            }
        }
        let code = code.to_string();

        exchange_and_persist(provider_id, account, spec, vault, &pkce, &code, &state).await
    }
}

/// Parse `http://localhost:PORT/…` → `PORT`.
fn parse_localhost_port(uri: &str) -> Option<u16> {
    // Strip scheme
    let rest = uri
        .strip_prefix("http://localhost:")
        .or_else(|| uri.strip_prefix("http://127.0.0.1:"))?;
    let port_str = rest.split('/').next()?;
    port_str.parse::<u16>().ok()
}

/// Spin a one-shot TCP listener on `127.0.0.1:port`, wait for the browser
/// redirect, parse the `code` query parameter, and return it.
async fn receive_code_via_listener(port: u16) -> Result<String> {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(format!("127.0.0.1:{port}"))
        .await
        .map_err(|e| anyhow!("could not bind 127.0.0.1:{port}: {e}"))?;

    let (mut stream, _) = listener
        .accept()
        .await
        .map_err(|e| anyhow!("accept failed: {e}"))?;

    // Read the first line (the HTTP request line) to extract the path/query.
    let mut reader = tokio::io::BufReader::new(&mut stream);
    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .await
        .map_err(|e| anyhow!("read request line: {e}"))?;

    // Send a minimal HTTP response so the browser shows something useful.
    let response = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
        <html><body><h2>Authorization complete.</h2>\
        <p>You may close this tab.</p></body></html>";
    stream
        .write_all(response)
        .await
        .map_err(|e| anyhow!("write response: {e}"))?;
    drop(stream);

    // Parse: GET /?code=XXX&state=YYY HTTP/1.1
    //    or: GET /auth/callback?code=XXX HTTP/1.1
    let path = request_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("malformed HTTP request line"))?;

    let query = path.split_once('?').map_or("", |x| x.1);
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("code=") {
            return Ok(urlencoding::decode(value)
                .map_err(|e| anyhow!("url decode code: {e}"))?
                .into_owned());
        }
    }
    Err(anyhow!("no 'code' parameter in redirect: {path}"))
}

/// Call `OAuthClient::exchange` and print success.
async fn exchange_and_persist(
    provider_id: &str,
    account: &str,
    spec: &origin_provider::catalog::OAuthSpec,
    vault: &KeyVault,
    pkce: &Pkce,
    code: &str,
    state: &str,
) -> Result<()> {
    // Anthropic's /v1/oauth/token is JSON-only and rejects RFC 6749 form bodies
    // with a 400 "Invalid request format" (Anthropic's API error envelope, not
    // an OAuth error). Route it through the Anthropic-specific JSON exchange.
    if provider_id == "anthropic-oauth" {
        return exchange_anthropic_and_persist(provider_id, account, spec, vault, pkce, code, state)
            .await;
    }
    let oauth = OAuthClient::new(provider_id, spec.token_url.as_ref(), spec.client_id.as_ref());
    let req = AuthCodeRequest::new(code, pkce.verifier(), spec.redirect_uri.as_ref());
    oauth
        .exchange(vault, account, req)
        .await
        .map_err(|e| anyhow!("token exchange failed: {e}"))?;
    println!("Stored OAuth tokens for {provider_id}:{account}");
    Ok(())
}

// Anthropic OAuth token exchange — JSON body with `state` echoed back, matching
// the Claude.ai pairing flow used by the official Claude CLI and other
// Anthropic-aware clients. Refresh still flows through `OAuthClient::refresh`
// (form-encoded) and will need its own JSON-aware path before long-lived
// sessions can rotate tokens against this endpoint.
async fn exchange_anthropic_and_persist(
    provider_id: &str,
    account: &str,
    spec: &origin_provider::catalog::OAuthSpec,
    vault: &KeyVault,
    pkce: &Pkce,
    code: &str,
    state: &str,
) -> Result<()> {
    #[derive(Deserialize)]
    struct TokenResp {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        expires_in: u64,
    }

    let body = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": state,
        "client_id": spec.client_id.as_ref(),
        "redirect_uri": spec.redirect_uri.as_ref(),
        "code_verifier": pkce.verifier(),
    });

    let http = reqwest::Client::new();
    let resp = http
        .post(spec.token_url.as_ref())
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("oauth POST {}: {e}", spec.token_url))?;
    let status = resp.status();
    let body_text = resp
        .text()
        .await
        .map_err(|e| anyhow!("oauth read body: {e}"))?;
    if !status.is_success() {
        return Err(anyhow!(
            "token exchange failed: oauth token endpoint returned {status}: {body_text}"
        ));
    }
    let tok: TokenResp = serde_json::from_str(&body_text)
        .map_err(|e| anyhow!("parse token response: {e}"))?;
    let expires_at = now_epoch_secs().saturating_add(tok.expires_in);
    persist_tokens(
        vault,
        provider_id,
        account,
        &tok.access_token,
        tok.refresh_token.as_deref(),
        expires_at,
    )
    .await?;
    println!("Stored OAuth tokens for {provider_id}:{account}");
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Persist tokens directly into the vault under `(provider, "{account}/oauth")`.
/// Mirrors the `StoredTokens` JSON shape used by `OAuthClient::persist`.
async fn persist_tokens(
    vault: &KeyVault,
    provider: &str,
    account: &str,
    access: &str,
    refresh: Option<&str>,
    expires_at: u64,
) -> Result<()> {
    let blob = refresh.map_or_else(
        || {
            serde_json::json!({
                "access": access,
                "expires_at": expires_at,
            })
        },
        |r| {
            serde_json::json!({
                "access": access,
                "refresh": r,
                "expires_at": expires_at,
            })
        },
    );
    let json = serde_json::to_string(&blob).map_err(|e| anyhow!("serialize tokens: {e}"))?;
    vault
        .set(provider, &format!("{account}/oauth"), Secret::new(json))
        .await
        .map_err(|e| anyhow!("vault set: {e}"))
}
