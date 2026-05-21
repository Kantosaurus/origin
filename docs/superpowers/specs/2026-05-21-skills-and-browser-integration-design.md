---
title: Superpowers Skills + Agent/Cloak Browser + Web Tools Integration
date: 2026-05-21
status: approved
---

# Superpowers Skills + Browser/Web Tool Integration

## Goal

Land three additions in `origin`:

1. Bundle the 14 official "superpowers" skills (brainstorming, TDD, debugging, plans, code review, worktrees, etc.) so they are available to every origin session.
2. Integrate two browser backends — `vercel-labs/agent-browser` (fast, low-token snapshot/ref protocol) and `CloakHQ/CloakBrowser` (stealth fallback) — behind a router that transparently retries via Cloak when an agent-detection signal fires.
3. Add `WebFetch` and `WebSearch` builtin tools.

## Non-goals

- Reimplementing either browser in Rust.
- Replacing origin's existing skill loader/registry — both ship as-is and consume the new content.
- New permission tiers — the new tools slot into existing tiers.

## Decisions (locked)

- **Skill storage**: embed at build time **and** allow per-user override from `~/.origin/skills/<name>/SKILL.md`. User dir wins on name collision.
- **CloakBrowser packaging**: vendored npm sidecar at `vendor/cloak-browser/`. Origin runs `npm install --omit=dev` on first use into `~/.origin/cache/cloak-browser/node_modules/`, then `node` the entrypoint.
- **WebSearch provider**: Tavily only. Env: `TAVILY_API_KEY`. Clear error if unset.
- **WebFetch**: pure Rust (reqwest + readability + html2md), no subprocess.

## Architecture

```
crates/
  origin-browser/                       (new crate)
    src/
      lib.rs                            re-exports
      router.rs                         BrowserRouter: primary/fallback policy
      detectors.rs                      bot-detection signal classifier (pure fn)
      agent_browser.rs                  subprocess client for `agent-browser` CLI
      cloak.rs                          subprocess client for vendor/cloak-browser
      web_fetch.rs                      reqwest + readability + html2md
      web_search.rs                     Tavily HTTP client
    tests/
      detectors.rs                      table-driven: html snippet -> classification
      router_fallback.rs                fake clients prove fallback fires once and only once
  origin-skills/
    embedded/superpowers/<name>/SKILL.md      copied from superpowers 5.1.0
    src/embedded.rs                     new: include_dir! tree -> Vec<Skill>
    src/loader.rs                       extended: merge_embedded_with_user(...)
  origin-tools/src/builtins/
    web_fetch.rs                        thin wrapper -> origin_browser::web_fetch
    web_search.rs                       thin wrapper -> origin_browser::web_search
    browser.rs                          single tool, verbs: open/click/fill/extract/snapshot/screenshot
vendor/cloak-browser/
  package.json                          deps: cloak-browser, playwright-core
  cloak-cli.mjs                         exposes verb protocol matching agent-browser
```

### Subprocess protocol (shared by both backends)

Both backends speak newline-delimited JSON over stdio so the router can swap them mid-session:

```
> {"v":"open","url":"https://example.com","session":"s1"}
< {"ok":true,"ref":"r0","snapshot":"<200-400 tok abbreviated DOM>"}
> {"v":"click","ref":"r0:a[3]","session":"s1"}
< {"ok":true,"ref":"r1","snapshot":"..."}
```

`agent-browser` already emits this shape natively. `cloak-cli.mjs` is the thin translation layer we own.

### Router policy

`BrowserRouter` keeps two long-lived child processes per session (lazy-spawned). For each verb:

1. Send to **agent-browser**.
2. If `ok=false` OR the snapshot/response classifies as bot-detected (see `detectors.rs`), replay the same verb against **CloakBrowser** with the same `session` id. The session id is the router's, not the backend's, so state is rebuilt by replaying the verb log from `open` onward — this is what makes the fallback transparent.
3. After two consecutive Cloak successes for a session, the router sticks with Cloak for the remainder of that session (sticky fallback) to avoid bouncing.

### Detection signals (`detectors.rs`)

Pure function `classify(response: &SnapshotResp) -> Verdict` returns `Clean | BotDetected(reason)`. Rules:

- HTTP status 403 with body containing `cf-chl-` / `__cf_bm` / `cf-mitigated` → Cloudflare challenge.
- HTTP 429 from a top-100 hostname (origin domain check) → rate-limit gate.
- HTML body matches any of: `g-recaptcha`, `h-captcha`, `px-captcha`, `_pxhd`, `datadome`, `incapsula`, `_Incapsula_Resource`, `kasada`, `imperva` → JS challenge stack.
- `<title>` matches `/just a moment|attention required|access denied|verify you are human/i` → human-check page.

Rules live in one table; new signatures land as one-line additions with a paired test case.

### Skill loader changes

Add `embedded.rs`:

```rust
pub static EMBEDDED_SKILLS: include_dir::Dir<'_> = include_dir::include_dir!("$CARGO_MANIFEST_DIR/embedded/superpowers");
pub fn load_embedded() -> Vec<Skill> { /* walk include_dir tree */ }
```

Extend the public surface with `load_all(user_root: &Path)` that:
1. Loads embedded skills.
2. Loads `user_root` via existing `load_skills_dir`.
3. Merges, with user entries replacing embedded entries by `front.name`.

### Web tools

- `WebFetch(url, max_bytes=10_485_760)`: GET via reqwest with default 30s timeout, follows redirects, honors `User-Agent: origin/<version>`. Runs `readability` to extract main content, then `html2md` → returns markdown string + final URL + content-type. Aborts on `robots.txt: Disallow` for the path under the configured agent name.
- `WebSearch(query, count=10)`: POST `https://api.tavily.com/search` with `api_key`, `query`, `max_results=count`. Returns `{title,url,snippet}[]`.

## Testing

- Unit: detector table tests; frontmatter parsing of every embedded skill; router test with fake primary that injects a bot-detected response.
- Integration: `cargo test -p origin-browser router_fallback::*` spins up two trivial Node test scripts in `tests/fakes/` standing in for both backends — no real network.
- Skill embed test: assert `load_embedded().len() == 14` and every entry parses.

## Risks & mitigations

- **Node not installed**: detect at first browser tool call, error with install instructions and a link. WebFetch/WebSearch keep working without Node.
- **Tavily key missing**: WebSearch errors with a clear "set TAVILY_API_KEY" message; other tools unaffected.
- **CloakBrowser license**: confirm before merge; if restrictive, gate behind a build feature `cloak` (default on).
- **MSRV 1.83**: `include_dir`, `reqwest`, `readability`, `html2md` all build on 1.83 — verify in CI before merging.

## Out of scope (future)

- Cookie/auth persistence across sessions.
- A "browser pool" for parallel agents.
- Replacing Tavily with a self-hosted SearXNG instance.
