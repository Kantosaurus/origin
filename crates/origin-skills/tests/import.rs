// SPDX-License-Identifier: Apache-2.0
use origin_skills::{first_run_import, ImportDecision, ImportReport};
use std::fs;
use tempfile::tempdir;

fn write_skill(root: &std::path::Path, name: &str, body: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: t\nallowed-tools: []\n---\n{body}\n"),
    )
    .expect("write");
}

#[test]
fn dedupes_against_existing_skills() {
    let src = tempdir().expect("src");
    let dst = tempdir().expect("dst");
    write_skill(src.path(), "alpha", "shared body");
    write_skill(dst.path(), "alpha", "shared body"); // already imported

    let report: ImportReport =
        first_run_import(src.path(), dst.path(), |_skill| ImportDecision::Accept).expect("import");
    assert_eq!(report.imported, 0, "exact-body match should not re-import");
    assert_eq!(report.skipped_duplicate, 1);
}

#[test]
fn user_can_reject_individual_skills() {
    let src = tempdir().expect("src");
    let dst = tempdir().expect("dst");
    write_skill(src.path(), "alpha", "body a");
    write_skill(src.path(), "beta", "body b");

    let report = first_run_import(src.path(), dst.path(), |skill| {
        if skill.front.name == "beta" {
            ImportDecision::Reject
        } else {
            ImportDecision::Accept
        }
    })
    .expect("import");
    assert_eq!(report.imported, 1);
    assert_eq!(report.rejected, 1);

    assert!(dst.path().join("alpha/SKILL.md").exists());
    assert!(!dst.path().join("beta/SKILL.md").exists());
}
