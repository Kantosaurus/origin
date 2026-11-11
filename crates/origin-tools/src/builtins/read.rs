// SPDX-License-Identifier: Apache-2.0
//! `Read` v2 — line-numbered chunks with offset/limit, image/PDF dispatch.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};
use std::borrow::Cow;
use std::fmt::Write as _;

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct ReadArgs {
    pub file_path: String,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
    pub as_: Option<String>,
}

/// 0-based line offset: the number of leading lines to skip (0 ⇒ start at line
/// 1, matching the tool schema). Default limit = 1000 lines.
///
/// # Errors
/// Returns `ToolError` on I/O failure or non-UTF-8 (without BOM) content.
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::needless_pass_by_value)]
pub fn read_v2(args: ReadArgs) -> Result<String, ToolError> {
    let as_kind = args.as_.as_deref().unwrap_or("text");
    // Defense in depth: refuse to follow symlinks. SandboxProfile::ReadFs is the
    // real gate, but a symlink planted inside an allowed dir could otherwise
    // resolve to sensitive paths (~/.aws/credentials, SSH keys, etc.).
    let meta = std::fs::symlink_metadata(&args.file_path)
        .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path)))?;
    if meta.file_type().is_symlink() {
        return Err(ToolError::new(
            ErrClass::Validation,
            "symlink_denied",
            format!("refusing to read symlink: {}", args.file_path),
        ));
    }
    let bytes = std::fs::read(&args.file_path)
        .map_err(|e| ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path)))?;

    match as_kind {
        "text" => read_text(&bytes, &args),
        "image" => read_image(&bytes),
        "pdf" => read_pdf(&bytes),
        other => Err(ToolError::new(
            ErrClass::Validation,
            "bad_as",
            format!("unknown 'as' value: {other} (expected text|image|pdf)"),
        )),
    }
}

/// Byte cap on one text `Read` result, footer included.
const MAX_READ_BYTES: usize = 100_000;
/// Lines longer than this many characters are cut and marked (Read rows and Edit diff lines).
const MAX_LINE_CHARS: usize = 2_000;

/// `line` cut to [`MAX_LINE_CHARS`] chars plus a `… [line truncated: N chars]` marker; unchanged if shorter.
pub(crate) fn cap_line(line: &str) -> Cow<'_, str> {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((cut, _)) => Cow::Owned(format!(
            "{}\u{2026} [line truncated: {} chars]",
            &line[..cut],
            line.chars().count()
        )),
        None => Cow::Borrowed(line),
    }
}
/// Room kept under [`MAX_READ_BYTES`] for the continuation footer.
const FOOTER_RESERVE: usize = 128;

fn read_text(bytes: &[u8], args: &ReadArgs) -> Result<String, ToolError> {
    let det = text_fmt::detect(bytes);
    let text = text_fmt::normalise_to_lf(bytes, &det)?;
    let offset = args.offset.unwrap_or(0) as usize;
    let limit = args.limit.unwrap_or(1000).max(1) as usize;
    let mut out = String::new();
    // 0-based index of the first line NOT returned: the offset to resume at.
    let mut next = offset;
    for (idx, line) in text.lines().enumerate().skip(offset).take(limit) {
        let line_no = idx + 1;
        let row = format!("{line_no:>6}\t{}\n", cap_line(line));
        // A capped row is ≤ ~8 KB, so the first row always fits: paging always advances.
        if out.len() + row.len() > MAX_READ_BYTES - FOOTER_RESERVE {
            break;
        }
        out.push_str(&row);
        next = idx + 1;
    }
    let more = text.lines().count().saturating_sub(next);
    if more > 0 {
        let _ = writeln!(out, "[{more} more lines \u{2014} call Read with offset={next}]");
    }
    Ok(out)
}

fn read_image(bytes: &[u8]) -> Result<String, ToolError> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| ToolError::new(ErrClass::Io, "bad_image", e.to_string()))?;
    Ok(format!(
        "image: {}x{} ({})",
        img.width(),
        img.height(),
        match img.color() {
            image::ColorType::Rgb8 => "rgb8",
            image::ColorType::Rgba8 => "rgba8",
            image::ColorType::L8 => "l8",
            _ => "other",
        }
    ))
}

fn read_pdf(bytes: &[u8]) -> Result<String, ToolError> {
    pdf_extract::extract_text_from_mem(bytes)
        .map_err(|e| ToolError::new(ErrClass::Io, "bad_pdf", e.to_string()))
}

crate::origin_tool! {
    name: "Read",
    description: "Read a file at the given path. Optional `offset` (0-based line) and `limit` (default 1000). `as: image|pdf|text`.",
    tier: Tier::AutoAllowed,
    urgency: Urgency::Low,
    side_effects: SideEffects::Pure,
    input_schema: r#"{
        "type": "object",
        "properties": {
            "file_path": { "type": "string" },
            "offset":    { "type": "integer", "minimum": 0 },
            "limit":     { "type": "integer", "minimum": 1, "maximum": 50000 },
            "as":        { "type": "string", "enum": ["text", "image", "pdf"] }
        },
        "required": ["file_path"]
    }"#,
    sandbox: ::origin_sandbox::SandboxProfile::ReadFs,
}
