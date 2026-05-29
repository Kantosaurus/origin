// SPDX-License-Identifier: Apache-2.0
use std::process::Command;
use tempfile::tempdir;

#[test]
fn report_subcommand_consumes_a_json_file() {
    let dir = tempdir().expect("tempdir");
    let in_path = dir.path().join("r.json");
    std::fs::write(
        &in_path,
        r#"[{"contestant":"origin","task_id":"01-x","input_tokens":1,"output_tokens":1,"wall_ms":1,"tool_calls":0,"passed":true}]"#,
    )
    .expect("write input");
    let out_path = dir.path().join("out.md");

    let status = Command::new(env!("CARGO_BIN_EXE_origin-bench"))
        .args([
            "report",
            "--results",
            in_path.to_str().expect("utf8"),
            "--out",
            out_path.to_str().expect("utf8"),
        ])
        .status()
        .expect("spawn");
    assert!(status.success());
    let body = std::fs::read_to_string(&out_path).expect("read out");
    assert!(body.contains("origin"));
}
