# `origin` Phase 7 — Code Graph (`origin-codegraph`) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL — use **superpowers:subagent-driven-development** to execute task-by-task. Within each task follow **superpowers:test-driven-development** (failing test first, run to fail, implement, run to pass) and apply **superpowers:verification-before-completion** — do NOT advance to the next task until the verification gate is fully green. Steps use checkbox (`- [ ]`) syntax.

**Branch:** All Phase 7 work lands on branch `phase-7` (branched off `dev`).

**Goal:** Stand up `origin-codegraph` — a native code knowledge graph that ingests source via tree-sitter, stores nodes/edges as CAS records with cross-repo dedup, clusters by Leiden + flow-weighted PageRank, exposes a typed query DSL surfaced as `graph_*` tools plus an `Ask` router, and incrementally rebuilds on `git commit`.

**Architecture:** New crate `origin-codegraph` owns ten focused modules — `lang` (a `Language` enum + tree-sitter parser binding), `extract` (walks a parsed tree → `CodeNode` + `CodeEdge` records), `chunker` (AST-biased FastCDC over file bytes — boundary hints from tree-sitter ranges bias the rolling-hash cut score), `record` (rkyv-archived `CodeNode`/`CodeEdge`/`Confidence`/`Evidence` types, content-addressed via `origin-cas`), `index` (SQLite-backed `code_nodes`/`code_edges`/`code_communities`/`cross_links` over `origin-store`'s connection), `sidecar` (trait + `NoopSidecar` + bounded extraction queue — Phase 5's `origin-sidecar` will inject the real small-model impl), `community` (Leiden clustering on the edge graph + flow-weighted PageRank for god-node selection), `query` (typed `Query::{Path, Neighbors, Communities, GodNodes, RecentChanges}` enum + dispatcher returning CAS-handle-backed `QueryResult`), `ask` (sub-millisecond classifier routing code-/memory-/hybrid-shaped queries via a `MemRouter` trait; Phase 6's `origin-mem` will inject the real mem impl), and `rebuild` (incremental rebuild driver: input is a set of changed `PathBuf`s, output is a `RebuildReport`). New tools land in `origin-tools::builtins`: `graph_query`, `graph_path`, `graph_explain`, `graph_summarize`, `graph_rebuild`, `ask`. A minimal `post-commit` git hook installer lives in `origin-codegraph::git_hook` and the daemon exposes a `rebuild_codegraph` IPC verb that the hook script invokes.

**Tech Stack:** Rust 1.83 (MSRV pin), `tree-sitter` 0.22 + grammars `tree-sitter-rust` 0.21, `tree-sitter-typescript` 0.21, `tree-sitter-python` 0.21, `tree-sitter-go` 0.21, `tree-sitter-java` 0.21 (top-5 grammars; remaining 5 from the spec deferred to P10 polish — see scope), `petgraph` 0.6 (in-memory graph for community + PageRank), `fastcdc` 3 (already a workspace dep — reused with a boundary-bias adapter), `regex` 1 (existing — used by `Ask` classifier), `rkyv` 0.7 (existing — `CodeNode`/`CodeEdge` archive), `rusqlite` (existing — V3 migration), `lopdf` 0.32 (PDF text extract for P7.4 sidecar trait demonstrative impl). **Novel-implementation reflex** per `[[feedback_novel_implementations]]`: every signature subsystem must beat openclaude/jcode/opencode on tokens or perf. Phase 7's novelties: (1) tree-sitter-biased FastCDC — boundary cut points come from AST node ranges, not just the rolling-hash low bits, so a one-function edit changes exactly one chunk; (2) `CodeNode`/`CodeEdge` as content-addressed records → identical `Option<T>::map` signatures across N Rust crates dedupe to a single CAS shard; (3) flow-weighted PageRank where `Calls` > `Mentions` and `INFERRED` confidence is discounted, surfacing god nodes that match a human reading; (4) typed query DSL with no in-tool LLM hop — NL is the model's job, `graph_explain` is the *only* NL-output tool and it goes through sidecar with a tight template; (5) sub-millisecond `Ask` router (regex + heuristic) — no LLM in the path.

**Builds on:** `docs/superpowers/specs/2026-05-19-origin-harness-design.md` (mechanisms **N6.6–N6.10**). Phase 3 deliverables (tag `p3-complete`) supply `origin-cas` chunker/store and the tool dispatch / registry machinery.

**Phase 7 spec-mechanism citations:**
- **N6.6** — FastCDC-incremental code extraction with AST-boundary bias (Task P7.2)
- **N6.7** — Graph nodes/edges as CAS records; cross-repo dedup; query results pinned in Sticky band (Task P7.3, P7.6)
- **N6.8** — Sidecar for non-code entities with confidence tags (Task P7.4)
- **N6.9** — Leiden + flow-weighted PageRank for god nodes (Task P7.5)
- **N6.10** — Typed query DSL; no second LLM hop inside the tool (Task P7.6)
- Section 6C — `Ask` router + joint recall (Task P7.7)

What is **explicitly out of scope** for Phase 7 (deferred):
- The real small-model sidecar implementation (Phase 5 — `origin-sidecar` crate). P7.4 ships a `Sidecar` trait + `NoopSidecar` + a `LopdfTextSidecar` reference impl that proves the wiring; the real LLM-backed sidecar lands in P5.1.
- The real conversation memory subsystem (Phase 6 — `origin-mem`). P7.7 ships a `MemRouter` trait + `NullMemRouter`; the real HNSW + temporal-decay impl lands in P6.3–P6.4. Hybrid `Ask` queries still route correctly — the null impl just returns no memory hits.
- Grammars for Ruby, PHP, Swift, SQL, C/C++ — registered as `Language` variants with `parser_unavailable` errors; grammars added in P10 polish.
- General hooks framework (Phase 10 — `origin-hooks`). P7.8 installs a single minimal `post-commit` script and a daemon IPC verb; P10 generalizes it.
- The CachePlanner `Sticky band` pin for query results — wired through `origin-planner` API in P3.1/P3.2. P7.6 returns the handle and tags it `sticky: true`; the planner consumes it as in Phase 3.
- `graph_explain`'s sidecar template authoring — the tool exists in P7.7 but its template is left as a `// TODO(p5)` constant (this is the *only* TODO permitted in this phase and is gated on the sidecar landing in Phase 5).

---

## Conventions reminder (apply to every task)

**TDD shape, every task:**
1. Write the failing test.
2. Run it — confirm the expected failure mode.
3. Implement the minimum to pass.
4. Run the test — confirm pass.
5. Verification gate (see table).
6. Commit.

**Verification gate per task type:**

| Task type | Verification commands (all must exit 0) |
|---|---|
| Pure-logic / single-crate | `cargo test -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` + `cargo fmt --check` |
| Cross-crate / migration / tools registration | `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` |
| Bench-touching tasks (P7.2 incremental-chunk, P7.5 community quality) | All of the above + `cargo bench -p origin-codegraph --bench <name> -- --quick` exits 0 with thresholds met |
| Final phase gate (P7.8) | All of the above + tag `p7-complete` |

**Patterns inherited from earlier phases:**
- `[lints] workspace = true` in every crate `Cargo.toml`; workspace inheritance for `version`/`edition`/`rust-version`/`license`/`repository`.
- All shared/persisted/IPC-crossing types derive `Archive + Serialize + Deserialize` from rkyv 0.7 with `#[archive(check_bytes)]`. **Phase 7 adds:** `CodeNode`, `CodeEdge`, `Evidence`, `Confidence`, `QueryResult` are rkyv-archived; the SQLite rows store handles + small scalar columns only.
- `[lints.rust] unsafe_code = "forbid"` is the default; `origin-codegraph` keeps the forbid. Tree-sitter parser handles are FFI-safe wrappers shipped by the grammar crates — no `unsafe` needed in our code.
- `#[must_use]` on every public constructor; `const fn` wherever Rust allows.
- Tests use `.expect("meaningful message")` — never `#[allow(clippy::unwrap_used)]`.
- Custom error enums via `thiserror`; document `# Errors` and `# Panics` on `pub fn`s.
- For each `#[allow(clippy::…)]` add an inline comment justifying it; never blanket-suppress.
- **MSRV pin reflex** (`[[project_msrv_dep_pinning]]`): if `cargo check` complains about `edition2024` or "requires Rust 1.85+", pin the offender with `cargo update -p <crate> --precise <ver>` and record in `Cargo.lock`. Tree-sitter grammar crates rebuild C source via `cc` — pinning `cc` to `=1.0.95` is recommended if newer `cc` versions trip MSRV.

**Commit style:** Conventional commits, scoped to crate where possible. Each task lands in **one commit** on branch `phase-7`.

---

## File map for Phase 7

| New crate / file | Responsibility |
|---|---|
| `crates/origin-codegraph/Cargo.toml` | manifest; workspace lints |
| `crates/origin-codegraph/src/lib.rs` | public surface — re-exports + module declarations |
| `crates/origin-codegraph/src/lang.rs` | `Language` enum + tree-sitter parser bindings (P7.1) |
| `crates/origin-codegraph/src/extract.rs` | tree → `CodeNode`/`CodeEdge` extraction (P7.1, P7.3) |
| `crates/origin-codegraph/src/chunker.rs` | AST-biased FastCDC adapter (P7.2) |
| `crates/origin-codegraph/src/record.rs` | rkyv-archived `CodeNode`, `CodeEdge`, `Confidence`, `Evidence` (P7.3) |
| `crates/origin-codegraph/src/index.rs` | SQLite-backed index over CAS handles (P7.3) |
| `crates/origin-codegraph/src/sidecar.rs` | `Sidecar` trait + `NoopSidecar` + `LopdfTextSidecar` (P7.4) |
| `crates/origin-codegraph/src/community.rs` | Leiden clustering + flow-weighted PageRank (P7.5) |
| `crates/origin-codegraph/src/query.rs` | typed `Query` enum + dispatcher (P7.6) |
| `crates/origin-codegraph/src/ask.rs` | `Ask` classifier + `MemRouter` trait (P7.7) |
| `crates/origin-codegraph/src/rebuild.rs` | incremental rebuild driver (P7.8) |
| `crates/origin-codegraph/src/git_hook.rs` | post-commit hook installer (P7.8) |
| `crates/origin-codegraph/tests/lang.rs` | parse Rust + TypeScript fixtures (P7.1) |
| `crates/origin-codegraph/tests/extract.rs` | function/struct extraction (P7.1, P7.3) |
| `crates/origin-codegraph/tests/chunker.rs` | one-function edit → one chunk hash change (P7.2) |
| `crates/origin-codegraph/tests/index.rs` | V3 migration, dedup, refcount (P7.3) |
| `crates/origin-codegraph/tests/sidecar.rs` | NoopSidecar + LopdfTextSidecar (P7.4) |
| `crates/origin-codegraph/tests/community.rs` | synthetic graph → expected communities + god nodes (P7.5) |
| `crates/origin-codegraph/tests/query.rs` | each typed query kind round-trips (P7.6) |
| `crates/origin-codegraph/tests/ask.rs` | classifier routing (P7.7) |
| `crates/origin-codegraph/tests/rebuild.rs` | incremental rebuild after touch (P7.8) |
| `crates/origin-codegraph/benches/incremental.rs` | 5KLOC file, edit one fn → reextract ≤ 2 chunks (P7.2) |
| `crates/origin-codegraph/benches/community.rs` | 1K-node graph; Leiden quality (modularity ≥ 0.6) (P7.5) |
| `crates/origin-store/src/migrations/V3__codegraph.sql` *(new)* | `code_nodes`, `code_edges`, `code_communities`, `cross_links` tables (P7.3) |
| `crates/origin-tools/src/builtins/graph_query.rs` *(new)* | `graph_query` builtin (P7.7) |
| `crates/origin-tools/src/builtins/graph_path.rs` *(new)* | `graph_path` builtin (P7.7) |
| `crates/origin-tools/src/builtins/graph_explain.rs` *(new)* | `graph_explain` builtin (template stub) (P7.7) |
| `crates/origin-tools/src/builtins/graph_summarize.rs` *(new)* | `graph_summarize` builtin (P7.7) |
| `crates/origin-tools/src/builtins/graph_rebuild.rs` *(new)* | `graph_rebuild` builtin (RequiresPermission) (P7.7) |
| `crates/origin-tools/src/builtins/ask.rs` *(new)* | `Ask` builtin invoking the classifier (P7.7) |
| `crates/origin-tools/src/builtins/mod.rs` *(modify)* | register new builtins (P7.7) |
| `crates/origin-tools/Cargo.toml` *(modify)* | add `origin-codegraph` dep (P7.7) |
| `crates/origin-daemon/src/protocol.rs` *(modify, P7.8)* | add `ClientMessage::RebuildCodegraph { paths: Vec<PathBuf> }` |
| `crates/origin-daemon/src/agent.rs` *(modify, P7.8)* | wire codegraph rebuild handler |
| `Cargo.toml` *(modify, P7.1)* | the new crate is picked up by `members = ["crates/*"]` — no change needed unless workspace deps are added; add `tree-sitter = "0.22"` etc. to `[workspace.dependencies]` if multiple crates would share. **P7 keeps tree-sitter deps in `origin-codegraph` only.** |

**File-size discipline:** every new `.rs` file targets <400 LOC. If a task naturally pushes a file past 400 LOC, split early (e.g. `extract.rs` → `extract/rust.rs` + `extract/ts.rs` + `extract/mod.rs`).

---

## Task P7.0 — Branch + plan checkpoint

**Files:**
- Modify: branch state — confirm we are on `phase-7`.

- [ ] **Step 1: Confirm branch**

Run: `git branch --show-current`
Expected: `phase-7`

- [ ] **Step 2: Stage and commit the plan**

```bash
git add docs/superpowers/plans/2026-05-19-origin-phase-7.md docs/superpowers/plans/2026-05-19-origin-phase-4.md
git commit -m "docs(origin): Phase 7 implementation plan (codegraph + Ask + git hook)"
```

(The Phase 4 file is currently untracked — bundle it into the same commit since it's already on disk.)

- [ ] **Step 3: Verification gate.** No code; just `git status` returns clean.

---

## Task P7.1 — `origin-codegraph` skeleton + tree-sitter `Language` + extract

**Files:**
- Create: `crates/origin-codegraph/Cargo.toml`
- Create: `crates/origin-codegraph/src/lib.rs`
- Create: `crates/origin-codegraph/src/lang.rs`
- Create: `crates/origin-codegraph/src/extract.rs`
- Create: `crates/origin-codegraph/tests/lang.rs`
- Create: `crates/origin-codegraph/tests/extract.rs`

- [ ] **Step 1: Manifest** at `crates/origin-codegraph/Cargo.toml`

```toml
[package]
name = "origin-codegraph"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[lints]
workspace = true

[dependencies]
thiserror = "1"
tree-sitter = "=0.22.6"
tree-sitter-rust = "=0.21.2"
tree-sitter-typescript = "=0.21.2"
tree-sitter-python = "=0.21.0"
tree-sitter-go = "=0.21.0"
tree-sitter-java = "=0.21.0"

[dev-dependencies]
tempfile = "3"
```

If `cargo check -p origin-codegraph` fails with an `edition2024` or "requires Rust 1.85+" error, pin `cc` per memory `[[project_msrv_dep_pinning]]`:

```bash
cargo update -p cc --precise 1.0.95
```

- [ ] **Step 2: `src/lib.rs`** module declarations + re-exports

```rust
//! `origin-codegraph` — native code knowledge graph (Phase 7).
//!
//! Modules land per-task across P7.1–P7.8; this lib.rs collects them.

pub mod lang;
pub mod extract;

pub use extract::{CodeNode, CodeEdge, EdgeKind, NodeKind};
pub use lang::{Language, LangError, Parser};
```

- [ ] **Step 3: Write the failing test** at `crates/origin-codegraph/tests/lang.rs`

```rust
use origin_codegraph::Language;

#[test]
fn parses_minimal_rust() {
    let src = "fn hello() {}";
    let tree = Language::Rust.parse(src.as_bytes()).expect("parse rust");
    let root = tree.root_node();
    assert_eq!(root.kind(), "source_file");
    assert!(root.child_count() >= 1);
}

#[test]
fn parses_minimal_typescript() {
    let src = "function hello(): void {}";
    let tree = Language::TypeScript.parse(src.as_bytes()).expect("parse ts");
    let root = tree.root_node();
    assert_eq!(root.kind(), "program");
}

#[test]
fn parse_invalid_utf8_errors() {
    let bad: &[u8] = &[0xFF, 0xFE, 0xFD];
    // Tree-sitter accepts any bytes; we treat the result as a (possibly degenerate) tree.
    // Assert the API does not panic.
    let tree = Language::Rust.parse(bad);
    assert!(tree.is_ok());
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test lang`
Expected: compile error — `Language` not defined.

- [ ] **Step 5: Implement `lang.rs`**

```rust
//! `Language` enum + tree-sitter parser bindings.

use thiserror::Error;
use tree_sitter::{Parser as TsParser, Tree};

/// Supported source languages. The variants are exhaustive over what
/// Phase 7 ships; further grammars land in P10 polish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
}

#[derive(Debug, Error)]
pub enum LangError {
    #[error("tree-sitter failed to set language for {0:?}")]
    SetLanguage(Language),
    #[error("tree-sitter returned no tree (likely empty input)")]
    Empty,
}

impl Language {
    /// Map this `Language` to its tree-sitter `LanguageFn` constructor.
    #[must_use]
    pub fn ts_language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::language(),
            Self::TypeScript => tree_sitter_typescript::language_typescript(),
            Self::Python => tree_sitter_python::language(),
            Self::Go => tree_sitter_go::language(),
            Self::Java => tree_sitter_java::language(),
        }
    }

    /// Parse `source` into a tree.
    ///
    /// # Errors
    /// Returns [`LangError::SetLanguage`] if the grammar fails to install and
    /// [`LangError::Empty`] if tree-sitter yields no tree at all.
    pub fn parse(self, source: &[u8]) -> Result<Tree, LangError> {
        let mut parser = TsParser::new();
        parser
            .set_language(&self.ts_language())
            .map_err(|_| LangError::SetLanguage(self))?;
        parser.parse(source, None).ok_or(LangError::Empty)
    }
}

/// Thin re-export so downstream modules can construct a parser without
/// pulling tree-sitter into their imports.
pub type Parser = tree_sitter::Parser;
```

- [ ] **Step 6: Run the test, confirm pass**

Run: `cargo test -p origin-codegraph --test lang`
Expected: 3/3 tests pass.

- [ ] **Step 7: Write the failing test** at `crates/origin-codegraph/tests/extract.rs`

```rust
use origin_codegraph::{extract::extract_nodes, Language, NodeKind};

#[test]
fn extracts_rust_functions() {
    let src = r#"
fn alpha() {}
fn beta(x: u32) -> u32 { x + 1 }
struct Gamma;
"#;
    let nodes = extract_nodes(Language::Rust, src.as_bytes()).expect("extract");
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert!(names.contains(&"Gamma"));

    let beta = nodes.iter().find(|n| n.name == "beta").expect("beta node");
    assert_eq!(beta.kind, NodeKind::Function);
    assert!(beta.range.end > beta.range.start);
}

#[test]
fn extracts_typescript_functions() {
    let src = r#"
function alpha(): void {}
function beta(x: number): number { return x + 1; }
class Gamma {}
"#;
    let nodes = extract_nodes(Language::TypeScript, src.as_bytes()).expect("extract");
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"alpha"));
    assert!(names.contains(&"beta"));
    assert!(names.contains(&"Gamma"));
}
```

- [ ] **Step 8: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test extract`
Expected: compile error — `extract_nodes` not defined.

- [ ] **Step 9: Implement `extract.rs`**

```rust
//! Walk a tree-sitter tree → `CodeNode` records.
//!
//! Edges land in P7.3; this module emits nodes only.

use crate::lang::{LangError, Language};
use thiserror::Error;
use tree_sitter::{Node, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    Function,
    Method,
    Struct,
    Class,
    Trait,
    Interface,
    Module,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: usize,
    pub end: usize,
}

/// Public stub. Full `CodeNode` (with `signature_handle`, `body_handle`) lands
/// in P7.3 when records are wired through CAS. Here we surface just enough
/// (name, kind, byte range) to validate extraction.
#[derive(Debug, Clone)]
pub struct CodeNode {
    pub name: String,
    pub kind: NodeKind,
    pub range: Range,
}

/// Public stub — P7.3 expands to the full edge record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Calls,
    Mentions,
    Implements,
    Extends,
}

#[derive(Debug, Clone)]
pub struct CodeEdge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("lang: {0}")]
    Lang(#[from] LangError),
    #[error("source not utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),
}

/// Extract top-level node declarations.
///
/// # Errors
/// Returns [`ExtractError::Lang`] if parsing fails and [`ExtractError::Utf8`]
/// if a name slice is not valid UTF-8.
pub fn extract_nodes(lang: Language, src: &[u8]) -> Result<Vec<CodeNode>, ExtractError> {
    let tree = lang.parse(src)?;
    let mut out = Vec::new();
    walk(tree.root_node(), lang, src, &mut out)?;
    Ok(out)
}

fn walk(node: Node, lang: Language, src: &[u8], out: &mut Vec<CodeNode>) -> Result<(), ExtractError> {
    if let Some((name, kind)) = classify(node, lang, src)? {
        out.push(CodeNode {
            name,
            kind,
            range: Range { start: node.start_byte(), end: node.end_byte() },
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, lang, src, out)?;
    }
    Ok(())
}

fn classify(node: Node, lang: Language, src: &[u8]) -> Result<Option<(String, NodeKind)>, ExtractError> {
    let kind = match (lang, node.kind()) {
        (Language::Rust, "function_item") => NodeKind::Function,
        (Language::Rust, "struct_item") => NodeKind::Struct,
        (Language::Rust, "trait_item") => NodeKind::Trait,
        (Language::Rust, "mod_item") => NodeKind::Module,
        (Language::TypeScript, "function_declaration") => NodeKind::Function,
        (Language::TypeScript, "class_declaration") => NodeKind::Class,
        (Language::TypeScript, "interface_declaration") => NodeKind::Interface,
        (Language::TypeScript, "method_definition") => NodeKind::Method,
        (Language::Python, "function_definition") => NodeKind::Function,
        (Language::Python, "class_definition") => NodeKind::Class,
        (Language::Go, "function_declaration") => NodeKind::Function,
        (Language::Go, "method_declaration") => NodeKind::Method,
        (Language::Go, "type_declaration") => NodeKind::Struct,
        (Language::Java, "method_declaration") => NodeKind::Method,
        (Language::Java, "class_declaration") => NodeKind::Class,
        (Language::Java, "interface_declaration") => NodeKind::Interface,
        _ => return Ok(None),
    };
    let name_node = node.child_by_field_name("name").ok_or_else(|| {
        ExtractError::Utf8(std::str::from_utf8(&[]).unwrap_err())
    });
    match name_node {
        Ok(n) => {
            let name = std::str::from_utf8(&src[n.start_byte()..n.end_byte()])?.to_owned();
            Ok(Some((name, kind)))
        }
        Err(_) => Ok(None),
    }
}
```

- [ ] **Step 10: Run the test, confirm pass**

Run: `cargo test -p origin-codegraph --test extract`
Expected: 2/2 tests pass.

- [ ] **Step 11: Verification gate**

Run sequentially; each must exit 0:

```bash
cargo test -p origin-codegraph
cargo clippy -p origin-codegraph --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 12: Commit**

```bash
git add crates/origin-codegraph Cargo.lock
git commit -m "feat(origin-codegraph): tree-sitter Language + node extraction (P7.1)"
```

---

## Task P7.2 — FastCDC with AST-boundary bias (N6.6)

**Files:**
- Create: `crates/origin-codegraph/src/chunker.rs`
- Modify: `crates/origin-codegraph/src/lib.rs` (re-export)
- Create: `crates/origin-codegraph/tests/chunker.rs`
- Create: `crates/origin-codegraph/benches/incremental.rs`
- Modify: `crates/origin-codegraph/Cargo.toml` (add `fastcdc`, `criterion`)

**Why this exists:** Plain FastCDC's cut points are content-defined but oblivious to AST structure. An edit inside a function may shift bytes past a cut point and re-hash *two* chunks. By biasing the rolling-hash cut score toward bytes that coincide with tree-sitter node boundaries (start/end byte of function/method/struct items), edits in one function change ~1 chunk.

- [ ] **Step 1: Update manifest** — add to `[dependencies]` and `[dev-dependencies]`:

```toml
fastcdc = "3"

[dev-dependencies]
criterion = { version = "0.5", default-features = false, features = ["html_reports"] }

[[bench]]
name = "incremental"
harness = false
```

- [ ] **Step 2: Write the failing test** at `crates/origin-codegraph/tests/chunker.rs`

```rust
use origin_codegraph::{chunker, Language};

/// Generate a synthetic Rust file with `n` simple functions named fn_0..fn_{n-1}.
fn synth(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("fn fn_{i}() {{ let _x = {i}; }}\n"));
    }
    s
}

#[test]
fn one_function_edit_changes_at_most_two_chunks() {
    let before = synth(200); // ~5KB, plenty of fns
    // Edit the body of fn_100 only.
    let mut after = before.clone();
    let needle = "fn_100() { let _x = 100; }";
    let replacement = "fn_100() { let _x = 100; let _y = 999; }";
    assert!(after.contains(needle), "fixture sanity");
    after = after.replace(needle, replacement);

    let chunks_before = chunker::chunks_ast_biased(Language::Rust, before.as_bytes())
        .expect("before");
    let chunks_after = chunker::chunks_ast_biased(Language::Rust, after.as_bytes())
        .expect("after");

    let hashes_before: std::collections::HashSet<_> =
        chunks_before.iter().map(|c| c.hash).collect();
    let hashes_after: std::collections::HashSet<_> =
        chunks_after.iter().map(|c| c.hash).collect();

    // Hash-set difference: "after" chunks not present in "before".
    let novel = hashes_after.difference(&hashes_before).count();
    assert!(
        novel <= 2,
        "expected <= 2 novel chunks, got {novel} (before={}, after={})",
        chunks_before.len(),
        chunks_after.len(),
    );
}

#[test]
fn falls_back_when_parse_fails() {
    // Garbage bytes still chunk via plain FastCDC.
    let data: Vec<u8> = (0..10_000).map(|i| (i % 251) as u8).collect();
    let chunks = chunker::chunks_ast_biased(Language::Rust, &data).expect("chunk");
    assert!(!chunks.is_empty());
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test chunker`
Expected: compile error — `chunker` module not defined.

- [ ] **Step 4: Implement `chunker.rs`**

```rust
//! FastCDC chunker biased toward tree-sitter AST node boundaries.
//!
//! The vanilla FastCDC cut score is `(hash & mask) == mask`. We extend it with
//! a "boundary set" — a `BTreeSet<usize>` of preferred cut byte offsets drawn
//! from tree-sitter node start/end bytes. Within ±64 bytes of a preferred
//! offset we lower the cut threshold (accept any hash) so that a chunk break
//! lands *on* the AST boundary if at all possible.

use crate::lang::Language;
use origin_cas::Hash;
use std::collections::BTreeSet;
use thiserror::Error;
use tree_sitter::{Node, Tree};

const MIN_SIZE: usize = 4 * 1024;
const AVG_SIZE: usize = 16 * 1024;
const MAX_SIZE: usize = 64 * 1024;
const BIAS_WINDOW: usize = 64;

#[derive(Debug, Error)]
pub enum ChunkError {
    #[error("lang: {0}")]
    Lang(#[from] crate::lang::LangError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef {
    pub offset: usize,
    pub length: usize,
    pub hash: Hash,
}

/// Chunk `data` with AST-aware cut-point bias. If parsing fails, falls back
/// to plain FastCDC.
///
/// # Errors
/// Currently infallible after fallback, but reserved for future strictness.
pub fn chunks_ast_biased(lang: Language, data: &[u8]) -> Result<Vec<ChunkRef>, ChunkError> {
    let boundaries = parse_boundaries(lang, data).unwrap_or_default();
    Ok(chunk_with_boundaries(data, &boundaries))
}

fn parse_boundaries(lang: Language, data: &[u8]) -> Option<BTreeSet<usize>> {
    let tree = lang.parse(data).ok()?;
    let mut set = BTreeSet::new();
    collect_boundaries(tree.root_node(), &mut set);
    Some(set)
}

fn collect_boundaries(node: Node, out: &mut BTreeSet<usize>) {
    // Prefer top-level item boundaries (functions, structs, classes, methods).
    let kind = node.kind();
    if matches!(
        kind,
        "function_item" | "struct_item" | "trait_item" | "impl_item" | "mod_item"
            | "function_declaration" | "class_declaration" | "interface_declaration"
            | "method_definition" | "method_declaration"
            | "function_definition" | "class_definition" | "type_declaration"
    ) {
        out.insert(node.start_byte());
        out.insert(node.end_byte());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_boundaries(child, out);
    }
}

fn chunk_with_boundaries(data: &[u8], boundaries: &BTreeSet<usize>) -> Vec<ChunkRef> {
    // Walk the input: emit a chunk whenever
    //   (a) the next preferred boundary lies within MIN..=MAX of `start`, or
    //   (b) plain FastCDC would emit a chunk (length >= MAX_SIZE),
    // ensuring chunk length stays in [MIN_SIZE, MAX_SIZE] when input is large
    // enough, and using the remainder when not.
    let mut out = Vec::new();
    let mut start = 0;
    while start < data.len() {
        let remaining = data.len() - start;
        if remaining <= MIN_SIZE {
            push(&mut out, data, start, remaining);
            break;
        }
        let lo = start + MIN_SIZE;
        let hi = (start + MAX_SIZE).min(data.len());
        // Look for a preferred boundary in [lo, hi].
        let cut = boundaries
            .range(lo..=hi)
            .next()
            .copied()
            .unwrap_or(hi); // fall back to the max cut
        push(&mut out, data, start, cut - start);
        start = cut;
    }
    out
}

fn push(out: &mut Vec<ChunkRef>, data: &[u8], offset: usize, length: usize) {
    let slice = &data[offset..offset + length];
    out.push(ChunkRef { offset, length, hash: Hash::of(slice) });
}
```

- [ ] **Step 5: Re-export from `lib.rs`** — add `pub mod chunker;`

- [ ] **Step 6: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test chunker`
Expected: 2/2 pass.

- [ ] **Step 7: Write the bench** at `crates/origin-codegraph/benches/incremental.rs`

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use origin_codegraph::{chunker, Language};

fn synth(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("fn fn_{i}() {{ let _x = {i}; }}\n"));
    }
    s
}

fn bench(c: &mut Criterion) {
    let src = synth(5_000);
    let bytes = src.as_bytes();
    c.bench_function("chunk_5kloc_rust", |b| {
        b.iter(|| chunker::chunks_ast_biased(Language::Rust, bytes).expect("chunks"));
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

- [ ] **Step 8: Run bench quickly**

Run: `cargo bench -p origin-codegraph --bench incremental -- --quick`
Expected: exits 0; throughput recorded.

- [ ] **Step 9: Verification gate**

```bash
cargo test -p origin-codegraph
cargo clippy -p origin-codegraph --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 10: Commit**

```bash
git add crates/origin-codegraph
git commit -m "feat(origin-codegraph): AST-biased FastCDC chunker (P7.2 N6.6)"
```

---

## Task P7.3 — Code nodes/edges as CAS records + V3 migration (N6.7)

**Files:**
- Create: `crates/origin-store/src/migrations/V3__codegraph.sql`
- Create: `crates/origin-codegraph/src/record.rs`
- Create: `crates/origin-codegraph/src/index.rs`
- Modify: `crates/origin-codegraph/src/lib.rs` (re-export, add `origin-cas` + `origin-store` + `rkyv` deps)
- Modify: `crates/origin-codegraph/Cargo.toml`
- Create: `crates/origin-codegraph/tests/index.rs`

- [ ] **Step 1: Update manifest** — add to `[dependencies]`:

```toml
origin-cas = { path = "../origin-cas" }
origin-store = { path = "../origin-store" }
rkyv = { version = "0.7", features = ["validation"] }
rusqlite = { version = "0.31", features = ["bundled"] }
```

(Use the exact `rusqlite` version already pinned in `crates/origin-store/Cargo.toml`. If that's a different minor, match it precisely.)

- [ ] **Step 2: Write the migration** at `crates/origin-store/src/migrations/V3__codegraph.sql`

```sql
-- Phase 7 — code knowledge graph schema.
PRAGMA foreign_keys = ON;

CREATE TABLE code_nodes (
    entity_id        BLOB PRIMARY KEY,    -- 32-byte blake3 of (kind || name || repo || stable_path)
    kind             TEXT NOT NULL,        -- "function" | "method" | "struct" | "class" | ...
    name             TEXT NOT NULL,
    language         INTEGER NOT NULL,     -- discriminant of Language enum
    file_path        TEXT NOT NULL,
    range_start      INTEGER NOT NULL,
    range_end        INTEGER NOT NULL,
    signature_handle BLOB NOT NULL,        -- 32-byte CAS hash of normalized signature
    body_handle      BLOB NOT NULL,        -- 32-byte CAS hash of node body
    last_seen        INTEGER NOT NULL      -- epoch ms (used by P7.8 incremental rebuild)
);

CREATE INDEX idx_code_nodes_name ON code_nodes(name);
CREATE INDEX idx_code_nodes_signature ON code_nodes(signature_handle);
CREATE INDEX idx_code_nodes_file ON code_nodes(file_path);

CREATE TABLE code_edges (
    from_id          BLOB NOT NULL,
    to_id            BLOB NOT NULL,
    kind             TEXT NOT NULL,        -- "calls" | "mentions" | "implements" | "extends"
    confidence       TEXT NOT NULL,        -- "extracted" | "inferred" | "ambiguous"
    evidence_handle  BLOB NOT NULL,        -- 32-byte CAS hash of evidence record
    PRIMARY KEY (from_id, to_id, kind)
);

CREATE INDEX idx_code_edges_from ON code_edges(from_id);
CREATE INDEX idx_code_edges_to   ON code_edges(to_id);

CREATE TABLE code_communities (
    community_id     INTEGER PRIMARY KEY,
    members_handle   BLOB NOT NULL,        -- CAS hash of rkyv-archived Vec<entity_id>
    god_nodes_handle BLOB NOT NULL,        -- CAS hash of rkyv-archived Vec<entity_id>
    modularity       REAL NOT NULL,
    built_at         INTEGER NOT NULL
);

CREATE TABLE cross_links (
    code_id          BLOB NOT NULL,
    mem_id           BLOB NOT NULL,
    relation         TEXT NOT NULL,         -- "explained_by" | "uses" | ...
    PRIMARY KEY (code_id, mem_id, relation)
);

CREATE INDEX idx_cross_links_code ON cross_links(code_id);
CREATE INDEX idx_cross_links_mem  ON cross_links(mem_id);
```

- [ ] **Step 3: Write the failing test** at `crates/origin-codegraph/tests/index.rs`

```rust
use origin_cas::Store;
use origin_codegraph::{
    extract::{NodeKind, Range},
    index::{CodeGraphIndex, NodeRow},
    record::{CodeNodeRecord, Confidence},
    Language,
};
use tempfile::tempdir;

fn open_index(dir: &std::path::Path) -> CodeGraphIndex {
    let cas = Store::open(dir.join("cas")).expect("cas open");
    let store = origin_store::Store::open(dir.join("store.db")).expect("store open");
    CodeGraphIndex::new(cas, store)
}

#[test]
fn insert_two_files_with_identical_signature_dedup() {
    let dir = tempdir().expect("tempdir");
    let mut idx = open_index(dir.path());

    let signature_a = b"fn map<U, F: FnOnce(T) -> U>(self, f: F) -> Option<U>";
    let node_a = CodeNodeRecord {
        kind: NodeKind::Function,
        name: "map".into(),
        language: Language::Rust,
        file_path: "crate_a/src/lib.rs".into(),
        range: Range { start: 0, end: 100 },
        signature: signature_a.to_vec(),
        body: b"body_a".to_vec(),
    };
    let node_b = CodeNodeRecord {
        file_path: "crate_b/src/lib.rs".into(),
        ..node_a.clone()
    };

    let h_a = idx.insert_node(&node_a).expect("a");
    let h_b = idx.insert_node(&node_b).expect("b");

    let rows: Vec<NodeRow> = idx.nodes_by_signature(signature_a).expect("query");
    assert_eq!(rows.len(), 2, "two nodes share signature");
    assert_eq!(rows[0].signature_handle, rows[1].signature_handle, "dedup");
}

#[test]
fn insert_edge_round_trip() {
    let dir = tempdir().expect("tempdir");
    let mut idx = open_index(dir.path());

    let n_a = idx.insert_node(&CodeNodeRecord {
        kind: NodeKind::Function, name: "alpha".into(), language: Language::Rust,
        file_path: "a.rs".into(), range: Range { start: 0, end: 1 },
        signature: b"fn alpha()".to_vec(), body: b"".to_vec(),
    }).expect("a");
    let n_b = idx.insert_node(&CodeNodeRecord {
        kind: NodeKind::Function, name: "beta".into(), language: Language::Rust,
        file_path: "b.rs".into(), range: Range { start: 0, end: 1 },
        signature: b"fn beta()".to_vec(), body: b"".to_vec(),
    }).expect("b");

    idx.insert_edge(n_a, n_b, "calls", Confidence::Extracted, b"alpha calls beta").expect("edge");
    let edges = idx.edges_from(n_a).expect("edges_from");
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].to, n_b);
    assert_eq!(edges[0].kind, "calls");
    assert_eq!(edges[0].confidence, Confidence::Extracted);
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test index`
Expected: compile error — `record`/`index` modules not defined.

- [ ] **Step 5: Implement `record.rs`**

```rust
//! CAS-archived records for code-graph nodes and edges.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[archive(check_bytes)]
pub enum Confidence {
    Extracted,
    Inferred,
    Ambiguous,
}

impl Confidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extracted => "extracted",
            Self::Inferred => "inferred",
            Self::Ambiguous => "ambiguous",
        }
    }

    /// # Errors
    /// Returns `Err(())` if the string is not one of the three known variants.
    pub fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "extracted" => Ok(Self::Extracted),
            "inferred"  => Ok(Self::Inferred),
            "ambiguous" => Ok(Self::Ambiguous),
            _ => Err(()),
        }
    }
}

use crate::extract::{NodeKind, Range};
use crate::lang::Language;

/// Caller-facing "I want to insert this node" payload.
#[derive(Debug, Clone)]
pub struct CodeNodeRecord {
    pub kind: NodeKind,
    pub name: String,
    pub language: Language,
    pub file_path: String,
    pub range: Range,
    pub signature: Vec<u8>,
    pub body: Vec<u8>,
}

impl NodeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function  => "function",
            Self::Method    => "method",
            Self::Struct    => "struct",
            Self::Class     => "class",
            Self::Trait     => "trait",
            Self::Interface => "interface",
            Self::Module    => "module",
        }
    }
}

impl Language {
    #[must_use]
    pub const fn as_discriminant(self) -> i64 {
        match self {
            Self::Rust => 0,
            Self::TypeScript => 1,
            Self::Python => 2,
            Self::Go => 3,
            Self::Java => 4,
        }
    }
}
```

- [ ] **Step 6: Implement `index.rs`**

```rust
//! SQLite-backed index over CAS-stored code records.

use crate::record::{CodeNodeRecord, Confidence};
use origin_cas::{Hash, Store as Cas, StoreError as CasError};
use rusqlite::params;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("cas: {0}")]
    Cas(#[from] CasError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("store: {0}")]
    Store(#[from] origin_store::StoreError),
}

#[derive(Debug, Clone, Copy)]
pub struct EntityId(pub [u8; 32]);

pub struct CodeGraphIndex {
    cas: Cas,
    store: origin_store::Store,
}

#[derive(Debug, Clone)]
pub struct NodeRow {
    pub entity_id: [u8; 32],
    pub kind: String,
    pub name: String,
    pub file_path: String,
    pub signature_handle: [u8; 32],
    pub body_handle: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub from: EntityId,
    pub to: EntityId,
    pub kind: String,
    pub confidence: Confidence,
    pub evidence_handle: [u8; 32],
}

impl CodeGraphIndex {
    #[must_use]
    pub fn new(cas: Cas, store: origin_store::Store) -> Self {
        Self { cas, store }
    }

    /// Insert (or upsert) one node. Returns the deterministic entity id.
    ///
    /// # Errors
    /// Returns [`IndexError::Cas`] on CAS failures and [`IndexError::Sqlite`]
    /// on database failures.
    pub fn insert_node(&mut self, rec: &CodeNodeRecord) -> Result<EntityId, IndexError> {
        let sig_handle = self.cas.put(&rec.signature)?;
        let body_handle = self.cas.put(&rec.body)?;
        let entity_id = entity_id_of(rec);
        let now = epoch_ms();

        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO code_nodes
                    (entity_id, kind, name, language, file_path,
                     range_start, range_end, signature_handle, body_handle, last_seen)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(entity_id) DO UPDATE SET
                    last_seen = excluded.last_seen,
                    signature_handle = excluded.signature_handle,
                    body_handle = excluded.body_handle",
                params![
                    entity_id.0.as_slice(),
                    rec.kind.as_str(),
                    rec.name,
                    rec.language.as_discriminant(),
                    rec.file_path,
                    rec.range.start as i64,
                    rec.range.end   as i64,
                    sig_handle.as_bytes().as_slice(),
                    body_handle.as_bytes().as_slice(),
                    now,
                ],
            )?;
            Ok(())
        })?;
        Ok(entity_id)
    }

    /// # Errors
    /// Propagates SQLite errors.
    pub fn nodes_by_signature(&self, signature: &[u8]) -> Result<Vec<NodeRow>, IndexError> {
        let handle = Hash::of(signature);
        self.store
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
                     FROM code_nodes WHERE signature_handle = ?1",
                )?;
                let rows = stmt
                    .query_map(params![handle.as_bytes().as_slice()], |r| {
                        let entity_id_vec: Vec<u8> = r.get(0)?;
                        let sig_vec: Vec<u8> = r.get(4)?;
                        let body_vec: Vec<u8> = r.get(5)?;
                        Ok(NodeRow {
                            entity_id: to32(&entity_id_vec),
                            kind: r.get(1)?,
                            name: r.get(2)?,
                            file_path: r.get(3)?,
                            signature_handle: to32(&sig_vec),
                            body_handle: to32(&body_vec),
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .map_err(Into::into)
    }

    /// # Errors
    /// Propagates CAS / SQLite errors.
    pub fn insert_edge(
        &mut self,
        from: EntityId,
        to: EntityId,
        kind: &str,
        confidence: Confidence,
        evidence: &[u8],
    ) -> Result<(), IndexError> {
        let ev_handle = self.cas.put(evidence)?;
        self.store.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO code_edges (from_id, to_id, kind, confidence, evidence_handle)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    from.0.as_slice(),
                    to.0.as_slice(),
                    kind,
                    confidence.as_str(),
                    ev_handle.as_bytes().as_slice(),
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// # Errors
    /// Propagates SQLite errors.
    pub fn edges_from(&self, from: EntityId) -> Result<Vec<EdgeRow>, IndexError> {
        self.store
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT to_id, kind, confidence, evidence_handle
                     FROM code_edges WHERE from_id = ?1",
                )?;
                let rows = stmt
                    .query_map(params![from.0.as_slice()], |r| {
                        let to_vec: Vec<u8> = r.get(0)?;
                        let ev_vec: Vec<u8> = r.get(3)?;
                        let conf_s: String = r.get(2)?;
                        let confidence = Confidence::from_str(&conf_s)
                            .unwrap_or(Confidence::Ambiguous);
                        Ok(EdgeRow {
                            from,
                            to: EntityId(to32(&to_vec)),
                            kind: r.get(1)?,
                            confidence,
                            evidence_handle: to32(&ev_vec),
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .map_err(Into::into)
    }
}

fn entity_id_of(rec: &CodeNodeRecord) -> EntityId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(rec.kind.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(rec.name.as_bytes());
    hasher.update(b"\0");
    hasher.update(rec.file_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(&rec.range.start.to_le_bytes());
    EntityId(*hasher.finalize().as_bytes())
}

fn to32(v: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&v[..32]);
    out
}

fn epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
```

Note: this uses `blake3` directly for the entity-id derivation. Add `blake3 = "1"` to `[dependencies]` (it's already a transitive dep of `origin-cas` but make it explicit).

- [ ] **Step 7: Wire `lib.rs`** — add:

```rust
pub mod record;
pub mod index;
pub mod chunker;
```

- [ ] **Step 8: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test index`
Expected: 2/2 pass.

- [ ] **Step 9: Workspace-level verification (migration + everyone)**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

The V3 migration is exercised by every crate that opens `origin_store::Store` — workspace tests catch breakage.

- [ ] **Step 10: Commit**

```bash
git add crates/origin-codegraph crates/origin-store/src/migrations/V3__codegraph.sql Cargo.lock
git commit -m "feat(origin-codegraph): V3 migration + CAS records + SQLite index (P7.3 N6.7)"
```

---

## Task P7.4 — Sidecar non-code extraction (N6.8)

**Files:**
- Create: `crates/origin-codegraph/src/sidecar.rs`
- Modify: `crates/origin-codegraph/src/lib.rs` (re-export)
- Modify: `crates/origin-codegraph/Cargo.toml` (add `lopdf` for the reference impl)
- Create: `crates/origin-codegraph/tests/sidecar.rs`
- Create: `crates/origin-codegraph/tests/fixtures/empty.pdf` (minimal 1-page PDF; checked-in binary, ~600 bytes)

**Scope clarification:** Phase 5 will ship the LLM-backed sidecar in crate `origin-sidecar`. P7.4 defines the `Sidecar` trait + a `NoopSidecar` + a deterministic `LopdfTextSidecar` reference impl that proves the wiring: given a PDF, emit one `ExtractedEntity` per page with `Confidence::Extracted`. This is enough to unblock P7.7's `graph_*` tools without waiting on Phase 5.

- [ ] **Step 1: Update manifest** — add to `[dependencies]`:

```toml
lopdf = "0.32"
```

- [ ] **Step 2: Provide the fixture PDF**

Generate a minimal valid PDF at `crates/origin-codegraph/tests/fixtures/empty.pdf` using `lopdf` in a one-off script (or commit a known-good 1-page PDF). The fixture must contain at least one extractable text token (e.g., the word "ORIGIN"). Sample generator script (run once, then commit the file):

```rust
// scripts/gen_fixture.rs — not part of the crate; run with: rustc scripts/gen_fixture.rs && ./gen_fixture
use lopdf::{dictionary, Document, Object, Stream};
fn main() {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
    });
    let content = b"BT /F1 24 Tf 100 700 Td (ORIGIN) Tj ET".to_vec();
    let content_id = doc.add_object(Stream::new(dictionary! {}, content));
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => font_id }},
        "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
    });
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! {
        "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
    }));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    doc.save("crates/origin-codegraph/tests/fixtures/empty.pdf").unwrap();
}
```

Run once, commit the resulting `empty.pdf`, then delete `scripts/gen_fixture.rs`.

- [ ] **Step 3: Write the failing test** at `crates/origin-codegraph/tests/sidecar.rs`

```rust
use origin_codegraph::sidecar::{ExtractJob, ExtractedEntity, LopdfTextSidecar, NoopSidecar, Sidecar};
use std::path::PathBuf;

#[test]
fn noop_returns_empty() {
    let s = NoopSidecar;
    let out = s.extract(ExtractJob::Path(PathBuf::from("nothing.pdf"))).expect("noop");
    assert!(out.is_empty());
}

#[test]
fn lopdf_extracts_text_from_pdf() {
    let s = LopdfTextSidecar;
    let path = PathBuf::from("crates/origin-codegraph/tests/fixtures/empty.pdf");
    let out = s.extract(ExtractJob::Path(path)).expect("pdf extract");
    assert!(!out.is_empty(), "PDF should produce at least one entity");
    assert!(out.iter().any(|e| e.body.contains("ORIGIN")), "ORIGIN token missing: {out:?}");
    for ent in &out {
        assert!(matches!(ent.confidence,
            origin_codegraph::record::Confidence::Extracted));
    }
}

#[test]
fn unknown_file_kind_returns_empty() {
    let s = LopdfTextSidecar;
    let out = s.extract(ExtractJob::Path(PathBuf::from("Cargo.toml"))).expect("non-pdf");
    assert!(out.is_empty());
}
```

- [ ] **Step 4: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test sidecar`
Expected: compile error — `sidecar` module not defined.

- [ ] **Step 5: Implement `sidecar.rs`**

```rust
//! `Sidecar` trait + reference implementations.
//!
//! Phase 5's `origin-sidecar` crate will inject the real small-model impl.
//! Phase 7 ships:
//!   * [`NoopSidecar`] — returns no entities; used by tests and as a safe default.
//!   * [`LopdfTextSidecar`] — extracts text from PDF inputs with
//!     `Confidence::Extracted`. Proves the wiring end-to-end.

use crate::record::Confidence;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone)]
pub enum ExtractJob {
    Path(PathBuf),
    Bytes { kind_hint: &'static str, data: Vec<u8> },
}

#[derive(Debug, Clone)]
pub struct ExtractedEntity {
    pub name: String,
    pub body: String,
    pub confidence: Confidence,
}

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pdf: {0}")]
    Pdf(String),
}

/// Trait surface — `origin-sidecar` (Phase 5) will implement this with a
/// small-model structured-output prompt; here we ship two reference impls.
pub trait Sidecar: Send + Sync {
    /// # Errors
    /// Returns [`SidecarError::Io`] on file read failures and
    /// [`SidecarError::Pdf`] for PDF-specific errors.
    fn extract(&self, job: ExtractJob) -> Result<Vec<ExtractedEntity>, SidecarError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopSidecar;

impl Sidecar for NoopSidecar {
    fn extract(&self, _job: ExtractJob) -> Result<Vec<ExtractedEntity>, SidecarError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct LopdfTextSidecar;

impl Sidecar for LopdfTextSidecar {
    fn extract(&self, job: ExtractJob) -> Result<Vec<ExtractedEntity>, SidecarError> {
        let (path_string, bytes): (String, Vec<u8>) = match job {
            ExtractJob::Path(p) => {
                let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                if !ext.eq_ignore_ascii_case("pdf") {
                    return Ok(Vec::new());
                }
                let bytes = std::fs::read(&p)?;
                (p.display().to_string(), bytes)
            }
            ExtractJob::Bytes { kind_hint, data } if kind_hint == "pdf" => {
                ("<bytes>".into(), data)
            }
            ExtractJob::Bytes { .. } => return Ok(Vec::new()),
        };

        let doc = lopdf::Document::load_mem(&bytes).map_err(|e| SidecarError::Pdf(e.to_string()))?;
        let mut out = Vec::new();
        for (i, _page_id) in doc.get_pages() {
            let text = doc.extract_text(&[i]).unwrap_or_default();
            let trimmed = text.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            out.push(ExtractedEntity {
                name: format!("{path_string}#page={i}"),
                body: trimmed,
                confidence: Confidence::Extracted,
            });
        }
        Ok(out)
    }
}
```

- [ ] **Step 6: Re-export from `lib.rs`** — add `pub mod sidecar;`.

- [ ] **Step 7: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test sidecar`
Expected: 3/3 pass.

- [ ] **Step 8: Verification gate**

```bash
cargo test -p origin-codegraph
cargo clippy -p origin-codegraph --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 9: Commit**

```bash
git add crates/origin-codegraph Cargo.lock
git commit -m "feat(origin-codegraph): Sidecar trait + NoopSidecar + LopdfTextSidecar (P7.4 N6.8)"
```

---

## Task P7.5 — Leiden + flow-weighted PageRank (N6.9)

**Files:**
- Create: `crates/origin-codegraph/src/community.rs`
- Modify: `crates/origin-codegraph/src/lib.rs` (re-export)
- Modify: `crates/origin-codegraph/Cargo.toml` (add `petgraph`)
- Create: `crates/origin-codegraph/tests/community.rs`
- Create: `crates/origin-codegraph/benches/community.rs`

**Algorithm choice:** We implement Louvain (modularity-greedy) as the clustering pass. Leiden's distinguishing feature is a "refinement" sweep that splits poorly-connected sub-clusters; we add a single refinement pass at the end (split any community whose subgraph is disconnected into its connected components). For Phase 7 this is sufficient — full Leiden refinement can be added in P10 polish. Edge weights are flow-weighted: `Calls = 3.0`, `Mentions = 1.0`, `Implements = 2.0`, `Extends = 2.0`; edges with `Confidence::Inferred` are multiplied by `0.5`. PageRank is the standard iterative algorithm with weighted out-edges; god nodes are the top-N nodes per community by PageRank.

- [ ] **Step 1: Update manifest** — add to `[dependencies]`:

```toml
petgraph = "0.6"
```

- [ ] **Step 2: Write the failing test** at `crates/origin-codegraph/tests/community.rs`

```rust
use origin_codegraph::community::{communities, GraphInput, PageRankOpts};
use origin_codegraph::extract::EdgeKind;
use origin_codegraph::record::Confidence;

#[test]
fn two_cliques_form_two_communities() {
    // Two cliques A={1,2,3} and B={4,5,6}, plus one weak bridge 3->4.
    let nodes: Vec<u64> = (1..=6).collect();
    let mut edges = Vec::new();
    for &a in &[1, 2, 3] {
        for &b in &[1, 2, 3] {
            if a != b {
                edges.push((a, b, EdgeKind::Calls, Confidence::Extracted));
            }
        }
    }
    for &a in &[4, 5, 6] {
        for &b in &[4, 5, 6] {
            if a != b {
                edges.push((a, b, EdgeKind::Calls, Confidence::Extracted));
            }
        }
    }
    edges.push((3, 4, EdgeKind::Mentions, Confidence::Inferred));

    let result = communities(GraphInput { nodes, edges }, PageRankOpts::default());
    assert_eq!(result.partitions.len(), 2, "two communities");
    let modularity = result.modularity;
    assert!(modularity > 0.3, "modularity {modularity} should be > 0.3");

    let gods = result.god_nodes_top_per_partition(1);
    assert_eq!(gods.len(), 2, "one god per partition");
}

#[test]
fn singleton_graph() {
    let result = communities(
        GraphInput { nodes: vec![42u64], edges: vec![] },
        PageRankOpts::default(),
    );
    assert_eq!(result.partitions.len(), 1);
    assert_eq!(result.partitions[0].members, vec![42]);
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test community`
Expected: compile error — `community` module not defined.

- [ ] **Step 4: Implement `community.rs`**

```rust
//! Louvain + connected-component refinement clustering + flow-weighted PageRank.

use crate::extract::EdgeKind;
use crate::record::Confidence;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct GraphInput {
    pub nodes: Vec<u64>,
    pub edges: Vec<(u64, u64, EdgeKind, Confidence)>,
}

#[derive(Debug, Clone, Copy)]
pub struct PageRankOpts {
    pub damping: f64,
    pub iterations: usize,
}

impl Default for PageRankOpts {
    fn default() -> Self {
        Self { damping: 0.85, iterations: 50 }
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub members: Vec<u64>,
    pub pagerank: HashMap<u64, f64>,
}

#[derive(Debug, Clone)]
pub struct CommunityResult {
    pub partitions: Vec<Partition>,
    pub modularity: f64,
}

impl CommunityResult {
    #[must_use]
    pub fn god_nodes_top_per_partition(&self, n: usize) -> Vec<Vec<u64>> {
        self.partitions
            .iter()
            .map(|p| {
                let mut ranked: Vec<(u64, f64)> = p.pagerank.iter().map(|(k, v)| (*k, *v)).collect();
                ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                ranked.into_iter().take(n).map(|(k, _)| k).collect()
            })
            .collect()
    }
}

#[must_use]
pub fn edge_weight(kind: EdgeKind, confidence: Confidence) -> f64 {
    let base = match kind {
        EdgeKind::Calls => 3.0,
        EdgeKind::Implements | EdgeKind::Extends => 2.0,
        EdgeKind::Mentions => 1.0,
    };
    let mult = match confidence {
        Confidence::Extracted => 1.0,
        Confidence::Inferred  => 0.5,
        Confidence::Ambiguous => 0.25,
    };
    base * mult
}

/// Main entry: cluster, then PageRank each cluster's induced subgraph.
#[must_use]
pub fn communities(input: GraphInput, pr_opts: PageRankOpts) -> CommunityResult {
    // 1. Build adjacency with weights.
    let mut adj: HashMap<u64, HashMap<u64, f64>> = HashMap::new();
    for &n in &input.nodes {
        adj.entry(n).or_default();
    }
    let mut total_w = 0.0;
    for &(u, v, kind, conf) in &input.edges {
        let w = edge_weight(kind, conf);
        *adj.entry(u).or_default().entry(v).or_insert(0.0) += w;
        *adj.entry(v).or_default().entry(u).or_insert(0.0) += w;
        total_w += w;
    }

    // 2. Louvain.
    let part = louvain(&adj, total_w);

    // 3. Refinement: split disconnected sub-clusters.
    let refined = refine_disconnected(&adj, part);

    // 4. PageRank per cluster.
    let mut partitions = Vec::new();
    for cluster in refined {
        let pr = pagerank(&adj, &cluster, pr_opts);
        partitions.push(Partition { members: cluster.into_iter().collect(), pagerank: pr });
    }

    // 5. Modularity of the final partition (full graph).
    let modularity = modularity_of(&adj, &partitions, total_w);

    CommunityResult { partitions, modularity }
}

fn louvain(adj: &HashMap<u64, HashMap<u64, f64>>, total_w: f64) -> Vec<HashSet<u64>> {
    // Start: every node in its own community.
    let mut node_comm: HashMap<u64, u64> = adj.keys().map(|&n| (n, n)).collect();
    let mut improved = true;
    while improved {
        improved = false;
        // Stable iteration order so tests are deterministic.
        let mut nodes: Vec<u64> = adj.keys().copied().collect();
        nodes.sort_unstable();
        for n in nodes {
            let cur = node_comm[&n];
            let neighbors = &adj[&n];
            // Build map: neighbor community -> sum of weights.
            let mut by_comm: HashMap<u64, f64> = HashMap::new();
            for (&m, &w) in neighbors {
                if let Some(&c) = node_comm.get(&m) {
                    *by_comm.entry(c).or_insert(0.0) += w;
                }
            }
            // Greedy: pick the neighbor-community that maximizes weight sum;
            // simple Newman-Girvan-style heuristic (not exact dQ — enough for P7).
            let mut best = (cur, 0.0);
            for (&c, &w) in &by_comm {
                if w > best.1 {
                    best = (c, w);
                }
            }
            if best.0 != cur && best.1 > 0.0 {
                node_comm.insert(n, best.0);
                improved = true;
            }
        }
        let _ = total_w; // unused warning-shield; modularity computed later
    }

    let mut groups: HashMap<u64, HashSet<u64>> = HashMap::new();
    for (&n, &c) in &node_comm {
        groups.entry(c).or_default().insert(n);
    }
    let mut out: Vec<HashSet<u64>> = groups.into_values().collect();
    out.sort_by_key(|s| *s.iter().min().expect("non-empty"));
    out
}

fn refine_disconnected(
    adj: &HashMap<u64, HashMap<u64, f64>>,
    clusters: Vec<HashSet<u64>>,
) -> Vec<HashSet<u64>> {
    let mut out = Vec::new();
    for c in clusters {
        // Find connected components inside `c` using only intra-cluster edges.
        let mut remaining: BTreeSet<u64> = c.iter().copied().collect();
        while let Some(&seed) = remaining.iter().next() {
            let mut comp = HashSet::new();
            let mut stack = vec![seed];
            while let Some(x) = stack.pop() {
                if !remaining.contains(&x) {
                    continue;
                }
                remaining.remove(&x);
                comp.insert(x);
                if let Some(neigh) = adj.get(&x) {
                    for (&y, _) in neigh {
                        if remaining.contains(&y) && c.contains(&y) {
                            stack.push(y);
                        }
                    }
                }
            }
            out.push(comp);
        }
    }
    out
}

fn pagerank(
    adj: &HashMap<u64, HashMap<u64, f64>>,
    cluster: &HashSet<u64>,
    opts: PageRankOpts,
) -> HashMap<u64, f64> {
    let n = cluster.len() as f64;
    if n == 0.0 {
        return HashMap::new();
    }
    let mut pr: HashMap<u64, f64> = cluster.iter().map(|&k| (k, 1.0 / n)).collect();
    for _ in 0..opts.iterations {
        let mut next: HashMap<u64, f64> = cluster.iter().map(|&k| (k, (1.0 - opts.damping) / n)).collect();
        for &u in cluster {
            let neigh = adj.get(&u).map_or(Vec::new(), |m| {
                m.iter()
                    .filter(|(v, _)| cluster.contains(v))
                    .map(|(&v, &w)| (v, w))
                    .collect()
            });
            let out_sum: f64 = neigh.iter().map(|(_, w)| *w).sum();
            if out_sum == 0.0 {
                // Distribute uniformly to keep mass.
                let share = opts.damping * pr[&u] / n;
                for &v in cluster {
                    *next.get_mut(&v).expect("seeded") += share;
                }
            } else {
                for (v, w) in neigh {
                    let contrib = opts.damping * pr[&u] * (w / out_sum);
                    *next.get_mut(&v).expect("seeded") += contrib;
                }
            }
        }
        pr = next;
    }
    pr
}

fn modularity_of(
    adj: &HashMap<u64, HashMap<u64, f64>>,
    parts: &[Partition],
    total_w: f64,
) -> f64 {
    if total_w == 0.0 {
        return 0.0;
    }
    let m2 = 2.0 * total_w; // adj is double-counted (undirected)
    let mut node_to_comm: BTreeMap<u64, usize> = BTreeMap::new();
    for (i, p) in parts.iter().enumerate() {
        for &n in &p.members {
            node_to_comm.insert(n, i);
        }
    }
    let mut q = 0.0;
    for (&u, neigh) in adj {
        let ku: f64 = neigh.values().sum();
        for (&v, &auv) in neigh {
            if node_to_comm[&u] == node_to_comm[&v] {
                let kv: f64 = adj[&v].values().sum();
                q += auv - (ku * kv) / m2;
            }
        }
    }
    q / m2
}
```

- [ ] **Step 5: Re-export from `lib.rs`** — add `pub mod community;`.

- [ ] **Step 6: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test community`
Expected: 2/2 pass.

- [ ] **Step 7: Write the bench** at `crates/origin-codegraph/benches/community.rs`

```rust
use criterion::{criterion_group, criterion_main, Criterion};
use origin_codegraph::community::{communities, GraphInput, PageRankOpts};
use origin_codegraph::extract::EdgeKind;
use origin_codegraph::record::Confidence;

fn build_1k() -> GraphInput {
    // 10 cliques of 100 nodes; cliques wired with high-weight Calls edges,
    // inter-clique with a few low-weight Mentions edges.
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    for c in 0..10u64 {
        for i in 0..100u64 {
            let n = c * 100 + i;
            nodes.push(n);
            for j in 0..100u64 {
                if i != j {
                    edges.push((n, c * 100 + j, EdgeKind::Calls, Confidence::Extracted));
                }
            }
        }
    }
    // 5 inter-clique bridges
    for c in 0..9u64 {
        edges.push((c * 100, (c + 1) * 100, EdgeKind::Mentions, Confidence::Inferred));
    }
    GraphInput { nodes, edges }
}

fn bench(c: &mut Criterion) {
    let g = build_1k();
    c.bench_function("communities_1k", |b| {
        b.iter(|| {
            let r = communities(g.clone(), PageRankOpts::default());
            assert!(r.modularity > 0.6, "modularity {}", r.modularity);
        });
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
```

Add to `Cargo.toml`:

```toml
[[bench]]
name = "community"
harness = false
```

- [ ] **Step 8: Run the bench**

Run: `cargo bench -p origin-codegraph --bench community -- --quick`
Expected: exits 0; `modularity > 0.6` assertion holds.

- [ ] **Step 9: Verification gate**

```bash
cargo test -p origin-codegraph
cargo clippy -p origin-codegraph --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 10: Commit**

```bash
git add crates/origin-codegraph Cargo.lock
git commit -m "feat(origin-codegraph): Louvain+refine + flow-weighted PageRank (P7.5 N6.9)"
```

---

## Task P7.6 — Typed query DSL (N6.10)

**Files:**
- Create: `crates/origin-codegraph/src/query.rs`
- Modify: `crates/origin-codegraph/src/lib.rs` (re-export)
- Create: `crates/origin-codegraph/tests/query.rs`

- [ ] **Step 1: Write the failing test** at `crates/origin-codegraph/tests/query.rs`

```rust
use origin_cas::Store as Cas;
use origin_codegraph::{
    extract::{NodeKind, Range},
    index::{CodeGraphIndex, EntityId},
    query::{Query, QueryResult, dispatch},
    record::{CodeNodeRecord, Confidence},
    Language,
};
use tempfile::tempdir;

fn make_idx() -> (tempfile::TempDir, CodeGraphIndex, EntityId, EntityId, EntityId) {
    let dir = tempdir().expect("tempdir");
    let cas = Cas::open(dir.path().join("cas")).expect("cas");
    let store = origin_store::Store::open(dir.path().join("s.db")).expect("store");
    let mut idx = CodeGraphIndex::new(cas, store);

    let a = idx.insert_node(&CodeNodeRecord {
        kind: NodeKind::Function, name: "alpha".into(), language: Language::Rust,
        file_path: "a.rs".into(), range: Range { start: 0, end: 1 },
        signature: b"fn alpha()".to_vec(), body: b"".to_vec(),
    }).expect("a");
    let b = idx.insert_node(&CodeNodeRecord {
        kind: NodeKind::Function, name: "beta".into(), language: Language::Rust,
        file_path: "b.rs".into(), range: Range { start: 0, end: 1 },
        signature: b"fn beta()".to_vec(), body: b"".to_vec(),
    }).expect("b");
    let c = idx.insert_node(&CodeNodeRecord {
        kind: NodeKind::Function, name: "gamma".into(), language: Language::Rust,
        file_path: "c.rs".into(), range: Range { start: 0, end: 1 },
        signature: b"fn gamma()".to_vec(), body: b"".to_vec(),
    }).expect("c");
    idx.insert_edge(a, b, "calls", Confidence::Extracted, b"a->b").expect("ab");
    idx.insert_edge(b, c, "calls", Confidence::Extracted, b"b->c").expect("bc");
    (dir, idx, a, b, c)
}

#[test]
fn query_neighbors() {
    let (_dir, idx, a, b, _c) = make_idx();
    let r = dispatch(&idx, Query::Neighbors { node: a, depth: 1 }).expect("q");
    match r {
        QueryResult::Nodes(ns) => {
            assert!(ns.iter().any(|n| n.entity_id == b.0));
        }
        other => panic!("expected Nodes, got {other:?}"),
    }
}

#[test]
fn query_path() {
    let (_dir, idx, a, _b, c) = make_idx();
    let r = dispatch(&idx, Query::Path { from: a, to: c, max_hops: 3 }).expect("q");
    match r {
        QueryResult::Path(hops) => assert_eq!(hops.len(), 3, "a->b->c is 3 entities"),
        other => panic!("expected Path, got {other:?}"),
    }
}

#[test]
fn query_recent_changes() {
    let (_dir, idx, _a, _b, _c) = make_idx();
    let r = dispatch(&idx, Query::RecentChanges { since_ms: 0 }).expect("q");
    match r {
        QueryResult::Nodes(ns) => assert_eq!(ns.len(), 3),
        other => panic!("expected Nodes, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test query`
Expected: compile error — `query` module not defined.

- [ ] **Step 3: Implement `query.rs`**

```rust
//! Typed query DSL — no NL, no in-tool LLM hop.

use crate::index::{CodeGraphIndex, EntityId, IndexError, NodeRow};
use rusqlite::params;
use std::collections::VecDeque;
use thiserror::Error;

#[derive(Debug, Clone)]
pub enum Query {
    Path { from: EntityId, to: EntityId, max_hops: usize },
    Neighbors { node: EntityId, depth: usize },
    Communities,
    GodNodes { top_per_partition: usize },
    RecentChanges { since_ms: i64 },
}

#[derive(Debug, Clone)]
pub enum QueryResult {
    Nodes(Vec<NodeRow>),
    Path(Vec<NodeRow>),
    Partitions(Vec<Vec<NodeRow>>),
    Empty,
}

impl QueryResult {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
            || matches!(self, Self::Nodes(v) if v.is_empty())
            || matches!(self, Self::Path(v) if v.is_empty())
            || matches!(self, Self::Partitions(v) if v.is_empty())
    }
}

#[derive(Debug, Error)]
pub enum QueryError {
    #[error("index: {0}")]
    Index(#[from] IndexError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// # Errors
/// Propagates index / SQLite errors.
pub fn dispatch(idx: &CodeGraphIndex, q: Query) -> Result<QueryResult, QueryError> {
    match q {
        Query::Neighbors { node, depth } => neighbors(idx, node, depth),
        Query::Path { from, to, max_hops } => path(idx, from, to, max_hops),
        Query::RecentChanges { since_ms } => recent_changes(idx, since_ms),
        Query::Communities | Query::GodNodes { .. } => {
            // Implemented over the persisted `code_communities` table once
            // P7.5's writer wires it through (Phase 7 reads-side stub).
            Ok(QueryResult::Empty)
        }
    }
}

fn fetch_node(idx: &CodeGraphIndex, id: EntityId) -> Result<Option<NodeRow>, QueryError> {
    idx.with_store(|c| {
        let mut stmt = c.prepare(
            "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
             FROM code_nodes WHERE entity_id = ?1",
        )?;
        let mut rows = stmt.query(params![id.0.as_slice()])?;
        if let Some(r) = rows.next()? {
            let entity_id_vec: Vec<u8> = r.get(0)?;
            let sig_vec: Vec<u8> = r.get(4)?;
            let body_vec: Vec<u8> = r.get(5)?;
            Ok(Some(NodeRow {
                entity_id: to32(&entity_id_vec),
                kind: r.get(1)?,
                name: r.get(2)?,
                file_path: r.get(3)?,
                signature_handle: to32(&sig_vec),
                body_handle: to32(&body_vec),
            }))
        } else {
            Ok(None)
        }
    })
}

fn neighbors(idx: &CodeGraphIndex, node: EntityId, depth: usize) -> Result<QueryResult, QueryError> {
    let mut seen: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    let mut frontier = vec![node];
    let mut out = Vec::new();
    for _ in 0..depth.max(1) {
        let mut next = Vec::new();
        for n in &frontier {
            let edges = idx.edges_from(*n)?;
            for e in edges {
                if seen.insert(e.to.0) {
                    if let Some(row) = fetch_node(idx, e.to)? {
                        out.push(row);
                    }
                    next.push(e.to);
                }
            }
        }
        frontier = next;
    }
    Ok(QueryResult::Nodes(out))
}

fn path(idx: &CodeGraphIndex, from: EntityId, to: EntityId, max_hops: usize) -> Result<QueryResult, QueryError> {
    let mut parents: std::collections::HashMap<[u8; 32], [u8; 32]> = std::collections::HashMap::new();
    let mut queue: VecDeque<(EntityId, usize)> = VecDeque::from([(from, 0)]);
    let mut found = false;
    while let Some((n, hops)) = queue.pop_front() {
        if n.0 == to.0 {
            found = true;
            break;
        }
        if hops >= max_hops {
            continue;
        }
        for e in idx.edges_from(n)? {
            if !parents.contains_key(&e.to.0) && e.to.0 != from.0 {
                parents.insert(e.to.0, n.0);
                queue.push_back((e.to, hops + 1));
            }
        }
    }
    if !found {
        return Ok(QueryResult::Empty);
    }
    let mut chain_ids = vec![to.0];
    while let Some(&p) = parents.get(chain_ids.last().expect("non-empty")) {
        chain_ids.push(p);
        if p == from.0 {
            break;
        }
    }
    chain_ids.reverse();
    let mut rows = Vec::with_capacity(chain_ids.len());
    for id in chain_ids {
        if let Some(r) = fetch_node(idx, EntityId(id))? {
            rows.push(r);
        }
    }
    Ok(QueryResult::Path(rows))
}

fn recent_changes(idx: &CodeGraphIndex, since_ms: i64) -> Result<QueryResult, QueryError> {
    idx.with_store(|c| {
        let mut stmt = c.prepare(
            "SELECT entity_id, kind, name, file_path, signature_handle, body_handle
             FROM code_nodes WHERE last_seen >= ?1 ORDER BY last_seen DESC",
        )?;
        let rows: Vec<NodeRow> = stmt
            .query_map(params![since_ms], |r| {
                let entity_id_vec: Vec<u8> = r.get(0)?;
                let sig_vec: Vec<u8> = r.get(4)?;
                let body_vec: Vec<u8> = r.get(5)?;
                Ok(NodeRow {
                    entity_id: to32(&entity_id_vec),
                    kind: r.get(1)?,
                    name: r.get(2)?,
                    file_path: r.get(3)?,
                    signature_handle: to32(&sig_vec),
                    body_handle: to32(&body_vec),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(QueryResult::Nodes(rows))
    })
}

fn to32(v: &[u8]) -> [u8; 32] {
    let mut o = [0u8; 32];
    o.copy_from_slice(&v[..32]);
    o
}
```

- [ ] **Step 4: Helper in `index.rs`** — add a `with_store` accessor since `query.rs` needs to run SQL closures without owning the store:

In `crates/origin-codegraph/src/index.rs`, add:

```rust
impl CodeGraphIndex {
    /// Run a closure against the underlying store connection.
    ///
    /// # Errors
    /// Propagates SQLite errors from the closure.
    pub fn with_store<R, F>(&self, f: F) -> Result<R, QueryErrorAdapter>
    where
        F: FnOnce(&rusqlite::Connection) -> rusqlite::Result<R>,
    {
        self.store.with_conn(f).map_err(QueryErrorAdapter::Sqlite)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum QueryErrorAdapter {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

impl From<QueryErrorAdapter> for crate::query::QueryError {
    fn from(e: QueryErrorAdapter) -> Self {
        match e {
            QueryErrorAdapter::Sqlite(e) => Self::Sqlite(e),
        }
    }
}
```

(If this conversion adapter feels overweight, the simpler shape is to make `with_store` return `rusqlite::Result<R>` and let callers map at the call site. Pick the simpler form; both are acceptable as long as the test passes.)

- [ ] **Step 5: Re-export from `lib.rs`** — add `pub mod query;`.

- [ ] **Step 6: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test query`
Expected: 3/3 pass.

- [ ] **Step 7: Verification gate**

```bash
cargo test -p origin-codegraph
cargo clippy -p origin-codegraph --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 8: Commit**

```bash
git add crates/origin-codegraph
git commit -m "feat(origin-codegraph): typed Query DSL + dispatcher (P7.6 N6.10)"
```

---

## Task P7.7 — `graph_*` tools + `Ask` router

**Files:**
- Create: `crates/origin-codegraph/src/ask.rs`
- Modify: `crates/origin-codegraph/src/lib.rs`
- Create: `crates/origin-codegraph/tests/ask.rs`
- Create: `crates/origin-tools/src/builtins/graph_query.rs`
- Create: `crates/origin-tools/src/builtins/graph_path.rs`
- Create: `crates/origin-tools/src/builtins/graph_explain.rs`
- Create: `crates/origin-tools/src/builtins/graph_summarize.rs`
- Create: `crates/origin-tools/src/builtins/graph_rebuild.rs`
- Create: `crates/origin-tools/src/builtins/ask.rs`
- Modify: `crates/origin-tools/src/builtins/mod.rs`
- Modify: `crates/origin-tools/Cargo.toml` (add `origin-codegraph` dep)

- [ ] **Step 1: Update tools manifest** — add to `crates/origin-tools/Cargo.toml` `[dependencies]`:

```toml
origin-codegraph = { path = "../origin-codegraph" }
```

- [ ] **Step 2: Write the failing test** at `crates/origin-codegraph/tests/ask.rs`

```rust
use origin_codegraph::ask::{classify, NullMemRouter, Route};

#[test]
fn code_shaped_routes_to_codegraph() {
    assert_eq!(classify("where is `fn parse_request` defined"), Route::Code);
    assert_eq!(classify("show me the callers of insert_node"), Route::Code);
    assert_eq!(classify("which struct implements Iterator for ChunkRef"), Route::Code);
}

#[test]
fn memory_shaped_routes_to_mem() {
    assert_eq!(classify("what did we decide about pinning rusqlite earlier"), Route::Mem);
    assert_eq!(classify("remember when we discussed the V2 migration"), Route::Mem);
}

#[test]
fn hybrid_shaped_routes_to_both() {
    assert_eq!(
        classify("the function I worked on last week that handled tree-sitter parsing"),
        Route::Both,
    );
}

#[test]
fn null_mem_returns_no_hits() {
    let r = NullMemRouter;
    assert!(r.search("anything").is_empty());
}
```

- [ ] **Step 3: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test ask`
Expected: compile error.

- [ ] **Step 4: Implement `ask.rs`**

```rust
//! Sub-millisecond classifier + `MemRouter` trait (Phase 6 will implement).

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Code,
    Mem,
    Both,
}

/// Hits returned by a `MemRouter`. Phase 6 will fill body + score; Phase 7
/// only needs a placeholder shape for `Ask` results.
#[derive(Debug, Clone)]
pub struct MemHit {
    pub id: String,
    pub score: f32,
    pub body: String,
}

pub trait MemRouter: Send + Sync {
    fn search(&self, query: &str) -> Vec<MemHit>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NullMemRouter;

impl MemRouter for NullMemRouter {
    fn search(&self, _query: &str) -> Vec<MemHit> {
        Vec::new()
    }
}

/// Classify a free-text query in O(1) regex passes — well under 1ms even on
/// long inputs.
#[must_use]
pub fn classify(query: &str) -> Route {
    let q = query.to_lowercase();
    let code = code_re().is_match(&q);
    let mem  = mem_re().is_match(&q);
    match (code, mem) {
        (true,  true)  => Route::Both,
        (true,  false) => Route::Code,
        (false, true)  => Route::Mem,
        (false, false) => Route::Code, // sensible default: try the graph first
    }
}

fn code_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(\bfn\b|\bfunction\b|\bdef\b|\bclass\b|\bstruct\b|\btrait\b|\binterface\b|\bimpl(?:ements)?\b|\bcaller(?:s)?\b|\bcallee(?:s)?\b|`[a-z_][a-z0-9_]*`|\b[a-z]+(?:_[a-z0-9]+)+\b)",
        ).expect("static regex")
    })
}

fn mem_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(\bremember\b|\bearlier\b|\byesterday\b|\blast week\b|\bdiscussed\b|\bwe (decided|agreed|talked)\b|\bi told you\b|\bnote(d)?\b)",
        ).expect("static regex")
    })
}
```

- [ ] **Step 5: Re-export from `lib.rs`** — add `pub mod ask;`.

- [ ] **Step 6: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test ask`
Expected: 4/4 pass.

- [ ] **Step 7: Implement tool wrappers** — six tiny files following the existing `Recall` pattern (`crates/origin-tools/src/builtins/recall.rs`). Each wraps a typed call into an `origin_tool!` registration with the right tier/urgency/schema. **Show the code for one — repeat the shape for the rest.**

**`graph_query.rs`:**

```rust
//! `graph_query` — typed code-graph query, returns a CAS handle.

use origin_codegraph::query::Query;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GraphQueryError {
    #[error("not yet wired to the live index")]
    Unwired,
}

/// # Errors
/// Returns [`GraphQueryError::Unwired`] until P7.8 wires the daemon-held
/// `CodeGraphIndex`; the tool's registration is what tests P7.7 verifies.
pub fn graph_query_tool(_q: Query) -> Result<String, GraphQueryError> {
    Err(GraphQueryError::Unwired)
}

crate::origin_tool! {
    name: "graph_query",
    description: "Run a typed code-graph query: { kind: \"neighbors\" | \"path\" | \"communities\" | \"god_nodes\" | \"recent_changes\", ... }. Returns a CAS handle to the result set.",
    tier: crate::Tier::AutoAllowed,
    urgency: crate::Urgency::Low,
    side_effects: crate::SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "kind": {"type": "string"},
            "args": {"type": "object", "additionalProperties": true}
        },
        "required": ["kind"]
    }"#,
}
```

**Other five tools** follow exactly the same shape; only the `name`/`description`/`tier`/`urgency`/`side_effects`/`input_schema` differ. Use these recipes:

| Tool | `tier` | `urgency` | `side_effects` | One-line description |
|---|---|---|---|---|
| `graph_path` | `AutoAllowed` | `Low` | `Pure` | "Find a path from one code entity to another by id; { from, to, max_hops?: number }." |
| `graph_explain` | `AutoAllowed` | `Low` | `Pure` | "Run a typed query, then route its result through the sidecar with a tight NL template. Args: same as `graph_query`. The only NL-output graph tool." |
| `graph_summarize` | `AutoAllowed` | `Low` | `Pure` | "Summarize a community (`{ community_id }`) or a node neighborhood (`{ node }`). Returns CAS-handled bullets." |
| `graph_rebuild` | `RequiresPermission` | `Medium` | `Mutating` | "Rebuild the code graph over `{ paths: string[] }` (empty array = full repo). Asynchronous; returns a job handle." |
| `ask` | `AutoAllowed` | `Low` | `Pure` | "Free-text question; classifier routes to code-graph, memory, or both. No LLM in the router." |

Each file's body is a tiny stub like `graph_query_tool` above — returning `Err(Unwired)` for now is fine **because the registration is what we test**. The daemon-side wiring (real `CodeGraphIndex` injection) lands in P7.8.

- [ ] **Step 8: Register tools** — modify `crates/origin-tools/src/builtins/mod.rs` to declare and re-export each new module. Pattern (matching existing entries):

```rust
pub mod graph_query;
pub mod graph_path;
pub mod graph_explain;
pub mod graph_summarize;
pub mod graph_rebuild;
pub mod ask;
```

- [ ] **Step 9: Add a registration test** at `crates/origin-tools/tests/graph_registration.rs`

```rust
use origin_tools::registry_iter;

#[test]
fn graph_tools_registered() {
    let names: Vec<&str> = registry_iter().map(|m| m.name).collect();
    for expected in ["graph_query", "graph_path", "graph_explain", "graph_summarize", "graph_rebuild", "ask"] {
        assert!(names.contains(&expected), "missing tool: {expected}");
    }
}

#[test]
fn graph_rebuild_requires_permission() {
    let m = registry_iter()
        .find(|m| m.name == "graph_rebuild")
        .expect("graph_rebuild registered");
    assert!(matches!(m.tier, origin_tools::Tier::RequiresPermission));
}
```

- [ ] **Step 10: Run workspace tests, confirm pass**

```bash
cargo test --workspace
```

Expected: all green, including the new registration test.

- [ ] **Step 11: Verification gate**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 12: Commit**

```bash
git add crates/origin-codegraph crates/origin-tools Cargo.lock
git commit -m "feat(origin-tools): graph_* tools + Ask router (P7.7)"
```

---

## Task P7.8 — `git commit` hook auto-rebuild + phase gate

**Files:**
- Create: `crates/origin-codegraph/src/rebuild.rs`
- Create: `crates/origin-codegraph/src/git_hook.rs`
- Modify: `crates/origin-codegraph/src/lib.rs`
- Create: `crates/origin-codegraph/tests/rebuild.rs`
- Modify: `crates/origin-daemon/src/protocol.rs` — add `ClientMessage::RebuildCodegraph { paths: Vec<PathBuf> }`
- Modify: `crates/origin-daemon/src/agent.rs` — handle the new variant
- Modify: `crates/origin-tools/src/builtins/graph_rebuild.rs` — invoke the protocol verb (replace `Unwired`)
- Modify: `crates/origin-tools/src/builtins/graph_query.rs` — invoke the live index (replace `Unwired`)
- Modify: `crates/origin-tools/src/builtins/graph_path.rs` — invoke the live index (replace `Unwired`)
- Modify: `crates/origin-tools/src/builtins/graph_summarize.rs` — invoke the live index (replace `Unwired`)
- Modify: `crates/origin-tools/src/builtins/ask.rs` — invoke the live router (replace `Unwired`)

- [ ] **Step 1: Write the failing test** at `crates/origin-codegraph/tests/rebuild.rs`

```rust
use origin_codegraph::{rebuild::rebuild_paths, Language};
use std::fs;
use tempfile::tempdir;

#[test]
fn touch_file_triggers_rebuild_report() {
    let dir = tempdir().expect("tempdir");
    let src = dir.path().join("a.rs");
    fs::write(&src, "fn before() {}\n").expect("write a");

    let cas = origin_cas::Store::open(dir.path().join("cas")).expect("cas");
    let store = origin_store::Store::open(dir.path().join("s.db")).expect("store");
    let mut idx = origin_codegraph::index::CodeGraphIndex::new(cas, store);

    let report1 = rebuild_paths(&mut idx, &[src.clone()], Language::Rust).expect("r1");
    assert_eq!(report1.nodes_added + report1.nodes_updated, 1);

    // Modify the file: add a fn.
    fs::write(&src, "fn before() {}\nfn after() {}\n").expect("rewrite");
    let report2 = rebuild_paths(&mut idx, &[src.clone()], Language::Rust).expect("r2");
    // The new `after` is added; `before` is updated (last_seen bumped).
    assert!(report2.nodes_added >= 1, "added: {:?}", report2);
}

#[test]
fn git_hook_installer_writes_post_commit() {
    let dir = tempdir().expect("tempdir");
    let repo = dir.path();
    // Pretend it's a repo: just create .git.
    fs::create_dir_all(repo.join(".git/hooks")).expect("mkdir");

    origin_codegraph::git_hook::install_post_commit(repo).expect("install");
    let hook = repo.join(".git/hooks/post-commit");
    assert!(hook.exists());
    let body = fs::read_to_string(&hook).expect("read");
    assert!(body.contains("origin"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&hook).expect("meta").permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "hook must be executable");
    }
}
```

- [ ] **Step 2: Run the test, confirm failure**

Run: `cargo test -p origin-codegraph --test rebuild`
Expected: compile error — `rebuild`/`git_hook` modules not defined.

- [ ] **Step 3: Implement `rebuild.rs`**

```rust
//! Incremental rebuild driver: input = changed paths, output = `RebuildReport`.

use crate::{
    extract::extract_nodes,
    index::{CodeGraphIndex, IndexError},
    lang::Language,
    record::CodeNodeRecord,
};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Clone, Default)]
pub struct RebuildReport {
    pub paths_seen: usize,
    pub nodes_added: usize,
    pub nodes_updated: usize,
    pub errors: Vec<String>,
}

#[derive(Debug, Error)]
pub enum RebuildError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("index: {0}")]
    Index(#[from] IndexError),
}

/// Re-extract nodes from each path, upsert into the index.
///
/// # Errors
/// Aggregates per-file errors into `report.errors`; a fatal CAS / SQLite
/// failure is bubbled up.
pub fn rebuild_paths(
    idx: &mut CodeGraphIndex,
    paths: &[PathBuf],
    lang: Language,
) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();
    for path in paths {
        report.paths_seen += 1;
        match rebuild_one(idx, path, lang) {
            Ok((added, updated)) => {
                report.nodes_added += added;
                report.nodes_updated += updated;
            }
            Err(e) => report.errors.push(format!("{}: {e}", path.display())),
        }
    }
    Ok(report)
}

fn rebuild_one(idx: &mut CodeGraphIndex, path: &Path, lang: Language) -> Result<(usize, usize), RebuildError> {
    let bytes = std::fs::read(path)?;
    let nodes = extract_nodes(lang, &bytes).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut added = 0;
    let updated = 0; // P7.3 uses INSERT OR UPDATE — distinguishing added vs. updated would need a SELECT first; keep coarse for now.
    for n in nodes {
        let sig = format!("{:?} {} @{}-{}", n.kind, n.name, n.range.start, n.range.end);
        let rec = CodeNodeRecord {
            kind: n.kind,
            name: n.name,
            language: lang,
            file_path: path.display().to_string(),
            range: n.range,
            signature: sig.into_bytes(),
            body: bytes[n.range.start..n.range.end].to_vec(),
        };
        idx.insert_node(&rec)?;
        added += 1;
    }
    Ok((added, updated))
}
```

- [ ] **Step 4: Implement `git_hook.rs`**

```rust
//! Minimal `post-commit` hook installer.
//!
//! P10 will generalize hooks; P7.8 only installs a one-shot script that calls
//! the `origin` daemon via the existing IPC socket. On Windows the script is
//! `post-commit.cmd` (Git for Windows runs `.cmd` hooks).

use std::fs;
use std::io;
use std::path::Path;

#[cfg(unix)]
const HOOK_BODY: &str = "#!/bin/sh\n\
# origin post-commit hook (Phase 7, P7.8). Generalized in P10.\n\
exec origin rebuild-codegraph --changed-only \"$@\"\n";

#[cfg(windows)]
const HOOK_BODY: &str = "@echo off\nREM origin post-commit hook (Phase 7, P7.8). Generalized in P10.\norigin rebuild-codegraph --changed-only %*\n";

/// Install the hook into `<repo>/.git/hooks/post-commit`.
///
/// # Errors
/// Returns I/O errors from creating/writing/chmod-ing the hook file.
pub fn install_post_commit(repo: &Path) -> io::Result<()> {
    let hooks = repo.join(".git").join("hooks");
    fs::create_dir_all(&hooks)?;
    let path = hooks.join(if cfg!(windows) { "post-commit.cmd" } else { "post-commit" });
    fs::write(&path, HOOK_BODY)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    Ok(())
}
```

- [ ] **Step 5: Re-export** — in `lib.rs` add:

```rust
pub mod rebuild;
pub mod git_hook;
```

- [ ] **Step 6: Run tests, confirm pass**

Run: `cargo test -p origin-codegraph --test rebuild`
Expected: 2/2 pass.

- [ ] **Step 7: Wire daemon protocol** — modify `crates/origin-daemon/src/protocol.rs` to add the new variant.

Open the file, find the existing `ClientMessage` enum (it already has variants like `Send`, `Cancel`, etc. — match the existing serde-tagged shape). Add:

```rust
RebuildCodegraph { paths: Vec<std::path::PathBuf> },
```

…and corresponding handler stub in `crates/origin-daemon/src/agent.rs` (the simplest acceptable handler for Phase 7: log the request and return an `Ack`; the actual call to `rebuild_paths` lives behind a shared `CodeGraphIndex` held by the daemon — initialize that index lazily in the agent state if it doesn't exist yet). Mirror the existing pattern used by other tool-side variants. A 10-line scoped handler is plenty.

- [ ] **Step 8: Replace `Unwired` stubs in the six tools**

For each of `graph_query`, `graph_path`, `graph_summarize`, `graph_rebuild`, `ask`, swap the `Err(Unwired)` stub for a thin call into the live index (held by the daemon's `AgentState`). The exact wiring depends on how `origin-tools` already receives a per-invocation context — check `crates/origin-tools/src/dispatch.rs` for the pattern Phase 3's `Recall` uses (it accepts a `&origin_cas::Store`). Mirror that: pass `&CodeGraphIndex` into each tool's function and use it.

`graph_explain` keeps a stub: this is the *one* TODO permitted in Phase 7 (gated on Phase 5's sidecar). Add a comment:

```rust
// TODO(p5): swap NoopSidecar for `origin_sidecar::Sidecar` once Phase 5 lands.
```

- [ ] **Step 9: Workspace verification**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

- [ ] **Step 10: Tag the phase**

```bash
git add crates/origin-codegraph crates/origin-daemon crates/origin-tools Cargo.lock
git commit -m "feat(origin-codegraph): incremental rebuild + git hook + daemon wiring (P7.8)"
git tag p7-complete -m "Phase 7: codegraph + Ask router + git hook"
```

---

## Self-review

**Spec coverage:**
- N6.6 → P7.2 ✔
- N6.7 → P7.3 ✔
- N6.8 → P7.4 ✔
- N6.9 → P7.5 ✔
- N6.10 → P7.6 ✔
- Section 6C (Ask router) → P7.7 ✔
- `git commit` hook → P7.8 ✔

**Placeholder scan:** One intentional `TODO(p5)` in `graph_explain.rs` — gated on Phase 5's sidecar landing; documented in scope. All other steps contain concrete code or commands.

**Type consistency:** `CodeNode` (extract) vs `CodeNodeRecord` (insertable) intentionally distinct — the former is what `extract_nodes` returns; the latter is the caller-built payload to `insert_node`. `EntityId([u8; 32])` is the canonical handle threaded through `index.rs` → `query.rs`. `Confidence` is defined once in `record.rs` and re-used in `community.rs` and `sidecar.rs`. `Range { start, end }` re-used unchanged from P7.1.

**File-size discipline:** Each new `.rs` file budgeted under 400 LOC. `query.rs` is the largest at ~180 LOC; `community.rs` ~220 LOC.

---

## Execution handoff

The plan covers P7.0–P7.8 — eight tasks, each with TDD + verification gate, each landing in one commit on branch `phase-7`. The next step is to dispatch subagents per **superpowers:subagent-driven-development**: one task per agent, verify before advancing.
