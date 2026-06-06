// SPDX-License-Identifier: Apache-2.0
//! `Language` enum + tree-sitter parser bindings.

use std::path::Path;

use thiserror::Error;
use tree_sitter::{Parser as TsParser, Tree};

/// Supported source languages.
///
/// The original five shipped in Phase 7; the extended set (PHP … Lua) lands
/// later to reach parity with the `origin-repomap` heuristic scanner. Variant
/// order is load-bearing: the `as_discriminant` mapping in `record.rs` keys the
/// SQL `language` column off it and MUST stay stable, so new variants are
/// appended, never interleaved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    C,
    Cpp,
    CSharp,
    Ruby,
    Bash,
    // --- Extended grammars (codegraph ⇄ repomap parity) ---
    Php,
    Swift,
    Kotlin,
    Scala,
    Haskell,
    Elixir,
    Lua,
}

/// Errors produced when configuring or running a tree-sitter parser.
// `LangError` is the public error type for this module; the `Lang` prefix
// disambiguates against `ExtractError` and matches the Phase 7 plan API.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum LangError {
    #[error("tree-sitter failed to set language for {0:?}")]
    SetLanguage(Language),
    #[error("tree-sitter returned no tree (likely empty input)")]
    Empty,
}

impl Language {
    /// Detect a [`Language`] from a file extension (without the leading dot).
    ///
    /// The match is case-insensitive. Returns `None` for any extension that
    /// does not correspond to one of the supported grammars (callers fall back
    /// to the `origin-repomap` heuristic scanner for those).
    #[must_use]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_ascii_lowercase().as_str() {
            "rs" => Some(Self::Rust),
            "ts" | "tsx" | "mts" | "cts" => Some(Self::TypeScript),
            "py" | "pyi" => Some(Self::Python),
            "go" => Some(Self::Go),
            "java" => Some(Self::Java),
            // `.h` is ambiguous (C or C++); route it to the C grammar, which
            // parses the bulk of header content acceptably.
            "c" | "h" => Some(Self::C),
            "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => Some(Self::Cpp),
            "cs" => Some(Self::CSharp),
            "rb" => Some(Self::Ruby),
            "sh" | "bash" => Some(Self::Bash),
            // --- Extended grammars (codegraph ⇄ repomap parity) ---
            "php" => Some(Self::Php),
            "swift" => Some(Self::Swift),
            "kt" | "kts" => Some(Self::Kotlin),
            "scala" | "sc" => Some(Self::Scala),
            "hs" => Some(Self::Haskell),
            "ex" | "exs" => Some(Self::Elixir),
            "lua" => Some(Self::Lua),
            _ => None,
        }
    }

    /// Detect a [`Language`] from a file path by inspecting its extension.
    ///
    /// Delegates to [`Language::from_extension`]; returns `None` when the path
    /// has no extension or the extension is not recognised.
    #[must_use]
    pub fn from_path(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|ext| ext.to_str())
            .and_then(Self::from_extension)
    }

    /// Map this `Language` to its tree-sitter grammar.
    #[must_use]
    pub fn ts_language(self) -> tree_sitter::Language {
        match self {
            Self::Rust => tree_sitter_rust::language(),
            Self::TypeScript => tree_sitter_typescript::language_typescript(),
            Self::Python => tree_sitter_python::language(),
            Self::Go => tree_sitter_go::language(),
            Self::Java => tree_sitter_java::language(),
            Self::C => tree_sitter_c::language(),
            Self::Cpp => tree_sitter_cpp::language(),
            Self::CSharp => tree_sitter_c_sharp::language(),
            Self::Ruby => tree_sitter_ruby::language(),
            Self::Bash => tree_sitter_bash::language(),
            Self::Php => tree_sitter_php::language_php(),
            Self::Swift => tree_sitter_swift::language(),
            Self::Kotlin => tree_sitter_kotlin::language(),
            Self::Scala => tree_sitter_scala::language(),
            Self::Haskell => tree_sitter_haskell::language(),
            Self::Elixir => tree_sitter_elixir::language(),
            Self::Lua => tree_sitter_lua::language(),
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
