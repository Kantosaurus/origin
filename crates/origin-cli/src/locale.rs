// SPDX-License-Identifier: Apache-2.0
//! Locale resolution for user-facing CLI chrome.
//!
//! Routes a handful of visible CLI strings through [`origin_i18n`] so the
//! terminal chrome can render in the user's locale. The active language is
//! resolved from (in order) the process-global override set from the top-level
//! `--lang <code>` flag, then the `LC_ALL` and `LANG` environment variables,
//! falling back to [`Lang::En`]. This is additive and default-off in effect:
//! with no `--lang` flag and no locale env set (or an unrecognized one) every
//! string resolves to its original English text, so default behaviour is
//! byte-identical.

use std::sync::OnceLock;

use origin_i18n::{t, tf, Lang};

/// Process-global locale override, populated once at startup from the top-level
/// `--lang <code>` flag (see `set_locale_override`). `None` until set; when set,
/// [`resolve`] consults it before any environment variable. A `OnceLock` is the
/// right home: the override is chosen once at process start and never mutated,
/// so there is no need for interior mutability beyond first-write.
static LOCALE_OVERRIDE: OnceLock<String> = OnceLock::new();

/// Record the startup `--lang <code>` override.
///
/// Idempotent: only the first call takes effect (later calls are ignored),
/// matching the once-at-startup lifecycle. The stored code is consulted *first*
/// by [`resolve`] — ahead of `$LC_ALL` / `$LANG` — so an explicit `--lang`
/// always wins. An unrecognized code does not pin a bogus locale: [`resolve`]
/// simply falls through to the environment (then English) when the override
/// fails to parse.
pub fn set_locale_override(code: impl Into<String>) {
    let _ = LOCALE_OVERRIDE.set(code.into());
}

/// Resolve the active UI language.
///
/// Resolution order: the explicit `override_code` argument, then the
/// process-global `--lang` override (see [`set_locale_override`]), then
/// `$LC_ALL`, then `$LANG`. The first value that parses into a known [`Lang`]
/// wins; otherwise English is used. `line`/`linef` call this with `None`, so
/// they pick up the process-global `--lang` override automatically.
#[must_use]
pub fn resolve(override_code: Option<&str>) -> Lang {
    if let Some(code) = override_code {
        if let Some(lang) = Lang::from_code(code) {
            return lang;
        }
    }
    if let Some(code) = LOCALE_OVERRIDE.get() {
        if let Some(lang) = Lang::from_code(code) {
            return lang;
        }
    }
    for var in ["LC_ALL", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            // POSIX locales look like `en_US.UTF-8`; strip the codeset suffix.
            let primary = val.split('.').next().unwrap_or(&val);
            if let Some(lang) = Lang::from_code(primary) {
                return lang;
            }
        }
    }
    Lang::En
}

/// Look up a localized chrome string for the resolved environment locale.
#[must_use]
pub fn line(key: &str) -> &'static str {
    t(resolve(None), key)
}

/// Look up a localized chrome string with `{name}` placeholder substitution.
#[must_use]
pub fn linef(key: &str, args: &[(&str, &str)]) -> String {
    tf(resolve(None), key, args)
}

#[cfg(test)]
mod tests {
    use super::{line, linef, resolve};
    use origin_i18n::{t, tf, Lang};

    /// Tests that read AND tests that set the process-global lang override
    /// serialize on this lock. Without it, `process_override_flows_through_
    /// resolve_none` can flip the override to French between a parallel
    /// reader's `resolve(None)` snapshot and its later `line`/`linef` calls,
    /// making the two sides of an equality assert resolve different locales.
    static OVERRIDE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn override_takes_precedence() {
        assert_eq!(resolve(Some("es")), Lang::Es);
        assert_eq!(resolve(Some("fr-FR")), Lang::Fr);
    }

    #[test]
    fn unknown_override_resolves_deterministically() {
        // An unrecognized override must not pin a bogus locale; the result is
        // always one of the shipped locales (env-derived, else English).
        let resolved = resolve(Some("xx"));
        assert!(origin_i18n::available().contains(&resolved));
    }

    #[test]
    fn line_never_returns_sentinel_for_real_key() {
        assert_ne!(line("welcome"), "?");
    }

    #[test]
    fn linef_substitutes_placeholders() {
        let s = linef("cost.session", &[("usd", "$1.23")]);
        assert!(s.contains("$1.23"), "expected substituted value in {s:?}");
    }

    // The byte-identical English contract for every chrome key now routed
    // through the catalog. Each tuple is `(key, exact_current_english_literal)`
    // as it appears at the live call site (glyphs/markers/hints stay in code, so
    // only the localizable sentence portion is listed here). `t(Lang::En, ..)`
    // resolves these independent of any `--lang` override, so this asserts the
    // default-English path is unchanged for every routed key.
    #[test]
    fn routed_keys_are_byte_identical_in_english() {
        // `line()`-routed (no placeholders):
        assert_eq!(t(Lang::En, "interrupt"), "interrupt sent (Ctrl+D to exit)");
        // `linef()`-routed (with the catalog's own placeholder names):
        assert_eq!(
            t(Lang::En, "session.resumed"),
            "resumed session {short}\u{2026} \u{2014} the model will recall the earlier conversation"
        );
        assert_eq!(t(Lang::En, "goal.active"), "goal active: {condition}");
        assert_eq!(t(Lang::En, "goal.done"), "done: {reason}");
        assert_eq!(t(Lang::En, "permission.ask"), "Allow {tool} {args}?");
        // In-session command chrome routed this pass — En must equal the exact
        // pre-routing literal so the default-English output is byte-identical.
        assert_eq!(t(Lang::En, "cmd.model.usage"), "usage: /model <name>");
        assert_eq!(
            t(Lang::En, "cmd.effort.usage"),
            "usage: /effort <fast|low|medium|high|max>"
        );
        assert_eq!(
            t(Lang::En, "cmd.outputstyle.usage"),
            "usage: /output-style <default|explanatory|learning|concise>"
        );
        assert_eq!(
            t(Lang::En, "cmd.steer.usage"),
            "usage: /steer <hint to inject into the next turn>"
        );
        assert_eq!(
            t(Lang::En, "cmd.copy.ok"),
            "copied the last reply to the clipboard"
        );
        assert_eq!(t(Lang::En, "cmd.copy.empty"), "nothing to copy yet");
        assert_eq!(
            t(Lang::En, "cmd.turn.busy"),
            "a turn is already running (Ctrl+C to interrupt it)"
        );
        // `linef()`-routed command chrome rendered with the call site's args:
        assert_eq!(
            tf(Lang::En, "cmd.model.set", &[("name", "opus")]),
            "model set: opus"
        );
        assert_eq!(
            tf(Lang::En, "cmd.effort.set", &[("token", "high")]),
            "reasoning effort: high"
        );
        assert_eq!(
            tf(Lang::En, "cmd.outputstyle.set", &[("label", "concise")]),
            "output style: concise"
        );
        assert_eq!(
            tf(Lang::En, "cmd.steer.queued", &[("pending", "2")]),
            "steering hint queued (2 pending)"
        );
        assert_eq!(
            tf(
                Lang::En,
                "cmd.account.active",
                &[("provider", "anthropic"), ("account", "default")]
            ),
            "provider active: anthropic/default"
        );

        // The seven previously-unrouted keys, now wired to live call sites. Each
        // En literal is reconciled to the EXACT current call-site text so the
        // default-English output is byte-identical after routing.
        // `thinking` -> tui::localize_phase's "Thinking" status label (no ellipsis).
        assert_eq!(t(Lang::En, "thinking"), "Thinking");
        // `tool.running` -> main.rs tool-activity header `[tool]` (summary appended
        // in code).
        assert_eq!(tf(Lang::En, "tool.running", &[("tool", "Bash")]), "[Bash]");
        // `tool.done` -> main.rs tool-failure line "{tool} failed" (✘ glyph in code).
        assert_eq!(tf(Lang::En, "tool.done", &[("tool", "Edit")]), "Edit failed");
        // `permission.denied` -> main.rs y/n deny verb "denied".
        assert_eq!(t(Lang::En, "permission.denied"), "denied");
        // `cost.turn` -> status::format_turn_cost (opt-in `ORIGIN_TURN_COST=1`).
        assert_eq!(
            tf(Lang::En, "cost.turn", &[("usd", "$0.01")]),
            "This turn cost $0.01"
        );
        // `error.generic` -> main.rs turn-error line; bare `{message}` passthrough.
        assert_eq!(tf(Lang::En, "error.generic", &[("message", "boom")]), "boom");
        // `session.saved` -> admin::export_session "wrote {path}" confirmation.
        assert_eq!(
            tf(Lang::En, "session.saved", &[("path", "/tmp/s.md")]),
            "wrote /tmp/s.md"
        );
    }

    // Each newly-routed key, when accessed via the locale chrome helpers, resolves
    // to the catalog (not the literal key) and — under a non-English `--lang`
    // override — renders the localized template. The process-global override is set
    // in `process_override_flows_through_resolve_none`; here we assert against the
    // catalog directly so this test is order-independent: `line`/`linef` must equal
    // the resolved-locale catalog text for the key (proving they route, not hardcode).
    #[test]
    fn newly_routed_keys_route_through_locale_helpers() {
        let _guard = OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let lang = resolve(None);
        assert_eq!(line("thinking"), t(lang, "thinking"));
        assert_eq!(line("permission.denied"), t(lang, "permission.denied"));
        assert_eq!(
            linef("tool.running", &[("tool", "Bash")]),
            tf(lang, "tool.running", &[("tool", "Bash")])
        );
        assert_eq!(
            linef("tool.done", &[("tool", "Edit")]),
            tf(lang, "tool.done", &[("tool", "Edit")])
        );
        assert_eq!(
            linef("cost.turn", &[("usd", "$0.01")]),
            tf(lang, "cost.turn", &[("usd", "$0.01")])
        );
        assert_eq!(
            linef("error.generic", &[("message", "boom")]),
            tf(lang, "error.generic", &[("message", "boom")])
        );
        assert_eq!(
            linef("session.saved", &[("path", "/x")]),
            tf(lang, "session.saved", &[("path", "/x")])
        );
        // None of them collapse to the bare key (which would mean an unrouted miss).
        for k in [
            "thinking",
            "tool.running",
            "tool.done",
            "permission.denied",
            "cost.turn",
            "error.generic",
            "session.saved",
        ] {
            assert_ne!(line(k), k, "key {k} must resolve to catalog text, not the key");
        }
    }

    // A sample substitution under default English must reproduce the EXACT
    // current rendered line (proving placeholder names line up with the call
    // site's supplied args, byte-for-byte).
    #[test]
    fn linef_sample_substitution_is_byte_identical_in_english() {
        // `session.resumed` with a short id, as `main.rs` builds it.
        assert_eq!(
            tf(Lang::En, "session.resumed", &[("short", "abcd1234")]),
            "resumed session abcd1234\u{2026} \u{2014} the model will recall the earlier conversation"
        );
        // `goal.active` sentence portion (the `◎ ` glyph stays in code).
        assert_eq!(
            tf(Lang::En, "goal.active", &[("condition", "fix the build")]),
            "goal active: fix the build"
        );
    }

    #[test]
    fn process_override_flows_through_resolve_none() {
        // Setting the process-global override to French must make `resolve(None)`
        // (and thus `line`/`linef`, which both pass `None`) render the French
        // catalog string — proving the `--lang` wiring reaches the chrome path.
        // The override OnceLock is process-wide; this is the sole test that sets
        // it, so it cannot race another SETTER — but readers comparing across
        // the flip still race, hence the shared lock.
        let _guard = OVERRIDE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        super::set_locale_override("fr");
        assert_eq!(resolve(None), Lang::Fr);
        assert_eq!(line("bye"), "Au revoir");
        // A routed `line()` key renders in French under the override.
        assert_eq!(line("interrupt"), t(Lang::Fr, "interrupt"));
        assert_ne!(line("interrupt"), t(Lang::En, "interrupt"));
        // A routed `linef()` key substitutes into the French template under the
        // override (the `{condition}` arg is filled; the French words surround it).
        let fr_goal = linef("goal.active", &[("condition", "réparer la compilation")]);
        assert!(
            fr_goal.contains("réparer la compilation"),
            "expected substituted condition in {fr_goal:?}"
        );
        assert_eq!(
            fr_goal,
            tf(
                Lang::Fr,
                "goal.active",
                &[("condition", "réparer la compilation")]
            )
        );
        // A newly-routed command-chrome key also renders in French under --lang.
        assert_eq!(line("cmd.turn.busy"), t(Lang::Fr, "cmd.turn.busy"));
        assert_ne!(line("cmd.turn.busy"), t(Lang::En, "cmd.turn.busy"));
        let fr_model = linef("cmd.model.set", &[("name", "opus")]);
        assert_eq!(fr_model, tf(Lang::Fr, "cmd.model.set", &[("name", "opus")]));
        assert!(fr_model.contains("opus"));
        // A newly-routed key (`thinking`) also localizes under the override and
        // differs from English — proving the wiring reaches these call sites.
        assert_eq!(line("thinking"), t(Lang::Fr, "thinking"));
        assert_ne!(line("thinking"), t(Lang::En, "thinking"));
        // And a newly-routed `linef()` key (`tool.done`) substitutes into the
        // French template under the override.
        let fr_done = linef("tool.done", &[("tool", "Edit")]);
        assert_eq!(fr_done, tf(Lang::Fr, "tool.done", &[("tool", "Edit")]));
        assert!(fr_done.contains("Edit"));
    }
}
