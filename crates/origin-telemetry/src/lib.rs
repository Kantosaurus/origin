// SPDX-License-Identifier: Apache-2.0
//! Opt-in, self-hostable product telemetry pipeline.
//!
//! Provides a product-event layer (distinct from any OTLP transport exporter)
//! that builds redacted, sampled JSONL telemetry events. The crate is pure:
//! it computes the JSONL lines a host should ship, honoring `DO_NOT_TRACK`,
//! explicit opt-in, deterministic hash-based sampling, and secret redaction.
//! Network or filesystem delivery is left to the caller via an injected sink.
#![forbid(unsafe_code)]

use serde::Serialize;

/// Placeholder substituted for any value that looks like a secret.
pub const REDACTED: &str = "***";

/// Error type for telemetry serialization failures.
#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    /// JSON serialization of an event failed.
    #[error("serialization failed: {0}")]
    Serde(String),
}

/// A single product-telemetry event.
///
/// Properties are stored as ordered key/value pairs so the serialized output
/// is stable and so duplicate keys are preserved exactly as recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    /// Event name, e.g. `"session_start"`.
    pub name: String,
    /// Ordered property key/value pairs.
    pub props: Vec<(String, String)>,
    /// Event timestamp in Unix milliseconds.
    pub ts_unix_ms: u64,
}

impl Event {
    /// Creates a new event with no properties.
    #[must_use]
    pub const fn new(name: String, ts_unix_ms: u64) -> Self {
        Self { name, props: Vec::new(), ts_unix_ms }
    }
}

/// Returns `true` when `c` is a lowercase or uppercase hexadecimal digit.
const fn is_hex_digit(c: u8) -> bool {
    c.is_ascii_digit() || matches!(c, b'a'..=b'f' | b'A'..=b'F')
}

/// Returns `true` when `c` is a plausible base64 / base64url character.
const fn is_base64_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'-' | b'_' | b'=')
}

/// Returns `true` when the whole string is hex and long enough to be a secret.
fn looks_like_long_hex(s: &str) -> bool {
    s.len() >= 32 && s.bytes().all(is_hex_digit)
}

/// Returns `true` when the whole string is base64-ish and long enough.
fn looks_like_long_base64(s: &str) -> bool {
    // Require some non-trivial length and at least one digit OR mixed case so
    // ordinary long English words are not flagged.
    if s.len() < 40 || !s.bytes().all(is_base64_char) {
        return false;
    }
    let has_digit = s.bytes().any(|b| b.is_ascii_digit());
    let has_lower = s.bytes().any(|b| b.is_ascii_lowercase());
    let has_upper = s.bytes().any(|b| b.is_ascii_uppercase());
    has_digit || (has_lower && has_upper)
}

/// Returns `true` when a value should be treated as a secret.
fn is_secret_value(value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    // Common provider/token prefixes.
    if lower.starts_with("sk-")
        || lower.starts_with("sk_")
        || lower.starts_with("pk-")
        || lower.starts_with("ghp_")
        || lower.starts_with("xoxb-")
        || lower.starts_with("aiza")
        || lower.starts_with("bearer ")
    {
        return true;
    }
    // Inline `key=secret` style assignments.
    if lower.contains("api_key=")
        || lower.contains("apikey=")
        || lower.contains("access_token=")
        || lower.contains("authorization:")
    {
        return true;
    }
    looks_like_long_hex(v) || looks_like_long_base64(v)
}

/// Redacts property values that look like secrets, in place.
///
/// Values matching known secret shapes (`sk-`, `Bearer ...`, `api_key=...`,
/// long hexadecimal or base64 blobs) are replaced with [`REDACTED`]. Keys are
/// never altered. Returns the number of values that were redacted.
pub fn redact(props: &mut [(String, String)]) -> usize {
    let mut count = 0;
    for (_key, value) in props.iter_mut() {
        if is_secret_value(value) {
            REDACTED.clone_into(value);
            count += 1;
        }
    }
    count
}

/// Runtime configuration for the telemetry pipeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Master switch; when `false` nothing is ever emitted.
    pub enabled: bool,
    /// Fraction of events to keep, clamped to `0.0..=1.0`.
    pub sample_rate: f64,
    /// Optional delivery endpoint for a host-side sink.
    pub endpoint: Option<String>,
}

impl Config {
    /// Builds a [`Config`] from environment-derived flags.
    ///
    /// `do_not_track` (the `DO_NOT_TRACK` convention) always wins and forces
    /// `enabled = false`. Otherwise telemetry is enabled only when `opt_in`
    /// is `true`. The sample rate is clamped into `0.0..=1.0`.
    #[must_use]
    pub fn from_env(do_not_track: bool, opt_in: bool, sample: f64) -> Self {
        let enabled = opt_in && !do_not_track;
        let sample_rate = if sample.is_nan() { 0.0 } else { sample.clamp(0.0, 1.0) };
        Self { enabled, sample_rate, endpoint: None }
    }

    /// Returns a copy of this config with the given delivery endpoint set.
    #[must_use]
    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = Some(endpoint);
        self
    }
}

/// The largest representable `u64`, as `f64`, used as the sampling denominator.
#[allow(clippy::cast_precision_loss)]
const U64_MAX_AS_F64: f64 = u64::MAX as f64;

/// Decides whether an event with the given hash should be emitted.
///
/// Sampling is deterministic: the same `event_hash` always yields the same
/// decision for a given `sample_rate`, so retries do not change inclusion.
/// A disabled config or a `sample_rate <= 0.0` never emits; `>= 1.0` always
/// emits (for an enabled config).
#[must_use]
pub fn should_emit(cfg: &Config, event_hash: u64) -> bool {
    if !cfg.enabled || cfg.sample_rate <= 0.0 {
        return false;
    }
    if cfg.sample_rate >= 1.0 {
        return true;
    }
    #[allow(clippy::cast_precision_loss)]
    let position = event_hash as f64 / U64_MAX_AS_F64;
    position < cfg.sample_rate
}

/// Computes a stable 64-bit hash of an event for sampling decisions.
///
/// Uses an FNV-1a hash over the event name and timestamp so that the value is
/// reproducible across processes and platforms (unlike [`std::hash`] defaults).
#[must_use]
pub fn event_hash(e: &Event) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    let mut mix = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    mix(e.name.as_bytes());
    mix(&e.ts_unix_ms.to_le_bytes());
    hash
}

/// Serializes an event to a single redacted JSON line (JSONL).
///
/// The event is cloned, its properties redacted via [`redact`], then encoded
/// as one line of compact JSON with no trailing newline.
///
/// # Errors
///
/// Returns [`TelemetryError::Serde`] if JSON serialization fails.
pub fn to_jsonl(e: &Event) -> Result<String, TelemetryError> {
    let mut redacted = e.clone();
    redact(&mut redacted.props);
    serde_json::to_string(&redacted).map_err(|err| TelemetryError::Serde(err.to_string()))
}

/// Buffers events and produces redacted, sampled JSONL lines on demand.
#[derive(Debug)]
pub struct Pipeline {
    cfg: Config,
    buffer: Vec<Event>,
}

impl Pipeline {
    /// Creates a new pipeline bound to the given configuration.
    #[must_use]
    pub const fn new(cfg: Config) -> Self {
        Self { cfg, buffer: Vec::new() }
    }

    /// Returns a reference to the pipeline's configuration.
    #[must_use]
    pub const fn config(&self) -> &Config {
        &self.cfg
    }

    /// Returns the number of buffered (not yet drained) events.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    /// Buffers an event for later draining.
    ///
    /// Events are always buffered; the enabled/sampling policy is applied at
    /// [`Pipeline::drain`] time so that toggling config before draining takes
    /// effect.
    pub fn record(&mut self, e: Event) {
        self.buffer.push(e);
    }

    /// Drains buffered events into redacted JSONL lines ready to ship.
    ///
    /// Honors the configured enabled flag and deterministic sampling: when the
    /// config is disabled the buffer is cleared and an empty vector is
    /// returned. Events that fail serialization are skipped. The internal
    /// buffer is always emptied.
    pub fn drain(&mut self) -> Vec<String> {
        let drained = std::mem::take(&mut self.buffer);
        if !self.cfg.enabled {
            return Vec::new();
        }
        let mut lines = Vec::new();
        for event in drained {
            if should_emit(&self.cfg, event_hash(&event)) {
                if let Ok(line) = to_jsonl(&event) {
                    lines.push(line);
                }
            }
        }
        lines
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::*;

    fn ev(name: &str, props: &[(&str, &str)], ts: u64) -> Event {
        Event {
            name: name.to_owned(),
            props: props.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect(),
            ts_unix_ms: ts,
        }
    }

    #[test]
    fn do_not_track_forces_disabled() {
        let cfg = Config::from_env(true, true, 1.0);
        assert!(!cfg.enabled);
        assert!(!should_emit(&cfg, 0));
        assert!(!should_emit(&cfg, u64::MAX / 2));
    }

    #[test]
    fn opt_in_without_dnt_enables() {
        let cfg = Config::from_env(false, true, 0.5);
        assert!(cfg.enabled);
        assert_eq!(cfg.sample_rate, 0.5);
        // opt_in false stays disabled.
        assert!(!Config::from_env(false, false, 1.0).enabled);
    }

    #[test]
    fn sample_rate_is_clamped_and_nan_safe() {
        assert_eq!(Config::from_env(false, true, 2.0).sample_rate, 1.0);
        assert_eq!(Config::from_env(false, true, -3.0).sample_rate, 0.0);
        assert_eq!(Config::from_env(false, true, f64::NAN).sample_rate, 0.0);
    }

    #[test]
    fn redact_hides_sk_and_bearer_tokens() {
        let mut props = vec![
            ("model".to_owned(), "gpt-4".to_owned()),
            ("key".to_owned(), "sk-ABC123def456ghi789".to_owned()),
            ("auth".to_owned(), "Bearer abc.def.ghi".to_owned()),
            ("note".to_owned(), "hello world".to_owned()),
        ];
        let n = redact(&mut props);
        assert_eq!(n, 2);
        assert_eq!(props[0].1, "gpt-4");
        assert_eq!(props[1].1, REDACTED);
        assert_eq!(props[2].1, REDACTED);
        assert_eq!(props[3].1, "hello world");
    }

    #[test]
    fn redact_hides_assignments_and_long_blobs() {
        let mut props = vec![
            ("q".to_owned(), "url?api_key=secretvalue".to_owned()),
            ("hex".to_owned(), "deadbeefdeadbeefdeadbeefdeadbeef00".to_owned()),
            ("b64".to_owned(), "QWxhZGRpbjpvcGVuIHNlc2FtZTEyMzQ1Njc4OTBhYmNk".to_owned()),
            ("short".to_owned(), "abc123".to_owned()),
        ];
        let n = redact(&mut props);
        assert_eq!(n, 3);
        assert_eq!(props[0].1, REDACTED);
        assert_eq!(props[1].1, REDACTED);
        assert_eq!(props[2].1, REDACTED);
        assert_eq!(props[3].1, "abc123");
    }

    #[test]
    fn should_emit_respects_sample_extremes() {
        let all = Config::from_env(false, true, 1.0);
        let none = Config::from_env(false, true, 0.0);
        for h in [0_u64, 1, 42, u64::MAX / 3, u64::MAX] {
            assert!(should_emit(&all, h));
            assert!(!should_emit(&none, h));
        }
    }

    #[test]
    fn should_emit_is_deterministic() {
        let cfg = Config::from_env(false, true, 0.5);
        let h = event_hash(&ev("click", &[("x", "1")], 1234));
        assert_eq!(should_emit(&cfg, h), should_emit(&cfg, h));
        // Half rate: a low hash is in, the max hash is out.
        assert!(should_emit(&cfg, 0));
        assert!(!should_emit(&cfg, u64::MAX));
    }

    #[test]
    fn to_jsonl_is_valid_and_redacted() {
        let event = ev("login", &[("token", "sk-supersecretvalue123")], 999);
        let line = to_jsonl(&event).unwrap();
        assert!(!line.contains('\n'));
        assert!(!line.contains("supersecret"));
        assert!(line.contains(REDACTED));
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["name"], "login");
        assert_eq!(parsed["ts_unix_ms"], 999);
        assert_eq!(parsed["props"][0][0], "token");
        assert_eq!(parsed["props"][0][1], REDACTED);
    }

    #[test]
    fn pipeline_drain_empty_when_disabled() {
        let cfg = Config::from_env(true, true, 1.0);
        let mut pipe = Pipeline::new(cfg);
        pipe.record(ev("a", &[], 1));
        pipe.record(ev("b", &[], 2));
        assert_eq!(pipe.pending(), 2);
        assert!(pipe.drain().is_empty());
        // Buffer is emptied even when disabled.
        assert_eq!(pipe.pending(), 0);
    }

    #[test]
    fn pipeline_drain_emits_and_redacts_when_enabled() {
        let cfg = Config::from_env(false, true, 1.0);
        let mut pipe = Pipeline::new(cfg);
        pipe.record(ev("login", &[("k", "sk-abcdefghijklmnop123")], 7));
        let lines = pipe.drain();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains(REDACTED));
        assert!(!lines[0].contains("abcdefghijklmnop"));
        assert_eq!(pipe.pending(), 0);
    }

    #[test]
    fn config_with_endpoint_sets_field() {
        let cfg = Config::from_env(false, true, 1.0).with_endpoint("https://t.example".to_owned());
        assert_eq!(cfg.endpoint.as_deref(), Some("https://t.example"));
        let pipe = Pipeline::new(cfg);
        assert_eq!(pipe.config().endpoint.as_deref(), Some("https://t.example"));
    }
}
