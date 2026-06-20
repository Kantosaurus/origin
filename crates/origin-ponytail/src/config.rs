// SPDX-License-Identifier: Apache-2.0
//! Mode resolution + the dependency allowlist (`~/.origin/ponytail.toml`).

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::mode::PonytailMode;

#[must_use]
pub fn origin_home() -> PathBuf {
    if let Some(h) = std::env::var_os("ORIGIN_HOME") {
        return PathBuf::from(h);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".origin")
}

#[must_use]
pub fn config_path() -> PathBuf {
    origin_home().join("ponytail.toml")
}

fn default_mode_from_toml(content: &str) -> Option<PonytailMode> {
    content
        .parse::<toml::Value>()
        .ok()?
        .get("defaultMode")?
        .as_str()
        .and_then(PonytailMode::parse_level)
}

/// Parse the `allow = [...]` array, lowercased. Pure; for tests + `allowlist()`.
#[must_use]
pub fn parse_allowlist(content: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(v) = content.parse::<toml::Value>() {
        if let Some(arr) = v.get("allow").and_then(|a| a.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    set.insert(s.trim().to_ascii_lowercase());
                }
            }
        }
    }
    set
}

/// Testable core of `resolve_mode`: explicit request > config token > env token > Full.
#[must_use]
pub fn resolve_mode_with(
    requested: Option<PonytailMode>,
    config_token: Option<&str>,
    env_token: Option<&str>,
) -> PonytailMode {
    if let Some(m) = requested {
        return m;
    }
    if let Some(m) = env_token.and_then(PonytailMode::parse_level) {
        return m;
    }
    if let Some(m) = config_token.and_then(PonytailMode::parse_level) {
        return m;
    }
    PonytailMode::Full
}

/// Resolve the effective mode from the live environment + config file.
#[must_use]
pub fn resolve_mode(requested: Option<PonytailMode>) -> PonytailMode {
    if let Some(m) = requested {
        return m;
    }
    let env = std::env::var("PONYTAIL_DEFAULT_MODE")
        .or_else(|_| std::env::var("ORIGIN_PONYTAIL"))
        .ok();
    if let Some(m) = env.as_deref().and_then(PonytailMode::parse_level) {
        return m;
    }
    let cfg = std::fs::read_to_string(config_path()).ok();
    cfg.as_deref().and_then(default_mode_from_toml).unwrap_or(PonytailMode::Full)
}

#[must_use]
pub fn allowlist() -> BTreeSet<String> {
    std::fs::read_to_string(config_path()).map(|c| parse_allowlist(&c)).unwrap_or_default()
}

/// Append a package to the allowlist (idempotent). Best-effort; errors ignored.
pub fn remember(name: &str) {
    let name = name.trim().to_ascii_lowercase();
    let mut set = allowlist();
    if !set.insert(name) {
        return;
    }
    let path = config_path();
    let existing_mode = std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| default_mode_from_toml(&c))
        .unwrap_or(PonytailMode::Full);
    let list = set.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(", ");
    let body = format!("defaultMode = {:?}\nallow = [{}]\n", existing_mode.as_str(), list);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&path, body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_wins() {
        assert_eq!(resolve_mode_with(Some(PonytailMode::Ultra), None, None), PonytailMode::Ultra);
    }

    #[test]
    fn env_over_default() {
        assert_eq!(resolve_mode_with(None, None, Some("lite")), PonytailMode::Lite);
    }

    #[test]
    fn config_over_default_but_under_env() {
        assert_eq!(resolve_mode_with(None, Some("off"), None), PonytailMode::Off);
        assert_eq!(resolve_mode_with(None, Some("off"), Some("ultra")), PonytailMode::Ultra);
    }

    #[test]
    fn falls_back_to_full() {
        assert_eq!(resolve_mode_with(None, None, None), PonytailMode::Full);
        assert_eq!(resolve_mode_with(None, Some("garbage"), Some("garbage")), PonytailMode::Full);
    }

    #[test]
    fn allowlist_parses_toml() {
        let toml = "defaultMode = \"full\"\nallow = [\"axios\", \"React\"]\n";
        let set = parse_allowlist(toml);
        assert!(set.contains("axios"));
        assert!(set.contains("react")); // lowercased
    }
}
