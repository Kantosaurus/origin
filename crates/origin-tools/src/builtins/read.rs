//! `Read` v2 — line-numbered chunks with offset/limit, image/PDF dispatch.

use crate::error::{ErrClass, ToolError};
use crate::text_fmt;
use crate::{SideEffects, Tier, Urgency};

#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
pub struct ReadArgs {
    pub file_path: String,
    pub offset: Option<u32>,
    pub limit: Option<u32>,
    pub as_: Option<String>,
}

/// 1-based offset (0 means start from line 1). Default limit = 1000 lines.
///
/// # Errors
/// Returns `ToolError` on I/O failure or non-UTF-8 (without BOM) content.
#[allow(clippy::module_name_repetitions)]
#[allow(clippy::needless_pass_by_value)]
pub fn read_v2(args: ReadArgs) -> Result<String, ToolError> {
    let as_kind = args.as_.as_deref().unwrap_or("text");
    let bytes = std::fs::read(&args.file_path).map_err(|e| {
        ToolError::new(ErrClass::Io, "not_found", format!("{}: {e}", args.file_path))
    })?;

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

fn read_text(bytes: &[u8], args: &ReadArgs) -> Result<String, ToolError> {
    let det = text_fmt::detect(bytes);
    let text = text_fmt::normalise_to_lf(bytes, &det)?;
    let offset = args.offset.unwrap_or(0) as usize;
    let limit = args.limit.unwrap_or(1000) as usize;
    let mut out = String::with_capacity(text.len());
    for (idx, line) in text.lines().enumerate().skip(offset).take(limit) {
        let line_no = idx + 1;
        out.push_str(&format!("{line_no:>6}\t{line}\n"));
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
