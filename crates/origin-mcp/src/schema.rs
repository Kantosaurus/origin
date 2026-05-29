// SPDX-License-Identifier: Apache-2.0
//! Validate MCP `tools/call` arguments against the tool's registered
//! `input_schema` before the request is sent. Compiled schemas live in a
//! per-server cache keyed by tool name.
//!
//! P11.8 — implements N10.13's schema-validation half. The 16 MiB cap on
//! inbound responses is the other half (see [`crate::limits`]).

use std::collections::HashMap;
use std::sync::RwLock;

use jsonschema::JSONSchema;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("schema compile: {0}")]
    Compile(String),
    #[error("invalid args: {0}")]
    Invalid(String),
}

/// Per-server cache of compiled JSON Schemas keyed by tool name.
// `SchemaCache` repeats the module name `schema`; suppressed so callers can
// write `origin_mcp::SchemaCache` without disambiguating which module's cache.
#[allow(clippy::module_name_repetitions)]
#[derive(Default)]
pub struct SchemaCache {
    inner: RwLock<HashMap<String, JSONSchema>>,
}

impl std::fmt::Debug for SchemaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let len = self.inner.read().map(|g| g.len()).unwrap_or(0);
        f.debug_struct("SchemaCache").field("len", &len).finish()
    }
}

impl SchemaCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compile `schema` for `tool` and store it in the cache.
    ///
    /// # Errors
    /// Returns [`ValidationError::Compile`] if `schema` is not a valid JSON
    /// Schema document.
    ///
    /// # Panics
    /// Panics only if the internal `RwLock` is poisoned by a prior panic in a
    /// different thread — recoverable callers should treat that as fatal.
    pub fn register(&self, tool: &str, schema: &Value) -> Result<(), ValidationError> {
        let compiled = JSONSchema::options()
            .compile(schema)
            .map_err(|e| ValidationError::Compile(e.to_string()))?;
        {
            let mut guard = self.inner.write().expect("schema cache write lock poisoned");
            guard.insert(tool.to_string(), compiled);
        }
        Ok(())
    }

    /// Validate `args` against the schema registered for `tool`.
    ///
    /// Returns `Ok(())` if no schema is registered for `tool` — the daemon's
    /// `list_tools` refresh is responsible for population. Treating unknown
    /// tools as pass-through avoids breaking servers whose JSON Schemas don't
    /// round-trip through `jsonschema`.
    ///
    /// # Errors
    /// Returns [`ValidationError::Invalid`] when `args` violates the schema.
    ///
    /// # Panics
    /// Panics only if the internal `RwLock` is poisoned.
    pub fn validate(&self, tool: &str, args: &Value) -> Result<(), ValidationError> {
        // Collect the validation outcome inside the lock scope so the read
        // guard is dropped as early as possible (clippy::significant_drop_tightening).
        let outcome: Result<(), Vec<String>> = {
            let guard = self.inner.read().expect("schema cache read lock poisoned");
            guard
                .get(tool)
                .map_or(Ok(()), |schema| match schema.validate(args) {
                    Ok(()) => Ok(()),
                    Err(errors) => Err(errors.map(|e| format!("{e}")).collect()),
                })
        };
        outcome.map_err(|errs| ValidationError::Invalid(errs.join("; ")))
    }

    /// Forget `tool`'s schema, e.g. when the MCP server disappears.
    ///
    /// # Panics
    /// Panics only if the internal `RwLock` is poisoned.
    pub fn forget(&self, tool: &str) {
        let mut guard = self.inner.write().expect("schema cache write lock poisoned");
        let _ = guard.remove(tool);
    }

    /// Number of registered schemas. Useful for tests + observability.
    ///
    /// # Panics
    /// Panics only if the internal `RwLock` is poisoned.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().expect("schema cache read lock poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
