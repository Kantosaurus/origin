// SPDX-License-Identifier: Apache-2.0
//! C9: redact the harness's own credentials by VALUE.
//!
//! At daemon start [`collect`] reads the values of credential-named env vars
//! (`origin_tools::is_redactable_secret_env_name`) and of the keyvault entries
//! the provider factory reads, and [`install`] makes them process-wide. Every
//! exact occurrence is then replaced with `[REDACTED:<NAME>]` where a tool
//! result is captured (agent loop, before CAS/TUI/transcript), in turn-end gate
//! output, and where a message is persisted (`SessionStore`). Values shorter
//! than [`MIN_LEN`] or blank are ignored. Nothing installed ⇒ every helper is a
//! no-op. File-mutating tools refuse content carrying an installed marker
//! ([`installed_marker_in`]), so a redacted value is never written back to disk.
//!
//! Not covered: keys added at runtime (`KeyringSet`), OAuth tokens rotated
//! after startup, accounts other than the startup account, a secret that
//! appears multi-line or JSON-escaped (exact-value match only), and a secret cut
//! in half by output truncation (the surviving fragment does not match).

use std::borrow::Cow;
use std::sync::OnceLock;

use origin_core::types::{Block, Message};

/// Values shorter than this are never redacted (too likely to collide with
/// ordinary text).
pub const MIN_LEN: usize = 8;

/// Exact-value matcher, built once.
#[derive(Default)]
pub struct Redactor {
    /// `(value, "[REDACTED:<NAME>]")`, longest value first so a value that
    /// contains another is replaced whole.
    pairs: Vec<(String, String)>,
}

/// Prints only the markers (names), never a value.
impl std::fmt::Debug for Redactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.pairs.iter().map(|(_, m)| m)).finish()
    }
}

impl Redactor {
    /// Build from `(name, value)` pairs; values shorter than [`MIN_LEN`] or
    /// blank after trimming are dropped.
    #[must_use]
    pub fn new(secrets: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut pairs: Vec<(String, String)> = secrets
            .into_iter()
            .filter(|(_, v)| v.len() >= MIN_LEN && !v.trim().is_empty())
            .map(|(n, v)| (v, format!("[REDACTED:{n}]")))
            .collect();
        // Stable: on equal values the first-collected name (env before vault) wins.
        pairs.sort_by_key(|p| std::cmp::Reverse(p.0.len()));
        Self { pairs }
    }

    /// Replace every exact occurrence of a known value. Borrowed when clean.
    #[must_use]
    pub fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        // ponytail: one linear scan per secret (k = a handful of keys); switch to
        // aho-corasick if the secret count ever grows large.
        for (value, marker) in &self.pairs {
            if out.contains(value.as_str()) {
                out = Cow::Owned(out.replace(value.as_str(), marker));
            }
        }
        out
    }
}

static ACTIVE: OnceLock<Redactor> = OnceLock::new();

/// Install the process-wide redactor. First call wins (values do not change
/// during a run).
pub fn install(r: Redactor) {
    let _ = ACTIVE.set(r);
}

fn owned(c: Cow<'_, str>) -> Option<String> {
    match c {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => None,
    }
}

/// The first installed `[REDACTED:<NAME>]` marker found in `s`, if any. A
/// marker for a name that is not installed is ordinary text (`None`).
#[must_use]
pub fn installed_marker_in(s: &str) -> Option<&'static str> {
    ACTIVE
        .get()?
        .pairs
        .iter()
        .map(|(_, m)| m.as_str())
        .find(|m| s.contains(m))
}

/// Redact `s` with the installed redactor.
#[must_use]
pub fn text(s: &str) -> Cow<'_, str> {
    ACTIVE.get().map_or(Cow::Borrowed(s), |r| r.redact(s))
}

/// Redact a tool-result body. Non-UTF-8 bytes are matched on their lossy
/// decoding (what a provider would send) and only rewritten on a hit.
#[must_use]
pub fn bytes(b: Vec<u8>) -> Vec<u8> {
    let hit = std::str::from_utf8(&b)
        .map_or_else(|_| owned(text(&String::from_utf8_lossy(&b))), |s| owned(text(s)));
    hit.map_or(b, String::into_bytes)
}

/// Redact an owned string; returns it unchanged (no copy) when clean.
#[must_use]
pub fn string(s: String) -> String {
    owned(text(&s)).unwrap_or(s)
}

fn string_in_place(s: &mut String) {
    if let Some(r) = owned(text(s)) {
        *s = r;
    }
}

/// Redact a message for persistence.
///
/// Covers text, tool-use input and inline tool results. `Thinking` is left
/// alone: it is signed (editing it would break the signature on resume), and
/// the model never saw a raw key anyway.
#[must_use]
pub fn message(m: &Message) -> Cow<'_, Message> {
    if ACTIVE.get().is_none_or(|r| r.pairs.is_empty()) {
        return Cow::Borrowed(m);
    }
    let mut m = m.clone();
    for b in &mut m.blocks {
        match b {
            Block::Text { text, .. } => string_in_place(text),
            Block::ToolUse { input_json: v, .. } | Block::ToolResult { inline: Some(v), .. } => {
                *v = bytes(std::mem::take(v));
            }
            Block::ToolResult { inline: None, .. } | Block::Thinking { .. } => {}
        }
    }
    Cow::Owned(m)
}

/// Collect the secret values to redact.
///
/// Every credential-named env var (`*_API_KEY`, `*_TOKEN`, …), plus the keyvault credential of each catalog
/// provider for `account` (the raw key, or the `access`/`refresh`/`secret`
/// fields of a JSON blob). Fail-safe: a vault read error skips that entry and
/// is logged once (never the value).
pub async fn collect(
    vault: &origin_keyvault::KeyVault,
    catalog: &origin_provider::catalog::Catalog,
    account: &str,
) -> Redactor {
    use origin_provider::catalog::AuthScheme;
    let mut secrets: Vec<(String, String)> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .filter(|(k, _)| origin_tools::is_redactable_secret_env_name(k))
        .collect();
    let mut failed = 0u32;
    // ponytail: sequential reads (one per catalog provider) at startup; run them
    // concurrently if a slow keyring backend makes startup noticeably slower.
    for e in catalog.entries() {
        let acct = match e.auth {
            AuthScheme::None => continue,
            AuthScheme::OAuth(_) => format!("{account}/oauth"),
            _ => account.to_string(),
        };
        let id = e.id.as_ref();
        match vault.get(id, &acct).await {
            Ok(s) => match serde_json::from_str::<serde_json::Value>(s.expose()) {
                Ok(serde_json::Value::Object(o)) => {
                    for field in ["access", "refresh", "secret"] {
                        if let Some(v) = o.get(field).and_then(serde_json::Value::as_str) {
                            secrets.push((format!("{id}.{field}"), v.to_string()));
                        }
                    }
                }
                _ => secrets.push((id.to_string(), s.expose().clone())),
            },
            Err(origin_keyvault::Error::NotFound { .. }) => {}
            Err(_) => failed += 1,
        }
    }
    if failed > 0 {
        tracing::warn!(
            failed,
            "secret redaction: some keyvault reads failed; redacting with the values that were read"
        );
    }
    Redactor::new(secrets)
}

#[cfg(test)]
mod tests {
    use super::Redactor;

    fn r() -> Redactor {
        Redactor::new([
            (
                "ANTHROPIC_API_KEY".to_string(),
                "sk-ant-test-0123456789".to_string(),
            ),
            ("SHORT_TOKEN".to_string(), "abc1234".to_string()),
            (
                "OUTER_TOKEN".to_string(),
                "xx-sk-ant-test-0123456789-yy".to_string(),
            ),
        ])
    }

    #[test]
    fn exact_values_are_replaced_with_their_name() {
        assert_eq!(
            r().redact("key=sk-ant-test-0123456789; again sk-ant-test-0123456789"),
            "key=[REDACTED:ANTHROPIC_API_KEY]; again [REDACTED:ANTHROPIC_API_KEY]"
        );
    }

    #[test]
    fn short_values_and_near_misses_are_untouched() {
        let s = "abc1234 and sk-ant-test-012345678 (one char short)";
        assert!(matches!(r().redact(s), std::borrow::Cow::Borrowed(_)));
    }

    #[test]
    fn a_value_containing_another_is_replaced_whole() {
        assert_eq!(
            r().redact("v=xx-sk-ant-test-0123456789-yy"),
            "v=[REDACTED:OUTER_TOKEN]"
        );
    }
}
