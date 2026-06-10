// SPDX-License-Identifier: Apache-2.0
//! Smoke test — supervisor restarts a SIGKILL'd daemon.
//!
//! Strategy: build a fake-daemon shell stub at runtime that just sleeps;
//! wait for its first launch, SIGKILL it, then assert the supervisor
//! re-spawns it. All waits are condition-based with generous ceilings so
//! loaded CI runners don't flake: the pass path exits each loop as soon as
//! the condition holds, so the ceilings never slow a healthy run.

#[cfg(unix)]
mod unix_only {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// Lines in the stub's run log = number of times the stub was launched.
    fn run_count(runs_path: &Path) -> usize {
        std::fs::read_to_string(runs_path).map_or(0, |s| s.lines().count())
    }

    /// Poll `runs_path` until it records at least `want` launches or the
    /// ceiling elapses; returns the final count either way.
    fn wait_for_runs(runs_path: &Path, want: usize, ceiling: Duration) -> usize {
        let start = Instant::now();
        loop {
            let n = run_count(runs_path);
            if n >= want || start.elapsed() >= ceiling {
                return n;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn restarts_dead_daemon() {
        let tmp = tempfile::tempdir().expect("tmp");
        // Per-run log inside the tempdir: unique path ⇒ no residue from (or
        // interference with) any other run on the same machine. The stub path
        // is unique too, so the pkill pattern below can't match anything else.
        let runs_path = tmp.path().join("runs.log");
        let stub_path = tmp.path().join("fake-daemon.sh");
        std::fs::write(
            &stub_path,
            format!(
                "#!/bin/sh\necho started $$ >> {}\nsleep 60\n",
                runs_path.display()
            ),
        )
        .expect("write stub");
        std::fs::set_permissions(&stub_path, std::fs::Permissions::from_mode(0o755)).expect("chmod");

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

        // Wait for the FIRST launch before killing — a fixed pre-kill sleep
        // raced slow runners (pkill fired before the stub existed, so nothing
        // died and no restart ever happened: the old flake).
        let first = wait_for_runs(&runs_path, 1, Duration::from_secs(10));
        if first < 1 {
            let _ = sup.kill();
            let _ = sup.wait();
        }
        assert!(first >= 1, "supervisor never launched the daemon stub within 10s");

        // Kill exactly the current stub instance (its PID is in the run log)
        // to force the restart path. The old `pkill -f <stub>` also matched
        // the SUPERVISOR's own cmdline (`--daemon-path <stub>`), so whether a
        // restart was ever observed depended on signal-delivery order — the
        // other half of the flake.
        let stub_pid = std::fs::read_to_string(&runs_path)
            .ok()
            .and_then(|s| {
                s.lines()
                    .last()
                    .and_then(|l| l.split_whitespace().nth(1).map(str::to_owned))
            })
            .expect("run log records the stub pid");
        let _ = Command::new("kill").args(["-9", &stub_pid]).status();

        let restart_count = wait_for_runs(&runs_path, 2, Duration::from_secs(10));
        let _ = sup.kill();
        let _ = sup.wait(); // reap the supervisor so it doesn't linger as a zombie
        assert!(
            restart_count >= 2,
            "supervisor should have launched the daemon at least twice (initial + 1 restart); got {restart_count}"
        );
    }
}
