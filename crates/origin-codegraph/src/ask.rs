// SPDX-License-Identifier: Apache-2.0
//! Sub-millisecond classifier + `MemRouter` trait (Phase 6 will implement).
//!
//! The router is intentionally lexical-only: zero LLM hops, two precompiled
//! regexes, one truth-table. Phase 7 ships the classifier and a [`NullMemRouter`]
//! that always returns no hits; the live [`MemRouter`] backed by Phase 6
//! sleep-time memory lands later.

use std::sync::OnceLock;

use regex::Regex;

/// Routing decision produced by [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Code-graph query only.
    Code,
    /// Memory store query only.
    Mem,
    /// Fan out to both — caller merges the hits.
    Both,
}

/// One memory hit returned by a [`MemRouter`] implementation.
#[derive(Debug, Clone)]
pub struct MemHit {
    pub id: String,
    pub score: f32,
    pub body: String,
}

/// Result of an `ask` dispatch: which backend was chosen plus any memory hits.
///
/// Code-side hits will fold in here in Phase 6 when free-text → typed-Query
/// translation lands; for P7.8 we carry only the classifier decision and the
/// memory hits returned by the supplied [`MemRouter`].
// `AskResult` is the plan-mandated public name; the `Ask` prefix matches the
// surrounding `Route`/`MemHit`/`MemRouter` API and disambiguates against
// `std::result::Result` at call sites.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct AskResult {
    pub route: Route,
    pub mem_hits: Vec<MemHit>,
}

impl AskResult {
    /// Construct a result from its parts.
    #[must_use]
    pub const fn new(route: Route, mem_hits: Vec<MemHit>) -> Self {
        Self { route, mem_hits }
    }
}

/// Pluggable memory search backend. Phase 7 ships only [`NullMemRouter`];
/// Phase 6's sleep-time memory implements this trait against its own store.
pub trait MemRouter: Send + Sync {
    /// Look up hits for `query`. Implementations should treat `query` as a
    /// raw natural-language string and return at most a bounded set of hits.
    fn search(&self, query: &str) -> Vec<MemHit>;
}

/// No-op [`MemRouter`]; always returns an empty hit list.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullMemRouter;

impl MemRouter for NullMemRouter {
    fn search(&self, _query: &str) -> Vec<MemHit> {
        Vec::new()
    }
}

fn code_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(\bfn\b|\bfunction\b|\bdef\b|\bclass\b|\bstruct\b|\btrait\b|\binterface\b|\bimpl(?:ements)?\b|\bcaller(?:s)?\b|\bcallee(?:s)?\b|`[a-z_][a-z0-9_]*`|\b[a-z]+(?:_[a-z0-9]+)+\b)",
        )
        .expect("static regex")
    })
}

fn mem_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(\bremember\b|\bearlier\b|\byesterday\b|\blast week\b|\bdiscussed\b|\bwe (decided|agreed|talked)\b|\bi told you\b|\bnote(d)?\b)",
        )
        .expect("static regex")
    })
}

/// Classify `query` into a [`Route`] in O(query length).
///
/// The classifier is purely lexical — see the module docs. The truth table
/// defaults to [`Route::Code`] when neither family matches, on the theory that
/// the code graph is the cheaper backend and a miss there is a fast no-op.
#[must_use]
pub fn classify(query: &str) -> Route {
    let lowered = query.to_lowercase();
    let code = code_re().is_match(&lowered);
    let mem = mem_re().is_match(&lowered);
    match (code, mem) {
        (true, true) => Route::Both,
        (false, true) => Route::Mem,
        // (true, false) → Code; (false, false) → Code (sensible default).
        _ => Route::Code,
    }
}
