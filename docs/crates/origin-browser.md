# origin-browser

> Dual-backend browser router with one-shot WebFetch and Tavily WebSearch

## Purpose

`origin-browser` provides the agent's web access in three shapes: a stateful
browsing router that runs verbs against a primary "agent-browser" CLI and falls
back to a stealthier "cloak" backend on bot detection; a pure-Rust one-shot
`WebFetch` that GETs a URL and returns reader-mode markdown; and a `WebSearch`
helper. An opt-in `VisualCapture` adds a screenshot + console "visual loop".

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `BrowserRouter` | struct | Primary/fallback router; `new()`, `with_commands(...)`, `run(verb)`. |
| `RouterError` | enum | `Primary(String)` / `Fallback(String)`. |
| `Verb` / `SnapshotResp` | enum/struct | The browsing command protocol and its snapshot response. |
| `VisualCapture` | struct | Opt-in screenshot + console capture loop. |
| `web_fetch::fetch` | fn | One-shot reader-mode fetch → `FetchResult { final_url, content_type, markdown }`. |
| `web_fetch::FetchOptions` / `FetchError` | struct/enum | Timeout, `max_bytes`, user-agent; `Http`/`TooLarge` errors. |
| `web_search::search` | fn | Tavily-backed search entry point. |

Internal modules: `agent_browser` (primary CLI client), `cloak` (fallback CLI
client), `detectors` (`classify` → `Verdict`), `protocol`, `router`, `visual`.

## Key types

```rust
pub struct BrowserRouter {
    primary: AgentBrowserClient,
    cloak: CloakClient,
    state: HashMap<String, SessionState>,
}

// web_fetch
pub struct FetchOptions {
    pub timeout: Duration,      // default 30s
    pub max_bytes: usize,       // default 10 MiB
    pub user_agent: String,     // "origin/<version>"
}
pub struct FetchResult { pub final_url: String, pub content_type: String, pub markdown: String }
```

## How it works

The router's policy is fixed: try `agent-browser` first; if `detectors::classify`
returns a bot-detected `Verdict`, replay the **same** verb against `CloakBrowser`
and emit the cloak response instead. After two consecutive cloak fallbacks in a
session the router marks that session `sticky` so future verbs skip the primary
entirely. `WebFetch` is a separate, subprocess-free path: `reqwest` GET →
`readability` extraction → `html2md`, bounded by `FetchOptions::max_bytes`
(`FetchError::TooLarge`). This is preferred for one-shot summaries where the
stateful snapshot/ref protocol would be overkill.

```
run(verb) ──▶ AgentBrowserClient ──response──▶ classify
                                                  │bot-detected
                                                  ▼
                              CloakClient (replay) ──▶ response  (streak++ → sticky)
```

## Dependencies & features

No cargo features. Networking via `reqwest`; reader mode via `readability` +
`html2md`; URL handling via `url`; child backends and async via `tokio`;
diagnostics via `tracing`. `origin-keyvault` supplies the Tavily API key for
`WebSearch`.

## Used by

```
crates/origin-browser/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-tools/Cargo.toml
```

## Testing

The router is constructed in tests via `with_commands`, which spawns explicit
stub CLIs so primary/fallback/sticky behaviour is exercised without real
browsers. `detectors::classify` has its own fixture tests; `web_fetch` is
validated against canned HTML. `tempfile` backs fixture files.

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
