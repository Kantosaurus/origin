// SPDX-License-Identifier: Apache-2.0
use origin_bench::task_set::load;
use std::path::PathBuf;

#[test]
fn task_set_has_eight_tasks() {
    let root = PathBuf::from("../../bench/tasks");
    let tasks = load(&root).expect("load");
    assert_eq!(tasks.len(), 8);
    for t in &tasks {
        assert!(!t.id.is_empty());
        assert!(!t.prompt.is_empty());
    }
}
