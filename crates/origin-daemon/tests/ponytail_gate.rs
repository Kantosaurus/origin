// SPDX-License-Identifier: Apache-2.0
//! Task 12: lock the dep-extraction + classification wiring the pre-dispatch
//! ponytail gate depends on. The full interactive prompter round-trip is covered
//! by the `ipc_prompter` choice tests; here we pin that (a) a Write adding a
//! replaceable dep is flagged in `full`, and (b) a bare `npm install` (no named
//! package) is never flagged — the highest-leverage false-positive guard.

use std::collections::BTreeSet;

#[test]
fn write_adding_lodash_is_flagged_in_full() {
    let args = serde_json::json!({
        "file_path": "package.json",
        "content": r#"{"dependencies":{"lodash":"4"}}"#
    });
    // Mirror `ponytail_added_deps` for a Write with no prior on-disk file.
    let content = args["content"].as_str().expect("content is a JSON string");
    let deps = origin_ponytail::manifest_deps_added("package.json", None, content);
    let allow: BTreeSet<String> = BTreeSet::new();
    let flags = origin_ponytail::classify(&deps, origin_ponytail::PonytailMode::Full, &allow);
    assert_eq!(flags.len(), 1);
    assert_eq!(flags[0].dep.name, "lodash");
}

#[test]
fn bare_npm_install_not_flagged() {
    // Installing existing manifest deps is NOT adding a dep ⇒ extraction is empty
    // ⇒ the gate is a no-op.
    let deps = origin_ponytail::bash_installs("npm install");
    assert!(deps.is_empty());
}

#[test]
fn off_mode_flags_nothing_even_for_replaceable_dep() {
    let deps = origin_ponytail::bash_installs("npm install lodash");
    assert_eq!(deps.len(), 1);
    let allow: BTreeSet<String> = BTreeSet::new();
    let flags = origin_ponytail::classify(&deps, origin_ponytail::PonytailMode::Off, &allow);
    assert!(flags.is_empty());
}
