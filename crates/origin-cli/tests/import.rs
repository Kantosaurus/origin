// SPDX-License-Identifier: Apache-2.0
use origin_cli::import::{run_import, ImportArgs, ImportSource};
use std::path::PathBuf;

#[test]
fn dry_run_against_claude_code_fixture_summarizes() {
    let args = ImportArgs {
        source: ImportSource::ClaudeCode,
        from: PathBuf::from("../origin-migrate/tests/fixtures/claude-code"),
        apply: false,
        json: true,
        db: None,
    };
    let report = run_import(&args).expect("run import");
    assert_eq!(report.sessions_inserted, 1);
    assert_eq!(report.skills_inserted, 1);
}
