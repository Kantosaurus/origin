// SPDX-License-Identifier: Apache-2.0
//! Manpage generation via `clap_mangen` (P14.D.4).
//!
//! Renders `origin.1` plus one `<sub>.1` per registered subcommand into the
//! requested output directory. Introspection uses `origin_cli::main_cli()`
//! so we never have to depend on the binary crate.

use clap_mangen::Man;
use std::fs;
use std::path::Path;

/// Generate manpages for `origin` and every subcommand into `out_dir`.
///
/// # Errors
/// Returns any filesystem error or any error surfaced by `clap_mangen`.
pub fn generate(out_dir: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(out_dir)?;
    let cmd = origin_cli::main_cli();
    write_recursive(&cmd, out_dir)?;
    Ok(())
}

fn write_recursive(cmd: &clap::Command, out_dir: &Path) -> anyhow::Result<()> {
    let name = cmd.get_name().to_string();
    let man = Man::new(cmd.clone());
    let mut buf: Vec<u8> = Vec::new();
    man.render(&mut buf)?;
    fs::write(out_dir.join(format!("{name}.1")), buf)?;
    for sub in cmd.get_subcommands() {
        write_recursive(sub, out_dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::generate;
    use tempfile::tempdir;

    #[test]
    fn generates_at_least_origin_1() {
        let dir = tempdir().expect("tempdir");
        generate(dir.path()).expect("gen");
        assert!(dir.path().join("origin.1").exists());
    }
}
