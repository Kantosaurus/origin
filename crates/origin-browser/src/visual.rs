// SPDX-License-Identifier: Apache-2.0
//! Browser "visual loop": capture a screenshot + recent console logs after an
//! action so the model can *visually* verify the page it just acted on.
//!
//! This module is intentionally pure and dependency-free. It does not spawn a
//! browser, open files speculatively, or reach the network on its own — the
//! caller (the daemon's `Browser` tool arm) drives a [`crate::protocol::Verb`]
//! through the router, optionally hands the produced screenshot **path** here
//! to read back the PNG bytes, and threads the resulting console lines from
//! [`crate::protocol::SnapshotResp::console`].
//!
//! The output [`VisualCapture`] is rendered into a provider-agnostic
//! content-block JSON shape (`{kind,media_type,base64,text}`) that mirrors
//! `origin_multimodal::ContentBlock` exactly, so the daemon can convert it to a
//! real `ContentBlock` (or embed it directly in the tool-result body) without
//! this crate taking a dependency on the multimodal/image stack.
//!
//! ## Gating
//! Nothing here runs unless the daemon's `ORIGIN_BROWSER_VISUAL` gate is on.
//! When off, the daemon never calls into this module and the `console` field
//! is `skip_serializing_if = "Option::is_none"`, so the `Browser` tool output
//! is byte-identical to the pre-visual-loop behavior.

use serde_json::{json, Value};

/// IANA media type emitted for the screenshot image block.
const SCREENSHOT_MEDIA_TYPE: &str = "image/png";

/// Hard cap on console lines folded into the textual tail, newest kept.
///
/// Bounds the worst-case tool-result size regardless of how chatty a page is.
pub const MAX_CONSOLE_LINES: usize = 50;

/// Per-line console cap (chars). Long lines are truncated with an ellipsis so a
/// single multi-megabyte `console.log` cannot blow up the tool result.
pub const MAX_CONSOLE_LINE_CHARS: usize = 2_000;

/// A screenshot + console snapshot captured immediately after a browser action.
///
/// Either half may be absent: a backend that cannot screenshot leaves
/// `screenshot_png` `None`; a page that logged nothing leaves `console` empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(clippy::module_name_repetitions)] // `VisualCapture` is the documented public type callers expect from the `visual` module.
pub struct VisualCapture {
    /// Raw PNG bytes of the post-action screenshot, when one was produced.
    pub screenshot_png: Option<Vec<u8>>,
    /// Recent browser-console lines, oldest first, already length-bounded.
    pub console: Vec<String>,
}

impl VisualCapture {
    /// Assemble a capture from an optional screenshot **path** and the console
    /// lines carried on a [`SnapshotResp`](crate::protocol::SnapshotResp).
    ///
    /// The screenshot is read from `screenshot_path` only when that path is
    /// `Some` and the file both exists and reads back as bytes; any read error
    /// degrades gracefully to "no screenshot" rather than failing the action.
    /// Console lines are sanitized and bounded by [`sanitize_console`].
    ///
    /// This is the one impure entry point (a single filesystem read of a path
    /// the caller already asked the browser to write); everything downstream is
    /// pure.
    #[must_use]
    pub fn from_action(screenshot_path: Option<&str>, console: &[String]) -> Self {
        let screenshot_png = screenshot_path.and_then(read_png_if_present);
        Self {
            screenshot_png,
            console: sanitize_console(console),
        }
    }

    /// Build a capture directly from already-decoded bytes — used by tests and
    /// by backends that hand back PNG bytes inline rather than via a path.
    #[must_use]
    pub fn from_parts(screenshot_png: Option<Vec<u8>>, console: &[String]) -> Self {
        Self {
            screenshot_png,
            console: sanitize_console(console),
        }
    }

    /// `true` when there is nothing to attach (no screenshot, no console).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.screenshot_png.is_none() && self.console.is_empty()
    }

    /// Render the screenshot as a content-block-shaped JSON image, if present.
    ///
    /// Shape: `{"kind":"image","media_type":"image/png","base64":"…"}` — the
    /// exact field layout of `origin_multimodal::ContentBlock::image`, so the
    /// daemon can deserialize it into a real `ContentBlock` or forward it as-is.
    #[must_use]
    pub fn image_block(&self) -> Option<Value> {
        let png = self.screenshot_png.as_deref()?;
        Some(json!({
            "kind": "image",
            "media_type": SCREENSHOT_MEDIA_TYPE,
            "base64": base64_encode(png),
        }))
    }

    /// Render the console tail as a single text block, if any lines exist.
    ///
    /// Shape mirrors `origin_multimodal::ContentBlock::text_block`:
    /// `{"kind":"text","text":"console (N lines):\n…"}`.
    #[must_use]
    pub fn console_block(&self) -> Option<Value> {
        let text = self.console_text()?;
        Some(json!({
            "kind": "text",
            "text": text,
        }))
    }

    /// The console tail as plain text suitable for appending to a tool result.
    ///
    /// Returns `None` when no console lines were captured so callers can keep
    /// the result body unchanged.
    #[must_use]
    pub fn console_text(&self) -> Option<String> {
        if self.console.is_empty() {
            return None;
        }
        let header = format!("console ({} lines):", self.console.len());
        let body_len: usize = self.console.iter().map(|l| l.len() + 1).sum();
        let mut out = String::with_capacity(header.len() + body_len);
        out.push_str(&header);
        for line in &self.console {
            out.push('\n');
            out.push_str(line);
        }
        Some(out)
    }
}

/// Sanitize and bound a slice of raw console lines.
///
/// - Strips a single trailing `\r`/`\n` per line (backends vary on EOL).
/// - Truncates over-long lines on a char boundary, appending a `…[truncated]`
///   marker so the model knows the line was clipped.
/// - Keeps only the newest [`MAX_CONSOLE_LINES`] lines.
#[must_use]
pub fn sanitize_console(lines: &[String]) -> Vec<String> {
    let start = lines.len().saturating_sub(MAX_CONSOLE_LINES);
    lines[start..]
        .iter()
        .map(|raw| clip_line(raw.trim_end_matches(['\r', '\n'])))
        .collect()
}

/// Clip a single line to [`MAX_CONSOLE_LINE_CHARS`] on a char boundary.
fn clip_line(line: &str) -> String {
    if line.chars().count() <= MAX_CONSOLE_LINE_CHARS {
        return line.to_owned();
    }
    let kept: String = line.chars().take(MAX_CONSOLE_LINE_CHARS).collect();
    format!("{kept}…[truncated]")
}

/// Read a PNG file back as bytes, returning `None` on any IO error or when the
/// bytes do not begin with the PNG magic signature.
///
/// Validating the magic bytes keeps a misconfigured/empty screenshot path from
/// being mislabeled `image/png` downstream.
fn read_png_if_present(path: &str) -> Option<Vec<u8>> {
    const PNG_MAGIC: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let bytes = std::fs::read(path).ok()?;
    bytes.starts_with(PNG_MAGIC).then_some(bytes)
}

/// Encode bytes as standard (RFC 4648) base64 with `=` padding.
///
/// Hand-rolled to keep this crate free of an extra dependency; matches
/// `origin_multimodal::base64_encode` byte-for-byte.
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk[1]) << 8) | u32::from(chunk[2]);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(n & 0x3F) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        1 => {
            let n = u32::from(rem[0]) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        2 => {
            let n = (u32::from(rem[0]) << 16) | (u32::from(rem[1]) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{base64_encode, sanitize_console, VisualCapture, MAX_CONSOLE_LINES, MAX_CONSOLE_LINE_CHARS};

    // A minimal valid-looking PNG header (magic + a few bytes). The visual
    // module only checks the magic prefix, not full PNG validity.
    fn fake_png() -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(b"IHDRdata");
        v
    }

    #[test]
    fn base64_matches_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn image_block_shape_matches_content_block() {
        let png = fake_png();
        let cap = VisualCapture::from_parts(Some(png.clone()), &[]);
        let block = cap.image_block().unwrap();
        assert_eq!(block["kind"], "image");
        assert_eq!(block["media_type"], "image/png");
        assert_eq!(block["base64"], base64_encode(&png));
        // No console ⇒ no console block / text.
        assert!(cap.console_block().is_none());
        assert!(cap.console_text().is_none());
    }

    #[test]
    fn console_block_shape_and_header() {
        let lines = vec!["[log] hello".to_owned(), "[error] boom".to_owned()];
        let cap = VisualCapture::from_parts(None, &lines);
        // No screenshot ⇒ no image block.
        assert!(cap.image_block().is_none());
        let block = cap.console_block().unwrap();
        assert_eq!(block["kind"], "text");
        assert_eq!(
            block["text"].as_str().unwrap(),
            "console (2 lines):\n[log] hello\n[error] boom"
        );
    }

    #[test]
    fn from_parts_combines_both_halves() {
        let cap = VisualCapture::from_parts(Some(fake_png()), &["[log] x".to_owned()]);
        assert!(!cap.is_empty());
        assert!(cap.image_block().is_some());
        assert!(cap.console_block().is_some());
    }

    #[test]
    fn empty_capture_yields_nothing() {
        let cap = VisualCapture::from_parts(None, &[]);
        assert!(cap.is_empty());
        assert!(cap.image_block().is_none());
        assert!(cap.console_block().is_none());
        assert!(cap.console_text().is_none());
    }

    #[test]
    fn console_is_trimmed_and_bounded() {
        // More than the cap, each with trailing EOL noise.
        let raw: Vec<String> = (0..(MAX_CONSOLE_LINES + 10))
            .map(|i| format!("line {i}\r\n"))
            .collect();
        let cleaned = sanitize_console(&raw);
        assert_eq!(cleaned.len(), MAX_CONSOLE_LINES, "kept only the newest cap");
        // Newest kept: the last raw line is the last cleaned line, EOL stripped.
        assert_eq!(
            cleaned.last().unwrap(),
            &format!("line {}", MAX_CONSOLE_LINES + 9)
        );
        assert!(!cleaned[0].ends_with('\n'));
        assert!(!cleaned[0].ends_with('\r'));
    }

    #[test]
    fn over_long_line_is_clipped_with_marker() {
        let long = "x".repeat(MAX_CONSOLE_LINE_CHARS + 100);
        let cleaned = sanitize_console(std::slice::from_ref(&long));
        assert_eq!(cleaned.len(), 1);
        assert!(cleaned[0].ends_with("…[truncated]"));
        // Body is exactly the cap (excluding the marker).
        let body = cleaned[0].trim_end_matches("…[truncated]");
        assert_eq!(body.chars().count(), MAX_CONSOLE_LINE_CHARS);
    }

    #[test]
    fn from_action_reads_real_png_and_rejects_non_png() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("shot.png");
        std::fs::write(&good, fake_png()).unwrap();
        let cap = VisualCapture::from_action(Some(good.to_str().unwrap()), &[]);
        assert!(cap.screenshot_png.is_some(), "valid PNG read back");

        let bad = dir.path().join("notpng.bin");
        std::fs::write(&bad, b"GIF89a not a png").unwrap();
        let cap2 = VisualCapture::from_action(Some(bad.to_str().unwrap()), &[]);
        assert!(cap2.screenshot_png.is_none(), "non-PNG magic rejected");

        // Missing path ⇒ no screenshot, no panic.
        let cap3 = VisualCapture::from_action(Some("does/not/exist.png"), &[]);
        assert!(cap3.screenshot_png.is_none());

        // None path ⇒ no screenshot.
        let cap4 = VisualCapture::from_action(None, &["[log] a".to_owned()]);
        assert!(cap4.screenshot_png.is_none());
        assert_eq!(cap4.console.len(), 1);
    }
}
