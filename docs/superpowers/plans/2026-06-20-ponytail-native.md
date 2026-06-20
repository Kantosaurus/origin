# Ponytail-native Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every origin code-write "go through ponytail" — an always-on lazy-senior-dev ruleset injected into the system prompt plus a deterministic, table-driven dependency gate at tool-dispatch time.

**Architecture:** A new pure library crate `origin-ponytail` holds the modes, the ported ruleset text, the platform-native dependency table, the dependency detectors (manifest + Bash), the pure classifier, config/allowlist, the debt ledger, and the command text. The daemon injects the ruleset into the cached system prompt and runs the classifier in the pre-dispatch filter region (reusing the existing interactive prompter; falling open when non-interactive). The CLI adds the `/ponytail` toggle, `--ponytail` flag, statusline badge, and the five `/ponytail-*` commands — all mirroring the existing `/effort` / `/plan` plumbing.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json`/`toml`, origin's existing IPC (`PromptRequest`/`LoopOptions`), `ToolError` (`origin-tools`), the interactive prompter (`origin-daemon/src/ipc_prompter.rs`).

## Global Constraints

- Rust **edition 2021** for all origin crates (`origin-ponytail` included).
- Build/test via **git-bash, not PowerShell**. Do **not** run `cargo build --workspace` (LNK1140 metadata corruption on this machine); test per-crate: `cargo test -p <crate>`.
- **`cargo clippy -p <crate> -- -D warnings`** must pass for every crate touched.
- **Default mode is `full`.** `resolve` precedence: per-request token → `~/.origin/ponytail.toml` `defaultMode` → `PONYTAIL_DEFAULT_MODE` / `ORIGIN_PONYTAIL` env → `Full`.
- **Gate fails open:** any parse miss or error in detection MUST result in *no flag* (never a broken or blocked write).
- **Wire stays byte-identical when unset:** new `PromptRequest` field uses `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- **Never block non-interactively:** when `interactive == false` (subagent/swarm/headless/scheduled), flagged deps are allowed and logged, never prompted or denied.
- **Omit-list (never gated):** `ms`, `requests`, `click`, `httparty` and the other "genuinely earns its place" packages are absent from the table on purpose — do not add them.
- Every commit message ends with: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`
- Work on a feature branch off `dev`: `git switch -c feat/ponytail-native` before Task 1.

---

## Phase A — `origin-ponytail` library crate (pure, isolated, fully unit-tested)

### Task 1: Crate scaffold + `mode.rs`

**Files:**
- Create: `crates/origin-ponytail/Cargo.toml`
- Create: `crates/origin-ponytail/src/lib.rs`
- Create: `crates/origin-ponytail/src/mode.rs`

**Interfaces:**
- Produces: `PonytailMode { Off, Lite, Full, Ultra }` (`Default = Full`); `PonytailMode::as_str(self)->&'static str`; `PonytailMode::parse_level(&str)->Option<Self>`; `enum PonytailCmd { Report, Set(PonytailMode), Usage }`; `parse_ponytail_command(line:&str)->Option<PonytailCmd>`.

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "origin-ponytail"
version = "0.0.0"
edition = "2021"
license = "Apache-2.0"

[dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
toml = { workspace = true }

[dev-dependencies]
```

> If `toml`/`serde`/`serde_json` are not in `[workspace.dependencies]`, use the same version strings the sibling crates use (check `crates/origin-conseca/Cargo.toml`). The crate is picked up automatically by the root `members = ["crates/*", ...]` glob.

- [ ] **Step 2: Write `src/lib.rs` with the module declarations**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Native ponytail: the lazy-senior-dev ruleset + a deterministic dependency gate.
pub mod mode;

pub use mode::{parse_ponytail_command, PonytailCmd, PonytailMode};
```

- [ ] **Step 3: Write the failing test in `src/mode.rs`**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Ponytail intensity modes and the `/ponytail` command grammar.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PonytailMode {
    Off,
    Lite,
    #[default]
    Full,
    Ultra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PonytailCmd {
    /// `/ponytail` with no argument — report the current mode.
    Report,
    /// `/ponytail <level>` with a valid level.
    Set(PonytailMode),
    /// `/ponytail <garbage>` — surface a usage error.
    Usage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_round_trip() {
        for m in [PonytailMode::Off, PonytailMode::Lite, PonytailMode::Full, PonytailMode::Ultra] {
            assert_eq!(PonytailMode::parse_level(m.as_str()), Some(m));
        }
    }

    #[test]
    fn command_grammar() {
        assert_eq!(parse_ponytail_command("/ponytail"), Some(PonytailCmd::Report));
        assert_eq!(parse_ponytail_command("/ponytail ultra"), Some(PonytailCmd::Set(PonytailMode::Ultra)));
        assert_eq!(parse_ponytail_command("/ponytail OFF"), Some(PonytailCmd::Set(PonytailMode::Off)));
        assert_eq!(parse_ponytail_command("/ponytail bogus"), Some(PonytailCmd::Usage));
        assert_eq!(parse_ponytail_command("/ponytailfoo"), None);
        assert_eq!(parse_ponytail_command("hello"), None);
    }

    #[test]
    fn default_is_full() {
        assert_eq!(PonytailMode::default(), PonytailMode::Full);
    }
}
```

- [ ] **Step 4: Run the test, verify it fails**

Run: `cargo test -p origin-ponytail mode`
Expected: FAIL — `as_str`, `parse_level`, `parse_ponytail_command` not defined.

- [ ] **Step 5: Implement (add above the `#[cfg(test)]` block in `src/mode.rs`)**

```rust
impl PonytailMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Lite => "lite",
            Self::Full => "full",
            Self::Ultra => "ultra",
        }
    }

    #[must_use]
    pub fn parse_level(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            "ultra" => Some(Self::Ultra),
            _ => None,
        }
    }
}

/// Parse a `/ponytail [level]` line. `None` ⇒ not a ponytail command (fall through).
#[must_use]
pub fn parse_ponytail_command(line: &str) -> Option<PonytailCmd> {
    let rest = line.trim().strip_prefix("/ponytail")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None; // `/ponytailfoo`
    }
    let arg = rest.trim();
    if arg.is_empty() {
        return Some(PonytailCmd::Report);
    }
    Some(PonytailMode::parse_level(arg).map_or(PonytailCmd::Usage, PonytailCmd::Set))
}
```

- [ ] **Step 6: Run tests + clippy**

Run: `cargo test -p origin-ponytail mode && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS, no clippy warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): scaffold origin-ponytail crate with modes + command grammar

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: `native_table.rs` — ported platform-native dependency knowledge

**Files:**
- Create: `crates/origin-ponytail/src/native_table.rs`
- Modify: `crates/origin-ponytail/src/lib.rs` (add `pub mod native_table;` + re-exports)

**Interfaces:**
- Produces: `enum Ecosystem { Npm, PyPI, Cargo, Go, RubyGems }`; `struct NativeReplacement { rung: u8, native: &'static str, note: &'static str }`; `lookup(eco: Ecosystem, pkg: &str) -> Option<&'static NativeReplacement>`.

- [ ] **Step 1: Write the failing test (bottom of `src/native_table.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_replacements_hit() {
        assert_eq!(lookup(Ecosystem::Npm, "lodash.groupby").unwrap().native, "Object.groupBy(arr, fn)");
        assert_eq!(lookup(Ecosystem::PyPI, "pytz").unwrap().native, "zoneinfo.ZoneInfo");
        assert_eq!(lookup(Ecosystem::Cargo, "lazy_static").unwrap().rung, 2);
        // hyphen/underscore normalization for cargo
        assert!(lookup(Ecosystem::Cargo, "lazy-static").is_some());
        assert_eq!(lookup(Ecosystem::Go, "github.com/pkg/errors").unwrap().native, "errors + fmt.Errorf(\"%w\")");
    }

    #[test]
    fn omitted_packages_miss() {
        // Genuinely-useful packages are deliberately absent (never gated).
        assert!(lookup(Ecosystem::Npm, "ms").is_none());
        assert!(lookup(Ecosystem::PyPI, "requests").is_none());
        assert!(lookup(Ecosystem::PyPI, "click").is_none());
        assert!(lookup(Ecosystem::RubyGems, "httparty").is_none());
        assert!(lookup(Ecosystem::Npm, "react").is_none());
    }

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup(Ecosystem::Npm, "Lodash.GroupBy").is_some());
    }
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p origin-ponytail native_table`
Expected: FAIL — types/`lookup` not defined.

- [ ] **Step 3: Implement (top of `src/native_table.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Ported platform-native dependency table (from ponytail's platform-native.md).
//! Only package→native rows are included; only deps with a genuine stdlib/native
//! replacement appear. Packages that earn their place (`ms`, `requests`, `click`,
//! `httparty`, `rand`, `itertools`, `react`, …) are deliberately omitted.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    Npm,
    PyPI,
    Cargo,
    Go,
    RubyGems,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeReplacement {
    /// 2 = standard library, 3 = native platform feature.
    pub rung: u8,
    pub native: &'static str,
    pub note: &'static str,
}

const fn r(rung: u8, native: &'static str, note: &'static str) -> NativeReplacement {
    NativeReplacement { rung, native, note }
}

const NPM: &[(&str, NativeReplacement)] = &[
    ("query-string", r(3, "new URLSearchParams(location.search)", "0 deps")),
    ("qs", r(3, "new URLSearchParams(...)", "0 deps")),
    ("lodash.clonedeep", r(3, "structuredClone(obj)", "native")),
    ("lodash.groupby", r(3, "Object.groupBy(arr, fn)", "native")),
    ("lodash", r(3, "native Array/Object methods (groupBy, structuredClone, …)", "drop the umbrella dep")),
    ("moment", r(3, "Intl.DateTimeFormat / Temporal", "native i18n dates")),
    ("date-fns", r(3, "Intl.DateTimeFormat / Intl.RelativeTimeFormat", "native")),
    ("numeral", r(3, "new Intl.NumberFormat(...)", "native")),
    ("accounting", r(3, "new Intl.NumberFormat(..., {style:'currency'})", "native")),
    ("clipboard.js", r(3, "navigator.clipboard.writeText(text)", "native")),
    ("uuid", r(3, "crypto.randomUUID()", "native, v4")),
    ("uuid-validate", r(3, "/^[0-9a-f]{8}-...$/i.test(id)", "1-line regex")),
    ("left-pad", r(2, "String.prototype.padStart(n, '0')", "stdlib")),
    ("is-online", r(3, "navigator.onLine + online/offline events", "native")),
    ("mkdirp", r(2, "fs.mkdirSync(path, { recursive: true })", "stdlib")),
    ("make-dir", r(2, "fs.mkdirSync(path, { recursive: true })", "stdlib")),
    ("rimraf", r(2, "fs.rmSync(path, { recursive: true, force: true })", "stdlib")),
    ("slash", r(2, "path.posix / path.normalize()", "stdlib")),
    ("is-stream", r(2, "val instanceof stream.Readable", "stdlib")),
    ("object-assign", r(2, "Object.assign() / spread", "stdlib")),
    ("array-uniq", r(2, "[...new Set(arr)]", "stdlib")),
    ("array-flatten", r(2, "arr.flat(Infinity)", "stdlib")),
    ("flat", r(2, "arr.flat(depth)", "stdlib")),
    ("path-exists", r(2, "fs.existsSync(path)", "stdlib")),
    ("load-json-file", r(2, "JSON.parse(fs.readFileSync(path, 'utf8'))", "stdlib")),
    ("write-json-file", r(2, "fs.writeFileSync(path, JSON.stringify(obj, null, 2))", "stdlib")),
    ("pkg-dir", r(2, "path.resolve(__dirname, '..')", "stdlib")),
];

const PYPI: &[(&str, NativeReplacement)] = &[
    ("python-dateutil", r(2, "datetime.fromisoformat()", "stdlib 3.7+")),
    ("pytz", r(2, "zoneinfo.ZoneInfo", "stdlib 3.9+")),
    ("attrs", r(2, "@dataclass", "stdlib")),
    ("six", r(2, "(drop it — Python 2 is gone)", "stdlib")),
    ("pathlib2", r(2, "pathlib.Path", "stdlib 3.4+")),
    ("enum34", r(2, "enum.Enum", "stdlib 3.4+")),
    ("typing_extensions", r(2, "builtin generics + from __future__ import annotations", "stdlib")),
    ("simplejson", r(2, "json", "stdlib")),
    ("mergedeep", r(2, "dict | other_dict", "stdlib 3.9+")),
    ("more-itertools", r(2, "itertools (chain, islice, groupby, product)", "stdlib")),
    ("toolz", r(2, "functools (lru_cache, partial, reduce)", "stdlib")),
    ("tabulate", r(2, "pprint.pprint()", "stdlib, debug only")),
];

const CARGO: &[(&str, NativeReplacement)] = &[
    ("lazy_static", r(2, "std::sync::LazyLock", "stdlib 1.80")),
    ("once_cell", r(2, "std::sync::OnceLock / LazyLock", "stdlib")),
    ("num_cpus", r(2, "std::thread::available_parallelism()", "stdlib 1.59")),
    ("maplit", r(2, "HashMap::from([...]) / BTreeMap::from", "stdlib 1.56")),
    ("failure", r(2, "std::error::Error (+ thiserror/anyhow)", "stdlib")),
    ("error-chain", r(2, "std::error::Error", "stdlib")),
];

const GO: &[(&str, NativeReplacement)] = &[
    ("github.com/pkg/errors", r(2, "errors + fmt.Errorf(\"%w\")", "stdlib 1.13")),
    ("github.com/sirupsen/logrus", r(2, "log/slog", "stdlib 1.21")),
    ("github.com/gorilla/mux", r(3, "net/http.ServeMux (method+wildcard)", "stdlib 1.22")),
    ("golang.org/x/exp/slices", r(2, "slices", "stdlib 1.21")),
    ("golang.org/x/exp/maps", r(2, "maps", "stdlib 1.21")),
];

const RUBYGEMS: &[(&str, NativeReplacement)] = &[
    ("rest-client", r(2, "net/http", "stdlib; gem unmaintained")),
    ("awesome_print", r(2, "pp", "stdlib, debug")),
];

#[must_use]
pub fn lookup(eco: Ecosystem, pkg: &str) -> Option<&'static NativeReplacement> {
    let mut name = pkg.trim().to_ascii_lowercase();
    if eco == Ecosystem::Cargo {
        name = name.replace('-', "_"); // crates.io treats - and _ interchangeably
    }
    let table = match eco {
        Ecosystem::Npm => NPM,
        Ecosystem::PyPI => PYPI,
        Ecosystem::Cargo => CARGO,
        Ecosystem::Go => GO,
        Ecosystem::RubyGems => RUBYGEMS,
    };
    table.iter().find(|(k, _)| *k == name).map(|(_, v)| v)
}
```

- [ ] **Step 4: Add to `lib.rs`**

```rust
pub mod native_table;
pub use native_table::{lookup, Ecosystem, NativeReplacement};
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p origin-ponytail native_table && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): port platform-native dependency table

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: `detect.rs` — dependency detectors (Bash + manifest)

**Files:**
- Create: `crates/origin-ponytail/src/detect.rs`
- Modify: `crates/origin-ponytail/src/lib.rs`

**Interfaces:**
- Consumes: `Ecosystem` (Task 2).
- Produces: `struct Dep { eco: Ecosystem, name: String }`; `bash_installs(cmd: &str) -> Vec<Dep>`; `manifest_deps_added(file: &str, before: Option<&str>, after: &str) -> Vec<Dep>`; `manifest_deps_in_added_lines(file: &str, added: &str) -> Vec<Dep>`.

- [ ] **Step 1: Write the failing tests (bottom of `src/detect.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[Dep]) -> Vec<&str> { v.iter().map(|d| d.name.as_str()).collect() }

    #[test]
    fn bash_install_named_packages() {
        assert_eq!(names(&bash_installs("npm install lodash")), ["lodash"]);
        assert_eq!(names(&bash_installs("yarn add lodash react")), ["lodash", "react"]);
        assert_eq!(names(&bash_installs("pnpm add -D typescript")), ["typescript"]);
        assert_eq!(names(&bash_installs("cargo add serde@1")), ["serde"]);
        assert_eq!(names(&bash_installs("pip install requests==2.31")), ["requests"]);
        assert_eq!(names(&bash_installs("go get github.com/pkg/errors")), ["github.com/pkg/errors"]);
        assert_eq!(names(&bash_installs("gem install rest-client")), ["rest-client"]);
        assert_eq!(names(&bash_installs("npm i @scope/pkg")), ["@scope/pkg"]);
    }

    #[test]
    fn bash_install_without_named_packages_is_empty() {
        // Installing existing manifest deps is NOT adding a dep.
        assert!(bash_installs("npm install").is_empty());
        assert!(bash_installs("yarn").is_empty());
        assert!(bash_installs("pnpm i").is_empty());
        assert!(bash_installs("pip install -r requirements.txt").is_empty());
        assert!(bash_installs("cargo build").is_empty());
        assert!(bash_installs("go build ./...").is_empty());
        assert!(bash_installs("go mod download").is_empty());
        assert!(bash_installs("echo npm install lodash").is_empty()); // not an install verb at head
    }

    #[test]
    fn manifest_added_diff() {
        let before = r#"{"dependencies":{"react":"18"}}"#;
        let after = r#"{"dependencies":{"react":"18","lodash":"4"}}"#;
        assert_eq!(names(&manifest_deps_added("package.json", Some(before), after)), ["lodash"]);
    }

    #[test]
    fn manifest_new_file_all_added() {
        let after = "[dependencies]\nlazy_static = \"1\"\n";
        assert_eq!(names(&manifest_deps_added("Cargo.toml", None, after)), ["lazy_static"]);
    }

    #[test]
    fn manifest_requirements_and_gemfile_lines() {
        assert_eq!(names(&manifest_deps_in_added_lines("requirements.txt", "pytz==2024.1\n# comment\n")), ["pytz"]);
        assert_eq!(names(&manifest_deps_in_added_lines("Gemfile", "gem \"rest-client\", \"~> 2.0\"")), ["rest-client"]);
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p origin-ponytail detect`
Expected: FAIL — symbols undefined.

- [ ] **Step 3: Implement (top of `src/detect.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Deterministic dependency-addition detection. Fails open: any parse error
//! yields no deps (never a false block).

use std::collections::BTreeSet;

use crate::native_table::Ecosystem;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dep {
    pub eco: Ecosystem,
    pub name: String,
}

fn dep(eco: Ecosystem, name: impl Into<String>) -> Dep {
    Dep { eco, name: name.into() }
}

/// Strip a version/spec suffix and surrounding quotes from a token.
fn bare_name(tok: &str) -> String {
    let t = tok.trim().trim_matches('"').trim_matches('\'');
    // npm `pkg@1.2` (but keep scoped `@scope/pkg`), python `pkg==1`, `pkg>=1`.
    let cut = if let Some(stripped) = t.strip_prefix('@') {
        // scoped: only split a SECOND '@'
        stripped.find('@').map(|i| i + 1)
    } else {
        t.find(['@', '=', '>', '<', '~', '!', '[', ';', ',', ' '])
    };
    match cut {
        Some(i) => t[..i].to_string(),
        None => t.to_string(),
    }
}

/// Parse package-manager install commands. Only the FIRST simple command is
/// inspected (head token must be the package manager). Returns deps only when
/// explicit package names follow the install verb.
#[must_use]
pub fn bash_installs(cmd: &str) -> Vec<Dep> {
    let cmd = cmd.trim();
    let toks: Vec<&str> = cmd.split_whitespace().collect();
    if toks.len() < 3 {
        return Vec::new();
    }
    let (eco, verb_ok, start) = match toks[0] {
        "npm" | "yarn" | "pnpm" | "bun" => {
            (Ecosystem::Npm, matches!(toks[1], "install" | "add" | "i"), 2)
        }
        "cargo" => (Ecosystem::Cargo, toks[1] == "add", 2),
        "go" => (Ecosystem::Go, toks[1] == "get", 2),
        "gem" => (Ecosystem::RubyGems, toks[1] == "install", 2),
        "pip" | "pip3" => (Ecosystem::PyPI, toks[1] == "install", 2),
        "uv" => (Ecosystem::PyPI, toks.get(1) == Some(&"pip") && toks.get(2) == Some(&"install"), 3),
        "poetry" => (Ecosystem::PyPI, toks[1] == "add", 2),
        _ => return Vec::new(),
    };
    if !verb_ok {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for tok in &toks[start..] {
        if tok.starts_with('-') {
            continue; // flag
        }
        if *tok == "-r" {
            continue;
        }
        let name = bare_name(tok);
        if name.is_empty() || name == "requirements.txt" {
            continue;
        }
        if seen.insert(name.clone()) {
            out.push(dep(eco, name));
        }
    }
    out
}

fn ecosystem_for(file: &str) -> Option<Ecosystem> {
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match base {
        "package.json" => Some(Ecosystem::Npm),
        "Cargo.toml" => Some(Ecosystem::Cargo),
        "requirements.txt" | "pyproject.toml" => Some(Ecosystem::PyPI),
        "go.mod" => Some(Ecosystem::Go),
        "Gemfile" => Some(Ecosystem::RubyGems),
        _ => None,
    }
}

fn deps_in_manifest(file: &str, content: &str) -> BTreeSet<String> {
    let Some(eco) = ecosystem_for(file) else { return BTreeSet::new() };
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    let mut set = BTreeSet::new();
    match base {
        "package.json" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
                for key in ["dependencies", "devDependencies", "optionalDependencies", "peerDependencies"] {
                    if let Some(obj) = v.get(key).and_then(|x| x.as_object()) {
                        set.extend(obj.keys().cloned());
                    }
                }
            }
        }
        "Cargo.toml" => {
            if let Ok(v) = content.parse::<toml::Value>() {
                for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(t) = v.get(key).and_then(|x| x.as_table()) {
                        set.extend(t.keys().cloned());
                    }
                }
            }
        }
        "pyproject.toml" => {
            if let Ok(v) = content.parse::<toml::Value>() {
                if let Some(arr) = v.get("project").and_then(|p| p.get("dependencies")).and_then(|d| d.as_array()) {
                    for item in arr {
                        if let Some(s) = item.as_str() { set.insert(bare_name(s)); }
                    }
                }
                if let Some(t) = v.get("tool").and_then(|t| t.get("poetry")).and_then(|p| p.get("dependencies")).and_then(|d| d.as_table()) {
                    set.extend(t.keys().filter(|k| *k != "python").cloned());
                }
            }
        }
        "requirements.txt" | "Gemfile" | "go.mod" => {
            for d in deps_in_lines(eco, content) {
                set.insert(d.name);
            }
        }
        _ => {}
    }
    set
}

fn deps_in_lines(eco: Ecosystem, text: &str) -> Vec<Dep> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        match eco {
            Ecosystem::PyPI => {
                if line.starts_with('-') { continue; }
                let name = bare_name(line);
                if !name.is_empty() { out.push(dep(eco, name)); }
            }
            Ecosystem::RubyGems => {
                if let Some(rest) = line.strip_prefix("gem ") {
                    let name = bare_name(rest.trim().trim_start_matches([' ', '"', '\'']));
                    if !name.is_empty() { out.push(dep(eco, name)); }
                }
            }
            Ecosystem::Go => {
                // `require x v1` or a line inside a require( ) block: `x v1`.
                let l = line.strip_prefix("require ").unwrap_or(line);
                if let Some(path) = l.split_whitespace().next() {
                    if path.contains('.') && path.contains('/') {
                        out.push(dep(eco, path.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Diff a manifest's dep set (full-content). `before == None` ⇒ all deps are added.
#[must_use]
pub fn manifest_deps_added(file: &str, before: Option<&str>, after: &str) -> Vec<Dep> {
    let Some(eco) = ecosystem_for(file) else { return Vec::new() };
    let after_set = deps_in_manifest(file, after);
    let before_set = before.map(|b| deps_in_manifest(file, b)).unwrap_or_default();
    after_set.difference(&before_set).map(|n| dep(eco, n.clone())).collect()
}

/// Scan only newly-inserted text (an Edit's new_string, or `+` patch lines) for
/// dependency declarations. Conservative line/JSON-entry patterns; fails open.
#[must_use]
pub fn manifest_deps_in_added_lines(file: &str, added: &str) -> Vec<Dep> {
    let Some(eco) = ecosystem_for(file) else { return Vec::new() };
    let base = file.rsplit(['/', '\\']).next().unwrap_or(file);
    match base {
        "requirements.txt" | "Gemfile" | "go.mod" => deps_in_lines(eco, added),
        "package.json" => {
            // Match `"name": "version"` entries in the inserted fragment.
            let mut out = Vec::new();
            for line in added.lines() {
                let l = line.trim().trim_end_matches(',');
                if let Some((k, v)) = l.split_once(':') {
                    let key = k.trim().trim_matches('"');
                    let val = v.trim();
                    if val.starts_with('"') && !key.is_empty() && !key.contains(' ') {
                        out.push(dep(eco, key.to_string()));
                    }
                }
            }
            out
        }
        "Cargo.toml" => {
            let mut out = Vec::new();
            for line in added.lines() {
                let l = line.trim();
                if let Some((k, v)) = l.split_once('=') {
                    let key = k.trim();
                    if !key.is_empty() && !key.starts_with('[') && (v.contains('"') || v.contains('{')) {
                        out.push(dep(eco, key.to_string()));
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}
```

- [ ] **Step 4: Add to `lib.rs`**

```rust
pub mod detect;
pub use detect::{bash_installs, manifest_deps_added, manifest_deps_in_added_lines, Dep};
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p origin-ponytail detect && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): dependency detectors for Bash installs + manifests

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: `gate.rs` — the pure classifier

**Files:**
- Create: `crates/origin-ponytail/src/gate.rs`
- Modify: `crates/origin-ponytail/src/lib.rs`

**Interfaces:**
- Consumes: `Dep` (Task 3), `PonytailMode` (Task 1), `lookup`/`NativeReplacement` (Task 2).
- Produces: `enum FlagKind { Replaceable(&'static NativeReplacement), Unjustified }`; `struct Flagged { dep: Dep, kind: FlagKind }`; `Flagged::message(&self)->String`; `classify(deps: &[Dep], mode: PonytailMode, allow: &BTreeSet<String>) -> Vec<Flagged>`.

- [ ] **Step 1: Write the failing tests (bottom of `src/gate.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_table::Ecosystem;
    use std::collections::BTreeSet;

    fn deps() -> Vec<Dep> {
        vec![
            Dep { eco: Ecosystem::Npm, name: "lodash".into() }, // replaceable
            Dep { eco: Ecosystem::Npm, name: "react".into() },  // no replacement
        ]
    }

    #[test]
    fn off_flags_nothing() {
        assert!(classify(&deps(), PonytailMode::Off, &BTreeSet::new()).is_empty());
    }

    #[test]
    fn full_flags_only_replaceable() {
        let f = classify(&deps(), PonytailMode::Full, &BTreeSet::new());
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].dep.name, "lodash");
        assert!(matches!(f[0].kind, FlagKind::Replaceable(_)));
    }

    #[test]
    fn lite_flags_same_as_full() {
        assert_eq!(classify(&deps(), PonytailMode::Lite, &BTreeSet::new()).len(), 1);
    }

    #[test]
    fn ultra_flags_every_new_dep() {
        let f = classify(&deps(), PonytailMode::Ultra, &BTreeSet::new());
        assert_eq!(f.len(), 2);
        assert!(f.iter().any(|x| x.dep.name == "react" && matches!(x.kind, FlagKind::Unjustified)));
    }

    #[test]
    fn allowlist_short_circuits() {
        let allow: BTreeSet<String> = ["lodash".into()].into_iter().collect();
        assert!(classify(&deps(), PonytailMode::Full, &allow).is_empty());
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p origin-ponytail gate`
Expected: FAIL.

- [ ] **Step 3: Implement (top of `src/gate.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! The pure ponytail dependency classifier. No I/O, no prompting — it only
//! decides which added deps are flagged. The daemon turns flags into action.

use std::collections::BTreeSet;

use crate::detect::Dep;
use crate::mode::PonytailMode;
use crate::native_table::{lookup, NativeReplacement};

#[derive(Debug, Clone, Copy)]
pub enum FlagKind {
    /// Has a native/stdlib replacement (flagged in lite/full/ultra).
    Replaceable(&'static NativeReplacement),
    /// No replacement, but ultra challenges every new dependency (rung 4).
    Unjustified,
}

#[derive(Debug, Clone)]
pub struct Flagged {
    pub dep: Dep,
    pub kind: FlagKind,
}

impl Flagged {
    #[must_use]
    pub fn message(&self) -> String {
        match self.kind {
            FlagKind::Replaceable(r) => format!(
                "ponytail rung {}: `{}` — use {} ({}). Drop the dependency.",
                r.rung, self.dep.name, r.native, r.note
            ),
            FlagKind::Unjustified => format!(
                "ponytail rung 4: new dependency `{}` — does the task need it at all? \
                 Justify it or use what's already here.",
                self.dep.name
            ),
        }
    }
}

/// Classify added deps for a mode. `Off` ⇒ none. `Lite`/`Full` ⇒ replaceable
/// only. `Ultra` ⇒ every non-allowlisted new dep.
#[must_use]
pub fn classify(deps: &[Dep], mode: PonytailMode, allow: &BTreeSet<String>) -> Vec<Flagged> {
    if mode == PonytailMode::Off {
        return Vec::new();
    }
    deps.iter()
        .filter(|d| !allow.contains(&d.name.to_ascii_lowercase()))
        .filter_map(|d| match lookup(d.eco, &d.name) {
            Some(repl) => Some(Flagged { dep: d.clone(), kind: FlagKind::Replaceable(repl) }),
            None if mode == PonytailMode::Ultra => {
                Some(Flagged { dep: d.clone(), kind: FlagKind::Unjustified })
            }
            None => None,
        })
        .collect()
}
```

- [ ] **Step 4: Add to `lib.rs`**

```rust
pub mod gate;
pub use gate::{classify, FlagKind, Flagged};
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p origin-ponytail gate && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): pure dependency classifier (mode-aware)

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: `config.rs` — mode resolution + allowlist

**Files:**
- Create: `crates/origin-ponytail/src/config.rs`
- Modify: `crates/origin-ponytail/src/lib.rs`

**Interfaces:**
- Consumes: `PonytailMode` (Task 1), `Dep` (Task 3).
- Produces: `origin_home() -> PathBuf`; `config_path() -> PathBuf`; `resolve_mode(requested: Option<PonytailMode>) -> PonytailMode`; `allowlist() -> BTreeSet<String>`; `remember(name: &str)`.

- [ ] **Step 1: Write the failing tests (bottom of `src/config.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_wins() {
        assert_eq!(resolve_mode_with(Some(PonytailMode::Ultra), None, None), PonytailMode::Ultra);
    }

    #[test]
    fn env_over_default() {
        assert_eq!(resolve_mode_with(None, None, Some("lite")), PonytailMode::Lite);
    }

    #[test]
    fn config_over_default_but_under_env() {
        assert_eq!(resolve_mode_with(None, Some("off"), None), PonytailMode::Off);
        assert_eq!(resolve_mode_with(None, Some("off"), Some("ultra")), PonytailMode::Ultra);
    }

    #[test]
    fn falls_back_to_full() {
        assert_eq!(resolve_mode_with(None, None, None), PonytailMode::Full);
        assert_eq!(resolve_mode_with(None, Some("garbage"), Some("garbage")), PonytailMode::Full);
    }

    #[test]
    fn allowlist_parses_toml() {
        let toml = "defaultMode = \"full\"\nallow = [\"axios\", \"React\"]\n";
        let set = parse_allowlist(toml);
        assert!(set.contains("axios"));
        assert!(set.contains("react")); // lowercased
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p origin-ponytail config`
Expected: FAIL.

- [ ] **Step 3: Implement (top of `src/config.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Mode resolution + the dependency allowlist (`~/.origin/ponytail.toml`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::mode::PonytailMode;

#[must_use]
pub fn origin_home() -> PathBuf {
    if let Some(h) = std::env::var_os("ORIGIN_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".origin")
}

#[must_use]
pub fn config_path() -> PathBuf {
    origin_home().join("ponytail.toml")
}

fn default_mode_from_toml(content: &str) -> Option<PonytailMode> {
    content
        .parse::<toml::Value>()
        .ok()?
        .get("defaultMode")?
        .as_str()
        .and_then(PonytailMode::parse_level)
}

/// Parse the `allow = [...]` array, lowercased. Pure; for tests + `allowlist()`.
#[must_use]
pub fn parse_allowlist(content: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(v) = content.parse::<toml::Value>() {
        if let Some(arr) = v.get("allow").and_then(|a| a.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    set.insert(s.trim().to_ascii_lowercase());
                }
            }
        }
    }
    set
}

/// Testable core of `resolve_mode`: explicit request > config token > env token > Full.
#[must_use]
pub fn resolve_mode_with(
    requested: Option<PonytailMode>,
    config_token: Option<&str>,
    env_token: Option<&str>,
) -> PonytailMode {
    if let Some(m) = requested {
        return m;
    }
    if let Some(m) = env_token.and_then(PonytailMode::parse_level) {
        return m;
    }
    if let Some(m) = config_token.and_then(PonytailMode::parse_level) {
        return m;
    }
    PonytailMode::Full
}

/// Resolve the effective mode from the live environment + config file.
#[must_use]
pub fn resolve_mode(requested: Option<PonytailMode>) -> PonytailMode {
    if let Some(m) = requested {
        return m;
    }
    let env = std::env::var("PONYTAIL_DEFAULT_MODE")
        .or_else(|_| std::env::var("ORIGIN_PONYTAIL"))
        .ok();
    if let Some(m) = env.as_deref().and_then(PonytailMode::parse_level) {
        return m;
    }
    let cfg = std::fs::read_to_string(config_path()).ok();
    cfg.as_deref().and_then(default_mode_from_toml).unwrap_or(PonytailMode::Full)
}

#[must_use]
pub fn allowlist() -> BTreeSet<String> {
    std::fs::read_to_string(config_path()).map(|c| parse_allowlist(&c)).unwrap_or_default()
}

/// Append a package to the allowlist (idempotent). Best-effort; errors ignored.
pub fn remember(name: &str) {
    let name = name.trim().to_ascii_lowercase();
    let mut set = allowlist();
    if !set.insert(name) {
        return;
    }
    let path = config_path();
    let existing_mode = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| default_mode_from_toml(&c))
        .unwrap_or(PonytailMode::Full);
    let list = set.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ");
    let body = format!("defaultMode = {:?}\nallow = [{}]\n", existing_mode.as_str(), list);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, body);
}
```

- [ ] **Step 4: Add to `lib.rs`**

```rust
pub mod config;
pub use config::{allowlist, config_path, origin_home, remember, resolve_mode};
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p origin-ponytail config && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): mode resolution + dependency allowlist

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: `ruleset.rs` — embedded ruleset + mode-filtered system block

**Files:**
- Create: `crates/origin-ponytail/assets/ponytail-ruleset.md`
- Create: `crates/origin-ponytail/src/ruleset.rs`
- Modify: `crates/origin-ponytail/src/lib.rs`

**Interfaces:**
- Consumes: `PonytailMode` (Task 1).
- Produces: `system_block(mode: PonytailMode) -> String`.

- [ ] **Step 1: Create the canonical asset**

Copy the body of `ponytail/skills/ponytail/SKILL.md` (everything after the closing `---` of the YAML frontmatter) into `crates/origin-ponytail/assets/ponytail-ruleset.md` verbatim. (Source path on disk: `C:\Users\wooai\Documents\origin\ponytail\skills\ponytail\SKILL.md`, lines 17–101.) Do not edit the text — it is the single source of truth.

- [ ] **Step 2: Write the failing test (bottom of `src/ruleset.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_is_empty() {
        assert!(system_block(PonytailMode::Off).is_empty());
    }

    #[test]
    fn block_is_wrapped_and_carries_level() {
        let b = system_block(PonytailMode::Full);
        assert!(b.starts_with("<origin-ponytail level=\"full\">"));
        assert!(b.trim_end().ends_with("</origin-ponytail>"));
        assert!(b.contains("lazy senior developer"));
    }

    #[test]
    fn intensity_rows_are_filtered_by_mode() {
        // The ultra worked-example bullet appears only in ultra.
        let full = system_block(PonytailMode::Full);
        let ultra = system_block(PonytailMode::Ultra);
        assert!(!full.contains("YAGNI extremist"));
        assert!(ultra.contains("YAGNI extremist"));
    }
}
```

- [ ] **Step 3: Run, verify failure**

Run: `cargo test -p origin-ponytail ruleset`
Expected: FAIL.

- [ ] **Step 4: Implement (top of `src/ruleset.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! The injected ponytail ruleset, filtered to the active intensity. Mirrors
//! ponytail's hooks/ponytail-instructions.js::filterSkillBodyForMode.

use crate::mode::PonytailMode;

const RULESET: &str = include_str!("../assets/ponytail-ruleset.md");

fn line_label_mode(line: &str) -> Option<PonytailMode> {
    // Intensity table row: `| **lite** | ... |`
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("| **") {
        if let Some(end) = rest.find("**") {
            return PonytailMode::parse_level(&rest[..end]);
        }
    }
    // Worked-example bullet: `- lite: ...`
    if let Some(rest) = t.strip_prefix("- ") {
        if let Some((label, _)) = rest.split_once(':') {
            return PonytailMode::parse_level(label.trim());
        }
    }
    None
}

/// Build the `<origin-ponytail>` system block for the mode, keeping only the
/// intensity-specific lines that match. `Off` ⇒ empty string.
#[must_use]
pub fn system_block(mode: PonytailMode) -> String {
    if mode == PonytailMode::Off {
        return String::new();
    }
    let body: String = RULESET
        .lines()
        .filter(|line| match line_label_mode(line) {
            Some(m) => m == mode, // mode-keyed line: keep only the active mode's
            None => true,         // ordinary rule line: always keep
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("<origin-ponytail level=\"{}\">\n{}\n</origin-ponytail>", mode.as_str(), body.trim())
}
```

- [ ] **Step 5: Run tests + clippy**

Run: `cargo test -p origin-ponytail ruleset && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS. If the `intensity_rows_are_filtered_by_mode` test fails, confirm the asset contains the verbatim `| **ultra** |` row and `- ultra:` bullet from the source SKILL.md.

- [ ] **Step 6: Add to `lib.rs` + commit**

```rust
pub mod ruleset;
pub use ruleset::system_block;
```

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): embed ruleset + mode-filtered system block

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: `debt.rs` — append-only override/advisory ledger

**Files:**
- Create: `crates/origin-ponytail/src/debt.rs`
- Modify: `crates/origin-ponytail/src/lib.rs`

**Interfaces:**
- Consumes: `origin_home()` (Task 5).
- Produces: `enum DebtAction { Advisory, OverrideOnce, Remembered, HeadlessAllow }`; `struct DebtEvent { action, dep, native, ts }`; `ledger_path() -> PathBuf`; `log(action: DebtAction, dep: &str, native: &str)`; `read() -> Vec<DebtEvent>`.

- [ ] **Step 1: Write the failing test (bottom of `src/debt.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_round_trips_jsonl() {
        let line = serde_json::to_string(&DebtEvent {
            action: DebtAction::OverrideOnce,
            dep: "lodash".into(),
            native: "Object.groupBy".into(),
            ts: 0,
        })
        .unwrap();
        let back: DebtEvent = serde_json::from_str(&line).unwrap();
        assert_eq!(back.dep, "lodash");
        assert!(matches!(back.action, DebtAction::OverrideOnce));
    }
}
```

- [ ] **Step 2: Run, verify failure**

Run: `cargo test -p origin-ponytail debt`
Expected: FAIL.

- [ ] **Step 3: Implement (top of `src/debt.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Append-only ledger of ponytail advisories/overrides (`~/.origin/ponytail-debt.jsonl`).

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::origin_home;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtAction {
    Advisory,
    OverrideOnce,
    Remembered,
    HeadlessAllow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebtEvent {
    pub action: DebtAction,
    pub dep: String,
    pub native: String,
    /// Unix seconds; 0 when unknown (the daemon stamps the real time).
    #[serde(default)]
    pub ts: u64,
}

#[must_use]
pub fn ledger_path() -> PathBuf {
    origin_home().join("ponytail-debt.jsonl")
}

/// Append one event. Best-effort: never panics, never blocks a write on failure.
pub fn log(action: DebtAction, dep: &str, native: &str) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ev = DebtEvent { action, dep: dep.to_string(), native: native.to_string(), ts };
    let Ok(line) = serde_json::to_string(&ev) else { return };
    let path = ledger_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

#[must_use]
pub fn read() -> Vec<DebtEvent> {
    std::fs::read_to_string(ledger_path())
        .map(|c| c.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
        .unwrap_or_default()
}
```

- [ ] **Step 4: Add to `lib.rs` + run + commit**

```rust
pub mod debt;
pub use debt::{DebtAction, DebtEvent};
```

Run: `cargo test -p origin-ponytail debt && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS.

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): append-only debt ledger

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 8: `commands.rs` — command text + debt harvest

**Files:**
- Create: `crates/origin-ponytail/assets/review.md`, `crates/origin-ponytail/assets/audit.md`
- Create: `crates/origin-ponytail/src/commands.rs`
- Modify: `crates/origin-ponytail/src/lib.rs`

**Interfaces:**
- Consumes: `debt::read` (Task 7).
- Produces: `review_prompt()->&'static str`; `audit_prompt()->&'static str`; `gain_text()->&'static str`; `help_text()->&'static str`; `harvest_comments(tree: &str) -> Vec<String>` (find `ponytail:` markers in a concatenated tree dump); `debt_report() -> String`.

- [ ] **Step 1: Create the prompt assets**

Copy the body of `ponytail/skills/ponytail-review/SKILL.md` (after frontmatter) into `assets/review.md`, and `ponytail/skills/ponytail-audit/SKILL.md` into `assets/audit.md`, verbatim.

- [ ] **Step 2: Write the failing test (bottom of `src/commands.rs`)**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_nonempty() {
        assert!(review_prompt().contains("over-engineering") || review_prompt().contains("delete"));
        assert!(!audit_prompt().is_empty());
        assert!(help_text().contains("/ponytail"));
        assert!(gain_text().contains('%'));
    }

    #[test]
    fn harvest_finds_markers() {
        let tree = "src/a.rs:12: // ponytail: global lock, per-account if throughput matters\nsrc/b.rs:3: let x = 1;\n";
        let hits = harvest_comments(tree);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].contains("global lock"));
    }
}
```

- [ ] **Step 3: Run, verify failure**

Run: `cargo test -p origin-ponytail commands`
Expected: FAIL.

- [ ] **Step 4: Implement (top of `src/commands.rs`)**

```rust
// SPDX-License-Identifier: Apache-2.0
//! Text + helpers backing the `/ponytail-*` commands.

use crate::debt::read as read_debt;

const REVIEW: &str = include_str!("../assets/review.md");
const AUDIT: &str = include_str!("../assets/audit.md");

#[must_use]
pub fn review_prompt() -> &'static str { REVIEW }

#[must_use]
pub fn audit_prompt() -> &'static str { AUDIT }

#[must_use]
pub fn gain_text() -> &'static str {
    "ponytail measured impact (Haiku 4.5, 12 agentic feature tasks vs no-skill baseline):\n\
     LOC -54%  ·  tokens -22%  ·  cost -20%  ·  time -27%  ·  safety 100%\n\
     Biggest cut where there's a real over-build trap; ~0 where code is already minimal."
}

#[must_use]
pub fn help_text() -> &'static str {
    "ponytail commands:\n\
     /ponytail [off|lite|full|ultra]  set intensity (no arg reports current)\n\
     /ponytail-review                 over-engineering review of the working diff\n\
     /ponytail-audit                  over-engineering scan of the whole repo\n\
     /ponytail-debt                   ledger of deferred ponytail: shortcuts + overrides\n\
     /ponytail-gain                   measured impact scoreboard\n\
     /ponytail-help                   this list"
}

/// Pull `ponytail:` markers out of a concatenated `file:line: text` tree dump.
#[must_use]
pub fn harvest_comments(tree: &str) -> Vec<String> {
    tree.lines()
        .filter(|l| l.contains("ponytail:"))
        .map(|l| l.trim().to_string())
        .collect()
}

/// Render the debt ledger + a hint to harvest code markers.
#[must_use]
pub fn debt_report() -> String {
    let events = read_debt();
    if events.is_empty() {
        return "ponytail debt: ledger empty. Nothing deferred yet.".to_string();
    }
    let mut out = format!("ponytail debt ledger ({} entries):\n", events.len());
    for e in events {
        out.push_str(&format!("  {:?}  {}  → {}\n", e.action, e.dep, e.native));
    }
    out
}
```

- [ ] **Step 5: Add to `lib.rs` + run + commit**

```rust
pub mod commands;
```

Run: `cargo test -p origin-ponytail commands && cargo clippy -p origin-ponytail -- -D warnings`
Expected: PASS.

```bash
git add crates/origin-ponytail
git commit -m "feat(ponytail): command text + debt harvest

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Crate-wide green gate

**Files:** none (verification + fixups only).

- [ ] **Step 1: Full crate test + clippy + fmt**

Run: `cargo test -p origin-ponytail && cargo clippy -p origin-ponytail -- -D warnings && cargo fmt -p origin-ponytail`
Expected: all tests PASS, no warnings. Fix any `fmt` diffs.

- [ ] **Step 2: Commit any fmt fixups**

```bash
git add -A && git commit -m "style(ponytail): cargo fmt

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase B — daemon wiring

### Task 10: Mode plumbing — `PromptRequest`, `LoopOptions`, resolution

**Files:**
- Modify: `crates/origin-daemon/Cargo.toml` (add `origin-ponytail = { path = "../origin-ponytail" }`)
- Modify: `crates/origin-daemon/src/protocol.rs:10-79` (`PromptRequest`)
- Modify: `crates/origin-daemon/src/agent.rs` (`LoopOptions` struct — search `pub struct LoopOptions`)
- Modify: `crates/origin-daemon/src/main.rs` (request → `LoopOptions` construction — search where `effort` is read off `PromptRequest`)

**Interfaces:**
- Consumes: `origin_ponytail::{PonytailMode, resolve_mode}`.
- Produces: `LoopOptions.ponytail: PonytailMode`; `PromptRequest.ponytail: Option<String>`.

- [ ] **Step 1: Add the dependency**

In `crates/origin-daemon/Cargo.toml` under `[dependencies]`:

```toml
origin-ponytail = { path = "../origin-ponytail" }
```

- [ ] **Step 2: Add the wire field to `PromptRequest`** (after the `effort` field at `protocol.rs:23`)

```rust
    /// Optional ponytail intensity for this turn (`off`/`lite`/`full`/`ultra`).
    /// `None` ⇒ the daemon resolves it (config/env/default `full`). Wire stays
    /// byte-identical when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ponytail: Option<String>,
```

- [ ] **Step 3: Add the field to `LoopOptions`**

Find `pub struct LoopOptions` in `agent.rs`. Add (mirror how `effort`/`thinking_tokens` are declared there — match their visibility and any `Default` derive):

```rust
    /// Resolved ponytail mode for this run (default `Full`).
    pub ponytail: origin_ponytail::PonytailMode,
```

If `LoopOptions` derives `Default`, `PonytailMode`'s `#[default] Full` makes this `Full` automatically. If it is built field-by-field (no `Default`), set it explicitly in every constructor (Step 4 covers the prompt path; grep for other `LoopOptions {` literals — e.g. tests, swarm — and add `ponytail: origin_ponytail::PonytailMode::Full` or `Default::default()`).

- [ ] **Step 4: Resolve at request time** (in `main.rs`, where `LoopOptions` is built from the `PromptRequest`, beside the `effort` read)

```rust
    let ponytail = origin_ponytail::resolve_mode(
        req.ponytail.as_deref().and_then(origin_ponytail::PonytailMode::parse_level),
    );
    // … set `ponytail` on the LoopOptions being constructed.
```

- [ ] **Step 5: Write a focused test** (in `crates/origin-daemon/src/main.rs` test module, or a new `tests/ponytail_resolve.rs`)

```rust
#[test]
fn request_token_overrides_default() {
    use origin_ponytail::{resolve_mode, PonytailMode};
    let req_token = Some("ultra".to_string());
    let mode = resolve_mode(req_token.as_deref().and_then(PonytailMode::parse_level));
    assert_eq!(mode, PonytailMode::Ultra);
}
```

- [ ] **Step 6: Build + test**

Run: `cargo test -p origin-daemon ponytail`
Expected: PASS, daemon compiles. Fix any `LoopOptions {` literals the compiler flags as missing the field.

- [ ] **Step 7: Commit**

```bash
git add crates/origin-daemon
git commit -m "feat(ponytail): thread ponytail mode through PromptRequest + LoopOptions

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 11: Inject the ruleset into the system prompt

**Files:**
- Modify: `crates/origin-daemon/src/agent.rs:3260-3431` (the system-prompt `parts` array)

**Interfaces:**
- Consumes: `LoopOptions.ponytail` (Task 10), `origin_ponytail::system_block`.

- [ ] **Step 1: Build the block before the `parts` array** (near where `subagents_block`/`result_encoding_block` are computed, ~`agent.rs:3378-3404`)

```rust
    let ponytail_block = origin_ponytail::system_block(opts.ponytail);
```

> Use the same accessor the surrounding code uses for `opts`/`self` — match how `edit_format_block`/`result_encoding_block` read their config in that scope.

- [ ] **Step 2: Add `&ponytail_block` to the `parts` array** (the `let parts: [&str; N] = [ … ];` literal, ~`agent.rs:3261`). Bump the array length and add the entry after `&result_encoding_block`:

```rust
        &result_encoding_block,
        &ponytail_block,
```

(The empty-string-when-Off case is already handled by the array's non-empty join filter.)

- [ ] **Step 3: Write an integration test** (`crates/origin-daemon/tests/ponytail_inject.rs` — or extend an existing system-prompt test; grep for a test that asserts on assembled system text)

```rust
// Verifies the ponytail block appears for Full and is absent for Off.
// Use the same system-prompt assembly helper the existing prompt tests use;
// if assembly is only reachable via run_loop, assert on system_block directly
// here and rely on Step 2 being a pure concatenation.
#[test]
fn ponytail_block_present_for_full_absent_for_off() {
    assert!(origin_ponytail::system_block(origin_ponytail::PonytailMode::Full)
        .contains("origin-ponytail"));
    assert!(origin_ponytail::system_block(origin_ponytail::PonytailMode::Off).is_empty());
}
```

- [ ] **Step 4: Build + test**

Run: `cargo test -p origin-daemon ponytail_inject`
Expected: PASS, daemon compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/origin-daemon
git commit -m "feat(ponytail): inject ruleset into the cached system prompt

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 12: The dependency gate in the pre-dispatch path

**Files:**
- Modify: `crates/origin-daemon/src/agent.rs` (pre-dispatch filter region, ~`4265-4330`, where path-touching tools are filtered alongside conseca; and a small helper near the other free `fn`s in the file)

**Interfaces:**
- Consumes: `LoopOptions.ponytail`, `LoopOptions` interactivity flag (the value derived from `PromptRequest.interactive` — grep `interactive` in `agent.rs`/`main.rs` to find how it reaches the loop), the interactive prompter (`ipc_prompter` / the same path `ask_user` / `RequiresPermission` uses), `origin_ponytail::{classify, allowlist, detect, debt}`, `origin_tools::ToolError`.

**Behavior (the spec, restated):** for `Edit/Write/MultiEdit/ApplyPatch/Bash`, collect added deps, `classify`, then:
- `Off` or no flags → proceed unchanged.
- `Lite` → never block: log `Advisory` to the ledger, proceed.
- `Full`/`Ultra`, **interactive** → prompt `[Allow once · Allow & remember · Deny]`; Deny → recoverable `ToolError`; remember → `config::remember` + proceed; once → proceed; all logged.
- `Full`/`Ultra`, **non-interactive** → allow + log `HeadlessAllow`.

- [ ] **Step 1: Add a pure helper that extracts deps from a tool call** (free fn near the top-level fns in `agent.rs`)

```rust
/// Collect newly-added deps from a code-write or shell tool call. Reads disk
/// for Write/manifest-diff; fails open (returns empty on any uncertainty).
fn ponytail_added_deps(tool: &str, args: &serde_json::Value) -> Vec<origin_ponytail::Dep> {
    use origin_ponytail::{bash_installs, manifest_deps_added, manifest_deps_in_added_lines};
    match tool {
        "Bash" => args
            .get("command")
            .and_then(|c| c.as_str())
            .map(bash_installs)
            .unwrap_or_default(),
        "Write" => {
            let Some(path) = args.get("file_path").and_then(|p| p.as_str()) else { return Vec::new() };
            let Some(content) = args.get("content").and_then(|c| c.as_str()) else { return Vec::new() };
            let before = std::fs::read_to_string(path).ok();
            manifest_deps_added(path, before.as_deref(), content)
        }
        "Edit" => {
            let Some(path) = args.get("file_path").and_then(|p| p.as_str()) else { return Vec::new() };
            let added = args.get("new_string").and_then(|s| s.as_str()).unwrap_or("");
            manifest_deps_in_added_lines(path, added)
        }
        "MultiEdit" => {
            let Some(path) = args.get("file_path").and_then(|p| p.as_str()) else { return Vec::new() };
            let mut all = Vec::new();
            if let Some(edits) = args.get("edits").and_then(|e| e.as_array()) {
                for e in edits {
                    let added = e.get("new_string").and_then(|s| s.as_str()).unwrap_or("");
                    all.extend(manifest_deps_in_added_lines(path, added));
                }
            }
            all
        }
        "ApplyPatch" => {
            // Scan only added (`+`) lines of the patch body, per target file.
            let patch = args.get("patch").or_else(|| args.get("input")).and_then(|p| p.as_str()).unwrap_or("");
            let mut all = Vec::new();
            let mut cur = String::new();
            for line in patch.lines() {
                if let Some(p) = line.strip_prefix("*** ").and_then(|l| l.strip_prefix("Update File: ").or_else(|| l.strip_prefix("Add File: "))) {
                    cur = p.trim().to_string();
                } else if let Some(added) = line.strip_prefix('+') {
                    if !cur.is_empty() {
                        all.extend(manifest_deps_in_added_lines(&cur, added));
                    }
                }
            }
            all
        }
        _ => Vec::new(),
    }
}
```

> If `ApplyPatch`'s arg/format differs in this codebase, adjust the field name and header parse to match `crates/origin-tools/src/builtins/apply_patch.rs`. A mismatch just means ApplyPatch fails open — acceptable.

- [ ] **Step 2: Insert the gate in the pre-dispatch region** (~`agent.rs:4265`, after the conseca/path filtering, before `dispatch_tool` is reached). Use the variable names already in scope for the tool name (`meta.name`), the parsed args (`args`), the resolved mode (`opts.ponytail`), and the interactive flag (call it `interactive` — wire it from the same source `permission_ask`/`interactive` uses):

```rust
    // --- ponytail dependency gate ---
    if opts.ponytail != origin_ponytail::PonytailMode::Off
        && matches!(meta.name.as_str(), "Edit" | "Write" | "MultiEdit" | "ApplyPatch" | "Bash")
    {
        let deps = ponytail_added_deps(&meta.name, args);
        if !deps.is_empty() {
            let flags = origin_ponytail::classify(&deps, opts.ponytail, &origin_ponytail::allowlist());
            for f in &flags {
                let native = match f.kind {
                    origin_ponytail::FlagKind::Replaceable(r) => r.native,
                    origin_ponytail::FlagKind::Unjustified => "(none)",
                };
                if opts.ponytail == origin_ponytail::PonytailMode::Lite {
                    origin_ponytail::debt::log(origin_ponytail::DebtAction::Advisory, &f.dep.name, native);
                    continue;
                }
                if !interactive {
                    origin_ponytail::debt::log(origin_ponytail::DebtAction::HeadlessAllow, &f.dep.name, native);
                    continue;
                }
                // interactive full/ultra: ask the user.
                match prompt_ponytail_choice(/* prompter handle in scope */, f).await {
                    PonytailChoice::Deny => {
                        return Err(LoopError::ToolFailure(format!(
                            "ponytail.blocked: {}",
                            f.message()
                        )));
                    }
                    PonytailChoice::AllowRemember => {
                        origin_ponytail::remember(&f.dep.name);
                        origin_ponytail::debt::log(origin_ponytail::DebtAction::Remembered, &f.dep.name, native);
                    }
                    PonytailChoice::AllowOnce => {
                        origin_ponytail::debt::log(origin_ponytail::DebtAction::OverrideOnce, &f.dep.name, native);
                    }
                }
            }
        }
    }
```

> `LoopError::ToolFailure` is the recoverable error arm the loop already surfaces to the model (`agent.rs:4748`). If the exact variant name differs, use whatever variant `dispatch_tool`'s recoverable errors use so the model sees the message and retries.

- [ ] **Step 3: Add the prompt helper using the existing prompter.** Find how `ask_user` / `RequiresPermission` raises an interactive choice (grep `PermissionAsk` / `ChoiceAsk` in `agent.rs` and `ipc_prompter.rs`). Mirror that exact call to present three options and map the reply:

```rust
enum PonytailChoice { AllowOnce, AllowRemember, Deny }

// Pseudtotype — implement against the real prompter signature found in
// ipc_prompter.rs. Present: "ponytail: <message>" with options
// ["Allow once", "Allow & remember", "Deny"], default Deny on any error/timeout.
async fn prompt_ponytail_choice(prompter: &Prompter, f: &origin_ponytail::Flagged) -> PonytailChoice {
    let title = format!("ponytail: {}", f.message());
    let opts = ["Allow once", "Allow & remember", "Deny"];
    match prompter.choose(&title, &opts).await {
        Ok(0) => PonytailChoice::AllowOnce,
        Ok(1) => PonytailChoice::AllowRemember,
        _ => PonytailChoice::Deny, // explicit deny, error, or no answer
    }
}
```

> The exact prompter type/method comes from `ipc_prompter.rs`. The contract that matters: three labeled choices, and **default to Deny** on any non-answer (consistent with "blocking is interactive-only and safe").

- [ ] **Step 4: Write an integration test** (`crates/origin-daemon/tests/ponytail_gate.rs`)

```rust
// Unit-level coverage of the extraction + classification the gate runs.
// (Full prompter round-trip is covered by the interactive harness tests; here
// we lock the dep-extraction + classify wiring that the gate depends on.)
#[test]
fn write_adding_lodash_is_flagged_in_full() {
    let args = serde_json::json!({
        "file_path": "package.json",
        "content": r#"{"dependencies":{"lodash":"4"}}"#
    });
    // mirror ponytail_added_deps for Write with no prior file:
    let content = args["content"].as_str().unwrap();
    let deps = origin_ponytail::manifest_deps_added("package.json", None, content);
    let flags = origin_ponytail::classify(&deps, origin_ponytail::PonytailMode::Full, &Default::default());
    assert_eq!(flags.len(), 1);
    assert_eq!(flags[0].dep.name, "lodash");
}

#[test]
fn bare_npm_install_not_flagged() {
    let deps = origin_ponytail::bash_installs("npm install");
    assert!(deps.is_empty());
}
```

- [ ] **Step 5: Build + test**

Run: `cargo test -p origin-daemon ponytail_gate`
Expected: PASS, daemon compiles.

- [ ] **Step 6: Commit**

```bash
git add crates/origin-daemon
git commit -m "feat(ponytail): deterministic dependency gate in pre-dispatch

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase C — CLI wiring

### Task 13: `App.ponytail_mode` + `--ponytail` flag + snapshot

**Files:**
- Modify: `crates/origin-cli/Cargo.toml` (add `origin-ponytail` dep)
- Modify: `crates/origin-cli/src/tui/mod.rs` (`App` struct — near `pub effort: Option<String>`)
- Modify: `crates/origin-cli/src/cli_def.rs` (global `Cli` + `run` subcommand — near `--effort`)
- Modify: `crates/origin-cli/src/main.rs` (snapshot beside the `effort` snapshot, ~`main.rs:2178`; `call_daemon` → `PromptRequest`)

**Interfaces:**
- Consumes: `origin_ponytail::PonytailMode`.
- Produces: `App.ponytail_mode: Option<PonytailMode>`; `PromptRequest.ponytail` populated.

- [ ] **Step 1: Add the dep** to `crates/origin-cli/Cargo.toml`:

```toml
origin-ponytail = { path = "../origin-ponytail" }
```

- [ ] **Step 2: Add the `App` field** (in `tui/mod.rs`, beside `pub effort`):

```rust
    /// Session ponytail intensity, mutated by `/ponytail`; carried on every
    /// PromptRequest. `None` ⇒ daemon resolves (default `full`).
    pub ponytail_mode: Option<origin_ponytail::PonytailMode>,
```

Initialize it to `None` wherever `App` is constructed (grep `effort:` in the constructor and add `ponytail_mode: None,` beside it; seed from the flag in Step 4).

- [ ] **Step 3: Add the CLI flag** (in `cli_def.rs`, beside `--effort` on both the global `Cli` and the `run` subcommand):

```rust
    /// Ponytail intensity for the session (`off`/`lite`/`full`/`ultra`).
    #[arg(long)]
    pub ponytail: Option<String>,
```

- [ ] **Step 4: Seed `App.ponytail_mode` from the flag** at startup (where `--effort` seeds `app.effort`):

```rust
    app.ponytail_mode = cli.ponytail.as_deref().and_then(origin_ponytail::PonytailMode::parse_level);
```

- [ ] **Step 5: Snapshot onto the request** (beside the `effort` snapshot at `main.rs:~2178`, and thread through `call_daemon` into `PromptRequest`):

```rust
    let ponytail = app.lock().ponytail_mode.map(|m| m.as_str().to_string());
    // … PromptRequest { …, ponytail, … }
```

- [ ] **Step 6: Build**

Run: `cargo build -p origin-cli`
Expected: compiles. (No new test here — covered by Task 14's command test.)

- [ ] **Step 7: Commit**

```bash
git add crates/origin-cli
git commit -m "feat(ponytail): session mode field, --ponytail flag, request snapshot

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 14: `/ponytail` toggle command

**Files:**
- Modify: `crates/origin-cli/src/main.rs` (command-handling region — beside the `/plan` handler at `main.rs:~1799` and the `/effort` handler at `~1677`)

**Interfaces:**
- Consumes: `origin_ponytail::{parse_ponytail_command, PonytailCmd, PonytailMode}`, `App.ponytail_mode`.

- [ ] **Step 1: Add the handler** (beside `/effort`/`/plan`; mirror their structure exactly):

```rust
    if let Some(cmd) = origin_ponytail::parse_ponytail_command(text) {
        app.lock().add_line("you> ", text);
        match cmd {
            origin_ponytail::PonytailCmd::Set(mode) => {
                app.lock().ponytail_mode = Some(mode);
                app.lock().add_line("system> ", &format!("ponytail mode: {}", mode.as_str()));
            }
            origin_ponytail::PonytailCmd::Report => {
                let cur = app
                    .lock()
                    .ponytail_mode
                    .map_or("full (default)".to_string(), |m| m.as_str().to_string());
                app.lock().add_line("system> ", &format!("ponytail mode: {cur}"));
            }
            origin_ponytail::PonytailCmd::Usage => {
                app.lock().add_line("error> ", "usage: /ponytail [off|lite|full|ultra]");
            }
        }
        handle.mark_dirty();
        return;
    }
```

> Place this **before** any generic `/`-prefixed fallthrough, and confirm it doesn't shadow `/ponytail-review` etc. — `parse_ponytail_command` returns `None` for `/ponytail-review` because `-review` is not whitespace after the prefix, so those fall through to Task 16's handlers. Keep this handler **after** the Task 16 handlers OR ensure `parse_ponytail_command` rejects the hyphen forms (it does: `/ponytail-review` → `strip_prefix("/ponytail")` leaves `-review`, which is not empty and not whitespace ⇒ `None`). Safe either way.

- [ ] **Step 2: Write a test** (in `crates/origin-cli` — extend the existing command-parsing test module, or `tests/ponytail_cmd.rs`):

```rust
#[test]
fn ponytail_command_grammar() {
    use origin_ponytail::{parse_ponytail_command, PonytailCmd, PonytailMode};
    assert_eq!(parse_ponytail_command("/ponytail ultra"), Some(PonytailCmd::Set(PonytailMode::Ultra)));
    assert_eq!(parse_ponytail_command("/ponytail"), Some(PonytailCmd::Report));
    assert_eq!(parse_ponytail_command("/ponytail-review"), None); // falls through to the review cmd
}
```

- [ ] **Step 3: Build + test**

Run: `cargo test -p origin-cli ponytail_cmd && cargo build -p origin-cli`
Expected: PASS, compiles.

- [ ] **Step 4: Commit**

```bash
git add crates/origin-cli
git commit -m "feat(ponytail): /ponytail toggle command

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 15: Statusline badge

**Files:**
- Modify: `crates/origin-cli/src/tui/mod.rs` (the status-readout builder — grep `statusline`/`status_readout`)

**Interfaces:**
- Consumes: `App.ponytail_mode`.

- [ ] **Step 1: Add a badge helper on `App`** (near the status-readout code):

```rust
    /// Status-bar badge for the active ponytail mode. Empty when off; the
    /// default (`None` ⇒ full) shows `[PONYTAIL]`.
    #[must_use]
    pub fn ponytail_badge(&self) -> String {
        match self.ponytail_mode {
            Some(origin_ponytail::PonytailMode::Off) => String::new(),
            Some(origin_ponytail::PonytailMode::Ultra) => "[PONYTAIL:ULTRA]".to_string(),
            _ => "[PONYTAIL]".to_string(),
        }
    }
```

- [ ] **Step 2: Append the badge** into the existing status-readout string (wherever the effort/model badges are concatenated). Add `self.ponytail_badge()` to that segment list, space-separated, skipping it when empty.

- [ ] **Step 3: Write a test**

```rust
#[test]
fn badge_reflects_mode() {
    // Construct a minimal App (use the existing test constructor/helper).
    // assert default/None and Full -> "[PONYTAIL]", Ultra -> "[PONYTAIL:ULTRA]", Off -> "".
    use origin_ponytail::PonytailMode;
    // pseudocode: let mut app = App::test_default();
    // app.ponytail_mode = Some(PonytailMode::Ultra); assert_eq!(app.ponytail_badge(), "[PONYTAIL:ULTRA]");
    // app.ponytail_mode = Some(PonytailMode::Off); assert!(app.ponytail_badge().is_empty());
    assert_eq!(PonytailMode::Ultra.as_str(), "ultra");
}
```

> Replace the pseudocode with the project's real `App` test constructor (grep existing `tui` tests for how `App` is built in tests). If no test constructor exists, assert on a free helper instead by extracting the match into `fn badge_for(mode: Option<PonytailMode>) -> String` and testing that.

- [ ] **Step 4: Build + test + commit**

Run: `cargo test -p origin-cli badge && cargo build -p origin-cli`

```bash
git add crates/origin-cli
git commit -m "feat(ponytail): statusline badge

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 16: `/ponytail-review` + `/ponytail-audit` (auto-run)

**Files:**
- Modify: `crates/origin-cli/src/main.rs` (command-handling region)

**Interfaces:**
- Consumes: `origin_ponytail::commands::{review_prompt, audit_prompt}`; the existing "send a prompt to the daemon" path (the same one a typed message uses — grep how a normal prompt is dispatched, e.g. `call_daemon`).

- [ ] **Step 1: Add the review handler** (auto-runs: gathers the diff, then sends prompt + diff as one turn):

```rust
    if text.trim() == "/ponytail-review" || text.trim() == "/ponytail-audit" {
        let is_audit = text.trim().ends_with("audit");
        app.lock().add_line("you> ", text);
        let context = if is_audit {
            // whole-repo: list tracked files (cheap) for the model to scan
            run_git(&["ls-files"]).unwrap_or_default()
        } else {
            run_git(&["diff"]).unwrap_or_default()
        };
        if context.trim().is_empty() {
            app.lock().add_line("system> ", "ponytail: nothing to review (clean working tree).");
            handle.mark_dirty();
            return;
        }
        let prompt = format!(
            "{}\n\n--- {} ---\n{}",
            if is_audit { origin_ponytail::commands::audit_prompt() } else { origin_ponytail::commands::review_prompt() },
            if is_audit { "repo file list" } else { "git diff" },
            context
        );
        // dispatch `prompt` exactly as a normal user turn (reuse the prompt path).
        send_prompt_to_daemon(&prompt /*, …same args a typed message uses… */);
        return;
    }
```

> `run_git` / `send_prompt_to_daemon` stand in for the project's real helpers. Find the existing git invocation helper (grep `Command::new("git")` in `origin-cli`) and the normal-prompt dispatch (the code that runs when the user just types a message). Reuse both — do not add new ones. For `audit`, the file list keeps the turn cheap; the model reads files it suspects.

- [ ] **Step 2: Test the prompt assembly** (pure part):

```rust
#[test]
fn review_prompt_is_injected() {
    assert!(!origin_ponytail::commands::review_prompt().is_empty());
    assert!(!origin_ponytail::commands::audit_prompt().is_empty());
}
```

- [ ] **Step 3: Build + test + commit**

Run: `cargo test -p origin-cli review_prompt && cargo build -p origin-cli`

```bash
git add crates/origin-cli
git commit -m "feat(ponytail): auto-running /ponytail-review and /ponytail-audit

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

### Task 17: `/ponytail-debt` + `/ponytail-gain` + `/ponytail-help`

**Files:**
- Modify: `crates/origin-cli/src/main.rs` (command-handling region)

**Interfaces:**
- Consumes: `origin_ponytail::commands::{debt_report, gain_text, help_text, harvest_comments}`; the git helper from Task 16.

- [ ] **Step 1: Add the three text commands**

```rust
    match text.trim() {
        "/ponytail-help" => {
            app.lock().add_line("you> ", text);
            app.lock().add_line("system> ", origin_ponytail::commands::help_text());
            handle.mark_dirty();
            return;
        }
        "/ponytail-gain" => {
            app.lock().add_line("you> ", text);
            app.lock().add_line("system> ", origin_ponytail::commands::gain_text());
            handle.mark_dirty();
            return;
        }
        "/ponytail-debt" => {
            app.lock().add_line("you> ", text);
            let mut out = origin_ponytail::commands::debt_report();
            // harvest code markers too: `git grep -n "ponytail:"`
            if let Some(grep) = run_git(&["grep", "-n", "ponytail:"]).ok().filter(|s| !s.trim().is_empty()) {
                let hits = origin_ponytail::commands::harvest_comments(&grep);
                if !hits.is_empty() {
                    out.push_str(&format!("\n\ncode markers ({}):\n  {}", hits.len(), hits.join("\n  ")));
                }
            }
            app.lock().add_line("system> ", &out);
            handle.mark_dirty();
            return;
        }
        _ => {}
    }
```

- [ ] **Step 2: Test** the harvest + text (pure):

```rust
#[test]
fn debt_and_help_text() {
    assert!(origin_ponytail::commands::help_text().contains("/ponytail-debt"));
    let hits = origin_ponytail::commands::harvest_comments("a.rs:1: // ponytail: naive O(n^2)\n");
    assert_eq!(hits.len(), 1);
}
```

- [ ] **Step 3: Build + test + commit**

Run: `cargo test -p origin-cli debt_and_help && cargo build -p origin-cli`

```bash
git add crates/origin-cli
git commit -m "feat(ponytail): /ponytail-debt, -gain, -help commands

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Phase D — finalize

### Task 18: Workspace verification, version bump, docs

**Files:**
- Modify: root `Cargo.toml` (`[workspace.package] version`) + `packaging/npm/package.json` (keep versions in lockstep, per project norm)
- Create: `docs/crates/origin-ponytail.md` (one-page crate doc, mirroring other `docs/crates/*.md`)

- [ ] **Step 1: Per-crate green sweep**

Run, in git-bash:
```
cargo test -p origin-ponytail
cargo clippy -p origin-ponytail -- -D warnings
cargo test -p origin-daemon ponytail
cargo clippy -p origin-daemon -- -D warnings
cargo build -p origin-cli && cargo clippy -p origin-cli -- -D warnings
```
Expected: all PASS. (Do not run `cargo build --workspace` — known LNK1140 failure on this machine.)

- [ ] **Step 2: Write the crate doc** `docs/crates/origin-ponytail.md`

```markdown
# origin-ponytail

Native ponytail: an always-on "lazy senior dev" ruleset injected into the system
prompt, plus a deterministic, table-driven dependency gate at tool-dispatch time.

- Modes: `off / lite / full (default) / ultra` — `/ponytail [level]`, `--ponytail`, `PONYTAIL_DEFAULT_MODE`.
- Gate: blocks adding a dependency that has a native/stdlib replacement (manifest
  edits + package-manager Bash). `full` blocks replaceable deps; `ultra` challenges
  every new dep. Interactive → prompt (allow once / remember / deny); non-interactive
  → allow + log to `~/.origin/ponytail-debt.jsonl`. Allowlist: `~/.origin/ponytail.toml`.
- Commands: `/ponytail-review`, `-audit`, `-debt`, `-gain`, `-help`.

See the design spec: `docs/superpowers/specs/2026-06-20-ponytail-native-design.md`.
```

- [ ] **Step 3: Bump version** (PATCH, 0.9.x series per project norm — next patch above the current release). Update root `Cargo.toml` `[workspace.package] version` and `packaging/npm/package.json` to match.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "docs(ponytail): crate doc + version bump for ponytail-native

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

**1. Spec coverage:**
- §1 goal (injection + gate) → Tasks 6/11 (injection), 4/12 (gate). ✓
- §3.1 modules → Tasks 1–8 (mode, ruleset, native_table, detect, gate, config, debt, commands). ✓
- §3.2 ruleset filtering + sync → Task 6. ✓
- §3.3 native table incl. curated cargo/go/ruby + omit-list → Task 2. ✓
- §3.4 detectors incl. negative cases → Task 3. ✓
- §3.5 classifier → Task 4. ✓
- §4 daemon wiring (PromptRequest/LoopOptions/resolve, injection point, gate point, prompter, headless-allow) → Tasks 10/11/12. ✓
- §5 CLI wiring (App field, /ponytail, --flag, snapshot) → Tasks 13/14. ✓
- §6 commands → Tasks 16/17. ✓
- §7 statusline → Task 15. ✓
- §8 files/config (ponytail.toml, debt.jsonl, env) → Tasks 5/7. ✓
- §9 mode semantics → enforced in Tasks 4 (classify) + 12 (action per mode). ✓
- §10 testing → tests in every task. ✓

**2. Placeholder scan:** No `TBD`/`TODO`. Three spots reuse existing project helpers named descriptively (`prompt_ponytail_choice` against `ipc_prompter`, `send_prompt_to_daemon`, `run_git`) with explicit grep pointers and exact contracts — these are integration seams into unfamiliar existing code, not unfinished logic; each carries the precise behavior required.

**3. Type consistency:** `PonytailMode`, `PonytailCmd`, `Dep`, `Ecosystem`, `NativeReplacement`, `FlagKind`, `Flagged`, `DebtAction`/`DebtEvent`, `classify`, `system_block`, `resolve_mode`, `allowlist`, `remember`, `bash_installs`, `manifest_deps_added`, `manifest_deps_in_added_lines` are used identically across the crate (Tasks 1–8) and the wiring (Tasks 10–17). `as_str()`/`parse_level()` match the `effort.rs` pattern. ✓

---

## Execution Handoff

(filled in by the writing-plans skill's handoff prompt)
