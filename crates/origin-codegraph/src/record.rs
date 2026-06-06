// SPDX-License-Identifier: Apache-2.0
//! CAS-archived records for code-graph nodes and edges (P7.3).
//!
//! `CodeNodeRecord` is the in-memory shape used by callers when inserting
//! into the graph. The actual on-disk representation splits into:
//! - a small `SQLite` row (kind, name, language, file path, byte range,
//!   CAS handles for signature & body, last-seen epoch ms)
//! - CAS-stored signature & body bytes (deduplicated content-addressed)
//!
//! `Confidence` is rkyv-archived so it can ride the same byte buffers as
//! `Evidence` records (P7.4+).

use rkyv::{Archive, Deserialize, Serialize};

use crate::extract::{NodeKind, Range};
use crate::lang::Language;

/// Quality tag attached to an inferred edge. `Extracted` ⇒ derived directly
/// from the AST; `Inferred` ⇒ heuristic match (e.g. unresolved call by name);
/// `Ambiguous` ⇒ multiple candidate resolutions.
#[derive(Archive, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[archive(check_bytes)]
pub enum Confidence {
    Extracted,
    Inferred,
    Ambiguous,
}

/// Error returned by [`Confidence::from_str`] when the input is not one of the
/// canonical lowercase tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseConfidenceError;

impl core::fmt::Display for ParseConfidenceError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("expected one of: extracted, inferred, ambiguous")
    }
}

impl std::error::Error for ParseConfidenceError {}

impl Confidence {
    /// Canonical lowercase tag used in the `confidence` SQL column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extracted => "extracted",
            Self::Inferred => "inferred",
            Self::Ambiguous => "ambiguous",
        }
    }

    /// Parse from the lowercase tag stored in `SQLite`.
    ///
    /// # Errors
    /// Returns [`ParseConfidenceError`] when `s` is not one of `extracted`,
    /// `inferred`, or `ambiguous`.
    // Implementing `std::str::FromStr` would force an `Err = ParseConfidenceError`
    // bound that ripples to `str::parse` callers; we keep the inherent method so
    // the entire confidence vocabulary stays local to this module.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Result<Self, ParseConfidenceError> {
        match s {
            "extracted" => Ok(Self::Extracted),
            "inferred" => Ok(Self::Inferred),
            "ambiguous" => Ok(Self::Ambiguous),
            _ => Err(ParseConfidenceError),
        }
    }
}

impl NodeKind {
    /// Canonical lowercase tag used in the `kind` SQL column.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::Struct => "struct",
            Self::Class => "class",
            Self::Trait => "trait",
            Self::Interface => "interface",
            Self::Module => "module",
        }
    }
}

impl Language {
    /// Stable integer discriminant for the `language` SQL column. The order is
    /// fixed by Phase 7 and MUST NOT change without a follow-up migration.
    ///
    /// Slots 5–9 are reserved for the C/C++/C#/Ruby/Bash grammars landed on a
    /// parallel branch; the extended grammars added here therefore start at 10
    /// so the two sets never collide when both are present in the same row
    /// space. Existing rows keep their discriminant; this is purely additive.
    #[must_use]
    pub const fn as_discriminant(self) -> i64 {
        match self {
            Self::Rust => 0,
            Self::TypeScript => 1,
            Self::Python => 2,
            Self::Go => 3,
            Self::Java => 4,
            // Appended for the curated grammar additions; never interleave the
            // above (the discriminant is a persisted SQL contract).
            Self::C => 5,
            Self::Cpp => 6,
            Self::CSharp => 7,
            Self::Ruby => 8,
            Self::Bash => 9,
            // Extended grammars (codegraph ⇄ repomap parity); appended at 10+.
            Self::Php => 10,
            Self::Swift => 11,
            Self::Kotlin => 12,
            Self::Scala => 13,
            Self::Haskell => 14,
            Self::Elixir => 15,
            Self::Lua => 16,
        }
    }
}

/// In-memory shape callers hand to [`crate::index::CodeGraphIndex::insert_node`].
// `CodeNodeRecord` matches the Phase 7 plan's public API; the `Record` suffix
// is intentional and disambiguates against the lighter [`crate::CodeNode`]
// extraction stub.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct CodeNodeRecord {
    pub kind: NodeKind,
    pub name: String,
    pub language: Language,
    pub file_path: String,
    pub range: Range,
    /// Stable signature bytes (typically the syntactic header). Deduplicated
    /// across files via CAS — two declarations with the same signature share a
    /// handle even when their bodies differ.
    pub signature: Vec<u8>,
    /// Body bytes (whole node). Also CAS-stored.
    pub body: Vec<u8>,
}
