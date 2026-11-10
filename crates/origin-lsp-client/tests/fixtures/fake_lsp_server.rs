// SPDX-License-Identifier: Apache-2.0
//! Test-only fake language server (stdio JSON-RPC).
//!
//! Answers `initialize`, rejects every other request with `MethodNotFound`, and
//! on `textDocument/didOpen` publishes one diagnostic for the opened document
//! whose `message` is the trimmed document text and whose `code` is this
//! process's pid. Text `ENV:<NAME>` publishes the value of env var `<NAME>`
//! instead (`<unset>` when absent). Fault modes, by document text:
//! `SLOW:<ms>` publishes after `<ms>` while still answering requests, `HANG`
//! publishes then stops reading and answering anything, `EXIT` publishes then
//! exits.
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use serde_json::{json, Value};

fn send(v: &Value) {
    let body = v.to_string();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

/// Read one frame body; `None` on EOF.
fn read_frame(r: &mut impl BufRead) -> Option<Value> {
    let mut len = None;
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length: ") {
            len = v.parse().ok();
        }
    }
    let mut body = vec![0u8; len?];
    r.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

fn main() {
    let mut stdin = BufReader::new(std::io::stdin().lock());
    while let Some(msg) = read_frame(&mut stdin) {
        let method = msg["method"].as_str().unwrap_or_default();
        if let Some(id) = msg.get("id") {
            if method == "initialize" {
                send(&json!({"jsonrpc": "2.0", "id": id, "result": {"capabilities": {}}}));
            } else {
                send(
                    &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}),
                );
            }
            continue;
        }
        if method != "textDocument/didOpen" {
            continue;
        }
        let doc = &msg["params"]["textDocument"];
        let text = doc["text"].as_str().unwrap_or_default().trim().to_owned();
        let message = text.strip_prefix("ENV:").map_or_else(
            || text.clone(),
            |name| std::env::var(name).unwrap_or_else(|_| "<unset>".to_owned()),
        );
        let publish = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": doc["uri"],
                "version": doc["version"],
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                    "severity": 1,
                    "message": message,
                    "code": std::process::id().to_string(),
                }],
            },
        });
        if let Some(ms) = text.strip_prefix("SLOW:") {
            let delay = Duration::from_millis(ms.parse().unwrap());
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                send(&publish);
            });
            continue;
        }
        send(&publish);
        match text.as_str() {
            "HANG" => std::thread::sleep(Duration::from_secs(3600)),
            "EXIT" => return,
            _ => {}
        }
    }
}
