// SPDX-License-Identifier: Apache-2.0
//! Resolve the default model for daemon-spawned sub-agent / self-dispatch loops.
//!
//! The parent session always carries its configured model verbatim (sent per
//! request as `req.model`), but Task workers and the daemon's own self-dispatch
//! loops (ambient, scheduler, overnight, self-dev, webhook) have no session to inherit
//! from. Historically they fell back to the env var `ORIGIN_MODEL` and, when
//! unset, the hardcoded sentinel `"claude-fable-5"` — a model the user's account
//! may not be able to serve, which made every such worker fail its first
//! provider call and surface (via `swarm_worker`) as `GoalUnreachable`.
//!
//! This module wires the previously-stranded `[subagent]` config slot and the
//! `[primary]` config model into that fallback, so a daemon worker defaults to a
//! model the user actually has provisioned.

/// Resolve the default model for daemon worker / self-dispatch contexts.
///
/// Resolution order (first non-empty wins):
/// 1. env `ORIGIN_MODEL` (if set and non-empty),
/// 2. the configured `[subagent].model`,
/// 3. the configured `[primary].model`,
/// 4. the last-resort sentinel `"claude-fable-5"`.
#[must_use]
pub fn configured_default_model() -> String {
    if let Ok(env_model) = std::env::var("ORIGIN_MODEL") {
        if !env_model.is_empty() {
            return env_model;
        }
    }
    read_config_model().unwrap_or_else(|| "claude-fable-5".to_string())
}

/// Read the configured sub-agent (preferred) or primary model from
/// `~/.origin/config.toml`.
///
/// The config home is `$ORIGIN_HOME` when set, else the user's home directory.
/// Returns `None` on any missing-file / IO / parse failure (never panics), and
/// prefers `[subagent].model` over `[primary].model` when both are present.
fn read_config_model() -> Option<String> {
    let home = std::env::var_os("ORIGIN_HOME")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir)?;
    let path = home.join(".origin").join("config.toml");
    let text = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&text).ok()?;
    value
        .get("subagent")
        .and_then(|t| t.get("model"))
        .or_else(|| value.get("primary").and_then(|t| t.get("model")))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // env vars are process-global; serialize all tests that mutate
    // ORIGIN_MODEL / ORIGIN_HOME so parallel runs don't clobber each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Restore an env var to its prior value (or remove it if it was unset).
    fn restore(key: &str, prior: Option<std::ffi::OsString>) {
        match prior {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    fn write_config(home: &std::path::Path, body: &str) {
        let dir = home.join(".origin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    #[test]
    fn primary_only_returns_primary_model() {
        let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prior_model = std::env::var_os("ORIGIN_MODEL");
        let prior_home = std::env::var_os("ORIGIN_HOME");
        std::env::remove_var("ORIGIN_MODEL");

        let tmp = tempfile::TempDir::new().unwrap();
        write_config(
            tmp.path(),
            "[primary]\nprovider=\"anthropic\"\naccount=\"default\"\nmodel=\"claude-opus-4-8\"\n",
        );
        std::env::set_var("ORIGIN_HOME", tmp.path());

        assert_eq!(configured_default_model(), "claude-opus-4-8");

        restore("ORIGIN_MODEL", prior_model);
        restore("ORIGIN_HOME", prior_home);
    }

    #[test]
    fn subagent_table_takes_precedence_over_primary() {
        let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prior_model = std::env::var_os("ORIGIN_MODEL");
        let prior_home = std::env::var_os("ORIGIN_HOME");
        std::env::remove_var("ORIGIN_MODEL");

        let tmp = tempfile::TempDir::new().unwrap();
        write_config(
            tmp.path(),
            "[primary]\nprovider=\"anthropic\"\naccount=\"default\"\nmodel=\"claude-opus-4-8\"\n\
             [subagent]\nprovider=\"anthropic\"\naccount=\"default\"\nmodel=\"claude-fable-5-haiku\"\n",
        );
        std::env::set_var("ORIGIN_HOME", tmp.path());

        assert_eq!(configured_default_model(), "claude-fable-5-haiku");

        restore("ORIGIN_MODEL", prior_model);
        restore("ORIGIN_HOME", prior_home);
    }

    #[test]
    fn env_var_has_highest_precedence() {
        let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prior_model = std::env::var_os("ORIGIN_MODEL");
        let prior_home = std::env::var_os("ORIGIN_HOME");

        let tmp = tempfile::TempDir::new().unwrap();
        write_config(
            tmp.path(),
            "[primary]\nprovider=\"anthropic\"\naccount=\"default\"\nmodel=\"claude-opus-4-8\"\n",
        );
        std::env::set_var("ORIGIN_HOME", tmp.path());
        std::env::set_var("ORIGIN_MODEL", "model-from-env");

        assert_eq!(configured_default_model(), "model-from-env");

        restore("ORIGIN_MODEL", prior_model);
        restore("ORIGIN_HOME", prior_home);
    }

    #[test]
    fn no_config_file_returns_sentinel() {
        let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let prior_model = std::env::var_os("ORIGIN_MODEL");
        let prior_home = std::env::var_os("ORIGIN_HOME");
        std::env::remove_var("ORIGIN_MODEL");

        // Empty temp dir: no `.origin/config.toml` present.
        let tmp = tempfile::TempDir::new().unwrap();
        std::env::set_var("ORIGIN_HOME", tmp.path());

        assert_eq!(configured_default_model(), "claude-fable-5");

        restore("ORIGIN_MODEL", prior_model);
        restore("ORIGIN_HOME", prior_home);
    }
}
