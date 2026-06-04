// SPDX-License-Identifier: Apache-2.0
//! Live-process integration test for the self-dev relaunch round-trip: a real
//! child exits with the relaunch sentinel (86) after writing a manifest, and the
//! supervisor swaps in the freshly-built binary and relaunches it.
//!
//! The pure swap/rollback/decision logic is unit-tested in `src/relaunch.rs`;
//! this test exercises the whole `run_child` → exit-86 → `perform_swap` →
//! relaunch chain through the real `origin-supervisor` binary, cross-platform
//! (no `#[cfg(unix)]` gate, no signals).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Trailing bytes appended to the v2 copy so the fixture knows it is the
/// swapped-in generation. Kept in sync with the same constant in the fixture.
const V2_SENTINEL: &[u8] = b"\n# origin-selfdev-v2\n";

fn exe_name(stem: &str) -> String {
    format!("{stem}{}", std::env::consts::EXE_SUFFIX)
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}
#[cfg(not(unix))]
const fn make_executable(_path: &Path) {}

/// The local-data base the spawned supervisor will resolve, given we override
/// its platform env var to `root`. Mirrors `relaunch::data_local_dir_with`.
fn data_local_base(root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        root.to_path_buf()
    }
    #[cfg(target_os = "macos")]
    {
        root.join("Library").join("Application Support")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        root.to_path_buf()
    }
}

/// Apply the per-OS local-data env override so the supervisor's
/// `default_relaunch_manifest_path()` lands under `root`.
fn set_local_data_env(cmd: &mut Command, root: &Path) {
    #[cfg(windows)]
    cmd.env("LOCALAPPDATA", root);
    #[cfg(target_os = "macos")]
    cmd.env("HOME", root);
    #[cfg(all(unix, not(target_os = "macos")))]
    cmd.env("XDG_DATA_HOME", root);
}

#[test]
fn child_exiting_86_is_swapped_and_v2_relaunched() {
    let fixture = env!("CARGO_BIN_EXE_selfdev-fixture-daemon");
    let supervisor = env!("CARGO_BIN_EXE_origin-supervisor");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // v1 = the fixture verbatim (no sentinel ⇒ v1 role), installed at daemon_path.
    let daemon_path = root.join(exe_name("origin-daemon"));
    std::fs::copy(fixture, &daemon_path).unwrap();
    make_executable(&daemon_path);
    let v1_bytes = std::fs::read(&daemon_path).unwrap();

    // v2 = fixture + sentinel (⇒ v2 role), referenced by the manifest and swapped in.
    let v2_dir = root.join("v2");
    std::fs::create_dir_all(&v2_dir).unwrap();
    let v2_path = v2_dir.join(exe_name("origin-daemon"));
    let mut v2_bytes = std::fs::read(fixture).unwrap();
    v2_bytes.extend_from_slice(V2_SENTINEL);
    std::fs::write(&v2_path, &v2_bytes).unwrap();
    make_executable(&v2_path);
    assert_ne!(v1_bytes, v2_bytes, "v1 and v2 must differ for a meaningful swap");

    let manifest_path = data_local_base(root)
        .join("origin")
        .join("selfdev")
        .join("relaunch.json");
    let marker_path = root.join("v2-ran.marker");
    let stop_path = root.join("stop");
    // `backup_path_for` appends ".bak" to the whole file name.
    let backup_path = daemon_path.with_file_name(format!(
        "{}.bak",
        daemon_path.file_name().unwrap().to_string_lossy()
    ));

    let mut cmd = Command::new(supervisor);
    cmd.arg("--daemon-path")
        .arg(&daemon_path)
        .arg("--max-restarts-per-min")
        .arg("30")
        .env("ORIGIN_FIXTURE_MANIFEST", &manifest_path)
        .env("ORIGIN_FIXTURE_MARKER", &marker_path)
        .env("ORIGIN_FIXTURE_STOP", &stop_path)
        .env("ORIGIN_FIXTURE_V2", &v2_path)
        .env("ORIGIN_FIXTURE_CURRENT", &daemon_path);
    set_local_data_env(&mut cmd, root);
    let mut sup = cmd.spawn().expect("spawn supervisor");

    // Poll until the swapped-in v2 has run (marker written). v2 then idles on the
    // stop file, so the supervisor stays blocked in run_child and the swap state
    // below is stable while we assert.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut observed = false;
    while Instant::now() < deadline {
        if marker_path.exists() {
            observed = true;
            break;
        }
        if matches!(sup.try_wait(), Ok(Some(_))) {
            break; // supervisor exited early (e.g. restart storm) — fail below.
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Read the swap state while it is stable (supervisor blocked in run_child on
    // the idling v2). Capture before teardown so the assertions run on a snapshot.
    let snapshot = observed.then(|| {
        (
            std::fs::read_to_string(&marker_path).unwrap(),
            std::fs::read(&backup_path).ok(),
            std::fs::read(&daemon_path).unwrap(),
            manifest_path.exists(),
        )
    });

    // Tear down: stop the supervisor first (so it can't restart/roll back), then
    // release the idling v2 child via the stop file, then wait for it to release
    // daemon_path so the tempdir can be cleaned up (best-effort).
    let _ = sup.kill();
    let _ = sup.wait();
    let _ = std::fs::write(&stop_path, b"stop");
    let drain = Instant::now() + Duration::from_secs(5);
    while Instant::now() < drain && std::fs::remove_file(&daemon_path).is_err() {
        std::thread::sleep(Duration::from_millis(50));
    }

    let (marker, backup, installed, manifest_exists) = snapshot.expect(
        "v2 marker was never written — the exit-86 swap+relaunch did not complete",
    );
    // The marker is written only by the sentinel-bearing v2, and records its own
    // length: proof that perform_swap installed v2 and the supervisor relaunched it.
    assert!(
        marker.contains(&format!("len={}", v2_bytes.len())),
        "marker should record the v2 byte length; got {marker:?}"
    );
    // The swap backed up the original v1 binary.
    assert_eq!(
        backup.as_deref(),
        Some(v1_bytes.as_slice()),
        "backup (.bak) should be the original v1 binary"
    );
    // daemon_path held the swapped-in v2 bytes at snapshot time.
    assert_eq!(installed, v2_bytes, "daemon_path should hold the swapped-in v2 bytes");
    // The manifest is consumed (deleted) after a successful swap.
    assert!(!manifest_exists, "relaunch manifest should be deleted after the swap");
}

#[test]
fn fixture_v1_writes_manifest_and_v2_writes_marker() {
    use origin_supervisor::relaunch::{load_manifest, SELFDEV_RELAUNCH_EXIT_CODE};

    let fixture = env!("CARGO_BIN_EXE_selfdev-fixture-daemon");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // Part A: the base fixture (no sentinel) acts as v1 — writes the manifest and
    // exits with the sentinel code.
    let manifest_path = root.join("relaunch.json");
    let v2_path = root.join("v2-bin");
    let current = root.join("current-bin");
    let status = Command::new(fixture)
        .env("ORIGIN_FIXTURE_MANIFEST", &manifest_path)
        .env("ORIGIN_FIXTURE_V2", &v2_path)
        .env("ORIGIN_FIXTURE_CURRENT", &current)
        .status()
        .expect("run v1 fixture");
    assert_eq!(status.code(), Some(SELFDEV_RELAUNCH_EXIT_CODE));
    let manifest = load_manifest(&manifest_path)
        .expect("read manifest")
        .expect("manifest present");
    assert_eq!(manifest.new_binary_path, v2_path);
    assert_eq!(manifest.previous_binary_path, current);
    assert_eq!(manifest.generation, 1);

    // Part B: a sentinel-bearing copy acts as v2 — writes the marker and exits 0.
    let v2_copy = root.join(exe_name("v2-fixture"));
    let mut bytes = std::fs::read(fixture).unwrap();
    bytes.extend_from_slice(V2_SENTINEL);
    std::fs::write(&v2_copy, &bytes).unwrap();
    make_executable(&v2_copy);
    let marker_path = root.join("marker");
    let stop_path = root.join("stop");
    std::fs::write(&stop_path, b"stop").unwrap(); // pre-set so v2 exits immediately
    let status = Command::new(&v2_copy)
        .env("ORIGIN_FIXTURE_MARKER", &marker_path)
        .env("ORIGIN_FIXTURE_STOP", &stop_path)
        .status()
        .expect("run v2 fixture");
    assert_eq!(status.code(), Some(0));
    let marker = std::fs::read_to_string(&marker_path).unwrap();
    assert!(marker.contains(&format!("len={}", bytes.len())), "marker={marker:?}");
}
