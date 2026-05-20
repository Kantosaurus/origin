//! Per-file allowlist for the `tokio::spawn` ban.
//!
//! Entries are workspace-relative paths. A path is allowed if it matches
//! any prefix in this list. Keep entries minimal and add a justification.

pub const ALLOWLIST: &[&str] = &[
    // The only sanctioned spawn site — `spawn_in` itself.
    "crates/origin-runtime/src/spawn.rs",
    // Sidecar runtime pre-dates the migration; covered by a P14 follow-up.
    "crates/origin-sidecar/src/runtime.rs",
    // Supervisor launches the daemon child via tokio::process::Command::spawn,
    // which is a different `spawn` and not the lint target — but we list the
    // file here too to make the intent explicit.
    "crates/origin-supervisor/src/launch_unix.rs",
    "crates/origin-supervisor/src/launch_windows.rs",
    // Provider crates carry a few one-off keepalive tasks; tracked for P14.
    "crates/origin-provider-anthropic/src",
    "crates/origin-provider-openai/src",
    "crates/origin-provider-gemini/src",
    "crates/origin-provider-ollama/src",
    "crates/origin-provider-bedrock/src",
    "crates/origin-provider-openrouter/src",
    "crates/origin-provider-github/src",
];

#[must_use]
pub fn is_allowlisted(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    ALLOWLIST.iter().any(|prefix| normalized.contains(prefix))
}
