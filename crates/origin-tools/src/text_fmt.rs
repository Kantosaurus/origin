// SPDX-License-Identifier: Apache-2.0
//! EOL / encoding / BOM detection and normalisation.
//!
//! Every file-touching tool reads bytes, calls [`detect`] once to capture
//! the file's original convention, then works against an LF-normalised
//! `String`. On write, [`denormalise`] restores the original convention
//! byte-for-byte, including per-source-line EOL for mixed-EOL files.

use crate::error::{ErrClass, ToolError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    Lf,
    Crlf,
    Cr,
    Mixed,
    /// File has no newlines at all.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bom {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

#[derive(Debug, Clone)]
pub struct Detected {
    pub eol: Eol,
    pub bom: Option<Bom>,
    pub encoding: Encoding,
    pub trailing_newline: bool,
    /// Per-source-line EOL, one entry per `\n`-terminated line after LF-normalisation.
    /// Used by [`denormalise`] to restore mixed-EOL files.
    pub per_line_eol: Vec<Eol>,
}

#[must_use]
pub fn detect(bytes: &[u8]) -> Detected {
    let (bom, body_start, encoding) = detect_bom(bytes);
    let body = &bytes[body_start..];

    // Classify EOLs against the DECODED text so UTF-16 files keep their real
    // line-ending convention. `\r` and `\n` are ASCII, so classifying the UTF-8
    // bytes of the decoded String is equivalent to classifying the UTF-16 code
    // units, and it reuses the byte-level classifier. Previously this short-
    // circuited to `Eol::Lf` for any non-UTF-8 encoding, which silently folded a
    // UTF-16 file's CRLF/CR endings to LF on the next edit write-back. A decode
    // error falls back to LF with no per-line data (the file is rejected later
    // by `normalise_to_lf`, so denormalise is never reached for it).
    let (eol, per_line_eol, trailing_newline) = match encoding {
        Encoding::Utf8 => classify_eols(body),
        Encoding::Utf16Le => classify_utf16(body, encoding_rs::UTF_16LE),
        Encoding::Utf16Be => classify_utf16(body, encoding_rs::UTF_16BE),
    };
    Detected {
        eol,
        bom,
        encoding,
        trailing_newline,
        per_line_eol,
    }
}

/// Decode a UTF-16 body and classify its EOLs. On a decode error, fall back to
/// LF with no per-line data (the file will be rejected by `normalise_to_lf`).
fn classify_utf16(body: &[u8], enc: &'static encoding_rs::Encoding) -> (Eol, Vec<Eol>, bool) {
    let (cow, _, had_errors) = enc.decode(body);
    if had_errors {
        (Eol::Lf, Vec::new(), false)
    } else {
        classify_eols(cow.as_bytes())
    }
}

fn detect_bom(bytes: &[u8]) -> (Option<Bom>, usize, Encoding) {
    if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
        (Some(Bom::Utf8), 3, Encoding::Utf8)
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (Some(Bom::Utf16Le), 2, Encoding::Utf16Le)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (Some(Bom::Utf16Be), 2, Encoding::Utf16Be)
    } else {
        (None, 0, Encoding::Utf8)
    }
}

fn classify_eols(body: &[u8]) -> (Eol, Vec<Eol>, bool) {
    let mut per_line = Vec::new();
    let mut i = 0;
    let mut seen_lf = false;
    let mut seen_crlf = false;
    let mut seen_cr = false;
    while i < body.len() {
        match body[i] {
            b'\r' if i + 1 < body.len() && body[i + 1] == b'\n' => {
                per_line.push(Eol::Crlf);
                seen_crlf = true;
                i += 2;
            }
            b'\r' => {
                per_line.push(Eol::Cr);
                seen_cr = true;
                i += 1;
            }
            b'\n' => {
                per_line.push(Eol::Lf);
                seen_lf = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    let trailing_newline = body.last().is_some_and(|&b| b == b'\n' || b == b'\r');

    let kind_count = u32::from(seen_lf) + u32::from(seen_crlf) + u32::from(seen_cr);
    let eol = match kind_count {
        0 => Eol::None,
        1 if seen_lf => Eol::Lf,
        1 if seen_crlf => Eol::Crlf,
        1 if seen_cr => Eol::Cr,
        _ => Eol::Mixed,
    };
    (eol, per_line, trailing_newline)
}

/// Decode bytes into a canonical LF-only String.
///
/// # Errors
/// `io.encoding` if bytes are not valid in the detected encoding.
pub fn normalise_to_lf(bytes: &[u8], det: &Detected) -> Result<String, ToolError> {
    let body_start = match det.bom {
        Some(Bom::Utf8) => 3,
        Some(Bom::Utf16Le | Bom::Utf16Be) => 2,
        None => 0,
    };
    let body = &bytes[body_start..];

    match det.encoding {
        Encoding::Utf8 => {
            let text = std::str::from_utf8(body).map_err(|e| {
                ToolError::new(
                    ErrClass::Io,
                    "encoding",
                    format!("not valid UTF-8 at byte {}: {e}", e.valid_up_to()),
                )
            })?;
            Ok(fold_eols_to_lf(text))
        }
        Encoding::Utf16Le => {
            let (cow, _, had_errors) = encoding_rs::UTF_16LE.decode(body);
            if had_errors {
                return Err(ToolError::new(ErrClass::Io, "encoding", "invalid UTF-16 LE"));
            }
            Ok(fold_eols_to_lf(&cow))
        }
        Encoding::Utf16Be => {
            let (cow, _, had_errors) = encoding_rs::UTF_16BE.decode(body);
            if had_errors {
                return Err(ToolError::new(ErrClass::Io, "encoding", "invalid UTF-16 BE"));
            }
            Ok(fold_eols_to_lf(&cow))
        }
    }
}

/// Fold every `\r\n` and bare `\r` to a single `\n`, preserving all other
/// characters verbatim. Operates at the `char` level so multi-byte UTF-8 /
/// UTF-16 scalar values are never split or reinterpreted.
fn fold_eols_to_lf(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push('\n');
            }
            c => out.push(c),
        }
    }
    out
}

/// Re-encode text back to the file's original byte convention.
#[must_use]
pub fn denormalise(text: &str, det: &Detected) -> Vec<u8> {
    let bom_bytes: &[u8] = match det.bom {
        Some(Bom::Utf8) => &[0xef, 0xbb, 0xbf],
        Some(Bom::Utf16Le) => &[0xff, 0xfe],
        Some(Bom::Utf16Be) => &[0xfe, 0xff],
        None => &[],
    };
    let body = match det.encoding {
        Encoding::Utf8 => restore_eols(text, det).into_bytes(),
        Encoding::Utf16Le => {
            let lf_restored = restore_eols(text, det);
            let mut out = Vec::with_capacity(lf_restored.len() * 2);
            for u in lf_restored.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            out
        }
        Encoding::Utf16Be => {
            let lf_restored = restore_eols(text, det);
            let mut out = Vec::with_capacity(lf_restored.len() * 2);
            for u in lf_restored.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
            out
        }
    };
    [bom_bytes, &body[..]].concat()
}

/// Walk `text` line by line; for each `\n` boundary in `text`, pick the EOL
/// for that line from `det.per_line_eol`. Lines beyond the per-line vector
/// (i.e. inserted) inherit the EOL of the line immediately preceding them.
/// If no preceding line exists, fall back to the file's dominant EOL.
fn restore_eols(text: &str, det: &Detected) -> String {
    let dominant = dominant_eol_str(det);
    let mut out = String::with_capacity(text.len() + det.per_line_eol.len());
    let mut line_idx = 0_usize;
    let mut last_eol_str: &'static str = dominant;

    for line in text.split_inclusive('\n') {
        if let Some(stripped) = line.strip_suffix('\n') {
            out.push_str(stripped);
            let eol_str: &'static str = match det.per_line_eol.get(line_idx) {
                Some(Eol::Crlf) => {
                    last_eol_str = "\r\n";
                    "\r\n"
                }
                Some(Eol::Cr) => {
                    last_eol_str = "\r";
                    "\r"
                }
                Some(Eol::Lf) => {
                    last_eol_str = "\n";
                    "\n"
                }
                Some(Eol::Mixed | Eol::None) | None => last_eol_str,
            };
            out.push_str(eol_str);
            line_idx += 1;
        } else {
            out.push_str(line);
        }
    }
    out
}

const fn dominant_eol_str(det: &Detected) -> &'static str {
    match det.eol {
        Eol::Crlf => "\r\n",
        Eol::Cr => "\r",
        Eol::Lf | Eol::Mixed | Eol::None => "\n",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{denormalise, detect, normalise_to_lf, Eol};

    fn utf16le_bytes(s: &str) -> Vec<u8> {
        let mut out = vec![0xff, 0xfe]; // UTF-16-LE BOM
        for u in s.encode_utf16() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out
    }

    #[test]
    fn utf16le_crlf_is_classified_and_roundtrips() {
        // A Windows-authored UTF-16-LE file using CRLF line endings.
        let original = utf16le_bytes("line one\r\nline two\r\n");
        let det = detect(&original);
        // Regression: detect() must classify the real EOL, not blindly report LF.
        assert_eq!(det.eol, Eol::Crlf, "UTF-16 CRLF must be detected as CRLF");
        let text = normalise_to_lf(&original, &det).unwrap();
        assert_eq!(text, "line one\nline two\n");
        // Round-trip with no edit must reproduce the original CRLF bytes exactly.
        let out = denormalise(&text, &det);
        assert_eq!(out, original, "UTF-16 CRLF must survive the edit round-trip");
    }

    #[test]
    fn utf16le_lf_stays_lf() {
        let original = utf16le_bytes("a\nb\n");
        let det = detect(&original);
        assert_eq!(det.eol, Eol::Lf);
        let text = normalise_to_lf(&original, &det).unwrap();
        let out = denormalise(&text, &det);
        assert_eq!(out, original);
    }
}
