// SPDX-License-Identifier: Apache-2.0
//! Test-only fake language server (stdio JSON-RPC).
//!
//! Answers `initialize`, rejects every other request with `MethodNotFound`, and
//! on `textDocument/didOpen` publishes one diagnostic for the opened document
//! whose `message` is the trimmed document text and whose `code` is this
//! process's pid. Text `ENV:<NAME>` publishes the value of env var `<NAME>`
//! instead (`<unset>` when absent), and text `LOG` publishes the space-joined log
//! of what the server received so far (method names; `initialize:watch` when
//! the client declared watched-files support; `changed:<uri>` per
//! `workspace/didChangeWatchedFiles` entry).
//!
//! Fault modes, by document text: `SLOW:<ms>` publishes after `<ms>` while
//! still answering requests, `HANG` publishes then stops reading and answering
//! anything, `EXIT` publishes then exits when the document is closed.
//! By argument: `--init-delay=<ms>` reads nothing for `<ms>` after start (a
//! slow-starting server), `--exit-at-start` exits at once, `--untagged` sends
//! publishes without a document `version`.
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

// `pub` so origin-daemon's `lsp_diagnostics_turns` test can run it in-process.
pub fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--exit-at-start") {
        return;
    }
    if let Some(ms) = args.iter().find_map(|a| a.strip_prefix("--init-delay=")) {
        std::thread::sleep(Duration::from_millis(ms.parse().unwrap()));
    }
    let untagged = args.iter().any(|a| a == "--untagged");
    let mut log: Vec<String> = Vec::new();
    let mut exit_on_close = false;
    let mut stdin = BufReader::new(std::io::stdin().lock());
    while let Some(msg) = read_frame(&mut stdin) {
        let method = msg["method"].as_str().unwrap_or_default();
        match method {
            "initialize"
                if !msg["params"]["capabilities"]["workspace"]["didChangeWatchedFiles"].is_null() =>
            {
                log.push("initialize:watch".to_owned());
            }
            "workspace/didChangeWatchedFiles" => {
                for c in msg["params"]["changes"].as_array().into_iter().flatten() {
                    log.push(format!("changed:{}", c["uri"].as_str().unwrap_or_default()));
                }
            }
            _ => log.push(method.to_owned()),
        }
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
        if method == "textDocument/didClose" && exit_on_close {
            return;
        }
        if method != "textDocument/didOpen" {
            continue;
        }
        let doc = &msg["params"]["textDocument"];
        let text = doc["text"].as_str().unwrap_or_default().trim().to_owned();
        let message = if text == "LOG" {
            log.join(" ")
        } else {
            text.strip_prefix("ENV:").map_or_else(
                || text.clone(),
                |name| std::env::var(name).unwrap_or_else(|_| "<unset>".to_owned()),
            )
        };
        let version = if untagged {
            Value::Null
        } else {
            doc["version"].clone()
        };
        let publish = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": doc["uri"],
                "version": version,
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
            "EXIT" => exit_on_close = true,
            _ => {}
        }
    }
}
