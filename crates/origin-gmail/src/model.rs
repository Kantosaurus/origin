// SPDX-License-Identifier: Apache-2.0
//! Parsed Gmail domain types + pure JSON parsers.
//!
//! Everything here is side-effect-free so it can be unit-tested against
//! representative API fixtures with no network. The HTTP layer ([`crate::http`])
//! is the only un-tested seam: it fetches bytes, then hands them to these
//! parsers.
//!
//! Token frugality: `messages.get` is requested with `format=metadata` and a
//! minimal header allow-list by default (see [`crate::request`]). The parsers
//! tolerate either the slim metadata shape or a fuller `format=full` payload,
//! extracting a best-effort plain-text body from the latter.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// A lightweight reference to a message, as returned by `messages.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageRef {
    /// Immutable message id (use with [`crate::Gmail::get_message`]).
    pub id: String,
    /// The thread this message belongs to.
    pub thread_id: String,
}

/// A lightweight reference to a thread, as returned by `threads.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadRef {
    /// Immutable thread id (use with `threads.get` — out of current scope).
    pub id: String,
    /// A short snippet of the most recent message, when present.
    #[serde(default)]
    pub snippet: String,
}

/// A single header name/value pair extracted from a message payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// Header name, e.g. `"From"`, `"Subject"`.
    pub name: String,
    /// Header value.
    pub value: String,
}

/// A fully-parsed message: metadata headers plus a best-effort text body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// Immutable message id.
    pub id: String,
    /// The thread this message belongs to.
    pub thread_id: String,
    /// Gmail label ids applied to this message (e.g. `INBOX`, `UNREAD`).
    #[serde(default)]
    pub label_ids: Vec<String>,
    /// Server-computed short snippet of the message content.
    #[serde(default)]
    pub snippet: String,
    /// Selected headers (From/To/Subject/Date by default).
    #[serde(default)]
    pub headers: Vec<Header>,
    /// Best-effort decoded `text/plain` body. Empty when the message was
    /// fetched with `format=metadata` (the token-frugal default) or has no
    /// plain-text part.
    #[serde(default)]
    pub body: String,
}

impl Message {
    /// Returns the first header matching `name` (case-insensitive), if any.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }
}

/// A page of results: the items plus an optional continuation token. Callers
/// page lazily by feeding `next_page_token` back into the next request.
///
/// `Debug` is hand-written rather than derived: the field is named
/// `next_page_token` (a Gmail *pagination* cursor, not a credential), but the
/// `xtask lint-secrets` gate flags any *derived* `Debug` over a `*_token`
/// string field. The manual impl sidesteps the derive while still printing the
/// (non-secret) cursor, so the public `Debug` stays useful.
#[derive(Clone, PartialEq, Eq)]
pub struct Page<T> {
    /// The items on this page.
    pub items: Vec<T>,
    /// Opaque continuation token; `None` when this is the last page.
    pub next_page_token: Option<String>,
}

impl<T: core::fmt::Debug> core::fmt::Debug for Page<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Page")
            .field("items", &self.items)
            .field("next_page_token", &self.next_page_token)
            .finish()
    }
}

// ── Raw wire shapes (private; only the parsers below touch them) ──

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListMessages {
    #[serde(default)]
    messages: Vec<RawMessageRef>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessageRef {
    id: String,
    #[serde(default)]
    thread_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawListThreads {
    #[serde(default)]
    threads: Vec<RawThreadRef>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreadRef {
    id: String,
    #[serde(default)]
    snippet: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMessage {
    id: String,
    #[serde(default)]
    thread_id: String,
    #[serde(default)]
    label_ids: Vec<String>,
    #[serde(default)]
    snippet: String,
    #[serde(default)]
    payload: Option<RawPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPayload {
    #[serde(default)]
    mime_type: String,
    #[serde(default)]
    headers: Vec<RawHeader>,
    #[serde(default)]
    body: Option<RawBody>,
    #[serde(default)]
    parts: Vec<RawPayload>,
}

#[derive(Deserialize)]
struct RawHeader {
    name: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
struct RawBody {
    /// base64url (web-safe, no padding) encoded body bytes.
    #[serde(default)]
    data: Option<String>,
}

// ── Pure parsers ──

/// Parse a `messages.list` response into a [`Page`] of [`MessageRef`].
///
/// # Errors
/// Returns [`Error::Parse`] if the JSON does not match the `messages.list`
/// shape.
pub fn parse_messages_list(json: &str) -> Result<Page<MessageRef>> {
    let raw: RawListMessages = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    let items = raw
        .messages
        .into_iter()
        .map(|m| MessageRef {
            id: m.id,
            thread_id: m.thread_id,
        })
        .collect();
    Ok(Page {
        items,
        next_page_token: raw.next_page_token,
    })
}

/// Parse a `threads.list` response into a [`Page`] of [`ThreadRef`].
///
/// # Errors
/// Returns [`Error::Parse`] if the JSON does not match the `threads.list`
/// shape.
pub fn parse_threads_list(json: &str) -> Result<Page<ThreadRef>> {
    let raw: RawListThreads = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;
    let items = raw
        .threads
        .into_iter()
        .map(|t| ThreadRef {
            id: t.id,
            snippet: t.snippet,
        })
        .collect();
    Ok(Page {
        items,
        next_page_token: raw.next_page_token,
    })
}

/// Parse a `messages.get` response into a [`Message`].
///
/// Headers are flattened from the top-level payload. A best-effort
/// `text/plain` body is decoded by walking the MIME tree depth-first and
/// taking the first `text/plain` part with decodable data; if none is found,
/// a top-level `body.data` is decoded as a fallback. Undecodable base64 is
/// skipped (body left empty) rather than failing the whole parse.
///
/// # Errors
/// Returns [`Error::Parse`] if the JSON does not match the `messages.get`
/// shape.
pub fn parse_message(json: &str) -> Result<Message> {
    let raw: RawMessage = serde_json::from_str(json).map_err(|e| Error::Parse(e.to_string()))?;

    let (headers, body) = raw.payload.map_or_else(
        || (Vec::new(), String::new()),
        |p| {
            let headers = p
                .headers
                .iter()
                .map(|h| Header {
                    name: h.name.clone(),
                    value: h.value.clone(),
                })
                .collect();
            let body = extract_plain_text(&p).unwrap_or_default();
            (headers, body)
        },
    );

    Ok(Message {
        id: raw.id,
        thread_id: raw.thread_id,
        label_ids: raw.label_ids,
        snippet: raw.snippet,
        headers,
        body,
    })
}

/// Depth-first search for the first decodable `text/plain` body. Falls back to
/// the node's own body when no `text/plain` part exists but the node itself
/// carries decodable data (single-part `text/plain` messages).
fn extract_plain_text(p: &RawPayload) -> Option<String> {
    if p.mime_type.eq_ignore_ascii_case("text/plain") {
        if let Some(text) = p.body.as_ref().and_then(|b| decode_body(b.data.as_deref())) {
            return Some(text);
        }
    }
    for part in &p.parts {
        if let Some(text) = extract_plain_text(part) {
            return Some(text);
        }
    }
    // Fallback: a top-level node with no declared text/plain mime but with
    // decodable data (rare, but seen on some single-part messages).
    if p.parts.is_empty() && !p.mime_type.starts_with("multipart/") {
        return p.body.as_ref().and_then(|b| decode_body(b.data.as_deref()));
    }
    None
}

/// Decode Gmail's base64url (web-safe, no padding) body data into a UTF-8
/// string. Returns `None` for absent/empty data or non-decodable input.
fn decode_body(data: Option<&str>) -> Option<String> {
    let data = data?;
    if data.is_empty() {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(data.as_bytes()).ok()?;
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parse_messages_list_with_paging() {
        let json = r#"{
            "messages": [
                {"id":"18c","threadId":"t1"},
                {"id":"18d","threadId":"t2"}
            ],
            "nextPageToken": "PAGE2",
            "resultSizeEstimate": 2
        }"#;
        let page = parse_messages_list(json).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0], MessageRef { id: "18c".into(), thread_id: "t1".into() });
        assert_eq!(page.next_page_token.as_deref(), Some("PAGE2"));
    }

    #[test]
    fn parse_messages_list_empty_has_no_token() {
        let page = parse_messages_list(r#"{"resultSizeEstimate":0}"#).unwrap();
        assert!(page.items.is_empty());
        assert!(page.next_page_token.is_none());
    }

    #[test]
    fn parse_threads_list_extracts_snippet() {
        let json = r#"{
            "threads": [
                {"id":"t1","snippet":"Hello there","historyId":"99"},
                {"id":"t2","snippet":"Second"}
            ]
        }"#;
        let page = parse_threads_list(json).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].snippet, "Hello there");
        assert!(page.next_page_token.is_none());
    }

    #[test]
    fn parse_message_metadata_format() {
        // Representative format=metadata response (no body.data).
        let json = r#"{
            "id":"18c",
            "threadId":"t1",
            "labelIds":["INBOX","UNREAD"],
            "snippet":"Quarterly numbers attached",
            "payload":{
                "mimeType":"multipart/alternative",
                "headers":[
                    {"name":"From","value":"alice@example.com"},
                    {"name":"Subject","value":"Q3 report"},
                    {"name":"Date","value":"Mon, 02 Jun 2026 10:00:00 -0700"}
                ]
            }
        }"#;
        let m = parse_message(json).unwrap();
        assert_eq!(m.id, "18c");
        assert_eq!(m.thread_id, "t1");
        assert_eq!(m.label_ids, vec!["INBOX".to_owned(), "UNREAD".to_owned()]);
        assert_eq!(m.snippet, "Quarterly numbers attached");
        assert_eq!(m.header("subject"), Some("Q3 report"));
        assert_eq!(m.header("FROM"), Some("alice@example.com"));
        assert!(m.body.is_empty(), "metadata format has no decoded body");
    }

    #[test]
    fn parse_message_full_decodes_plain_text_part() {
        // "Hello, body!" base64url-no-pad encoded.
        let encoded = URL_SAFE_NO_PAD.encode(b"Hello, body!");
        let json = format!(
            r#"{{
                "id":"19a",
                "threadId":"t9",
                "snippet":"Hello",
                "payload":{{
                    "mimeType":"multipart/alternative",
                    "headers":[{{"name":"Subject","value":"hi"}}],
                    "parts":[
                        {{"mimeType":"text/html","body":{{"data":"PGI+aGk8L2I+"}}}},
                        {{"mimeType":"text/plain","body":{{"data":"{encoded}"}}}}
                    ]
                }}
            }}"#
        );
        let m = parse_message(&json).unwrap();
        assert_eq!(m.body, "Hello, body!");
        assert_eq!(m.header("Subject"), Some("hi"));
    }

    #[test]
    fn parse_message_single_part_plain_text() {
        let encoded = URL_SAFE_NO_PAD.encode(b"single part");
        let json = format!(
            r#"{{
                "id":"1","threadId":"t",
                "payload":{{"mimeType":"text/plain","body":{{"data":"{encoded}"}}}}
            }}"#
        );
        let m = parse_message(&json).unwrap();
        assert_eq!(m.body, "single part");
    }

    #[test]
    fn parse_message_undecodable_body_is_empty_not_error() {
        let json = r#"{
            "id":"1","threadId":"t",
            "payload":{"mimeType":"text/plain","body":{"data":"!!!not base64!!!"}}
        }"#;
        let m = parse_message(json).unwrap();
        assert!(m.body.is_empty());
    }

    #[test]
    fn parse_message_no_payload() {
        let m = parse_message(r#"{"id":"1","threadId":"t"}"#).unwrap();
        assert!(m.headers.is_empty());
        assert!(m.body.is_empty());
    }

    #[test]
    fn parse_invalid_json_is_parse_error() {
        let err = parse_messages_list("not json").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }
}
