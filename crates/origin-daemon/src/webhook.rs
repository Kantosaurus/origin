// SPDX-License-Identifier: Apache-2.0
//! Default-off **authenticated** webhook listener (kilocode Triggers / the
//! HTTP-webhook trigger source for claude `/schedule` + opencode cron).
//!
//! When BOTH `ORIGIN_WEBHOOK=<bind-addr>` (e.g. `127.0.0.1:8787`) and
//! `ORIGIN_WEBHOOK_TOKEN=<secret>` are set, the daemon binds a small HTTP
//! listener. A `POST` carrying a matching `Authorization: Bearer <secret>`
//! fires its body as a prompt onto the live agent path (self-IPC, shared with
//! the scheduler via [`crate::scheduler::dispatch_prompt`]). The body may be
//! plain text or JSON `{"prompt": "…"}`.
//!
//! The token is REQUIRED: with `ORIGIN_WEBHOOK` set but no token the listener
//! refuses to start, so an unauthenticated prompt-firing endpoint can never be
//! exposed by accident. The endpoint ACKs `202 Accepted` immediately and runs
//! the prompt in the background, so callers are not blocked on a full turn.

use std::convert::Infallible;
use std::time::{SystemTime, UNIX_EPOCH};

use http_body_util::{BodyExt, Full};
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpListener;

/// Spawn the webhook listener when `ORIGIN_WEBHOOK` + `ORIGIN_WEBHOOK_TOKEN`
/// are both present. `sock_path` is the daemon's own IPC socket so fired
/// prompts connect back as ordinary clients.
pub fn maybe_spawn(sock_path: String) {
    let Ok(addr) = std::env::var("ORIGIN_WEBHOOK") else {
        return;
    };
    let token = match std::env::var("ORIGIN_WEBHOOK_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            tracing::warn!(
                "ORIGIN_WEBHOOK is set but ORIGIN_WEBHOOK_TOKEN is missing/empty — \
                 refusing to start an unauthenticated webhook listener"
            );
            return;
        }
    };
    let model = std::env::var("ORIGIN_MODEL").unwrap_or_else(|_| "claude-opus-4-7".to_string());
    tracing::info!(%addr, "webhook: ORIGIN_WEBHOOK set — starting authenticated listener");
    tokio::spawn(async move {
        run_listener(addr, token, sock_path, model).await;
    });
}

async fn run_listener(addr: String, token: String, sock_path: String, model: String) {
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, %addr, "webhook: bind failed");
            return;
        }
    };
    tracing::info!(%addr, "webhook: listening");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "webhook: accept failed");
                continue;
            }
        };
        let token = token.clone();
        let sock_path = sock_path.clone();
        let model = model.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let svc = service_fn(move |req: Request<Incoming>| {
                let token = token.clone();
                let sock_path = sock_path.clone();
                let model = model.clone();
                async move { Ok::<_, Infallible>(handle(req, &token, &sock_path, &model).await) }
            });
            if let Err(e) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, svc)
                .await
            {
                tracing::warn!(error = %e, "webhook: serve_connection error");
            }
        });
    }
}

async fn handle(
    req: Request<Incoming>,
    token: &str,
    sock_path: &str,
    model: &str,
) -> Response<Full<Bytes>> {
    if req.method() != Method::POST {
        return json_response(StatusCode::METHOD_NOT_ALLOWED, "{\"error\":\"POST only\"}");
    }
    let expected = format!("Bearer {token}");
    let authorized = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| constant_time_eq(h.trim(), &expected));
    if !authorized {
        return json_response(StatusCode::UNAUTHORIZED, "{\"error\":\"unauthorized\"}");
    }
    let body = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => {
            tracing::warn!(error = %e, "webhook: body read failed");
            return json_response(StatusCode::BAD_REQUEST, "{\"error\":\"bad body\"}");
        }
    };
    let prompt = parse_prompt(&body);
    if prompt.is_empty() {
        return json_response(StatusCode::BAD_REQUEST, "{\"error\":\"empty prompt\"}");
    }
    // Fire-and-forget so the HTTP response is not blocked on a full agent turn.
    let session_id = format!("webhook-{}", now_ms());
    let sock_path = sock_path.to_string();
    let model = model.to_string();
    tokio::spawn(async move {
        if let Err(e) = crate::scheduler::dispatch_prompt(&sock_path, &model, session_id, &prompt).await {
            tracing::warn!(error = %e, "webhook: dispatch failed");
        }
    });
    json_response(StatusCode::ACCEPTED, "{\"status\":\"accepted\"}")
}

/// Extract the prompt from a request body: JSON `{"prompt": "…"}` if it parses
/// as such, otherwise the raw body as UTF-8 text.
fn parse_prompt(body: &[u8]) -> String {
    if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(body) {
        if let Some(Value::String(p)) = map.get("prompt") {
            return p.trim().to_string();
        }
    }
    String::from_utf8_lossy(body).trim().to_string()
}

fn json_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    let mut resp = Response::new(Full::new(Bytes::from(body.to_string())));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("application/json"),
    );
    resp
}

/// Length-checked constant-time-ish comparison of the bearer header.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, parse_prompt};

    #[test]
    fn parse_prompt_handles_text_and_json() {
        assert_eq!(parse_prompt(b"  run the tests  "), "run the tests");
        assert_eq!(
            parse_prompt(br#"{"prompt": "refactor module x"}"#),
            "refactor module x"
        );
        // JSON without a `prompt` field falls back to the raw text.
        assert_eq!(parse_prompt(br#"{"other": 1}"#), r#"{"other": 1}"#);
    }

    #[test]
    fn constant_time_eq_matches_only_equal() {
        assert!(constant_time_eq("Bearer abc", "Bearer abc"));
        assert!(!constant_time_eq("Bearer abc", "Bearer abd"));
        assert!(!constant_time_eq("Bearer abc", "Bearer ab"));
    }
}
