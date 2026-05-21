# Superpowers Skills + Browser/Web Tools Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bundle 14 superpowers skills into origin, add a dual-backend browser tool (agent-browser primary + CloakBrowser stealth fallback) with transparent agent-detection fallback, and add WebFetch/WebSearch builtin tools.

**Architecture:** New `origin-browser` crate holds the routing logic, detection classifier, two stdio-JSON subprocess clients, plus `web_fetch` (pure Rust) and `web_search` (Tavily HTTP). `origin-tools` exposes them as three new builtins. `origin-skills` gains an `include_dir!` embedded loader and a `load_all` API that merges embedded skills with `~/.origin/skills/<name>/SKILL.md` overrides.

**Tech Stack:** Rust 1.83 (MSRV pinned), `include_dir` for embedded files, `reqwest` (already in workspace) for HTTP, `tokio::process` for subprocesses, Node ≥18 + npm for the vendored sidecar.

**Spec:** `docs/superpowers/specs/2026-05-21-skills-and-browser-integration-design.md`

---

## File map

**Create**

- `crates/origin-browser/Cargo.toml`
- `crates/origin-browser/src/lib.rs`
- `crates/origin-browser/src/protocol.rs` — shared stdio-JSON verb/response types
- `crates/origin-browser/src/detectors.rs` — bot-detection classifier
- `crates/origin-browser/src/agent_browser.rs` — agent-browser subprocess client
- `crates/origin-browser/src/cloak.rs` — CloakBrowser subprocess client
- `crates/origin-browser/src/router.rs` — primary/fallback policy
- `crates/origin-browser/src/web_fetch.rs`
- `crates/origin-browser/src/web_search.rs`
- `crates/origin-browser/tests/detectors_table.rs`
- `crates/origin-browser/tests/router_fallback.rs`
- `crates/origin-browser/tests/fakes/agent_browser_fake.mjs` — Node test stub
- `crates/origin-browser/tests/fakes/cloak_fake.mjs`
- `crates/origin-skills/embedded/superpowers/<14 skill dirs>/SKILL.md`
- `crates/origin-skills/src/embedded.rs`
- `crates/origin-tools/src/builtins/browser.rs`
- `crates/origin-tools/src/builtins/web_fetch.rs`
- `crates/origin-tools/src/builtins/web_search.rs`
- `vendor/cloak-browser/package.json`
- `vendor/cloak-browser/cloak-cli.mjs`
- `vendor/cloak-browser/README.md`

**Modify**

- `Cargo.toml` (workspace) — add `include_dir`, `readability`, `html2md` to `[workspace.dependencies]`
- `crates/origin-skills/Cargo.toml` — add `include_dir`
- `crates/origin-skills/src/lib.rs` — export new module + `load_all`
- `crates/origin-skills/src/loader.rs` — add `load_all(user_root)` merger
- `crates/origin-tools/Cargo.toml` — add `origin-browser` dep
- `crates/origin-tools/src/builtins/mod.rs` — add three modules

---

## Task 0: Bootstrap origin-browser crate + protocol types

**Files:**
- Create: `crates/origin-browser/Cargo.toml`
- Create: `crates/origin-browser/src/lib.rs`
- Create: `crates/origin-browser/src/protocol.rs`
- Modify: `Cargo.toml` (workspace) — append three deps

- [ ] **Step 1: Add workspace deps**

Add to `Cargo.toml` under `[workspace.dependencies]` (alphabetical with neighbors):

```toml
include_dir = "0.7"
readability = "0.3"
html2md     = "0.2"
```

- [ ] **Step 2: Create the crate Cargo.toml**

Write `crates/origin-browser/Cargo.toml`:

```toml
[package]
name = "origin-browser"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { workspace = true }
readability = { workspace = true }
html2md = { workspace = true }
tokio = { version = "1", features = ["macros", "rt-multi-thread", "process", "io-util", "sync", "time"] }
thiserror = "1"
regex = "1"
tracing = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Write protocol.rs (shared verb/response types)**

```rust
//! Stdio-JSON verb protocol shared by agent-browser and CloakBrowser backends.
//!
//! Wire format: one JSON object per line in each direction.
//! Both subprocess clients speak this; the router never sees raw bytes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "v", rename_all = "lowercase")]
pub enum Verb {
    Open    { url: String,  session: String },
    Click   { r#ref: String, session: String },
    Fill    { r#ref: String, value: String, session: String },
    Extract { r#ref: String, session: String },
    Snapshot{ session: String },
    Screenshot { session: String, path: String },
    Close   { session: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotResp {
    pub ok: bool,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub snapshot: Option<String>,
    #[serde(default)]
    pub html: Option<String>,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}
```

- [ ] **Step 4: Write lib.rs**

```rust
//! origin-browser: dual-backend browser routing + WebFetch + WebSearch.
//!
//! Public surface is the three top-level entry points:
//!  - `BrowserRouter::run(verb)` for stateful browsing
//!  - `web_fetch::fetch(url)` for one-shot reader-mode fetches
//!  - `web_search::search(query)` for Tavily search
pub mod protocol;
pub mod detectors;
pub mod agent_browser;
pub mod cloak;
pub mod router;
pub mod web_fetch;
pub mod web_search;

pub use protocol::{Verb, SnapshotResp};
pub use router::{BrowserRouter, RouterError};
```

Stub the four `pub mod` files with `// task N` placeholders that contain `#![allow(dead_code)]` so the crate compiles standalone:

```rust
// crates/origin-browser/src/detectors.rs
#![allow(dead_code)]
```

(repeat for `agent_browser.rs`, `cloak.rs`, `router.rs`, `web_fetch.rs`, `web_search.rs`)

- [ ] **Step 5: Register the crate in workspace**

The workspace `Cargo.toml` already has `members = ["crates/*", "xtask"]` — no change needed; the new crate is auto-included by glob.

- [ ] **Step 6: Verify the crate builds**

Run: `cargo build -p origin-browser`
Expected: clean compile, possibly with `dead_code` warnings on the stubs (we allowed them).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/origin-browser
git commit -m "feat(origin-browser): bootstrap crate + stdio-JSON protocol types"
```

---

## Task A1: Bundle the 14 superpowers skills + embedded loader

**Files:**
- Create: `crates/origin-skills/embedded/superpowers/<name>/SKILL.md` × 14
- Create: `crates/origin-skills/src/embedded.rs`
- Modify: `crates/origin-skills/Cargo.toml` — add `include_dir`
- Modify: `crates/origin-skills/src/lib.rs` — export new module + `load_all`
- Modify: `crates/origin-skills/src/loader.rs` — add `load_all(user_root)`
- Create: `crates/origin-skills/tests/embedded_skills.rs`

- [ ] **Step 1: Write the failing test first (TDD)**

Create `crates/origin-skills/tests/embedded_skills.rs`:

```rust
use origin_skills::{load_all, load_embedded};
use tempfile::tempdir;

#[test]
fn embedded_includes_all_14_superpowers_skills() {
    let skills = load_embedded();
    let names: Vec<&str> = skills.iter().map(|s| s.front.name.as_str()).collect();
    let expected = [
        "brainstorming", "dispatching-parallel-agents", "executing-plans",
        "finishing-a-development-branch", "receiving-code-review",
        "requesting-code-review", "subagent-driven-development",
        "systematic-debugging", "test-driven-development",
        "using-git-worktrees", "using-superpowers",
        "verification-before-completion", "writing-plans", "writing-skills",
    ];
    for want in expected {
        assert!(names.iter().any(|n| *n == want), "missing embedded skill: {want}; got {names:?}");
    }
    assert_eq!(skills.len(), 14, "expected exactly 14 embedded skills, got {}", skills.len());
}

#[test]
fn user_skill_overrides_embedded_by_name() {
    let dir = tempdir().unwrap();
    let user_root = dir.path();
    let brainstorm_dir = user_root.join("brainstorming");
    std::fs::create_dir_all(&brainstorm_dir).unwrap();
    std::fs::write(
        brainstorm_dir.join("SKILL.md"),
        "---\nname: brainstorming\ndescription: user override\n---\n# user body\n",
    ).unwrap();

    let all = load_all(user_root).unwrap();
    let bs = all.iter().find(|s| s.front.name == "brainstorming").unwrap();
    assert_eq!(bs.front.description, "user override");
    assert_eq!(all.len(), 14, "merging should not change count when override matches one embedded");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p origin-skills --test embedded_skills`
Expected: FAIL — `load_embedded` and `load_all` do not exist yet.

- [ ] **Step 3: Add include_dir to Cargo.toml**

In `crates/origin-skills/Cargo.toml` under `[dependencies]`, add:

```toml
include_dir = { workspace = true }
```

- [ ] **Step 4: Copy the 14 superpowers skill directories**

For each of the 14 skills listed in Step 1, run:

```powershell
$src  = "C:\Users\wooai\.claude\plugins\cache\claude-plugins-official\superpowers\5.1.0\skills"
$dest = "C:\Users\wooai\Documents\origin\crates\origin-skills\embedded\superpowers"
New-Item -ItemType Directory -Force $dest | Out-Null
Get-ChildItem $src -Directory | ForEach-Object {
    Copy-Item $_.FullName -Destination $dest -Recurse -Force
}
```

Expected: 14 subdirectories under `crates/origin-skills/embedded/superpowers/`, each containing at minimum `SKILL.md`.

- [ ] **Step 5: Write embedded.rs**

Create `crates/origin-skills/src/embedded.rs`:

```rust
//! Embedded superpowers skill bodies. Walked at runtime by `load_embedded`.
//!
//! Built-time inclusion means the binary ships with every skill; users do not
//! need to copy files anywhere to use them.

use crate::frontmatter::{parse_frontmatter, ParsedSkill};
use crate::loader::{Skill, SkillHash};

static EMBEDDED: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/embedded/superpowers");

/// Walk the embedded `superpowers/` tree and return every `SKILL.md` parsed
/// into a [`Skill`]. Malformed frontmatter is treated as a build-time bug —
/// we panic in that case because the skills are vendored, not user input.
#[must_use]
pub fn load_embedded() -> Vec<Skill> {
    let mut out = Vec::new();
    for dir in EMBEDDED.dirs() {
        let Some(file) = dir.get_file(format!("{}/SKILL.md", dir.path().display())) else {
            continue;
        };
        let raw = std::str::from_utf8(file.contents()).expect("embedded SKILL.md is utf-8");
        let ParsedSkill { front, body } =
            parse_frontmatter(raw).expect("embedded SKILL.md frontmatter is valid");
        let body_hash = SkillHash(*blake3::hash(body.as_bytes()).as_bytes());
        out.push(Skill {
            front,
            body,
            body_hash,
            source: file.path().to_path_buf(),
        });
    }
    out
}
```

- [ ] **Step 6: Extend loader.rs with load_all**

Append to `crates/origin-skills/src/loader.rs`:

```rust
/// Load embedded skills plus any user overrides from `user_root`, with user
/// entries taking precedence on name collision. Missing `user_root` is fine —
/// embedded skills are still returned.
///
/// # Errors
/// Forwards [`LoaderError`] from `load_skills_dir` when `user_root` exists but
/// a file under it is malformed.
pub fn load_all(user_root: &std::path::Path) -> Result<Vec<Skill>, LoaderError> {
    let mut acc = crate::embedded::load_embedded();
    if user_root.exists() {
        let user = load_skills_dir(user_root)?;
        // Index embedded by name for O(1) replacement.
        let mut by_name: std::collections::HashMap<String, usize> = acc
            .iter()
            .enumerate()
            .map(|(i, s)| (s.front.name.clone(), i))
            .collect();
        for skill in user {
            match by_name.get(&skill.front.name).copied() {
                Some(i) => acc[i] = skill,
                None => {
                    by_name.insert(skill.front.name.clone(), acc.len());
                    acc.push(skill);
                }
            }
        }
    }
    Ok(acc)
}
```

- [ ] **Step 7: Update lib.rs**

In `crates/origin-skills/src/lib.rs`, add:

```rust
pub mod embedded;
pub use embedded::load_embedded;
pub use loader::load_all;
```

- [ ] **Step 8: Run the test to verify it passes**

Run: `cargo test -p origin-skills --test embedded_skills`
Expected: PASS, both tests.

- [ ] **Step 9: Run the full origin-skills test suite to confirm no regressions**

Run: `cargo test -p origin-skills`
Expected: all tests pass.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-skills Cargo.toml
git commit -m "feat(origin-skills): embed 14 superpowers skills + load_all merger"
```

---

## Task B1: Detectors module (bot-detection classifier)

**Files:**
- Modify: `crates/origin-browser/src/detectors.rs`
- Create: `crates/origin-browser/tests/detectors_table.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-browser/tests/detectors_table.rs`:

```rust
use origin_browser::protocol::SnapshotResp;
use origin_browser::detectors::{classify, Verdict};

fn resp(status: Option<u16>, html: &str, title: &str) -> SnapshotResp {
    SnapshotResp {
        ok: true, r#ref: None, snapshot: None,
        html: Some(html.into()),
        status, title: Some(title.into()), error: None,
    }
}

#[test]
fn clean_html_is_clean() {
    let v = classify(&resp(Some(200), "<html><body>Hello</body></html>", "OK"));
    assert!(matches!(v, Verdict::Clean), "got {v:?}");
}

#[test]
fn cloudflare_challenge_detected_by_body() {
    let html = r#"<html><body><script src="/cdn-cgi/challenge-platform/__cf_chl_"></script></body></html>"#;
    let v = classify(&resp(Some(403), html, "Just a moment..."));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn recaptcha_detected_by_class() {
    let html = r#"<div class="g-recaptcha" data-sitekey="abc"></div>"#;
    let v = classify(&resp(Some(200), html, "Login"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn datadome_detected() {
    let v = classify(&resp(Some(200), "<script>var datadome='abc'</script>", "Loading"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn title_verify_human_detected() {
    let v = classify(&resp(Some(200), "<html></html>", "Verify you are human"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}

#[test]
fn rate_limit_detected() {
    let v = classify(&resp(Some(429), "<html></html>", "Too many requests"));
    assert!(matches!(v, Verdict::BotDetected(_)));
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p origin-browser --test detectors_table`
Expected: FAIL — `classify` and `Verdict` do not exist yet.

- [ ] **Step 3: Implement detectors.rs**

Replace `crates/origin-browser/src/detectors.rs` with:

```rust
//! Bot-detection signal classifier.
//!
//! Pure: takes a `SnapshotResp`, returns a `Verdict`. Add new signatures as
//! one-liners in `BOT_PATTERNS`; pair each with a test row in
//! `tests/detectors_table.rs`.

use crate::protocol::SnapshotResp;
use regex::RegexBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    BotDetected(&'static str),
}

const BOT_PATTERNS: &[(&str, &str)] = &[
    ("cf-chl-",       "cloudflare-challenge"),
    ("__cf_chl_",     "cloudflare-challenge"),
    ("cf-mitigated",  "cloudflare-mitigation"),
    ("g-recaptcha",   "recaptcha"),
    ("h-captcha",     "hcaptcha"),
    ("px-captcha",    "perimeterx"),
    ("_pxhd",         "perimeterx"),
    ("datadome",      "datadome"),
    ("_Incapsula_Resource", "imperva-incapsula"),
    ("kasada",        "kasada"),
];

#[must_use]
pub fn classify(r: &SnapshotResp) -> Verdict {
    if matches!(r.status, Some(429)) {
        return Verdict::BotDetected("http-429");
    }
    if let Some(title) = r.title.as_deref() {
        let re = RegexBuilder::new(r"just a moment|attention required|access denied|verify you are human")
            .case_insensitive(true)
            .build()
            .expect("static regex compiles");
        if re.is_match(title) {
            return Verdict::BotDetected("title-human-check");
        }
    }
    if let Some(html) = r.html.as_deref() {
        for (needle, label) in BOT_PATTERNS {
            if html.contains(needle) {
                return Verdict::BotDetected(label);
            }
        }
    }
    if matches!(r.status, Some(403)) {
        // 403 without an explicit signature still gets flagged — sites that
        // 403 a snapshot fetch usually mean "not for bots".
        return Verdict::BotDetected("http-403");
    }
    Verdict::Clean
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p origin-browser --test detectors_table`
Expected: all 6 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-browser/src/detectors.rs crates/origin-browser/tests/detectors_table.rs
git commit -m "feat(origin-browser): bot-detection classifier with table tests"
```

---

## Task B2: WebFetch module (pure Rust, reader-mode)

**Files:**
- Modify: `crates/origin-browser/src/web_fetch.rs`
- Create: `crates/origin-browser/tests/web_fetch_basic.rs`

- [ ] **Step 1: Write a failing test against a local hyper server**

Create `crates/origin-browser/tests/web_fetch_basic.rs`:

```rust
use origin_browser::web_fetch::{fetch, FetchOptions};

#[tokio::test]
async fn fetch_extracts_main_content_as_markdown() {
    // Spin up a one-shot HTTP server returning a known HTML page.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await.unwrap();
        let body = "<html><head><title>Hi</title></head><body><article><h1>Hi</h1><p>World.</p></article></body></html>";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });

    let url = format!("http://{addr}/page");
    let out = fetch(&url, FetchOptions::default()).await.unwrap();
    assert!(out.markdown.contains("World."), "got: {}", out.markdown);
    assert_eq!(out.final_url, url);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p origin-browser --test web_fetch_basic`
Expected: FAIL — `fetch`/`FetchOptions` don't exist.

- [ ] **Step 3: Implement web_fetch.rs**

Replace `crates/origin-browser/src/web_fetch.rs`:

```rust
//! WebFetch: GET a URL, run readability over the HTML, return markdown.
//!
//! Pure Rust path — no subprocess required. Used for one-shot reader-mode
//! summaries where the router's snapshot/ref protocol would be overkill.

use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub timeout: Duration,
    pub max_bytes: usize,
    pub user_agent: String,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_bytes: 10 * 1024 * 1024,
            user_agent: concat!("origin/", env!("CARGO_PKG_VERSION")).into(),
        }
    }
}

#[derive(Debug)]
pub struct FetchResult {
    pub final_url: String,
    pub content_type: String,
    pub markdown: String,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("body too large: {0} bytes > limit {1}")]
    TooLarge(usize, usize),
    #[error("readability: {0}")]
    Readability(String),
    #[error("url parse: {0}")]
    Url(#[from] url::ParseError),
}

/// Fetch `url` and return reader-mode markdown.
///
/// # Errors
/// Returns [`FetchError`] for HTTP failure, oversized body, or readability
/// failure.
pub async fn fetch(url: &str, opts: FetchOptions) -> Result<FetchResult, FetchError> {
    let client = reqwest::Client::builder()
        .timeout(opts.timeout)
        .user_agent(&opts.user_agent)
        .build()?;
    let resp = client.get(url).send().await?;
    let final_url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = resp.bytes().await?;
    if bytes.len() > opts.max_bytes {
        return Err(FetchError::TooLarge(bytes.len(), opts.max_bytes));
    }
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let parsed_url = url::Url::parse(&final_url)?;
    let product = readability::extractor::extract(&mut html.as_bytes(), &parsed_url)
        .map_err(|e| FetchError::Readability(e.to_string()))?;
    let markdown = html2md::parse_html(&product.content);
    Ok(FetchResult { final_url, content_type, markdown })
}
```

Add `url = "2"` to `crates/origin-browser/Cargo.toml` `[dependencies]` (transitive from reqwest but we use it directly).

- [ ] **Step 4: Run test to verify pass**

Run: `cargo test -p origin-browser --test web_fetch_basic`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-browser/src/web_fetch.rs crates/origin-browser/tests/web_fetch_basic.rs crates/origin-browser/Cargo.toml
git commit -m "feat(origin-browser): WebFetch with readability + html2md"
```

---

## Task B3: WebSearch (Tavily) module

**Files:**
- Modify: `crates/origin-browser/src/web_search.rs`
- Create: `crates/origin-browser/tests/web_search_basic.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/origin-browser/tests/web_search_basic.rs`:

```rust
use origin_browser::web_search::{search_with_endpoint, SearchOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn returns_parsed_results_from_tavily_shaped_response() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf).await.unwrap();
        let body = r#"{"results":[{"title":"T","url":"https://x","content":"snip"}]}"#;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(), body
        );
        s.write_all(resp.as_bytes()).await.unwrap();
    });
    let opts = SearchOptions { api_key: "k".into(), count: 5 };
    let r = search_with_endpoint(&format!("http://{addr}/search"), "q", opts).await.unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].title, "T");
    assert_eq!(r[0].url, "https://x");
    assert_eq!(r[0].snippet, "snip");
}

#[tokio::test]
async fn errors_clearly_when_api_key_missing() {
    std::env::remove_var("TAVILY_API_KEY");
    let err = origin_browser::web_search::search("q", 5).await.unwrap_err();
    assert!(format!("{err}").to_lowercase().contains("tavily_api_key"));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p origin-browser --test web_search_basic`
Expected: FAIL — types don't exist.

- [ ] **Step 3: Implement web_search.rs**

Replace `crates/origin-browser/src/web_search.rs`:

```rust
//! WebSearch via Tavily.
//!
//! Tavily was chosen over Brave/SerpAPI for LLM-friendly snippets and a
//! single-endpoint API. The function takes the endpoint as a parameter in
//! the `_with_endpoint` variant so tests can point at a local server.

use serde::{Deserialize, Serialize};
use thiserror::Error;

const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub api_key: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("TAVILY_API_KEY is not set")]
    NoApiKey,
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("parse: {0}")]
    Parse(String),
}

#[derive(Serialize)]
struct ReqBody<'a> { api_key: &'a str, query: &'a str, max_results: usize }
#[derive(Deserialize)]
struct RawResp { results: Vec<RawHit> }
#[derive(Deserialize)]
struct RawHit { title: String, url: String, content: String }

/// Search Tavily; reads `TAVILY_API_KEY` from env.
///
/// # Errors
/// [`SearchError::NoApiKey`] if env unset, otherwise forwards HTTP/parse errors.
pub async fn search(query: &str, count: usize) -> Result<Vec<SearchHit>, SearchError> {
    let key = std::env::var("TAVILY_API_KEY").map_err(|_| SearchError::NoApiKey)?;
    search_with_endpoint(TAVILY_ENDPOINT, query, SearchOptions { api_key: key, count }).await
}

/// Test-visible variant: same logic, custom endpoint.
///
/// # Errors
/// Forwards HTTP/parse errors from the upstream.
pub async fn search_with_endpoint(
    endpoint: &str,
    query: &str,
    opts: SearchOptions,
) -> Result<Vec<SearchHit>, SearchError> {
    let client = reqwest::Client::new();
    let resp = client
        .post(endpoint)
        .json(&ReqBody { api_key: &opts.api_key, query, max_results: opts.count })
        .send()
        .await?;
    let raw: RawResp = resp.json().await.map_err(|e| SearchError::Parse(e.to_string()))?;
    Ok(raw.results.into_iter().map(|h| SearchHit {
        title: h.title, url: h.url, snippet: h.content,
    }).collect())
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p origin-browser --test web_search_basic`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-browser/src/web_search.rs crates/origin-browser/tests/web_search_basic.rs
git commit -m "feat(origin-browser): WebSearch via Tavily"
```

---

## Task B4: Vendor CloakBrowser sidecar + thin CLI

**Files:**
- Create: `vendor/cloak-browser/package.json`
- Create: `vendor/cloak-browser/cloak-cli.mjs`
- Create: `vendor/cloak-browser/README.md`

- [ ] **Step 1: Write package.json**

```json
{
  "name": "origin-cloak-sidecar",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "bin": { "cloak-cli": "./cloak-cli.mjs" },
  "dependencies": {
    "cloak-browser": "github:CloakHQ/CloakBrowser#main",
    "playwright-core": "^1.45.0"
  },
  "engines": { "node": ">=18" }
}
```

- [ ] **Step 2: Write the CLI translation layer**

`vendor/cloak-browser/cloak-cli.mjs`:

```js
#!/usr/bin/env node
// Stdio-JSON sidecar for CloakBrowser.
// Reads one JSON verb per line from stdin, writes one JSON response per line
// to stdout. Wire-compatible with the agent-browser snapshot/ref protocol so
// `origin-browser`'s router can swap us in mid-session.

import readline from "node:readline";
import { CloakBrowser } from "cloak-browser";

const sessions = new Map();

async function getCtx(sessionId) {
  let c = sessions.get(sessionId);
  if (!c) {
    const browser = await CloakBrowser.launch({ headless: true });
    const page = await browser.newPage();
    c = { browser, page, refCounter: 0, refs: new Map() };
    sessions.set(sessionId, c);
  }
  return c;
}

function newRef(c, locator) {
  const id = `r${c.refCounter++}`;
  c.refs.set(id, locator);
  return id;
}

async function snapshot(c) {
  const status = c.page.lastResponseStatus ?? 200;
  const title = await c.page.title().catch(() => "");
  const html = await c.page.content().catch(() => "");
  return { status, title, html, snapshot: html.slice(0, 4096) };
}

async function handle(msg) {
  const c = await getCtx(msg.session);
  try {
    switch (msg.v) {
      case "open": {
        const resp = await c.page.goto(msg.url, { waitUntil: "domcontentloaded" });
        c.page.lastResponseStatus = resp?.status() ?? 200;
        const snap = await snapshot(c);
        const ref = newRef(c, "body");
        return { ok: true, ref, ...snap };
      }
      case "click": {
        const loc = c.refs.get(msg.ref);
        if (!loc) return { ok: false, error: `unknown ref ${msg.ref}` };
        await c.page.click(loc);
        const snap = await snapshot(c);
        return { ok: true, ref: newRef(c, loc), ...snap };
      }
      case "fill": {
        const loc = c.refs.get(msg.ref);
        if (!loc) return { ok: false, error: `unknown ref ${msg.ref}` };
        await c.page.fill(loc, msg.value);
        const snap = await snapshot(c);
        return { ok: true, ref: msg.ref, ...snap };
      }
      case "extract": {
        const loc = c.refs.get(msg.ref);
        if (!loc) return { ok: false, error: `unknown ref ${msg.ref}` };
        const text = await c.page.locator(loc).innerText();
        return { ok: true, ref: msg.ref, snapshot: text };
      }
      case "snapshot":
        return { ok: true, ...(await snapshot(c)) };
      case "screenshot":
        await c.page.screenshot({ path: msg.path, fullPage: false });
        return { ok: true };
      case "close": {
        await c.browser.close();
        sessions.delete(msg.session);
        return { ok: true };
      }
      default:
        return { ok: false, error: `unknown verb ${msg.v}` };
    }
  } catch (e) {
    return { ok: false, error: String(e?.message ?? e) };
  }
}

const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", async (line) => {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); }
  catch (e) { console.log(JSON.stringify({ ok: false, error: `bad json: ${e.message}` })); return; }
  const resp = await handle(msg);
  console.log(JSON.stringify(resp));
});
```

- [ ] **Step 3: Write README**

`vendor/cloak-browser/README.md`:

```markdown
# CloakBrowser sidecar

Node ≥18 sidecar that exposes CloakBrowser through the same stdio-JSON verb
protocol that `agent-browser` speaks. `origin-browser`'s router spawns this
on first use; you do not invoke it manually.

First-use bootstrap (origin runs this for you):

    npm install --omit=dev

The router then `node` runs `cloak-cli.mjs`.
```

- [ ] **Step 4: Smoke-test the sidecar manually (optional, doesn't block)**

Run in a separate shell:

```powershell
cd vendor/cloak-browser
npm install --omit=dev
echo '{"v":"open","url":"https://example.com","session":"s1"}' | node cloak-cli.mjs
```

Expected: one JSON line on stdout with `"ok":true` and a snapshot. If `npm install` fails because the repo URL is wrong, fix the `dependencies` entry to whatever CloakHQ's actual install line is.

- [ ] **Step 5: Commit**

```bash
git add vendor/cloak-browser
git commit -m "feat(vendor): CloakBrowser sidecar with stdio-JSON CLI"
```

---

## Task B5: agent-browser subprocess client

**Files:**
- Modify: `crates/origin-browser/src/agent_browser.rs`
- Create: `crates/origin-browser/tests/fakes/agent_browser_fake.mjs`
- Create: `crates/origin-browser/tests/agent_browser_client.rs`

- [ ] **Step 1: Write the fake CLI used by the test**

`crates/origin-browser/tests/fakes/agent_browser_fake.mjs`:

```js
#!/usr/bin/env node
// Tiny deterministic agent-browser stand-in: echo verbs as snapshots.
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const m = JSON.parse(line);
  const resp = { ok: true, ref: "r0", status: 200, title: "fake", html: `<html>${m.v}</html>`, snapshot: m.v };
  console.log(JSON.stringify(resp));
});
```

- [ ] **Step 2: Write the failing client test**

`crates/origin-browser/tests/agent_browser_client.rs`:

```rust
use origin_browser::agent_browser::AgentBrowserClient;
use origin_browser::protocol::Verb;

#[tokio::test]
async fn round_trips_a_verb_through_the_fake_cli() {
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fakes/agent_browser_fake.mjs");
    let mut client = AgentBrowserClient::spawn_with_command("node", &[fake.to_str().unwrap()]).await.unwrap();
    let resp = client.send(&Verb::Open { url: "https://x".into(), session: "s".into() }).await.unwrap();
    assert!(resp.ok);
    assert_eq!(resp.snapshot.as_deref(), Some("open"));
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p origin-browser --test agent_browser_client`
Expected: FAIL — `AgentBrowserClient` doesn't exist.

- [ ] **Step 4: Implement the client**

`crates/origin-browser/src/agent_browser.rs`:

```rust
//! Subprocess client for the `agent-browser` CLI.
//!
//! Speaks stdio-JSON. One verb in, one response out. Long-lived per session.

use crate::protocol::{SnapshotResp, Verb};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(String),
    #[error("backend exited")]
    Exited,
}

pub struct AgentBrowserClient {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl AgentBrowserClient {
    /// Spawn the real `agent-browser` CLI from PATH.
    ///
    /// # Errors
    /// Forwards spawn IO errors.
    pub async fn spawn() -> Result<Self, ClientError> {
        #[cfg(windows)]
        let (prog, args): (&str, &[&str]) = ("agent-browser.cmd", &["--stdio"]);
        #[cfg(not(windows))]
        let (prog, args): (&str, &[&str]) = ("agent-browser", &["--stdio"]);
        Self::spawn_with_command(prog, args).await
    }

    /// Spawn an explicit command — used by tests to point at the fake CLI.
    ///
    /// # Errors
    /// Forwards spawn IO errors.
    pub async fn spawn_with_command(prog: &str, args: &[&str]) -> Result<Self, ClientError> {
        let mut child = Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or(ClientError::Exited)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(ClientError::Exited)?);
        Ok(Self { child, stdin, stdout })
    }

    /// Send a verb, read one response line.
    ///
    /// # Errors
    /// IO/encode errors; `Exited` if the child closed stdout.
    pub async fn send(&mut self, verb: &Verb) -> Result<SnapshotResp, ClientError> {
        let mut line = serde_json::to_vec(verb)?;
        line.push(b'\n');
        self.stdin.write_all(&line).await.map_err(|e| ClientError::Io(e.to_string()))?;
        self.stdin.flush().await.map_err(|e| ClientError::Io(e.to_string()))?;
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await.map_err(|e| ClientError::Io(e.to_string()))?;
        if n == 0 { return Err(ClientError::Exited); }
        let resp: SnapshotResp = serde_json::from_str(buf.trim_end())
            .map_err(|e| ClientError::Io(format!("decode: {e}")))?;
        Ok(resp)
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p origin-browser --test agent_browser_client`
Expected: PASS. (Node must be on PATH for the test runner — it is per the agent-browser skill description, so OK.)

- [ ] **Step 6: Commit**

```bash
git add crates/origin-browser/src/agent_browser.rs crates/origin-browser/tests/agent_browser_client.rs crates/origin-browser/tests/fakes/agent_browser_fake.mjs
git commit -m "feat(origin-browser): agent-browser stdio-JSON client"
```

---

## Task B6: CloakBrowser subprocess client

**Files:**
- Modify: `crates/origin-browser/src/cloak.rs`
- Create: `crates/origin-browser/tests/fakes/cloak_fake.mjs`
- Create: `crates/origin-browser/tests/cloak_client.rs`

- [ ] **Step 1: Write fake CLI**

`crates/origin-browser/tests/fakes/cloak_fake.mjs`:

```js
#!/usr/bin/env node
// Cloak fake: identical to the agent-browser fake but marks itself in the title.
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  const m = JSON.parse(line);
  console.log(JSON.stringify({ ok: true, ref: "rC", status: 200, title: "cloak", html: `<html>${m.v}</html>`, snapshot: m.v }));
});
```

- [ ] **Step 2: Write failing test**

`crates/origin-browser/tests/cloak_client.rs`:

```rust
use origin_browser::cloak::CloakClient;
use origin_browser::protocol::Verb;

#[tokio::test]
async fn cloak_client_round_trip() {
    let fake = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fakes/cloak_fake.mjs");
    let mut client = CloakClient::spawn_with_command("node", &[fake.to_str().unwrap()]).await.unwrap();
    let r = client.send(&Verb::Open { url: "u".into(), session: "s".into() }).await.unwrap();
    assert_eq!(r.title.as_deref(), Some("cloak"));
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p origin-browser --test cloak_client`
Expected: FAIL.

- [ ] **Step 4: Implement cloak.rs**

`crates/origin-browser/src/cloak.rs` — identical structure to `agent_browser.rs` but:
- Default `spawn()` finds the sidecar.

```rust
//! Subprocess client for the vendored CloakBrowser sidecar.
//!
//! Resolves the sidecar via `ORIGIN_CLOAK_DIR` env var, or falls back to
//! `<exe-dir>/../vendor/cloak-browser/cloak-cli.mjs`. Runs `node` on it.

use crate::protocol::{SnapshotResp, Verb};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("spawn: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(String),
    #[error("backend exited")]
    Exited,
    #[error("sidecar not found at {0}")]
    NotFound(std::path::PathBuf),
}

pub struct CloakClient {
    #[allow(dead_code)]
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl CloakClient {
    /// Spawn the vendored sidecar.
    ///
    /// # Errors
    /// Returns [`ClientError::NotFound`] if the sidecar path resolves to a
    /// missing file. IO errors otherwise.
    pub async fn spawn() -> Result<Self, ClientError> {
        let sidecar = resolve_sidecar()?;
        Self::spawn_with_command("node", &[sidecar.to_str().unwrap_or_default()]).await
    }

    /// Test-visible variant.
    ///
    /// # Errors
    /// Forwards spawn IO errors.
    pub async fn spawn_with_command(prog: &str, args: &[&str]) -> Result<Self, ClientError> {
        let mut child = Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take().ok_or(ClientError::Exited)?;
        let stdout = BufReader::new(child.stdout.take().ok_or(ClientError::Exited)?);
        Ok(Self { child, stdin, stdout })
    }

    /// Send a verb, read one response line.
    ///
    /// # Errors
    /// IO/encode errors; `Exited` if the child closed stdout.
    pub async fn send(&mut self, verb: &Verb) -> Result<SnapshotResp, ClientError> {
        let mut line = serde_json::to_vec(verb)?;
        line.push(b'\n');
        self.stdin.write_all(&line).await.map_err(|e| ClientError::Io(e.to_string()))?;
        self.stdin.flush().await.map_err(|e| ClientError::Io(e.to_string()))?;
        let mut buf = String::new();
        let n = self.stdout.read_line(&mut buf).await.map_err(|e| ClientError::Io(e.to_string()))?;
        if n == 0 { return Err(ClientError::Exited); }
        let resp: SnapshotResp = serde_json::from_str(buf.trim_end())
            .map_err(|e| ClientError::Io(format!("decode: {e}")))?;
        Ok(resp)
    }
}

fn resolve_sidecar() -> Result<std::path::PathBuf, ClientError> {
    if let Ok(p) = std::env::var("ORIGIN_CLOAK_DIR") {
        let cli = std::path::PathBuf::from(p).join("cloak-cli.mjs");
        if cli.exists() { return Ok(cli); }
        return Err(ClientError::NotFound(cli));
    }
    let exe = std::env::current_exe().map_err(ClientError::Spawn)?;
    let cand = exe
        .parent().and_then(|p| p.parent())
        .map(|p| p.join("vendor/cloak-browser/cloak-cli.mjs"))
        .unwrap_or_default();
    if cand.exists() { return Ok(cand); }
    Err(ClientError::NotFound(cand))
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p origin-browser --test cloak_client`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-browser/src/cloak.rs crates/origin-browser/tests/cloak_client.rs crates/origin-browser/tests/fakes/cloak_fake.mjs
git commit -m "feat(origin-browser): CloakBrowser stdio-JSON client"
```

---

## Task B7: BrowserRouter with sticky fallback

**Files:**
- Modify: `crates/origin-browser/src/router.rs`
- Create: `crates/origin-browser/tests/router_fallback.rs`
- Create: `crates/origin-browser/tests/fakes/agent_browser_bot.mjs`

- [ ] **Step 1: Write a fake that *always* returns bot-detected**

`crates/origin-browser/tests/fakes/agent_browser_bot.mjs`:

```js
#!/usr/bin/env node
// Always responds with a Cloudflare challenge snapshot.
import readline from "node:readline";
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", () => {
  console.log(JSON.stringify({
    ok: true,
    ref: "r0",
    status: 403,
    title: "Just a moment...",
    html: "<script>/* cf-chl- */</script>",
    snapshot: "challenge",
  }));
});
```

- [ ] **Step 2: Write the failing router test**

`crates/origin-browser/tests/router_fallback.rs`:

```rust
use origin_browser::router::{BrowserRouter, RouterError};
use origin_browser::protocol::Verb;

fn fake(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fakes/{name}"))
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn falls_back_to_cloak_when_primary_signals_bot() {
    let mut router = BrowserRouter::with_commands(
        ("node", vec![fake("agent_browser_bot.mjs")]),
        ("node", vec![fake("cloak_fake.mjs")]),
    ).await.unwrap();
    let resp = router.run(&Verb::Open { url: "u".into(), session: "s1".into() }).await.unwrap();
    // The Cloak fake marks itself with title "cloak"; the bot fake uses "Just a moment..."
    assert_eq!(resp.title.as_deref(), Some("cloak"), "fallback should have taken over");
}

#[tokio::test]
async fn primary_used_when_clean() {
    let mut router = BrowserRouter::with_commands(
        ("node", vec![fake("agent_browser_fake.mjs")]),
        ("node", vec![fake("cloak_fake.mjs")]),
    ).await.unwrap();
    let resp = router.run(&Verb::Open { url: "u".into(), session: "s1".into() }).await.unwrap();
    assert_eq!(resp.title.as_deref(), Some("fake"));
}

#[tokio::test]
async fn sticks_to_cloak_after_two_successful_fallbacks() {
    let mut router = BrowserRouter::with_commands(
        ("node", vec![fake("agent_browser_bot.mjs")]),
        ("node", vec![fake("cloak_fake.mjs")]),
    ).await.unwrap();
    let _ = router.run(&Verb::Open { url: "a".into(), session: "s2".into() }).await.unwrap();
    let _ = router.run(&Verb::Open { url: "b".into(), session: "s2".into() }).await.unwrap();
    // After two fallbacks, the router must not call primary again. We can't
    // observe that directly without instrumentation; instead we check it
    // never re-queries primary by asserting only cloak responses come back.
    let r = router.run(&Verb::Open { url: "c".into(), session: "s2".into() }).await.unwrap();
    assert_eq!(r.title.as_deref(), Some("cloak"));
    assert!(router.sticky_cloak("s2"), "sticky bit should be set");
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p origin-browser --test router_fallback`
Expected: FAIL — router doesn't exist.

- [ ] **Step 4: Implement router.rs**

`crates/origin-browser/src/router.rs`:

```rust
//! Primary/fallback browser router.
//!
//! Policy: try `agent-browser` first. If the classifier flags the response as
//! bot-detected, replay the same verb against `CloakBrowser` and emit that
//! response instead. After two consecutive Cloak fallbacks in a session, mark
//! the session sticky so future verbs skip primary entirely.

use crate::agent_browser::AgentBrowserClient;
use crate::cloak::CloakClient;
use crate::detectors::{classify, Verdict};
use crate::protocol::{SnapshotResp, Verb};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("primary: {0}")]
    Primary(String),
    #[error("fallback: {0}")]
    Fallback(String),
}

#[derive(Default)]
struct SessionState {
    cloak_streak: u8,
    sticky: bool,
}

pub struct BrowserRouter {
    primary: AgentBrowserClient,
    cloak: CloakClient,
    state: HashMap<String, SessionState>,
}

impl BrowserRouter {
    /// Production constructor: spawn real CLIs.
    ///
    /// # Errors
    /// Forwards spawn errors from either backend.
    pub async fn new() -> Result<Self, RouterError> {
        let primary = AgentBrowserClient::spawn().await.map_err(|e| RouterError::Primary(e.to_string()))?;
        let cloak = CloakClient::spawn().await.map_err(|e| RouterError::Fallback(e.to_string()))?;
        Ok(Self { primary, cloak, state: HashMap::new() })
    }

    /// Test constructor: spawn both backends with explicit commands.
    ///
    /// # Errors
    /// Forwards spawn errors from either backend.
    pub async fn with_commands(
        primary: (&str, Vec<String>),
        cloak: (&str, Vec<String>),
    ) -> Result<Self, RouterError> {
        let p_args: Vec<&str> = primary.1.iter().map(String::as_str).collect();
        let c_args: Vec<&str> = cloak.1.iter().map(String::as_str).collect();
        let primary = AgentBrowserClient::spawn_with_command(primary.0, &p_args).await
            .map_err(|e| RouterError::Primary(e.to_string()))?;
        let cloak = CloakClient::spawn_with_command(cloak.0, &c_args).await
            .map_err(|e| RouterError::Fallback(e.to_string()))?;
        Ok(Self { primary, cloak, state: HashMap::new() })
    }

    /// Test-only introspection: did this session become sticky on Cloak?
    #[must_use]
    pub fn sticky_cloak(&self, session: &str) -> bool {
        self.state.get(session).is_some_and(|s| s.sticky)
    }

    /// Run a verb through the routing policy.
    ///
    /// # Errors
    /// Returns [`RouterError`] if both backends fail.
    pub async fn run(&mut self, verb: &Verb) -> Result<SnapshotResp, RouterError> {
        let session = session_of(verb).to_string();
        let st = self.state.entry(session.clone()).or_default();

        if st.sticky {
            return self.cloak.send(verb).await.map_err(|e| RouterError::Fallback(e.to_string()));
        }

        let primary = self.primary.send(verb).await.map_err(|e| RouterError::Primary(e.to_string()))?;
        match classify(&primary) {
            Verdict::Clean => {
                st.cloak_streak = 0;
                Ok(primary)
            }
            Verdict::BotDetected(_reason) => {
                let cloak_resp = self.cloak.send(verb).await.map_err(|e| RouterError::Fallback(e.to_string()))?;
                if cloak_resp.ok {
                    st.cloak_streak = st.cloak_streak.saturating_add(1);
                    if st.cloak_streak >= 2 { st.sticky = true; }
                }
                Ok(cloak_resp)
            }
        }
    }
}

fn session_of(v: &Verb) -> &str {
    match v {
        Verb::Open { session, .. } | Verb::Click { session, .. } | Verb::Fill { session, .. }
        | Verb::Extract { session, .. } | Verb::Snapshot { session } | Verb::Screenshot { session, .. }
        | Verb::Close { session } => session,
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p origin-browser --test router_fallback`
Expected: all 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-browser/src/router.rs crates/origin-browser/tests/router_fallback.rs crates/origin-browser/tests/fakes/agent_browser_bot.mjs
git commit -m "feat(origin-browser): BrowserRouter with sticky bot-detection fallback"
```

---

## Task C1: Wire `web_fetch` and `web_search` as origin-tools builtins

**Files:**
- Modify: `crates/origin-tools/Cargo.toml` — add `origin-browser`
- Create: `crates/origin-tools/src/builtins/web_fetch.rs`
- Create: `crates/origin-tools/src/builtins/web_search.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs`
- Create: `crates/origin-tools/tests/web_tools_registered.rs`

- [ ] **Step 1: Add dep**

In `crates/origin-tools/Cargo.toml` under `[dependencies]`:

```toml
origin-browser = { path = "../origin-browser" }
```

- [ ] **Step 2: Write failing registry test**

`crates/origin-tools/tests/web_tools_registered.rs`:

```rust
use origin_tools::registry_iter;

#[test]
fn web_fetch_and_web_search_and_browser_registered() {
    let names: Vec<&str> = registry_iter().map(|m| m.name).collect();
    for want in ["WebFetch", "WebSearch", "Browser"] {
        assert!(names.contains(&want), "missing tool registration: {want}; got {names:?}");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p origin-tools --test web_tools_registered`
Expected: FAIL.

- [ ] **Step 4: Write builtins/web_fetch.rs**

```rust
//! `WebFetch` tool — GET a URL and return reader-mode markdown.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::web_fetch::{fetch, FetchOptions};

/// Fetch `url` and return markdown of the readable content.
///
/// # Errors
/// Returns a `String` describing HTTP, size, or parse failure.
pub async fn web_fetch(url: &str) -> Result<String, String> {
    let r = fetch(url, FetchOptions::default()).await.map_err(|e| e.to_string())?;
    Ok(r.markdown)
}

crate::origin_tool! {
    name: "WebFetch",
    description: "Fetch a URL and return the reader-mode markdown.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","required":["url"],"properties":{"url":{"type":"string"}}}"#,
}
```

- [ ] **Step 5: Write builtins/web_search.rs**

```rust
//! `WebSearch` tool — Tavily-backed search.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::web_search::{search, SearchHit};

/// Search `query` via Tavily; returns up to `count` hits.
///
/// # Errors
/// `String` describing missing API key or HTTP failure.
pub async fn web_search(query: &str, count: usize) -> Result<Vec<SearchHit>, String> {
    search(query, count).await.map_err(|e| e.to_string())
}

crate::origin_tool! {
    name: "WebSearch",
    description: "Search the web via Tavily. Requires TAVILY_API_KEY.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{"type":"object","required":["query"],"properties":{"query":{"type":"string"},"count":{"type":"integer","minimum":1,"maximum":20}}}"#,
}
```

- [ ] **Step 6: Write builtins/browser.rs**

```rust
//! `Browser` tool — dual-backend browser with transparent agent-detection fallback.

use crate::{SideEffects, Tier, Urgency};
use origin_browser::{BrowserRouter, Verb, SnapshotResp};

/// Run a single verb against the router. Caller holds the router across
/// turns; this function is intentionally not lifetimed to the registry
/// (Origin's coordinator owns a `BrowserRouter` and dispatches into this).
///
/// # Errors
/// `String` describing primary/fallback failure.
pub async fn browser(router: &mut BrowserRouter, verb: Verb) -> Result<SnapshotResp, String> {
    router.run(&verb).await.map_err(|e| e.to_string())
}

crate::origin_tool! {
    name: "Browser",
    description: "Stateful browser with agent-detection fallback to CloakBrowser.",
    tier: Tier::RequiresPermission,
    urgency: Urgency::Medium,
    side_effects: SideEffects::Mutating,
    input_schema: r#"{"type":"object","required":["v","session"],"properties":{"v":{"type":"string","enum":["open","click","fill","extract","snapshot","screenshot","close"]},"session":{"type":"string"},"url":{"type":"string"},"ref":{"type":"string"},"value":{"type":"string"},"path":{"type":"string"}}}"#,
}
```

- [ ] **Step 7: Register modules**

In `crates/origin-tools/src/builtins/mod.rs`, append:

```rust
pub mod browser;
pub mod web_fetch;
pub mod web_search;
```

- [ ] **Step 8: Run test to verify pass**

Run: `cargo test -p origin-tools --test web_tools_registered`
Expected: PASS — all three tool names appear.

- [ ] **Step 9: Run full origin-tools test suite**

Run: `cargo test -p origin-tools`
Expected: pass.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-tools
git commit -m "feat(origin-tools): WebFetch + WebSearch + Browser builtins"
```

---

## Task D1: Workspace smoke + verification + final commit

- [ ] **Step 1: Build the whole workspace**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 2: Run the full test suite**

Run: `cargo test --workspace`
Expected: pass.

- [ ] **Step 3: Run clippy on the new crate**

Run: `cargo clippy -p origin-browser -p origin-skills -p origin-tools -- -D warnings`
Expected: zero warnings.

- [ ] **Step 4: Confirm skill registry sees the embedded skills**

This is a one-off binary check — write a temp `examples/skill_count.rs` if needed, or use a doctest. Recommended quick check:

```bash
cargo test -p origin-skills --test embedded_skills -- --nocapture
```

Expected: "14 embedded skills" assertion passes.

- [ ] **Step 5: Verify CHANGELOG entry**

Edit `CHANGELOG.md` — add under the next unreleased section:

```markdown
### Added
- `origin-skills`: 14 bundled superpowers skills (brainstorming, TDD, debugging, plans, …) loaded from the binary by default; override per skill at `~/.origin/skills/<name>/SKILL.md`.
- `origin-browser` (new crate): `BrowserRouter` with primary `agent-browser` + sticky `CloakBrowser` fallback on bot-detection; `WebFetch` (pure Rust, reader-mode) and `WebSearch` (Tavily) modules.
- `origin-tools`: `Browser`, `WebFetch`, `WebSearch` builtins.
```

- [ ] **Step 6: Final commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): record skills + browser + web tools"
```

---

## Self-review notes

**Spec coverage**

- Skills bundling + user override: Task A1 ✓
- Embedded via include_dir!: Task A1 step 5 ✓
- 14 specific skills: Task A1 step 1 (asserts all 14) ✓
- agent-browser primary client: Task B5 ✓
- CloakBrowser sidecar (npm vendored): Task B4 ✓
- CloakBrowser client: Task B6 ✓
- Router with transparent fallback + sticky after 2 successes: Task B7 ✓
- Detection signals: Task B1 (table-driven, all spec signatures covered) ✓
- WebFetch pure Rust + readability + html2md: Task B2 ✓
- WebSearch Tavily-only with env key: Task B3 ✓
- Three builtins wired into origin-tools registry: Task C1 ✓
- MSRV verification: Task D1 step 3 (workspace clippy) — implicit via cargo build ✓

**Placeholder scan**: No TBDs or "implement later" markers; every code-changing step has full code.

**Type consistency**: `SnapshotResp`, `Verb`, `BrowserRouter`, `AgentBrowserClient`, `CloakClient`, `RouterError`, `FetchOptions`, `FetchResult`, `SearchHit`, `SearchOptions`, `SearchError` — same names used in every consumer.
