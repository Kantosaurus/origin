// SPDX-License-Identifier: Apache-2.0
//! `origin copy-context` / `apply-clipboard` — copy/paste web-chat mode.
//!
//! `copy-context` bundles files plus an instruction into a prompt-ready block
//! and writes it to the OS clipboard; `apply-clipboard` reads the clipboard,
//! parses an LLM's pasted reply into structured edits, and applies them
//! (aider `--copy-paste` / `/copy-context` / `--apply-clipboard-edits`). The
//! formatting and parsing logic lives in the pure [`origin_clipboard`] crate.

use std::io::Write as _;
use std::path::{Component, PathBuf};
use std::process::{Command, Stdio};

use anyhow::Result;
use origin_clipboard::{
    format_for_paste, os_copy_command, os_paste_command, parse_pasted_edits, ContextBundle, EditBlock,
};

/// Run `origin copy-context`: bundle `files` + `instruction` to the clipboard.
///
/// # Errors
/// Returns on a file read failure or when the clipboard program cannot be run.
pub fn copy_context(instruction: Option<String>, files: &[String]) -> Result<()> {
    let mut bundle_files: Vec<(String, String)> = Vec::with_capacity(files.len());
    for path in files {
        let contents = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("reading {path}: {e}"))?;
        bundle_files.push((path.clone(), contents));
    }
    let count = bundle_files.len();
    let bundle = ContextBundle::new(bundle_files, instruction.unwrap_or_default());
    let payload = format_for_paste(&bundle);

    let (prog, args) = os_copy_command();
    pipe_to_clipboard(prog, &args, &payload)?;
    println!("copied {count} files to clipboard");
    Ok(())
}

/// Write `text` to the local OS clipboard via the platform copy program.
///
/// Uses `clip`/`pbcopy`/`wl-copy`/`xclip`. Best-effort companion to
/// [`osc52_sequence`]: the OSC 52 escape covers remote/SSH terminals, while this
/// covers the local clipboard reliably.
///
/// # Errors
/// Returns if the clipboard program cannot be spawned or exits non-zero.
pub fn copy_to_os_clipboard(text: &str) -> Result<()> {
    let (prog, args) = os_copy_command();
    pipe_to_clipboard(prog, &args, text)
}

/// Spawns the clipboard-write program and feeds `payload` on its stdin.
fn pipe_to_clipboard(prog: &str, args: &[String], payload: &str) -> Result<()> {
    let mut child = Command::new(prog)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawning {prog}: {e}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("clipboard program stdin unavailable"))?;
        stdin
            .write_all(payload.as_bytes())
            .map_err(|e| anyhow::anyhow!("writing to {prog}: {e}"))?;
    }
    let status = child
        .wait()
        .map_err(|e| anyhow::anyhow!("waiting on {prog}: {e}"))?;
    if !status.success() {
        anyhow::bail!("{prog} exited with status {status}");
    }
    Ok(())
}

/// Run `origin apply-clipboard`: parse pasted edits and apply them to disk.
///
/// # Errors
/// Returns when the clipboard cannot be read or an edit cannot be applied.
#[allow(clippy::module_name_repetitions)] // `apply_clipboard` mirrors the subcommand name.
pub fn apply_clipboard() -> Result<()> {
    let (prog, args) = os_paste_command();
    let output = Command::new(prog)
        .args(&args)
        .output()
        .map_err(|e| anyhow::anyhow!("spawning {prog}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{prog} failed: {}", stderr.trim());
    }
    let pasted = String::from_utf8_lossy(&output.stdout);
    let edits = parse_pasted_edits(&pasted);
    if edits.is_empty() {
        println!("no applicable edits found in clipboard");
        return Ok(());
    }

    let mut applied = 0usize;
    for edit in edits {
        match apply_edit(&edit) {
            Ok(summary) => {
                println!("{summary}");
                applied += 1;
            }
            Err(e) => println!("skipped: {e}"),
        }
    }
    println!("applied {applied} edit(s)");
    Ok(())
}

/// Confine an LLM-supplied edit path to the current working directory tree.
///
/// `apply-clipboard` applies edit blocks parsed from untrusted model output. A
/// pasted reply that names `/etc/cron.d/x`, `~/.ssh/authorized_keys`, a Windows
/// `C:\...`/UNC path, or `../../../.bashrc` would otherwise turn a clipboard
/// paste into an arbitrary file write anywhere the user can reach. Resolve the
/// path lexically (without touching the filesystem, so a hostile symlink cannot
/// race the check) and reject anything absolute, rooted, or escaping the
/// working directory via `..`.
fn confine_to_cwd(file: &str) -> Result<PathBuf> {
    let cwd = std::env::current_dir().map_err(|e| anyhow::anyhow!("resolving working directory: {e}"))?;
    let raw = std::path::Path::new(file);
    if raw.is_absolute() {
        anyhow::bail!("refusing to write outside the working directory: absolute path `{file}`");
    }
    let mut normalized = PathBuf::new();
    for comp in raw.components() {
        match comp {
            Component::Normal(seg) => normalized.push(seg),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    anyhow::bail!("refusing to write outside the working directory: `{file}` escapes via `..`");
                }
            }
            // A drive prefix (`C:\`, `\\server\share`) or a bare root (`\foo`)
            // is rooted even when `is_absolute` is false (e.g. `C:foo` on
            // Windows); reject explicitly rather than silently rebasing it.
            Component::Prefix(_) | Component::RootDir => {
                anyhow::bail!("refusing to write outside the working directory: rooted path `{file}`");
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        anyhow::bail!("refusing to write: empty edit path");
    }
    Ok(cwd.join(normalized))
}

/// Applies one [`EditBlock`] to disk, returning a one-line summary.
fn apply_edit(edit: &EditBlock) -> Result<String> {
    match edit {
        EditBlock::WholeFile { file, contents } => {
            let path = confine_to_cwd(file)?;
            std::fs::write(&path, contents).map_err(|e| anyhow::anyhow!("writing {file}: {e}"))?;
            Ok(format!("wrote {file} ({} bytes)", contents.len()))
        }
        EditBlock::SearchReplace {
            file,
            search,
            replace,
        } => {
            let path = confine_to_cwd(file)?;
            let original =
                std::fs::read_to_string(&path).map_err(|e| anyhow::anyhow!("reading {file}: {e}"))?;
            let Some(idx) = original.find(search.as_str()) else {
                anyhow::bail!("search text not found in {file}");
            };
            let mut updated = String::with_capacity(original.len());
            updated.push_str(&original[..idx]);
            updated.push_str(replace);
            updated.push_str(&original[idx + search.len()..]);
            std::fs::write(&path, updated).map_err(|e| anyhow::anyhow!("writing {file}: {e}"))?;
            Ok(format!("patched {file}"))
        }
    }
}

/// Encode `data` as standard base64 (RFC 4648, `=`-padded). Small, pure, and
/// dependency-free — just enough for an OSC 52 clipboard write.
#[must_use]
pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(T[(b0 >> 2) as usize] as char);
        out.push(T[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Build an OSC 52 escape sequence that copies `text` to the system clipboard.
/// Works over SSH because the terminal (not the host) owns the clipboard.
#[must_use]
pub fn osc52_sequence(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{base64_encode, confine_to_cwd, osc52_sequence};

    #[test]
    fn confine_allows_paths_inside_cwd() {
        let cwd = std::env::current_dir().expect("cwd");
        let p = confine_to_cwd("src/main.rs").expect("relative path allowed");
        assert!(p.starts_with(&cwd), "resolved path must stay under cwd: {p:?}");
        assert!(p.ends_with("src/main.rs") || p.ends_with(r"src\main.rs"));
        // Interior `..` that does not escape is normalized, not rejected.
        let p = confine_to_cwd("a/../b/c.txt").expect("non-escaping .. allowed");
        assert!(p.starts_with(&cwd));
        assert!(p.ends_with("b/c.txt") || p.ends_with(r"b\c.txt"));
    }

    #[test]
    fn confine_rejects_traversal_and_absolute_paths() {
        // `..` escaping the working directory.
        assert!(confine_to_cwd("../../etc/passwd").is_err());
        assert!(confine_to_cwd("a/../../b").is_err());
        // An absolute path (built portably from the real cwd).
        let abs = std::env::current_dir().expect("cwd").join("x");
        assert!(confine_to_cwd(abs.to_str().expect("utf8")).is_err());
        // Empty path.
        assert!(confine_to_cwd("").is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_base64_in_the_escape() {
        assert_eq!(osc52_sequence("hi"), "\x1b]52;c;aGk=\x07");
    }
}
