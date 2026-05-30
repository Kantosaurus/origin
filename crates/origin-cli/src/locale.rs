// SPDX-License-Identifier: Apache-2.0
//! Locale resolution for user-facing CLI chrome.
//!
//! Routes a handful of visible CLI strings through [`origin_i18n`] so the
//! terminal chrome can render in the user's locale. The active language is
//! resolved from (in order) an explicit override, then the `LC_ALL` and `LANG`
//! environment variables, falling back to [`Lang::En`]. This is additive and
//! default-off in effect: with no locale env set (or an unrecognized one) every
//! string resolves to its original English text, so default behaviour is
//! byte-identical.

use origin_i18n::{t, tf, Lang};

/// Resolve the active UI language.
///
/// Resolution order: the explicit `override_code` (e.g. from a `--lang` flag),
/// then `$LC_ALL`, then `$LANG`. The first value that parses into a known
/// [`Lang`] wins; otherwise English is used.
#[must_use]
pub fn resolve(override_code: Option<&str>) -> Lang {
    if let Some(code) = override_code {
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
    use origin_i18n::Lang;

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
}
