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
    let file = path
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())?;
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
    /// [`PostEditError::MissingTestCommand`] if `auto_test` is set without a
    /// `test_command`.
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
        if self.auto_test && self.test_command.is_none() {
            return Err(PostEditError::MissingTestCommand);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
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
        for path in ["a.ts", "b.tsx", "c.js", "d.jsx", "e.json", "f.css", "g.html", "h.md"] {
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
}
