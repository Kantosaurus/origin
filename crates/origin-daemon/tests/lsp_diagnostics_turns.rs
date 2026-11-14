// SPDX-License-Identifier: Apache-2.0
//! C10: the per-turn LSP diagnostics path keeps its server across edit-only
//! turns, reports EVERY edited file to it as changed (not just the probed
//! ones), and recycles it after a turn whose mutations it cannot list.
//!
//! `harness = false`: this binary doubles as the fake language server. It is
//! hard-linked as `rust-analyzer` (the registry's server for `.rs`) into a
//! directory put first on `PATH`; launched under that name, it runs the
//! origin-lsp-client fixture, whose diagnostic `code` is its pid and whose
//! `LOG` document reports what it received.
#![allow(clippy::unwrap_used)]

#[path = "../../origin-lsp-client/tests/fixtures/fake_lsp_server.rs"]
mod fake;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use origin_daemon::lsp_diagnostics::lsp_diagnostics_block;
use origin_lsp_client::LspPool;

fn main() {
    let exe = std::env::current_exe().unwrap();
    if exe.file_stem().is_some_and(|s| s == "rust-analyzer") {
        fake::main();
        return;
    }
    let bin = exe
        .parent()
        .unwrap()
        .join(format!("lsp-fake-{}", std::process::id()));
    std::fs::create_dir_all(&bin).unwrap();
    let link = bin.join(format!("rust-analyzer{}", std::env::consts::EXE_SUFFIX));
    if std::fs::hard_link(&exe, &link).is_err() {
        std::fs::copy(&exe, &link).unwrap();
    }
    let mut path = vec![bin.clone()];
    path.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::set_var("PATH", std::env::join_paths(path).unwrap());
    std::env::set_var("ORIGIN_LSP_DIAGNOSTICS", "1");
    std::env::remove_var("ORIGIN_LSP_AUTO");

    let work = tempfile::tempdir().unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(turns(work.path()));
    let _ = std::fs::remove_dir_all(&bin);
    println!("lsp_diagnostics_turns: ok");
}

/// Server pid reported in the `<lsp-diagnostics>` line for `file`.
fn pid_for(block: &str, file: &Path) -> String {
    let name = file.file_name().unwrap().to_str().unwrap();
    let line = block
        .lines()
        .find(|l| l.contains(&format!("{name}:1:1 error[")))
        .unwrap_or_else(|| unreachable!("no line for {name} in {block}"));
    let start = line.find("error[").unwrap() + "error[".len();
    line[start..start + line[start..].find(']').unwrap()].to_owned()
}

fn uri(p: &Path) -> String {
    format!("file://{}", p.display().to_string().replace('\\', "/"))
}

async fn turns(work: &Path) {
    // More files than the per-turn probe cap (8).
    let files: Vec<PathBuf> = (0..10).map(|i| work.join(format!("f{i}.rs"))).collect();
    for f in &files {
        std::fs::write(f, "v1").unwrap();
    }
    let edited: BTreeSet<String> = files.iter().map(|f| f.display().to_string()).collect();
    let pool = LspPool::default();

    let block = lsp_diagnostics_block(&pool, work, &edited, false).await.unwrap();
    let pid1 = pid_for(&block, &files[0]);

    // Edit-only turn: same server, told about all ten files.
    std::fs::write(&files[0], "LOG").unwrap();
    let block = lsp_diagnostics_block(&pool, work, &edited, false).await.unwrap();
    assert_eq!(
        pid_for(&block, &files[0]),
        pid1,
        "edit-only turn replaced the server: {block}"
    );
    let log_line = block.lines().find(|l| l.contains("initialize:watch")).unwrap();
    for f in &files {
        let want = format!("changed:{}", uri(f));
        assert!(
            log_line.split_whitespace().any(|w| w == want),
            "{want} not reported: {log_line}"
        );
    }

    // A turn with a mutation whose paths are unknown (e.g. Bash): fresh server.
    std::fs::write(&files[0], "v3").unwrap();
    let block = lsp_diagnostics_block(&pool, work, &edited, true).await.unwrap();
    assert_ne!(
        pid_for(&block, &files[0]),
        pid1,
        "unlisted mutation did not recycle: {block}"
    );
}
