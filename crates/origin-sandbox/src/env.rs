// SPDX-License-Identifier: Apache-2.0
//! Child-process environment allow-list (A6).
//!
//! Children (Bash, turn-end gate commands, hooks, MCP servers) get only the
//! allow-listed subset of the daemon's environment, so provider keys and other
//! credentials never reach model-driven processes. `ORIGIN_ENV_PASSTHROUGH`
//! (comma-separated names) adds names, overriding the secret filter;
//! `ORIGIN_ENV_SCRUB=0` restores full inheritance.

/// Names a child keeps (compared case-insensitively: Windows names are).
const ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "USERPROFILE",
    "TEMP",
    "TMP",
    "TMPDIR",
    "LANG",
    "TERM",
    "SystemRoot",
    "ComSpec",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "SHELL",
    "USER",
    "USERNAME",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "NVM_DIR",
    "VIRTUAL_ENV",
    "CONDA_PREFIX",
    "CI",
    // Windows: MSVC toolchain discovery (rustc's `link.exe` lookup, the `cc`
    // crate) reads Visual Studio instances under %ProgramData%; without it
    // `cargo build` fails with "linker `link.exe` not found".
    "ProgramData",
];

/// True when `name` looks like a credential: `*_API_KEY`, `*_TOKEN`,
/// `*_SECRET`, `*_PASSWORD`, `ANTHROPIC_*`, `OPENAI_*`, `AWS_*`, `GOOGLE_*`,
/// `GEMINI_*`, `GITHUB_TOKEN`, `GH_TOKEN`, `ORIGIN_*KEY*` (case-insensitive).
#[must_use]
pub fn is_secret_env_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD"]
        .iter()
        .any(|s| n.ends_with(s))
        || ["ANTHROPIC_", "OPENAI_", "AWS_", "GOOGLE_", "GEMINI_"]
            .iter()
            .any(|p| n.starts_with(p))
        || n == "GITHUB_TOKEN"
        || n == "GH_TOKEN"
        || (n.starts_with("ORIGIN_") && n.contains("KEY"))
}

/// Whether a child keeps `name`, given the raw `ORIGIN_ENV_PASSTHROUGH` value.
fn keep(name: &str, passthrough: &str) -> bool {
    if passthrough
        .split(',')
        .any(|p| p.trim().eq_ignore_ascii_case(name))
    {
        return true;
    }
    let listed = ALLOW.iter().any(|a| a.eq_ignore_ascii_case(name))
        || name.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("LC_"));
    listed && !is_secret_env_name(name)
}

/// Replace `cmd`'s inherited environment with the allow-listed subset of this
/// process's. Call BEFORE any explicit `cmd.env(..)`: `env_clear` drops those
/// too, while ones set afterwards are kept. No-op under `ORIGIN_ENV_SCRUB=0`.
pub fn scrub_env(cmd: &mut std::process::Command) {
    if std::env::var("ORIGIN_ENV_SCRUB").as_deref() == Ok("0") {
        return;
    }
    let passthrough = std::env::var("ORIGIN_ENV_PASSTHROUGH").unwrap_or_default();
    cmd.env_clear();
    cmd.envs(std::env::vars_os().filter(|(k, _)| k.to_str().is_some_and(|k| keep(k, &passthrough))));
}

#[cfg(test)]
mod tests {
    use super::{is_secret_env_name, keep};

    #[test]
    fn secret_names() {
        for s in [
            "ANTHROPIC_API_KEY",
            "my_api_key",
            "SLACK_TOKEN",
            "DB_PASSWORD",
            "CLIENT_SECRET",
            "AWS_REGION",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GEMINI_X",
            "OPENAI_BASE_URL",
            "GH_TOKEN",
            "ORIGIN_VAULT_KEYFILE",
        ] {
            assert!(is_secret_env_name(s), "{s}");
        }
        for s in ["PATH", "CARGO_HOME", "ORIGIN_SOCK", "TOKENIZERS_PARALLELISM"] {
            assert!(!is_secret_env_name(s), "{s}");
        }
    }

    #[test]
    fn keep_allow_list_and_passthrough() {
        assert!(keep("PATH", ""));
        assert!(keep("Path", ""), "Windows spells it Path");
        assert!(keep("LC_ALL", ""));
        assert!(!keep("ANTHROPIC_API_KEY", ""));
        assert!(!keep("RANDOM_VAR", ""));
        assert!(keep("RANDOM_VAR", "X, random_var"));
        assert!(
            keep("GITHUB_TOKEN", "GITHUB_TOKEN"),
            "explicit passthrough beats the secret filter"
        );
    }
}
