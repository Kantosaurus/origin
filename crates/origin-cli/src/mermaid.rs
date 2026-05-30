// SPDX-License-Identifier: Apache-2.0
//! `origin mermaid` — render a mermaid flowchart to ASCII via the
//! dependency-free [`origin_mermaid`] renderer (jcode parity).

use std::io::Read;

use anyhow::Result;

/// Read `path` (or stdin when `path == "-"`), parse it as a mermaid flowchart,
/// and print the ASCII rendering.
///
/// # Errors
/// Returns on I/O failure or when the source is not a supported mermaid graph.
pub fn run(path: &str) -> Result<()> {
    let src = if path == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| anyhow::anyhow!("reading mermaid source from stdin: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?
    };

    let diagram = origin_mermaid::parse(&src).map_err(|e| anyhow::anyhow!("{e}"))?;
    println!("{}", origin_mermaid::render_ascii(&diagram));
    Ok(())
}
