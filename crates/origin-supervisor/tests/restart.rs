// SPDX-License-Identifier: Apache-2.0
//! Smoke test — supervisor restarts a SIGKILL'd daemon within 2 s.
//!
//! Strategy: build a fake-daemon shell stub at runtime that just sleeps; SIGKILL
//! it after 200 ms; assert the supervisor re-spawns it.

#[cfg(unix)]
mod unix_only {
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn restarts_dead_daemon_within_2s() {
        let tmp = tempfile::tempdir().expect("tmp");
        // A trivial daemon stub.
        let stub_path = tmp.path().join("fake-daemon.sh");
        std::fs::write(
            &stub_path,
            "#!/bin/sh\necho started $$ >> /tmp/origin-supervisor-runs\nsleep 60\n",
        )
        .expect("write stub");
        std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let _ = std::fs::remove_file("/tmp/origin-supervisor-runs");

        let mut sup = Command::new(env!("CARGO_BIN_EXE_origin-supervisor"))
            .args([
                "--daemon-path",
                stub_path.to_str().expect("utf8"),
                "--max-restarts-per-min",
                "10",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn supervisor");
        std::thread::sleep(Duration::from_millis(300));

        // Kill any current fake-daemon to force the restart path.
        let _ = Command::new("pkill").args(["-f", "fake-daemon.sh"]).status();
        let start = Instant::now();
        let mut restart_count = 0_usize;
        while start.elapsed() < Duration::from_secs(2) {
            if let Ok(s) = std::fs::read_to_string("/tmp/origin-supervisor-runs") {
                restart_count = s.lines().count();
                if restart_count >= 2 {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = sup.kill();
        let _ = sup.wait(); // reap the supervisor so it doesn't linger as a zombie
        assert!(
            restart_count >= 2,
            "supervisor should have launched the daemon at least twice (initial + 1 restart); got {restart_count}"
        );
    }
}
