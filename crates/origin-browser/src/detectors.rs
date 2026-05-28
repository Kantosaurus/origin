//! Bot-detection signal classifier.
//!
//! Pure: takes a `SnapshotResp`, returns a `Verdict`. Add new signatures as
//! one-liners in `BOT_PATTERNS`; pair each with a test row in
//! `tests/detectors_table.rs`.

use crate::protocol::SnapshotResp;
use regex::RegexBuilder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    BotDetected(&'static str),
}

const BOT_PATTERNS: &[(&str, &str)] = &[
    ("cf-chl-", "cloudflare-challenge"),
    ("__cf_chl_", "cloudflare-challenge"),
    ("cf-mitigated", "cloudflare-mitigation"),
    ("g-recaptcha", "recaptcha"),
    ("h-captcha", "hcaptcha"),
    ("px-captcha", "perimeterx"),
    ("_pxhd", "perimeterx"),
    ("datadome", "datadome"),
    ("_Incapsula_Resource", "imperva-incapsula"),
    ("kasada", "kasada"),
];

/// Classify a snapshot response as `Verdict::Clean` or `Verdict::BotDetected`.
///
/// # Panics
///
/// Panics if the static title-detection regex fails to compile — which would
/// indicate a build-time bug in the literal pattern, not user input.
#[must_use]
pub fn classify(r: &SnapshotResp) -> Verdict {
    if matches!(r.status, Some(429)) {
        return Verdict::BotDetected("http-429");
    }
    if let Some(title) = r.title.as_deref() {
        let re = RegexBuilder::new(r"just a moment|attention required|access denied|verify you are human")
            .case_insensitive(true)
            .build()
            .expect("static regex compiles");
        if re.is_match(title) {
            return Verdict::BotDetected("title-human-check");
        }
    }
    if let Some(html) = r.html.as_deref() {
        for (needle, label) in BOT_PATTERNS {
            if html.contains(needle) {
                return Verdict::BotDetected(label);
            }
        }
    }
    if matches!(r.status, Some(403)) {
        // 403 without an explicit signature still gets flagged — sites that
        // 403 a snapshot fetch usually mean "not for bots".
        return Verdict::BotDetected("http-403");
    }
    Verdict::Clean
}
