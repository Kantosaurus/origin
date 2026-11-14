// SPDX-License-Identifier: Apache-2.0
//! Post-edit lint/test/format policy and a builtin formatter table for `origin`.
//!
//! After the agent edits a file, the daemon needs to decide what to do next:
//! which formatter to run (aider's `auto-lint`, opencode's ~25 builtin
//! auto-formatters), whether to lint and test, and — when a check fails — how
//! many times to let the model attempt a repair before giving up.
//!
//! This crate is pure config + decision logic. It never spawns a process or
//! touches the filesystem; the caller executes the chosen commands. That keeps
//! it std-only, deterministic, and trivially testable.
//!
//! ```
//! use origin_postedit::{formatter_for, repair_decision, PostEditConfig, RepairDecision};
//!
//! assert_eq!(formatter_for("src/main.rs"), Some("rustfmt"));
//! assert_eq!(formatter_for("app/page.tsx"), Some("prettier"));
//!
//! let cfg = PostEditConfig::default();
//! assert_eq!(repair_decision(0, 0, &cfg), RepairDecision::Stop);
//! assert_eq!(repair_decision(2, 0, &cfg), RepairDecision::Retry { iter: 1 });
//! ```

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// A single builtin formatter mapping: file extension to the format command.
///
/// The `command` is the program plus any subcommand/flags, exactly as the
/// caller should invoke it (the target path is appended by the caller).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormatterRule {
    /// Lowercase file extension this rule matches (no leading dot).
    pub ext: &'static str,
    /// The formatter command to run, e.g. `"rustfmt"` or `"prettier"`.
    pub command: &'static str,
}

/// Build one [`FormatterRule`] (keeps the static table dense and readable).
const fn rule(ext: &'static str, command: &'static str) -> FormatterRule {
    FormatterRule { ext, command }
}

/// Builtin formatter table (opencode parity: ~25 auto-formatters). Longest
/// list ships every common ecosystem; extensions are unique and lowercase.
static FORMATTERS: &[FormatterRule] = &[
    // Rust.
    rule("rs", "rustfmt"),
    // Go.
    rule("go", "gofmt"),
    // Python (ruff is the modern default; opencode/aider both ship it).
    rule("py", "ruff format"),
    rule("pyi", "ruff format"),
    // JavaScript / TypeScript / web assets -> prettier.
    rule("ts", "prettier"),
    rule("tsx", "prettier"),
    rule("js", "prettier"),
    rule("jsx", "prettier"),
    rule("mjs", "prettier"),
    rule("cjs", "prettier"),
    rule("json", "prettier"),
    rule("jsonc", "prettier"),
    rule("css", "prettier"),
    rule("scss", "prettier"),
    rule("less", "prettier"),
    rule("html", "prettier"),
    rule("vue", "prettier"),
    rule("svelte", "prettier"),
    rule("yaml", "prettier"),
    rule("yml", "prettier"),
    rule("md", "prettier"),
    rule("mdx", "prettier"),
    rule("graphql", "prettier"),
    // C / C++ family.
    rule("c", "clang-format"),
    rule("cc", "clang-format"),
    rule("cpp", "clang-format"),
    rule("cxx", "clang-format"),
    rule("h", "clang-format"),
    rule("hpp", "clang-format"),
    // Kotlin.
    rule("kt", "ktlint"),
    rule("kts", "ktlint"),
    // Elixir.
    rule("ex", "mix format"),
    rule("exs", "mix format"),
    // Ruby.
    rule("rb", "rubocop -a"),
    // Shell.
    rule("sh", "shfmt"),
    rule("bash", "shfmt"),
    // Lua.
    rule("lua", "stylua"),
    // TOML.
    rule("toml", "taplo fmt"),
    // Dart / Swift / Zig / Nix / Terraform / Java.
    rule("dart", "dart format"),
    rule("swift", "swift-format"),
    rule("zig", "zig fmt"),
    rule("nix", "nixpkgs-fmt"),
    rule("tf", "terraform fmt"),
    rule("java", "google-java-format"),
];

/// Builtin formatter table (opencode parity: ~25 auto-formatters).
///
/// Returns a slice of [`FormatterRule`]s keyed by lowercase extension. The
/// table is intentionally easy to amend; the *mechanism* (extension lookup with
/// per-config overrides) is the contribution.
#[must_use]
pub const fn builtin_formatters() -> &'static [FormatterRule] {
    FORMATTERS
}

/// Look up the builtin formatter command for `path` by its extension.
///
/// Matching is case-insensitive on the extension (so `Main.RS` resolves the
/// same as `main.rs`). Returns `None` for paths without a known extension —
/// callers should then skip auto-formatting rather than guess.
///
/// This consults only the builtin table; for per-session overrides use
/// [`PostEditConfig::formatter_for`].
#[must_use]
pub fn formatter_for(path: &str) -> Option<&'static str> {
    let ext = extension_of(path)?;
    builtin_formatters()
        .iter()
        .find(|rule| rule.ext == ext)
        .map(|rule| rule.command)
}

/// Extract the lowercase extension of `path`, or `None` if it has none.
///
/// Handles both `/` and `\` separators (Windows + POSIX) and ignores a leading
/// dot on dotfiles (e.g. `.gitignore` has no extension here).
fn extension_of(path: &str) -> Option<String> {
    let file = path.rsplit(['/', '\\']).next().filter(|s| !s.is_empty())?;
    let (stem, ext) = file.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

/// Post-edit policy for a session.
///
/// Mirrors aider (`auto-lint`, `auto-test`) and adds opencode-style formatter
/// overrides plus a bounded repair-iteration budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostEditConfig {
    /// Run a linter after each edit.
    pub auto_lint: bool,
    /// Lint command override; `None` falls back to the caller's default.
    pub lint_command: Option<String>,
    /// Run the test suite after each edit.
    pub auto_test: bool,
    /// Test command override; `None` falls back to the caller's default.
    pub test_command: Option<String>,
    /// C8: reproduction gate. When set, the model is instructed (via a
    /// `<repro-gate>` contract) to write ONE focused test that fails on current
    /// code BEFORE fixing a reported bug, and `test_command` is run at turn-end
    /// so the fail→pass transition is execution-checked. Opt-in, default-off.
    #[serde(default)]
    pub repro_gate: bool,
    /// Per-extension formatter overrides (extension, command), tried before the
    /// builtin table. Extensions are matched case-insensitively.
    pub format_overrides: Vec<(String, String)>,
    /// Maximum number of automatic repair attempts after a failing check.
    pub max_repair_iters: u32,
}

impl Default for PostEditConfig {
    fn default() -> Self {
        Self {
            auto_lint: false,
            lint_command: None,
            auto_test: false,
            test_command: None,
            repro_gate: false,
            format_overrides: Vec::new(),
            max_repair_iters: 2,
        }
    }
}

impl PostEditConfig {
    /// Resolve the formatter command for `path`, honoring overrides first.
    ///
    /// An override whose extension matches `path` (case-insensitively) wins;
    /// otherwise the builtin table is consulted. Returns an owned `String`
    /// because overrides are owned, and `None` when neither source matches.
    #[must_use]
    pub fn formatter_for(&self, path: &str) -> Option<String> {
        let ext = extension_of(path)?;
        if let Some((_, command)) = self
            .format_overrides
            .iter()
            .find(|(o_ext, _)| o_ext.eq_ignore_ascii_case(&ext))
        {
            return Some(command.clone());
        }
        formatter_for(path).map(ToString::to_string)
    }

    /// Is a turn-end test run armed for this config?
    ///
    /// True when the model has been asked to run tests at turn end for *any*
    /// reason — an explicit `auto_test`, or the reproduction gate (which also
    /// execution-checks the fail→pass transition). Both require a resolvable
    /// `test_command`; without one there is nothing to run, so this is `false`.
    /// This is the single predicate the agent loop consults to decide whether
    /// to run — and, for the repro gate, *enforce* — the turn-end oracle.
    #[must_use]
    pub const fn test_gate_armed(&self) -> bool {
        (self.auto_test || self.repro_gate) && self.test_command.is_some()
    }
}

/// What to do after evaluating post-edit check results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairDecision {
    /// No failures remain — the edit is clean, stop the loop.
    Stop,
    /// Failures remain and there is budget left; attempt repair iteration `iter`.
    Retry {
        /// 1-based index of the repair attempt about to start.
        iter: u32,
    },
    /// Failures remain but the repair budget is exhausted; surface to the user.
    GiveUp,
}

/// Decide the next step of the post-edit repair loop.
///
/// `failures` is the count of failing checks observed this round, `prev_iters`
/// is how many repair attempts have already been made, and `cfg` supplies the
/// budget ([`PostEditConfig::max_repair_iters`]).
///
/// * `failures == 0` -> [`RepairDecision::Stop`].
/// * otherwise, while `prev_iters < max_repair_iters` ->
///   [`RepairDecision::Retry`] with the next 1-based iteration number.
/// * otherwise -> [`RepairDecision::GiveUp`].
#[must_use]
pub const fn repair_decision(failures: u32, prev_iters: u32, cfg: &PostEditConfig) -> RepairDecision {
    if failures == 0 {
        return RepairDecision::Stop;
    }
    if prev_iters < cfg.max_repair_iters {
        RepairDecision::Retry {
            iter: prev_iters.saturating_add(1),
        }
    } else {
        RepairDecision::GiveUp
    }
}

/// Which ecosystem marker files are present at a repository root.
///
/// This is a *pure input* to [`detect_test_command`]: the caller probes the
/// filesystem (the crate never does) and reports which markers exist, keeping
/// this crate std-only, deterministic, and trivially testable. Fields are
/// ordered by detection precedence (see [`detect_test_command`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // a flag record per ecosystem marker — named bools read clearer than a bitset
pub struct RepoMarkers {
    /// A `Cargo.toml` at the root → Rust (`cargo test`).
    pub cargo_toml: bool,
    /// A `package.json` at the root → Node (`npm test`).
    pub package_json: bool,
    /// A `pyproject.toml` at the root → Python (`pytest`).
    pub pyproject_toml: bool,
    /// A `setup.py` / `setup.cfg` / `tox.ini` at the root → Python (`pytest`).
    pub python_legacy: bool,
    /// A `go.mod` at the root → Go (`go test ./...`).
    pub go_mod: bool,
    /// A `pom.xml` at the root → Java/Maven (`mvn -q test`).
    pub pom_xml: bool,
    /// A `build.gradle` / `build.gradle.kts` at the root → Gradle (`gradle test`).
    pub build_gradle: bool,
    /// A `Gemfile` at the root → Ruby (`bundle exec rake test`).
    pub gemfile: bool,
    /// A `Makefile` at the root exposing a `test` target → `make test`.
    /// (The caller confirms the target exists; this crate never reads files.)
    pub makefile_test_target: bool,
}

/// Best-effort ecosystem default test command from the repo's marker files.
///
/// Precedence is deliberate: language toolchains that own the whole repo
/// (`Cargo.toml`, `go.mod`) win over generic `Makefile`/build-tool wrappers,
/// and a concrete Python project file beats the legacy fallback. Returns
/// `None` when no marker is recognized — the caller should then leave
/// `test_command` unset (the gate stays inert rather than guessing).
///
/// This is intentionally conservative: it favours the single most common,
/// zero-config invocation per ecosystem. Projects with a non-standard runner
/// override it explicitly via `[post_edit] test_command` in `governance.toml`.
///
/// ```
/// use origin_postedit::{detect_test_command, RepoMarkers};
///
/// let rust = RepoMarkers { cargo_toml: true, ..RepoMarkers::default() };
/// assert_eq!(detect_test_command(&rust).as_deref(), Some("cargo test"));
///
/// let node = RepoMarkers { package_json: true, ..RepoMarkers::default() };
/// assert_eq!(detect_test_command(&node).as_deref(), Some("npm test"));
///
/// assert_eq!(detect_test_command(&RepoMarkers::default()), None);
/// ```
#[must_use]
pub fn detect_test_command(markers: &RepoMarkers) -> Option<String> {
    let cmd = if markers.cargo_toml {
        // `--no-fail-fast` would be nicer for regression coverage, but plain
        // `cargo test` is the universally-understood default; overriders can
        // opt into flags.
        "cargo test"
    } else if markers.go_mod {
        "go test ./..."
    } else if markers.pyproject_toml || markers.python_legacy {
        // `pytest` discovers tests without config in the overwhelming majority
        // of Python repos and is what SWE-bench's Python tasks use.
        "pytest"
    } else if markers.package_json {
        "npm test"
    } else if markers.pom_xml {
        "mvn -q test"
    } else if markers.build_gradle {
        "gradle test"
    } else if markers.gemfile {
        "bundle exec rake test"
    } else if markers.makefile_test_target {
        "make test"
    } else {
        return None;
    };
    Some(cmd.to_string())
}

/// How to narrow a whole-repo test command to a set of impacted files.
///
/// Regression-test *selection* (SWE-bench proposal 2.2): rather than run the
/// entire suite on every gate check, scope it to the files an edit could break
/// (`{edited ∪ code-graph reverse-deps}`). This is pure string logic — the
/// caller supplies the impacted file set (from the code graph); this crate
/// decides how to express "test just these" for the detected runner.
///
/// It is deliberately conservative: it only narrows runners where a
/// path/package argument is unambiguous and safe, and returns [`Selection::Full`]
/// (run the base command unchanged) whenever narrowing could *miss* a failure —
/// a false "GREEN" is far worse than a slightly-too-broad run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Run the base command unchanged (narrowing unsafe or unsupported).
    Full,
    /// Run this narrowed command instead — covers the impacted files.
    Narrowed(String),
}

/// Narrow `base_command` to the impacted `files`, if the runner supports it.
///
/// `files` is the union of edited paths and their code-graph reverse-deps
/// (relative to the repo root, `/`-separated). `base_command` is the ecosystem
/// default (from [`detect_test_command`] or config). Recognised narrowings:
///
/// * **pytest** → append the impacted test files (only `test_*.py`/`*_test.py`;
///   a non-test edit contributes its *directory* so pytest still discovers the
///   tests beside it). Non-Python files are ignored.
/// * **go test** → replace `./...` with the distinct Go *package dirs* touched.
/// * **cargo test** — NOT narrowed by file (cargo selects by crate/test target,
///   not path; a path arg means something else). Returns [`Selection::Full`] so
///   correctness is preserved; crate-level selection is a caller concern.
///
/// Any unrecognised runner, an empty `files`, or a `files` set that would not
/// clearly cover the suite ⇒ [`Selection::Full`].
#[must_use]
pub fn select_tests(base_command: &str, files: &[String]) -> Selection {
    if files.is_empty() || !all_shell_safe(files) {
        return Selection::Full;
    }
    let base = base_command.trim();
    // pytest: append impacted test files / their dirs.
    if base == "pytest" || base.starts_with("pytest ") {
        let mut targets: Vec<String> = Vec::new();
        for f in files {
            // Extension check via the trailing component (avoids clippy's
            // case-sensitive `ends_with(".py")` lint and dotfile edge cases).
            if extension_of(f).as_deref() != Some("py") {
                continue;
            }
            let name = f.rsplit(['/', '\\']).next().unwrap_or(f);
            if is_test_name(name) {
                targets.push(f.clone());
            } else if let Some((dir, _)) = f.rsplit_once('/') {
                // A non-test edit: hand pytest the directory so it discovers the
                // sibling tests without us guessing the test file's name.
                targets.push(dir.to_string());
            }
        }
        targets.sort_unstable();
        targets.dedup();
        if targets.is_empty() {
            return Selection::Full;
        }
        // Unquoted: `all_shell_safe` admitted only plain path characters.
        return Selection::Narrowed(format!("{base} {}", targets.join(" ")));
    }
    // go test: swap the ./... wildcard for the touched package dirs.
    if base == "go test ./..." || base == "go test ./…" {
        let mut pkgs: Vec<String> = Vec::new();
        for f in files {
            if extension_of(f).as_deref() != Some("go") {
                continue;
            }
            let dir = f.rsplit_once('/').map_or(".", |(d, _)| d);
            pkgs.push(format!("./{}", dir.trim_start_matches("./")));
        }
        pkgs.sort_unstable();
        pkgs.dedup();
        if pkgs.is_empty() {
            return Selection::Full;
        }
        return Selection::Narrowed(format!("go test {}", pkgs.join(" ")));
    }
    // Everything else (cargo, npm, mvn, gradle, make, custom): don't risk a
    // path-based narrowing that could silently drop coverage.
    Selection::Full
}

/// C5: may every path go into a shell line unquoted? Only `[A-Za-z0-9_./-]`,
/// and no leading `-` (an option); a drive prefix (`C:`) is allowed, and on
/// Windows — where gates run in PowerShell, which gives `\` no meaning — so is
/// `\`. Anything else ⇒ the caller does not narrow (runs the full suite)
/// rather than quote for an unknown shell.
fn all_shell_safe(paths: &[String]) -> bool {
    paths.iter().all(|p| {
        let rest = match p.as_bytes() {
            [d, b':', ..] if cfg!(windows) && d.is_ascii_alphabetic() => &p[2..],
            _ => p.as_str(),
        };
        !rest.is_empty()
            && !p.starts_with('-')
            && rest.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '_' | '.' | '/' | '-')
                    || (cfg!(windows) && c == '\\')
            })
    })
}

/// Is the file name `name` a test by naming convention: `test_*`, `*_test`,
/// `tests` (any extension: pytest, Go's `_test.go`), or `*.test.*` /
/// `*.spec.*` (JS/TS)?
fn is_test_name(name: &str) -> bool {
    let stem = name.rsplit_once('.').map_or(name, |(s, _)| s);
    stem.starts_with("test_")
        || stem.ends_with("_test")
        || stem == "tests"
        || name.contains(".test.")
        || name.contains(".spec.")
}

/// C5: is the repo-relative `path` a test file?
///
/// Its name follows a test convention, or it sits under a
/// `tests`/`test`/`__tests__`/`spec` directory (Rust integration tests, JS
/// suites). Pass a path RELATIVE to the repo: an absolute prefix such as
/// `/home/u/test/` would read as a test directory.
// ponytail: naming conventions only — a test helper with a plain name outside a
// test dir (or an inline `#[cfg(test)]` module) reads as source; a per-runner
// collection query (`pytest --collect-only`) is the upgrade path.
#[must_use]
pub fn is_test_file(path: &str) -> bool {
    let mut parts = path.rsplit(['/', '\\']);
    let name = parts.next().unwrap_or(path);
    is_test_name(name) || parts.any(|d| matches!(d, "tests" | "test" | "__tests__" | "spec"))
}

/// C5: how a reproduction test behaved when run BEFORE the fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReproRun {
    /// It ran and a test failed — a real reproduction (RED).
    Failed,
    /// It passed: the test does not reproduce the bug.
    Passed,
    /// It never got to run a test (import/compile/syntax error, nothing collected).
    CollectionError,
    /// It failed, but this runner's signals cannot tell a test failure from a
    /// collection error.
    Unclear,
}

/// C5: classify a run of `command` that exited `exit_code` with `output`.
///
/// Exit 0 is [`ReproRun::Passed`] for any runner. A non-zero exit is told apart
/// only where the runner gives a signal: pytest by exit code (1 = tests failed;
/// 2 interrupted/collection error, 3 internal, 4 usage, 5 nothing collected);
/// `cargo test` by its `test result: FAILED` / `could not compile` lines;
/// `go test` by `--- FAIL` vs `[build failed]` / `[setup failed]`; for any
/// runner, jest's `Test suite failed to run` vs `Tests: N failed` and Maven's
/// `COMPILATION ERROR` vs `Failures: N` (N > 0). Anything else is
/// [`ReproRun::Unclear`].
#[must_use]
pub fn classify_repro_run(command: &str, exit_code: i32, output: &str) -> ReproRun {
    if exit_code == 0 {
        return ReproRun::Passed;
    }
    if command.contains("pytest") {
        return match exit_code {
            1 => ReproRun::Failed,
            2..=5 => ReproRun::CollectionError,
            _ => ReproRun::Unclear,
        };
    }
    if command.contains("cargo test") {
        if output.contains("could not compile") || output.contains("no test target named") {
            return ReproRun::CollectionError;
        }
        if output.contains("test result: FAILED") {
            return ReproRun::Failed;
        }
    }
    if command.contains("go test") {
        if output.contains("[build failed]") || output.contains("[setup failed]") {
            return ReproRun::CollectionError;
        }
        if output.contains("--- FAIL") {
            return ReproRun::Failed;
        }
    }
    // jest / Maven, whatever wrapper (`npm test`, `mvn -q test`) ran them.
    if output.contains("Test suite failed to run") || output.contains("COMPILATION ERROR") {
        return ReproRun::CollectionError;
    }
    let jest_failed = output
        .lines()
        .any(|l| l.trim_start().starts_with("Tests:") && l.contains(" failed"));
    let maven_failed = output
        .match_indices("Failures: ")
        .any(|(i, m)| output[i + m.len()..].starts_with(|c: char| ('1'..='9').contains(&c)));
    if jest_failed || maven_failed {
        return ReproRun::Failed;
    }
    ReproRun::Unclear
}

/// C5: `base` narrowed to just the reproduction test `files`.
///
/// Only where the runner takes test files: pytest (also `python -m pytest`)
/// gets the `.py` files among them themselves, so a file its default patterns
/// would skip still runs (a non-`.py` fixture is left out);
/// `cargo test` gets `--test <stem>` for each `tests/<stem>.rs`; `go test ./...`
/// gets the packages, as in [`select_tests`]. `None` ⇒ run the full command,
/// also when any path is not plain enough to put in a shell line unquoted.
#[must_use]
pub fn narrow_to_test_files(base: &str, files: &[String]) -> Option<String> {
    if !all_shell_safe(files) {
        return None;
    }
    let base = base.trim();
    let is_pytest = ["pytest", "python -m pytest", "python3 -m pytest"]
        .iter()
        .any(|p| base == *p || base.starts_with(&format!("{p} ")));
    if is_pytest {
        let py: Vec<&str> = files
            .iter()
            .map(String::as_str)
            .filter(|f| extension_of(f).as_deref() == Some("py"))
            .collect();
        return (!py.is_empty()).then(|| format!("{base} {}", py.join(" ")));
    }
    if (base == "cargo test" || base.starts_with("cargo test ")) && !base.contains(" -- ") {
        let targets: Option<Vec<String>> = files
            .iter()
            .map(|f| {
                let (dir, name) = f.rsplit_once('/')?;
                let stem = name.strip_suffix(".rs")?;
                (dir == "tests" || dir.ends_with("/tests")).then(|| format!("--test {stem}"))
            })
            .collect();
        return targets
            .filter(|t| !t.is_empty())
            .map(|t| format!("{base} {}", t.join(" ")));
    }
    match select_tests(base, files) {
        Selection::Narrowed(c) if base.starts_with("go test") => Some(c),
        _ => None,
    }
}

/// C5: does a full-suite failure `output` name the reproduction test?
///
/// It does when a line that reports a failure (contains `fail` or `error`,
/// any case) also contains the file stem `stem` or a function `source`
/// defines (`def`/`fn`/`func`/`void` names) — progress and `PASS` lines name
/// passing files too. Names shorter than 4 characters are ignored as too common.
// ponytail: substring match per line — a same-named test elsewhere failing, or
// a failure line that merely mentions the file, still matches; per-runner
// result parsing (JUnit XML) is the upgrade path.
#[must_use]
pub fn output_names_reproduction(output: &str, stem: &str, source: &str) -> bool {
    let mut tokens = source
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .filter(|t| !t.is_empty());
    let mut names = vec![stem];
    while let Some(t) = tokens.next() {
        if matches!(t, "def" | "fn" | "func" | "void") {
            names.extend(tokens.next());
        }
    }
    names.retain(|n| n.len() >= 4);
    output.lines().any(|l| {
        // Surefire prints `Failures: 0, Errors: 0` on every class's line, passing ones too.
        let lower = l
            .to_ascii_lowercase()
            .replace("failures: 0", "")
            .replace("errors: 0", "");
        (lower.contains("fail") || lower.contains("error")) && names.iter().any(|n| l.contains(n))
    })
}

/// Errors that can arise when constructing or validating a [`PostEditConfig`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PostEditError {
    /// An override entry carried an empty extension or empty command.
    #[error("invalid formatter override: extension and command must be non-empty")]
    EmptyOverride,
    /// `auto_lint` was requested without any resolvable lint command.
    #[error("auto_lint is enabled but no lint command is configured")]
    MissingLintCommand,
    /// `auto_test` was requested without any resolvable test command.
    #[error("auto_test is enabled but no test command is configured")]
    MissingTestCommand,
}

impl PostEditConfig {
    /// Validate the policy before the caller relies on it.
    ///
    /// # Errors
    ///
    /// Returns [`PostEditError::EmptyOverride`] if any override has an empty
    /// extension or command, [`PostEditError::MissingLintCommand`] if
    /// `auto_lint` is set without a `lint_command`, or
    /// [`PostEditError::MissingTestCommand`] if `auto_test` **or** `repro_gate`
    /// is set without a `test_command` (the repro gate has nothing to
    /// execution-check without one).
    pub fn validate(&self) -> Result<(), PostEditError> {
        if self
            .format_overrides
            .iter()
            .any(|(ext, cmd)| ext.trim().is_empty() || cmd.trim().is_empty())
        {
            return Err(PostEditError::EmptyOverride);
        }
        if self.auto_lint && self.lint_command.is_none() {
            return Err(PostEditError::MissingLintCommand);
        }
        if (self.auto_test || self.repro_gate) && self.test_command.is_none() {
            return Err(PostEditError::MissingTestCommand);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn builtin_table_has_enough_entries_and_no_dupes() {
        let table = builtin_formatters();
        assert!(table.len() >= 20, "expected >= 20 formatter rules");
        // Extensions must be unique so lookup is deterministic.
        for (i, a) in table.iter().enumerate() {
            assert_eq!(a.ext, a.ext.to_ascii_lowercase(), "ext must be lowercase");
            for b in &table[i + 1..] {
                assert_ne!(a.ext, b.ext, "duplicate extension {}", a.ext);
            }
        }
    }

    #[test]
    fn formatter_for_known_extensions() {
        assert_eq!(formatter_for("a.rs"), Some("rustfmt"));
        assert_eq!(formatter_for("main.go"), Some("gofmt"));
        assert_eq!(formatter_for("script.py"), Some("ruff format"));
        assert_eq!(formatter_for("toml/Cargo.toml"), Some("taplo fmt"));
        assert_eq!(formatter_for("a.rb"), Some("rubocop -a"));
    }

    #[test]
    fn prettier_handles_web_assets() {
        for path in [
            "a.ts", "b.tsx", "c.js", "d.jsx", "e.json", "f.css", "g.html", "h.md",
        ] {
            assert_eq!(formatter_for(path), Some("prettier"), "{path}");
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive_and_path_aware() {
        assert_eq!(formatter_for("SRC/Main.RS"), Some("rustfmt"));
        assert_eq!(formatter_for(r"C:\proj\App.TSX"), Some("prettier"));
        // Deepest extension wins on multi-dot names.
        assert_eq!(formatter_for("bundle.min.css"), Some("prettier"));
    }

    #[test]
    fn unknown_or_extensionless_paths_return_none() {
        assert_eq!(formatter_for("a.unknownext"), None);
        assert_eq!(formatter_for("Makefile"), None);
        assert_eq!(formatter_for(".gitignore"), None);
        assert_eq!(formatter_for("trailing."), None);
        assert_eq!(formatter_for(""), None);
    }

    #[test]
    fn override_beats_builtin_and_falls_through_otherwise() {
        let mut cfg = PostEditConfig::default();
        cfg.format_overrides
            .push(("rs".to_string(), "leptosfmt".to_string()));
        // Override wins for rs.
        assert_eq!(cfg.formatter_for("lib.rs").as_deref(), Some("leptosfmt"));
        // Case-insensitive override match.
        assert_eq!(cfg.formatter_for("LIB.RS").as_deref(), Some("leptosfmt"));
        // No override for go -> builtin.
        assert_eq!(cfg.formatter_for("main.go").as_deref(), Some("gofmt"));
        // No override, no builtin -> None.
        assert_eq!(cfg.formatter_for("a.unknownext"), None);
    }

    #[test]
    fn default_config_values() {
        let cfg = PostEditConfig::default();
        assert!(!cfg.auto_lint);
        assert!(!cfg.auto_test);
        assert_eq!(cfg.lint_command, None);
        assert_eq!(cfg.test_command, None);
        assert!(cfg.format_overrides.is_empty());
        assert_eq!(cfg.max_repair_iters, 2);
    }

    #[test]
    fn repair_decision_stops_on_no_failures() {
        let cfg = PostEditConfig::default();
        assert_eq!(repair_decision(0, 0, &cfg), RepairDecision::Stop);
        assert_eq!(repair_decision(0, 99, &cfg), RepairDecision::Stop);
    }

    #[test]
    fn repair_decision_retries_within_budget_then_gives_up() {
        let cfg = PostEditConfig::default(); // max = 2
        assert_eq!(repair_decision(3, 0, &cfg), RepairDecision::Retry { iter: 1 });
        assert_eq!(repair_decision(3, 1, &cfg), RepairDecision::Retry { iter: 2 });
        assert_eq!(repair_decision(3, 2, &cfg), RepairDecision::GiveUp);
        assert_eq!(repair_decision(3, 5, &cfg), RepairDecision::GiveUp);
    }

    #[test]
    fn repair_decision_respects_zero_budget() {
        let cfg = PostEditConfig {
            max_repair_iters: 0,
            ..PostEditConfig::default()
        };
        assert_eq!(repair_decision(0, 0, &cfg), RepairDecision::Stop);
        assert_eq!(repair_decision(1, 0, &cfg), RepairDecision::GiveUp);
    }

    #[test]
    fn validate_catches_bad_config() {
        assert!(PostEditConfig::default().validate().is_ok());

        let mut bad = PostEditConfig::default();
        bad.format_overrides.push((String::new(), "x".to_string()));
        assert_eq!(bad.validate(), Err(PostEditError::EmptyOverride));

        let lint = PostEditConfig {
            auto_lint: true,
            ..PostEditConfig::default()
        };
        assert_eq!(lint.validate(), Err(PostEditError::MissingLintCommand));

        let test = PostEditConfig {
            auto_test: true,
            ..PostEditConfig::default()
        };
        assert_eq!(test.validate(), Err(PostEditError::MissingTestCommand));

        let good = PostEditConfig {
            auto_lint: true,
            lint_command: Some("cargo clippy".to_string()),
            auto_test: true,
            test_command: Some("cargo test".to_string()),
            ..PostEditConfig::default()
        };
        assert!(good.validate().is_ok());
    }

    #[test]
    fn serde_round_trips_config() {
        let cfg = PostEditConfig {
            auto_lint: true,
            lint_command: Some("ruff check".to_string()),
            format_overrides: vec![("rs".to_string(), "rustfmt --edition 2021".to_string())],
            max_repair_iters: 5,
            ..PostEditConfig::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: PostEditConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn repro_gate_requires_a_test_command() {
        // The repro gate execution-checks a fail→pass transition, so it needs a
        // command to run — validate must reject it without one, like auto_test.
        let no_cmd = PostEditConfig {
            repro_gate: true,
            ..PostEditConfig::default()
        };
        assert_eq!(no_cmd.validate(), Err(PostEditError::MissingTestCommand));

        let ok = PostEditConfig {
            repro_gate: true,
            test_command: Some("cargo test".to_string()),
            ..PostEditConfig::default()
        };
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn test_gate_armed_predicate() {
        // Neither flag ⇒ not armed.
        assert!(!PostEditConfig::default().test_gate_armed());
        // auto_test but no command ⇒ nothing to run ⇒ not armed.
        assert!(!PostEditConfig {
            auto_test: true,
            ..PostEditConfig::default()
        }
        .test_gate_armed());
        // repro_gate + command ⇒ armed.
        assert!(PostEditConfig {
            repro_gate: true,
            test_command: Some("pytest".to_string()),
            ..PostEditConfig::default()
        }
        .test_gate_armed());
        // auto_test + command ⇒ armed.
        assert!(PostEditConfig {
            auto_test: true,
            test_command: Some("cargo test".to_string()),
            ..PostEditConfig::default()
        }
        .test_gate_armed());
    }

    #[test]
    fn detect_test_command_precedence() {
        // Empty ⇒ None (never guess).
        assert_eq!(detect_test_command(&RepoMarkers::default()), None);
        // Each ecosystem's canonical command.
        assert_eq!(
            detect_test_command(&RepoMarkers {
                cargo_toml: true,
                ..RepoMarkers::default()
            })
            .as_deref(),
            Some("cargo test")
        );
        assert_eq!(
            detect_test_command(&RepoMarkers {
                go_mod: true,
                ..RepoMarkers::default()
            })
            .as_deref(),
            Some("go test ./...")
        );
        assert_eq!(
            detect_test_command(&RepoMarkers {
                pyproject_toml: true,
                ..RepoMarkers::default()
            })
            .as_deref(),
            Some("pytest")
        );
        assert_eq!(
            detect_test_command(&RepoMarkers {
                python_legacy: true,
                ..RepoMarkers::default()
            })
            .as_deref(),
            Some("pytest")
        );
        assert_eq!(
            detect_test_command(&RepoMarkers {
                package_json: true,
                ..RepoMarkers::default()
            })
            .as_deref(),
            Some("npm test")
        );
        assert_eq!(
            detect_test_command(&RepoMarkers {
                makefile_test_target: true,
                ..RepoMarkers::default()
            })
            .as_deref(),
            Some("make test")
        );
    }

    #[test]
    fn detect_test_command_rust_wins_over_generic_wrappers() {
        // A Rust repo that also ships a Makefile/package.json still tests via
        // cargo — language toolchains that own the repo win over wrappers.
        let mixed = RepoMarkers {
            cargo_toml: true,
            package_json: true,
            makefile_test_target: true,
            ..RepoMarkers::default()
        };
        assert_eq!(detect_test_command(&mixed).as_deref(), Some("cargo test"));
    }

    #[test]
    fn detect_test_command_python_project_beats_makefile() {
        let py = RepoMarkers {
            pyproject_toml: true,
            makefile_test_target: true,
            ..RepoMarkers::default()
        };
        assert_eq!(detect_test_command(&py).as_deref(), Some("pytest"));
    }

    #[test]
    fn select_tests_empty_is_full() {
        assert_eq!(select_tests("pytest", &[]), Selection::Full);
    }

    #[test]
    fn select_tests_pytest_appends_test_files_and_dirs() {
        let files = vec![
            "pkg/mod.py".to_string(),            // non-test ⇒ contributes its dir
            "pkg/tests/test_mod.py".to_string(), // test ⇒ contributes the file
            "README.md".to_string(),             // ignored (not .py)
        ];
        match select_tests("pytest", &files) {
            Selection::Narrowed(cmd) => {
                assert!(cmd.starts_with("pytest "));
                assert!(cmd.contains("pkg"), "dir of the non-test edit: {cmd}");
                assert!(cmd.contains("pkg/tests/test_mod.py"), "the test file: {cmd}");
                assert!(!cmd.contains("README"), "non-py excluded: {cmd}");
            }
            Selection::Full => panic!("expected a narrowed pytest command"),
        }
    }

    #[test]
    fn select_tests_pytest_preserves_base_flags() {
        let files = vec!["a/test_x.py".to_string()];
        match select_tests("pytest -q --no-header", &files) {
            Selection::Narrowed(cmd) => {
                assert!(cmd.starts_with("pytest -q --no-header "), "flags kept: {cmd}");
                assert!(cmd.ends_with("a/test_x.py"));
            }
            Selection::Full => panic!("expected narrowed"),
        }
    }

    #[test]
    fn select_tests_pytest_no_python_files_is_full() {
        // Only non-Python edits ⇒ we can't scope pytest ⇒ run the full suite.
        let files = vec!["src/main.rs".to_string(), "go.mod".to_string()];
        assert_eq!(select_tests("pytest", &files), Selection::Full);
    }

    #[test]
    fn select_tests_go_swaps_wildcard_for_packages() {
        let files = vec![
            "internal/foo/foo.go".to_string(),
            "internal/foo/bar.go".to_string(), // same pkg ⇒ deduped
            "cmd/app/main.go".to_string(),
            "docs/x.md".to_string(), // ignored
        ];
        match select_tests("go test ./...", &files) {
            Selection::Narrowed(cmd) => {
                assert!(cmd.starts_with("go test "));
                assert!(cmd.contains("./internal/foo"), "{cmd}");
                assert!(cmd.contains("./cmd/app"), "{cmd}");
                // Dedup: `internal/foo` appears once.
                assert_eq!(cmd.matches("./internal/foo").count(), 1, "{cmd}");
                assert!(!cmd.contains("docs"), "{cmd}");
            }
            Selection::Full => panic!("expected narrowed go command"),
        }
    }

    #[test]
    fn select_tests_cargo_and_npm_stay_full() {
        // Cargo selects by target/crate, not path — never narrow by file.
        let files = vec!["crates/x/src/lib.rs".to_string()];
        assert_eq!(select_tests("cargo test", &files), Selection::Full);
        assert_eq!(select_tests("npm test", &files), Selection::Full);
        assert_eq!(select_tests("make test", &files), Selection::Full);
    }

    #[test]
    fn is_test_file_by_name_and_directory() {
        for t in [
            "test_bug.py",
            "pkg/bug_test.py",
            "tests.py",
            "x/foo_test.go",
            "web/foo.test.ts",
            "web/foo.spec.js",
            "tests/repro.rs",
            "src/__tests__/a.js",
            "test/helper.rb",
        ] {
            assert!(is_test_file(t), "{t}");
        }
        for s in [
            "src/lib.rs",
            "pkg/mod.py",
            "contest.py",
            "latest.go",
            "testing/util.py",
        ] {
            assert!(!is_test_file(s), "{s}");
        }
    }

    #[test]
    fn classify_repro_run_by_runner_signals() {
        use ReproRun::{CollectionError, Failed, Passed, Unclear};
        assert_eq!(classify_repro_run("npm test", 0, ""), Passed);
        assert_eq!(classify_repro_run("pytest -q", 1, "1 failed"), Failed);
        for code in 2..=5 {
            assert_eq!(
                classify_repro_run("python -m pytest", code, ""),
                CollectionError,
                "{code}"
            );
        }
        assert_eq!(
            classify_repro_run("cargo test", 101, "test result: FAILED. 0 passed; 1 failed"),
            Failed
        );
        assert_eq!(
            classify_repro_run("cargo test", 101, "error: could not compile `x`"),
            CollectionError
        );
        assert_eq!(
            classify_repro_run("go test ./...", 1, "--- FAIL: TestBug"),
            Failed
        );
        assert_eq!(
            classify_repro_run("go test ./...", 1, "FAIL\tx [build failed]"),
            CollectionError
        );
        assert_eq!(classify_repro_run("npm test", 1, "1 failing"), Unclear);
        assert_eq!(classify_repro_run("cargo test", 101, "killed"), Unclear);
        assert_eq!(
            classify_repro_run(
                "cargo test --test repro",
                101,
                "error: no test target named `repro`"
            ),
            CollectionError
        );
        assert_eq!(
            classify_repro_run("npm test", 1, "FAIL a.test.js\n  ● Test suite failed to run"),
            CollectionError
        );
        assert_eq!(
            classify_repro_run("npm test", 1, "Tests:       1 failed, 3 passed, 4 total"),
            Failed
        );
        assert_eq!(
            classify_repro_run("mvn -q test", 1, "[ERROR] COMPILATION ERROR :"),
            CollectionError
        );
        assert_eq!(
            classify_repro_run("mvn -q test", 1, "Tests run: 4, Failures: 1, Errors: 0"),
            Failed
        );
        assert_eq!(
            classify_repro_run("mvn -q test", 1, "Tests run: 4, Failures: 0, Errors: 1"),
            Unclear
        );
    }

    #[test]
    fn narrow_to_test_files_per_runner() {
        let f = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            narrow_to_test_files("pytest -q", &f(&["tests/repro_bug.py"])).as_deref(),
            Some("pytest -q tests/repro_bug.py"),
            "the file itself, even one pytest's default patterns skip"
        );
        assert_eq!(
            narrow_to_test_files("python -m pytest", &f(&["t/x.py"])).as_deref(),
            Some("python -m pytest t/x.py")
        );
        assert_eq!(
            narrow_to_test_files("cargo test -p foo", &f(&["crates/foo/tests/repro.rs"])).as_deref(),
            Some("cargo test -p foo --test repro")
        );
        assert_eq!(narrow_to_test_files("cargo test", &f(&["src/lib.rs"])), None);
        assert_eq!(
            narrow_to_test_files("cargo test -- --nocapture", &f(&["tests/r.rs"])),
            None
        );
        assert_eq!(
            narrow_to_test_files("go test ./...", &f(&["pkg/a_test.go"])).as_deref(),
            Some("go test ./pkg")
        );
        assert_eq!(narrow_to_test_files("npm test", &f(&["a.test.js"])), None);
        assert_eq!(
            narrow_to_test_files("pytest", &f(&["tests/test_a.py", "tests/data.json"])).as_deref(),
            Some("pytest tests/test_a.py"),
            "the .py subset; a fixture does not switch narrowing off"
        );
    }

    #[test]
    fn narrowing_refuses_paths_that_are_not_plain() {
        let f = |p: &str| vec![p.to_string()];
        for bad in [
            "tests/a; touch x #.py",
            "tests/a b.py",
            "tests/$(id).py",
            "tests/a`b.py",
            "-k.py",
        ] {
            assert_eq!(narrow_to_test_files("pytest", &f(bad)), None, "{bad}");
            assert_eq!(select_tests("pytest", &f(bad)), Selection::Full, "{bad}");
        }
        assert!(narrow_to_test_files("pytest", &f("tests/test_ok-1.py")).is_some());
        assert_eq!(
            narrow_to_test_files("pytest", &f("C:/w/tests/test_a.py")).is_some(),
            cfg!(windows),
            "a drive prefix is plain on Windows only"
        );
        assert_eq!(narrow_to_test_files("pytest", &f("C:")), None);
        assert!(matches!(
            select_tests("pytest", &f("tests/test_ok-1.py")),
            Selection::Narrowed(_)
        ));
    }

    #[test]
    fn output_names_reproduction_by_stem_or_defined_name() {
        let src = "def test_slug_keeps_dashes():\n    assert slug('a-b') == 'a-b'\n";
        let names = |out: &str| output_names_reproduction(out, "repro_slug", src);
        assert!(names("FAILED x::test_slug_keeps_dashes"));
        assert!(names("FAIL tests/repro_slug.py"));
        assert!(!names("FAILED test_other.py::test_old"));
        // A progress/PASS line names a passing file: that is no attribution.
        assert!(!names(
            "tests/repro_slug.py .                [ 50%]\nFAILED tests/test_other.py::test_old"
        ));
        assert!(!names("PASS tests/repro_slug.test.js\nFAIL tests/other.test.js"));
        // Surefire prints a result line for every class, passing ones included.
        let surefire = |repro: &str| {
            format!(
                "Tests run: 1, Failures: {repro}, Errors: 0, Skipped: 0 - in com.x.ReproSlugTest\n\
                 Tests run: 2, Failures: 1, Errors: 0, Skipped: 0 <<< FAILURE! - in com.x.OtherTest"
            )
        };
        let class = |out: &str| output_names_reproduction(out, "ReproSlugTest", "");
        assert!(!class(&surefire("0")), "the reproduction class passed");
        assert!(class(&surefire("1")), "the reproduction class failed");
        // Short names are too common to count.
        assert!(!output_names_reproduction("fn ab failed", "ab", "fn ab() {}"));
    }
}
