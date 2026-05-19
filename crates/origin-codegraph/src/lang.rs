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
    /// Map this `Language` to its tree-sitter grammar.
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
