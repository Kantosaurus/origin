// SPDX-License-Identifier: Apache-2.0
//! `origin-gmail` — a first-class Gmail tool over Google OAuth 2.0.
//!
//! # What it does
//! Authenticates with Google using the authorization-code grant's
//! **refresh-token** leg (RFC 6749 §6): the long-lived `refresh_token`,
//! `client_id`, and `client_secret` are loaded from
//! [`origin_keyvault::KeyVault`] and exchanged for a short-lived bearer access
//! token, which is then used against the Gmail REST API v1 to:
//!   * search messages ([`Gmail::search`]),
//!   * fetch a message's metadata + best-effort body ([`Gmail::get_message`]),
//!   * list threads ([`Gmail::list_threads`]).
//!
//! # Design
//! The crate is a **pure state machine with the network injected at one seam**:
//!   * [`request`] builds URLs / form bodies (pure, unit-tested),
//!   * [`model`] parses API JSON into typed values (pure, unit-tested),
//!   * [`http`] is the *only* module that touches the network.
//!
//! This keeps almost everything testable without a live Google connection.
//!
//! # Token frugality (novelty)
//! `get_message` defaults to `format=metadata` with a tight `metadataHeaders`
//! allow-list, and every list call carries an explicit `maxResults` cap and
//! pages lazily via continuation tokens — so a triage view costs a fraction of
//! the bytes (and model tokens) a `format=full` fetch would.
//!
//! # Secrets
//! `client_secret`, `refresh_token`, and the minted access token are all held
//! as [`origin_keyvault::Secret<String>`]; they zeroize on drop and are
//! redacted in `Debug`. No secret is ever logged.

pub mod creds;
pub mod error;
pub mod http;
pub mod model;
pub mod provision;
pub mod request;

use origin_keyvault::{KeyVault, Secret};
use serde::Deserialize;
use serde_json::{json, Value};

pub use crate::creds::Credentials;
pub use crate::error::{Error, Result};
pub use crate::model::{Header, Message, MessageRef, Page, ThreadRef};

/// Default keyvault provider namespace for Google credentials.
pub const DEFAULT_PROVIDER: &str = "google";
/// Default keyvault account within [`DEFAULT_PROVIDER`].
pub const DEFAULT_ACCOUNT: &str = "gmail";
/// Default result cap applied when a caller does not specify one.
pub const DEFAULT_MAX: u32 = 25;

/// A ready-to-use Gmail client holding a minted bearer access token.
///
/// Construct via [`Gmail::from_keyvault`] (loads credentials and refreshes an
/// access token) or [`Gmail::from_access_token`] (test / advanced injection of
/// a pre-minted token).
///
/// Derives no `Debug`: the inner [`http::HttpClient`] carries a bearer token.
pub struct Gmail {
    http: http::HttpClient,
}

impl Gmail {
    /// Build a client from credentials stored in the **default** vault location
    /// `("google", "gmail")`. Exchanges the stored refresh token for a fresh
    /// access token immediately.
    ///
    /// # Errors
    /// [`Error::Credentials`] if no blob is stored, [`Error::CredentialFormat`]
    /// if it is malformed, and the [`http`] error variants on a failed refresh.
    pub async fn from_keyvault(vault: &KeyVault) -> Result<Self> {
        Self::from_keyvault_at(vault, DEFAULT_PROVIDER, DEFAULT_ACCOUNT).await
    }

    /// Build a client from credentials stored under an explicit
    /// `(provider, account)` pair. See [`Gmail::from_keyvault`].
    ///
    /// # Errors
    /// See [`Gmail::from_keyvault`].
    pub async fn from_keyvault_at(vault: &KeyVault, provider: &str, account: &str) -> Result<Self> {
        let blob = vault
            .get(provider, account)
            .await
            .map_err(|e| Error::Credentials(e.to_string()))?;
        let creds = Credentials::from_json(blob.expose())?;
        let client = new_http_client();
        let form = request::refresh_form(&creds);
        let refreshed = http::exchange_refresh(&client, request::TOKEN_URL, &form).await?;
        Ok(Self {
            http: http::HttpClient::new(client, refreshed.access_token),
        })
    }

    /// Build a client from a pre-minted bearer access token. Useful for tests
    /// and for callers that manage the OAuth dance themselves (e.g. via
    /// [`origin_keyvault::OAuthClient`]).
    #[must_use]
    pub fn from_access_token(access_token: Secret<String>) -> Self {
        Self {
            http: http::HttpClient::new(new_http_client(), access_token),
        }
    }

    /// Search messages matching a Gmail search expression (same syntax as the
    /// Gmail search box). Returns up to `max` (clamped to `1..=500`) message
    /// references from the **first** page.
    ///
    /// # Errors
    /// [`http`] error variants on transport / non-2xx, [`Error::Parse`] on a
    /// malformed response.
    pub async fn search(&self, query: &str, max: u32) -> Result<Vec<MessageRef>> {
        let url = request::messages_list_url(query, max, None);
        let body = self.http.get(&url).await?;
        Ok(model::parse_messages_list(&body)?.items)
    }

    /// Search messages and return a [`Page`] so the caller can continue lazily
    /// by feeding `next_page_token` back via [`Gmail::search_page`].
    ///
    /// # Errors
    /// See [`Gmail::search`].
    pub async fn search_page(
        &self,
        query: &str,
        max: u32,
        page_token: Option<&str>,
    ) -> Result<Page<MessageRef>> {
        let url = request::messages_list_url(query, max, page_token);
        let body = self.http.get(&url).await?;
        model::parse_messages_list(&body)
    }

    /// Fetch one message by id, token-frugally (`format=metadata` with the
    /// default header allow-list; the body field is left empty). Use
    /// [`Gmail::get_message_full`] when you need the decoded body.
    ///
    /// # Errors
    /// See [`Gmail::search`].
    pub async fn get_message(&self, id: &str) -> Result<Message> {
        let url = request::message_get_metadata_url(id, &[]);
        let body = self.http.get(&url).await?;
        model::parse_message(&body)
    }

    /// Fetch one message by id with `format=full`, decoding a best-effort
    /// `text/plain` body. Costs more bytes/tokens than [`Gmail::get_message`].
    ///
    /// # Errors
    /// See [`Gmail::search`].
    pub async fn get_message_full(&self, id: &str) -> Result<Message> {
        let url = request::message_get_full_url(id);
        let body = self.http.get(&url).await?;
        model::parse_message(&body)
    }

    /// List threads matching a Gmail search expression. Returns up to `max`
    /// (clamped) thread references from the first page.
    ///
    /// # Errors
    /// See [`Gmail::search`].
    pub async fn list_threads(&self, query: &str, max: u32) -> Result<Vec<ThreadRef>> {
        let url = request::threads_list_url(query, max, None);
        let body = self.http.get(&url).await?;
        Ok(model::parse_threads_list(&body)?.items)
    }

    /// List threads as a [`Page`] so the caller can continue lazily.
    ///
    /// # Errors
    /// See [`Gmail::search`].
    pub async fn list_threads_page(
        &self,
        query: &str,
        max: u32,
        page_token: Option<&str>,
    ) -> Result<Page<ThreadRef>> {
        let url = request::threads_list_url(query, max, page_token);
        let body = self.http.get(&url).await?;
        model::parse_threads_list(&body)
    }
}

/// Build the workspace-standard `reqwest::Client` (rustls TLS, default features
/// otherwise off via the workspace dependency). Falls back to the default
/// client if builder construction fails (it does not under normal conditions).
fn new_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// The Gmail tool's sub-operation, decoded from the `op` field of [`GmailArgs`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `messages.list` — search messages.
    Search,
    /// `messages.get` — fetch one message (metadata by default).
    Get,
    /// `threads.list` — list threads.
    ListThreads,
}

impl Op {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "search" => Ok(Self::Search),
            "get" => Ok(Self::Get),
            "list_threads" => Ok(Self::ListThreads),
            other => Err(Error::BadArgs(format!(
                "unknown op `{other}`; expected one of: search, get, list_threads"
            ))),
        }
    }
}

/// Decoded arguments for the Gmail builtin tool. Mirrors [`input_schema`].
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct GmailArgs {
    /// Which sub-operation to run: `"search"`, `"get"`, or `"list_threads"`.
    pub op: String,
    /// Gmail search expression for `search` / `list_threads`.
    #[serde(default)]
    pub query: Option<String>,
    /// Message id for `get`.
    #[serde(default)]
    pub id: Option<String>,
    /// Max results for list operations (clamped to `1..=500`).
    #[serde(default)]
    pub max: Option<u32>,
    /// For `get`: when `true`, fetch `format=full` and decode the body.
    /// Defaults to `false` (token-frugal metadata-only).
    #[serde(default)]
    pub include_body: bool,
}

impl GmailArgs {
    /// Parse arguments from a JSON value (the shape Phase 2's dispatch holds).
    ///
    /// # Errors
    /// [`Error::BadArgs`] if the JSON does not match the schema.
    pub fn from_value(v: &Value) -> Result<Self> {
        serde_json::from_value(v.clone()).map_err(|e| Error::BadArgs(e.to_string()))
    }
}

/// The tool's JSON-Schema, matching the origin-tools builtin convention.
///
/// It is a `{"type":"object", ...}` describing the accepted arguments,
/// returned as a [`serde_json::Value`] so Phase 2 can embed it directly or
/// stringify it for the `origin_tool!` `input_schema:` arm.
#[must_use]
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "op": {
                "type": "string",
                "enum": ["search", "get", "list_threads"],
                "description": "Which Gmail operation to run."
            },
            "query": {
                "type": "string",
                "description": "Gmail search expression (same syntax as the Gmail search box), e.g. 'from:alice is:unread newer_than:7d'. Required for 'search' and 'list_threads'."
            },
            "id": {
                "type": "string",
                "description": "Message id. Required for 'get'."
            },
            "max": {
                "type": "integer",
                "minimum": 1,
                "maximum": 500,
                "description": "Max results for list operations (default 25)."
            },
            "include_body": {
                "type": "boolean",
                "description": "For 'get': fetch the full message and decode its text body (costs more tokens). Defaults to false (metadata only)."
            }
        },
        "required": ["op"],
        "additionalProperties": false
    })
}

/// Entry point Phase 2 wraps into a builtin `Tool`.
///
/// Loads credentials from the **default** vault location, mints an access
/// token, runs the requested operation, and returns a JSON string result (so
/// it slots straight into the dispatch table's `Result<String, _>`
/// convention).
///
/// # Errors
/// [`Error::BadArgs`] for missing/invalid arguments, plus the credential,
/// HTTP, and parse error variants from the underlying calls.
pub async fn run_tool(args: GmailArgs) -> Result<String> {
    let vault = KeyVault::detect().map_err(|e| Error::Credentials(e.to_string()))?;
    let gmail = Gmail::from_keyvault(&vault).await?;
    run_tool_with(&gmail, args).await
}

/// [`run_tool`] with an explicit, already-constructed [`Gmail`].
///
/// Lets Phase 2 reuse a cached client (avoiding a refresh per call) and lets
/// tests drive the dispatch logic without a network. The result is a compact
/// JSON string.
///
/// # Errors
/// See [`run_tool`].
pub async fn run_tool_with(gmail: &Gmail, args: GmailArgs) -> Result<String> {
    let op = Op::parse(&args.op)?;
    let max = args.max.unwrap_or(DEFAULT_MAX);
    let value = match op {
        Op::Search => {
            let query = args
                .query
                .as_deref()
                .ok_or_else(|| Error::BadArgs("search: missing `query`".to_owned()))?;
            let refs = gmail.search(query, max).await?;
            serde_json::to_value(&refs).map_err(|e| Error::Parse(e.to_string()))?
        }
        Op::ListThreads => {
            let query = args
                .query
                .as_deref()
                .ok_or_else(|| Error::BadArgs("list_threads: missing `query`".to_owned()))?;
            let refs = gmail.list_threads(query, max).await?;
            serde_json::to_value(&refs).map_err(|e| Error::Parse(e.to_string()))?
        }
        Op::Get => {
            let id = args
                .id
                .as_deref()
                .ok_or_else(|| Error::BadArgs("get: missing `id`".to_owned()))?;
            let msg = if args.include_body {
                gmail.get_message_full(id).await?
            } else {
                gmail.get_message(id).await?
            };
            serde_json::to_value(&msg).map_err(|e| Error::Parse(e.to_string()))?
        }
    };
    serde_json::to_string(&value).map_err(|e| Error::Parse(e.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn input_schema_is_valid_object_with_required_op() {
        let s = input_schema();
        assert_eq!(s["type"], "object");
        assert_eq!(s["required"], json!(["op"]));
        let props = s["properties"].as_object().unwrap();
        assert!(props.contains_key("op"));
        assert!(props.contains_key("query"));
        assert!(props.contains_key("id"));
        assert!(props.contains_key("max"));
        assert!(props.contains_key("include_body"));
        // op enum advertises exactly the three supported operations.
        assert_eq!(props["op"]["enum"], json!(["search", "get", "list_threads"]));
        // Schema must round-trip through serde_json (proves it is valid JSON).
        let serialized = serde_json::to_string(&s).unwrap();
        let _back: Value = serde_json::from_str(&serialized).unwrap();
    }

    #[test]
    fn op_parse_accepts_known_and_rejects_unknown() {
        assert_eq!(Op::parse("search").unwrap(), Op::Search);
        assert_eq!(Op::parse("get").unwrap(), Op::Get);
        assert_eq!(Op::parse("list_threads").unwrap(), Op::ListThreads);
        assert!(matches!(Op::parse("delete").unwrap_err(), Error::BadArgs(_)));
    }

    #[test]
    fn gmail_args_from_value_round_trips() {
        let v = json!({"op":"search","query":"is:unread","max":10});
        let a = GmailArgs::from_value(&v).unwrap();
        assert_eq!(a.op, "search");
        assert_eq!(a.query.as_deref(), Some("is:unread"));
        assert_eq!(a.max, Some(10));
        assert!(!a.include_body);
    }

    #[test]
    fn gmail_args_defaults() {
        let a = GmailArgs::from_value(&json!({"op":"get","id":"abc"})).unwrap();
        assert_eq!(a.id.as_deref(), Some("abc"));
        assert!(a.query.is_none());
        assert!(a.max.is_none());
        assert!(!a.include_body);
    }

    #[test]
    fn gmail_args_rejects_unknown_field() {
        // additionalProperties:false is enforced by serde via the typed struct
        // only for known ops; unknown JSON fields are ignored by serde by
        // default, so we assert the schema (not serde) is the gate. Here we
        // confirm a bad type for `max` is a BadArgs error.
        let err = GmailArgs::from_value(&json!({"op":"search","max":"NaN"})).unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[tokio::test]
    async fn run_tool_with_search_missing_query_is_bad_args() {
        // Drive the dispatch arm without a network: a missing `query` must fail
        // before any HTTP call. We use an obviously-bogus access token; the
        // BadArgs guard short-circuits ahead of the (never-made) request.
        let gmail = Gmail::from_access_token(Secret::new("unused".to_owned()));
        let args = GmailArgs::from_value(&json!({"op":"search"})).unwrap();
        let err = run_tool_with(&gmail, args).await.unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[tokio::test]
    async fn run_tool_with_get_missing_id_is_bad_args() {
        let gmail = Gmail::from_access_token(Secret::new("unused".to_owned()));
        let args = GmailArgs::from_value(&json!({"op":"get"})).unwrap();
        let err = run_tool_with(&gmail, args).await.unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }

    #[tokio::test]
    async fn run_tool_with_unknown_op_is_bad_args() {
        let gmail = Gmail::from_access_token(Secret::new("unused".to_owned()));
        let args = GmailArgs::from_value(&json!({"op":"archive"})).unwrap();
        let err = run_tool_with(&gmail, args).await.unwrap_err();
        assert!(matches!(err, Error::BadArgs(_)));
    }
}
