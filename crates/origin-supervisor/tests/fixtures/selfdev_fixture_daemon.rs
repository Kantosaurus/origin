// SPDX-License-Identifier: Apache-2.0
//! Test fixture binary that stands in for `origin-daemon` in the supervisor's
//! self-dev relaunch round-trip integration test (`tests/relaunch_e2e.rs`).
//!
//! Its role is **content-driven**, not argv/env-driven, because the supervisor
//! forwards the same args + env to every generation it spawns (and after a swap
//! the same path holds different bytes). The fixture reads its own executable:
//!
//! - **v1** (no sentinel): writes a [`origin_supervisor::relaunch::RelaunchManifest`]
//!   to `ORIGIN_FIXTURE_MANIFEST` and exits with `SELFDEV_RELAUNCH_EXIT_CODE` (86),
//!   asking the supervisor to swap in `ORIGIN_FIXTURE_V2`.
//! - **v2** (the `ORIGIN_FIXTURE_V2` copy, which the test built by appending
//!   [`V2_SENTINEL`]): writes a marker to `ORIGIN_FIXTURE_MARKER` recording its
//!   own path + byte length (proving the swapped-in bytes ran), then waits for
//!   `ORIGIN_FIXTURE_STOP` to appear (so the test can hold the supervisor blocked
//!   in `run_child` while it asserts the stable swap state) before exiting 0.
//!   A generous fallback timeout guarantees it never hangs CI.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use origin_supervisor::relaunch::{RelaunchManifest, SELFDEV_RELAUNCH_EXIT_CODE};

/// Trailing bytes the test appends to the v2 copy so the running binary can tell
/// it is the swapped-in generation. Kept in sync with the same constant in
/// `tests/relaunch_e2e.rs`.
const V2_SENTINEL: &[u8] = b"\n# origin-selfdev-v2\n";

/// Exit code for any unexpected I/O failure — distinct from 0 (clean) and 86
/// (relaunch sentinel) so a fixture fault never masquerades as either.
const FIXTURE_IO_ERROR: i32 = 3;

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn main() {
    let Ok(exe) = std::env::current_exe() else {
        std::process::exit(FIXTURE_IO_ERROR);
    };
    let Ok(own_bytes) = std::fs::read(&exe) else {
        std::process::exit(FIXTURE_IO_ERROR);
    };

    if own_bytes.ends_with(V2_SENTINEL) {
        run_as_v2(&exe, own_bytes.len());
    } else {
        run_as_v1();
    }
}

/// v1: hand the supervisor a relaunch manifest, then exit with the sentinel.
fn run_as_v1() -> ! {
    let manifest_path = env("ORIGIN_FIXTURE_MANIFEST");
    let manifest = RelaunchManifest {
        new_binary_path: PathBuf::from(env("ORIGIN_FIXTURE_V2")),
        previous_binary_path: PathBuf::from(env("ORIGIN_FIXTURE_CURRENT")),
        generation: 1,
    };
    if let Some(parent) = Path::new(&manifest_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(bytes) = serde_json::to_vec(&manifest) else {
        std::process::exit(FIXTURE_IO_ERROR);
    };
    if std::fs::write(&manifest_path, bytes).is_err() {
        std::process::exit(FIXTURE_IO_ERROR);
    }
    std::process::exit(SELFDEV_RELAUNCH_EXIT_CODE);
}

/// v2: record that the swapped-in binary ran, then idle until told to stop.
fn run_as_v2(exe: &Path, own_len: usize) -> ! {
    let marker_path = env("ORIGIN_FIXTURE_MARKER");
    let marker = format!("v2 ran exe={} len={}", exe.display(), own_len);
    if std::fs::write(&marker_path, marker).is_err() {
        std::process::exit(FIXTURE_IO_ERROR);
    }

    // Stay alive (so the supervisor is blocked in `run_child` and the swap state
    // is stable for the test's assertions) until the test drops the stop file,
    // with a fallback so a crashed test can never leave this process hanging.
    let stop = env("ORIGIN_FIXTURE_STOP");
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if !stop.is_empty() && Path::new(&stop).exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    std::process::exit(0);
}
