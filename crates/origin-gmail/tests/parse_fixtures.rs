// SPDX-License-Identifier: Apache-2.0
//! Integration tests: parse representative Gmail REST API responses into the
//! crate's public types. No network — these exercise the pure parser seam.
#![allow(clippy::unwrap_used)]

use origin_gmail::model::{parse_message, parse_messages_list, parse_threads_list};
use origin_gmail::{input_schema, GmailArgs};
use serde_json::json;

/// A realistic `messages.list` page (fields beyond `messages`/`nextPageToken`
/// are ignored by the parser).
const MESSAGES_LIST: &str = r#"{
  "messages": [
    { "id": "18f0a1b2c3d4e5f6", "threadId": "18f0a1b2c3d4e5f6" },
    { "id": "18f0a1b2c3d4e500", "threadId": "18f0a1b2c3d4e4aa" }
  ],
  "nextPageToken": "07471004373189787241",
  "resultSizeEstimate": 2
}"#;

const THREADS_LIST: &str = r#"{
  "threads": [
    { "id": "18f0a1b2c3d4e5f6", "snippet": "Re: Q3 planning sync notes", "historyId": "523112" },
    { "id": "18f0a1b2c3d4e4aa", "snippet": "Your receipt from Acme", "historyId": "523001" }
  ],
  "resultSizeEstimate": 2
}"#;

/// A `format=metadata` `messages.get` response (no decoded body).
const MESSAGE_METADATA: &str = r#"{
  "id": "18f0a1b2c3d4e5f6",
  "threadId": "18f0a1b2c3d4e5f6",
  "labelIds": ["IMPORTANT", "CATEGORY_PERSONAL", "INBOX"],
  "snippet": "Here are the notes from today&#39;s sync",
  "sizeEstimate": 8423,
  "payload": {
    "mimeType": "multipart/alternative",
    "headers": [
      { "name": "Subject", "value": "Re: Q3 planning sync notes" },
      { "name": "From", "value": "Alice <alice@example.com>" },
      { "name": "To", "value": "me@example.com" },
      { "name": "Date", "value": "Mon, 02 Jun 2026 10:14:22 -0700" }
    ]
  }
}"#;

#[test]
fn messages_list_fixture() {
    let page = parse_messages_list(MESSAGES_LIST).unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].id, "18f0a1b2c3d4e5f6");
    assert_eq!(page.items[1].thread_id, "18f0a1b2c3d4e4aa");
    assert_eq!(page.next_page_token.as_deref(), Some("07471004373189787241"));
}

#[test]
fn threads_list_fixture() {
    let page = parse_threads_list(THREADS_LIST).unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].snippet, "Re: Q3 planning sync notes");
    assert!(page.next_page_token.is_none());
}

#[test]
fn message_metadata_fixture() {
    let m = parse_message(MESSAGE_METADATA).unwrap();
    assert_eq!(m.id, "18f0a1b2c3d4e5f6");
    assert_eq!(m.header("Subject"), Some("Re: Q3 planning sync notes"));
    assert_eq!(m.header("from"), Some("Alice <alice@example.com>"));
    assert!(m.label_ids.contains(&"INBOX".to_owned()));
    assert!(m.body.is_empty(), "metadata format carries no decoded body");
}

#[test]
fn message_full_fixture_decodes_body() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let encoded = URL_SAFE_NO_PAD.encode(b"The body text, decoded.");
    let json = format!(
        r#"{{
          "id":"1","threadId":"t",
          "payload":{{
            "mimeType":"multipart/mixed",
            "headers":[{{"name":"Subject","value":"hi"}}],
            "parts":[
              {{"mimeType":"text/html","body":{{"data":"PHA+aGk8L3A+"}}}},
              {{"mimeType":"text/plain","body":{{"data":"{encoded}"}}}}
            ]
          }}
        }}"#
    );
    let m = parse_message(&json).unwrap();
    assert_eq!(m.body, "The body text, decoded.");
}

#[test]
fn public_schema_and_args_align() {
    // The schema advertises `op` as required; a value satisfying it parses.
    let schema = input_schema();
    assert_eq!(schema["required"], json!(["op"]));
    let args = GmailArgs::from_value(&json!({"op":"search","query":"is:starred"})).unwrap();
    assert_eq!(args.op, "search");
    assert_eq!(args.query.as_deref(), Some("is:starred"));
}
