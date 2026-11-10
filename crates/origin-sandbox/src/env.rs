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
    // Windows: node-gyp and Python's MSVC discovery find `vswhere.exe` under
    // these; the rest are path/count values common build scripts read.
    "ProgramFiles",
    "ProgramFiles(x86)",
    "windir",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    // Project resolution: without these a child (test run, language server)
    // reports false import/resolution failures. No registry/proxy URL vars here:
    // those can embed `user:token@` credentials (use ORIGIN_ENV_PASSTHROUGH).
    "PYTHONPATH",
    "PYTHONHOME",
    "GOFLAGS",
    "GOMODCACHE",
    "GOPRIVATE",
    "GONOSUMDB",
    "GONOPROXY",
    "GOWORK",
    "RUSTUP_TOOLCHAIN",
    "RUSTFLAGS",
    "CARGO_TARGET_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "DOTNET_ROOT",
    "NODE_PATH",
    // Browser sidecars (agent-browser, CloakBrowser): where Playwright keeps
    // its browsers, and the display a headed browser opens on.
    "PLAYWRIGHT_BROWSERS_PATH",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_RUNTIME_DIR",
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

/// C9: the narrower set whose VALUES are redacted by value from tool output:
/// `*_API_KEY`, `*_TOKEN`, `*_SECRET`, `*_PASSWORD`, `GITHUB_TOKEN`, `GH_TOKEN`,
/// `ORIGIN_*_KEY`, `AWS_SECRET_ACCESS_KEY` and `PGPASSWORD` (case-insensitive). Unlike
/// [`is_secret_env_name`] it omits the whole-vendor prefixes (`AWS_REGION`,
/// `OPENAI_BASE_URL`, `ANTHROPIC_MODEL`…): scrubbing too much only drops a
/// variable, but redacting too much rewrites ordinary text.
#[must_use]
pub fn is_redactable_secret_env_name(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    ["_API_KEY", "_TOKEN", "_SECRET", "_PASSWORD"]
        .iter()
        .any(|s| n.ends_with(s))
        || n == "GITHUB_TOKEN"
        || n == "GH_TOKEN"
        || n == "AWS_SECRET_ACCESS_KEY"
        || n == "PGPASSWORD"
        // `ORIGIN_*_KEY` only: `ORIGIN_VAULT_KEYFILE` / `*_KEY_FILE` hold paths.
        || (n.starts_with("ORIGIN_") && n.ends_with("_KEY"))
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
    use super::{is_redactable_secret_env_name, is_secret_env_name, keep};

    #[test]
    fn redactable_names_are_credentials_only() {
        for s in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "AZURE_CLIENT_SECRET",
            "DB_PASSWORD",
            "PGPASSWORD",
            "ORIGIN_VAULT_KEY",
            "ORIGIN_BRAVE_KEY",
            "ORIGIN_TAVILY_KEY",
        ] {
            assert!(is_redactable_secret_env_name(s), "{s}");
        }
        for s in [
            // Paths / config, not secrets.
            "ORIGIN_VAULT_KEYFILE",
            "ORIGIN_REMOTE_CLIENT_KEY_FILE",
            "ORIGIN_KEYVAULT",
            "AWS_REGION",
            "AWS_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "OPENAI_BASE_URL",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_MODEL",
            "GOOGLE_CLOUD_PROJECT",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "PATH",
        ] {
            assert!(!is_redactable_secret_env_name(s), "{s}");
        }
    }

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
        for n in [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "windir",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ] {
            assert!(keep(n, ""), "Windows build discovery needs {n}");
        }
        for n in [
            "PYTHONPATH",
            "PYTHONHOME",
            "GOFLAGS",
            "GOMODCACHE",
            "GOPRIVATE",
            "GONOSUMDB",
            "GONOPROXY",
            "GOWORK",
            "RUSTUP_TOOLCHAIN",
            "RUSTFLAGS",
            "CARGO_TARGET_DIR",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "DOTNET_ROOT",
            "NODE_PATH",
        ] {
            assert!(keep(n, ""), "project resolution needs {n}");
        }
        for n in [
            "PLAYWRIGHT_BROWSERS_PATH",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "XDG_RUNTIME_DIR",
        ] {
            assert!(keep(n, ""), "browser sidecars need {n}");
        }
        for n in [
            "GOPROXY",
            "PIP_INDEX_URL",
            "NPM_CONFIG_REGISTRY",
            "CARGO_REGISTRY_TOKEN",
        ] {
            assert!(!keep(n, ""), "{n} may carry credentials");
        }
        assert!(!keep("RANDOM_VAR", ""));
        assert!(keep("RANDOM_VAR", "X, random_var"));
        assert!(
            keep("GITHUB_TOKEN", "GITHUB_TOKEN"),
            "explicit passthrough beats the secret filter"
        );
    }
}
