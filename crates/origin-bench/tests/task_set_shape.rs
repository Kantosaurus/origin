// SPDX-License-Identifier: Apache-2.0
use origin_bench::task_set::load;
use std::path::PathBuf;

#[test]
fn perf_task_set_has_two_read_only_probes() {
    // The origin-specific A/B task set (`bench/tasks`) was retired in favour of
    // SWE-bench Verified (`bench/swe`); `origin-bench` now only drives the two
    // read-only latency probes the perf-gate asserts on (`bench/perf/tasks`).
    let root = PathBuf::from("../../bench/perf/tasks");
    let tasks = load(&root).expect("load");
    assert_eq!(tasks.len(), 2);
    for t in &tasks {
        assert!(!t.id.is_empty());
        assert!(!t.prompt.is_empty());
    }
}
