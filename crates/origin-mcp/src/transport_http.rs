//! HTTP POST + (optional) SSE event-stream transport.
//!
//! - Synchronous request/response: POST `<base>` with JSON body, parse the JSON
//!   response.
//! - SSE subscription: GET `<base>/events`, framed by `eventsource-stream`. The
//!   stream is exposed via `HttpTransport::events()` and yields `serde_json::Value`s.

use crate::transport::{Transport, TransportError};
use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures_util::stream::{Stream, StreamExt};
use reqwest::Client;
use serde_json::Value;
use std::sync::Mutex;

// `HttpTransport` shares the suffix with the `transport_http` module, but the
// `Http*` prefix is the disambiguating convention across the transports.
#[allow(clippy::module_name_repetitions)]
pub struct HttpTransport {
    client: Client,
    url: String,
    bearer: Mutex<Option<String>>,
}

impl HttpTransport {
    #[must_use]
    pub fn new(url: impl Into<String>, bearer: Option<String>) -> Self {
        Self {
            client: Client::new(),
            url: url.into(),
            bearer: Mutex::new(bearer),
        }
    }

    /// Rotate the bearer token (used by [`crate::oauth`]).
    ///
    /// # Panics
    /// Panics if the bearer mutex has been poisoned by a prior panic.
    #[allow(clippy::expect_used)] // see docstring
    pub fn set_bearer(&self, token: Option<String>) {
        *self.bearer.lock().expect("bearer mutex poisoned") = token;
    }

    /// Inspect the currently configured bearer (used by tests + the SSE/POST
    /// paths). Returns `None` when no token has been attached.
    ///
    /// # Panics
    /// Panics if the bearer mutex has been poisoned by a prior panic.
    #[must_use]
    pub fn current_bearer(&self) -> Option<String> {
        #[allow(clippy::expect_used)] // same justification as set_bearer
        self.bearer.lock().expect("bearer mutex poisoned").clone()
    }

    /// Open an SSE stream against `<url>/events`. Each line is a JSON-RPC
    /// notification yielded as `serde_json::Value`.
    ///
    /// # Errors
    /// Returns [`TransportError::Io`] / [`TransportError::Other`] on connection failure.
    pub async fn events(
        &self,
    ) -> Result<impl Stream<Item = Result<Value, TransportError>> + Send, TransportError> {
        let url = format!("{}/events", self.url);
        let mut req = self.client.get(&url);
        if let Some(b) = self.current_bearer() {
            req = req.bearer_auth(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Other(e.to_string()))?;
        let stream = resp.bytes_stream().eventsource().filter_map(|ev| async move {
            match ev {
                Ok(e) => match serde_json::from_str::<Value>(&e.data) {
                    Ok(v) => Some(Ok(v)),
                    Err(err) => Some(Err(TransportError::Serde(err))),
                },
                Err(e) => Some(Err(TransportError::Other(e.to_string()))),
            }
        });
        Ok(stream)
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn round_trip(&self, request_json: &str) -> Result<Value, TransportError> {
        let mut req = self.client.post(&self.url).body(request_json.to_string());
        req = req.header(reqwest::header::CONTENT_TYPE, "application/json");
        if let Some(b) = self.current_bearer() {
            req = req.bearer_auth(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| TransportError::Other(e.to_string()))?;
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| TransportError::Other(e.to_string()))?;
        let v: Value = serde_json::from_slice(&bytes)?;
        Ok(v)
    }
}

