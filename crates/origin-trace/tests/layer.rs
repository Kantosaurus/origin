// SPDX-License-Identifier: Apache-2.0
#![allow(clippy::needless_collect, clippy::used_underscore_binding)]

use origin_trace::init;
use tempfile::tempdir;
use tracing::{info_span, instrument};

#[instrument(level = "info", fields(tool = "Read"))]
fn fake_tool(_arg: u32) {
    let _g = info_span!("inner", provider = "anthropic").entered();
    drop(_g);
}

#[test]
fn span_close_writes_a_row_to_the_ring() {
    let dir = tempdir().expect("tempdir");
    let guard = init(dir.path()).expect("init layer");
    fake_tool(1);
    // Allow the SPSC drain to fire.
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(guard); // forces flush via Drop
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("readdir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("parquet"))
        .collect();
    assert!(
        !files.is_empty(),
        "expected at least one parquet file after span close"
    );
}
