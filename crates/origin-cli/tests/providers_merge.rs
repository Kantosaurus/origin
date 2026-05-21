//! Regression coverage for the bug fixed in 1b49d96:
//! `origin providers ls` and `origin providers describe` must merge entries
//! from `~/.origin/providers.toml` on top of the builtin catalog, just like
//! the daemon does at startup. Before 1b49d96 the CLI surface only consulted
//! `Catalog::builtin()`, so custom providers were invisible.
//!
//! The CLI binary is exec'd with `ORIGIN_HOME` pointed at a scratch
//! directory containing a single `smoke-test` provider. Setting the env var
//! on the child only avoids the multi-thread `set_var` hazard that
//! Rust 1.83 flags as `unsafe`.

use std::fs;
use std::process::Command;

fn write_providers_toml(home: &std::path::Path) {
    let dir = home.join(".origin");
    fs::create_dir_all(&dir).expect("mkdir .origin");
    let body = r#"
[providers.smoke-test]
display_name = "Smoke Test"
wire = "openai-chat"
base_url = "https://example.invalid/v1"
default_model = "smoke-1"

[providers.smoke-test.auth]
kind = "api-key"
header = "Authorization"
prefix = "Bearer "
"#;
    fs::write(dir.join("providers.toml"), body).expect("write providers.toml");
}

#[test]
fn ls_includes_custom_provider_from_user_toml() {
    let home = tempfile::TempDir::new().expect("tempdir");
    write_providers_toml(home.path());

    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .env("ORIGIN_HOME", home.path())
        .args(["providers", "ls"])
        .output()
        .expect("spawn origin providers ls");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "providers ls failed: status={:?}\nstdout=\n{stdout}\nstderr=\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("smoke-test"),
        "custom provider missing from `providers ls` stdout:\n{stdout}"
    );
}

#[test]
fn describe_returns_custom_provider_from_user_toml() {
    let home = tempfile::TempDir::new().expect("tempdir");
    write_providers_toml(home.path());

    let out = Command::new(env!("CARGO_BIN_EXE_origin"))
        .env("ORIGIN_HOME", home.path())
        .args(["providers", "describe", "smoke-test"])
        .output()
        .expect("spawn origin providers describe");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "providers describe failed: status={:?}\nstdout=\n{stdout}\nstderr=\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("https://example.invalid/v1"),
        "base_url missing from `providers describe smoke-test` stdout:\n{stdout}"
    );
}
