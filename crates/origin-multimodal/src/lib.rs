// SPDX-License-Identifier: Apache-2.0
//! Image and PDF context ingestion for multimodal prompts.
//!
//! This crate classifies raw input bytes into a [`MediaKind`], extracts text
//! from PDFs, inspects image dimensions, and assembles provider-agnostic
//! [`ContentBlock`] values (images become base64 blocks; PDFs and text become
//! text blocks). All decoding is pure and offline, so the crate is fully unit
//! testable without network access.

#![forbid(unsafe_code)]

use thiserror::Error;

/// Errors produced while classifying, decoding, or encoding media.
#[derive(Debug, Error)]
pub enum MediaError {
    /// The input could not be decoded as the detected media type.
    #[error("decode error: {0}")]
    Decode(String),
    /// The input is of a kind this crate cannot turn into a content block.
    #[error("unsupported media: {0}")]
    Unsupported(String),
}

/// The classified kind of an input byte buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// PNG image.
    Png,
    /// JPEG image.
    Jpeg,
    /// `WebP` image.
    Webp,
    /// PDF document.
    Pdf,
    /// UTF-8 / plain text.
    Text,
    /// Unrecognized input.
    Unknown,
}

impl MediaKind {
    /// Returns the IANA media type for image kinds, if any.
    ///
    /// Non-image kinds (PDF, text, unknown) return [`None`] because their
    /// content blocks do not carry an image `media_type`.
    #[must_use]
    pub const fn image_media_type(self) -> Option<&'static str> {
        match self {
            Self::Png => Some("image/png"),
            Self::Jpeg => Some("image/jpeg"),
            Self::Webp => Some("image/webp"),
            Self::Pdf | Self::Text | Self::Unknown => None,
        }
    }

    /// Returns `true` when this kind is an image.
    #[must_use]
    pub const fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Webp)
    }
}

/// Metadata describing a decoded image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageMeta {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// The detected image kind.
    pub kind: MediaKind,
    /// Length of the original byte buffer.
    pub bytes_len: usize,
}

/// A provider-agnostic content block ready to be serialized into a request.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContentBlock {
    /// Block kind, e.g. `"image"` or `"text"`.
    pub kind: String,
    /// Textual payload, set for text/PDF blocks.
    pub text: Option<String>,
    /// IANA media type, set for image blocks.
    pub media_type: Option<String>,
    /// Base64-encoded data, set for image blocks.
    pub base64: Option<String>,
}

impl ContentBlock {
    /// Construct an image content block from a media type and base64 payload.
    ///
    /// Useful for callers (and tests) that already hold encoded image data.
    #[must_use]
    pub fn image(media_type: impl Into<String>, base64: impl Into<String>) -> Self {
        Self {
            kind: "image".to_string(),
            text: None,
            media_type: Some(media_type.into()),
            base64: Some(base64.into()),
        }
    }

    /// Construct a text content block.
    #[must_use]
    pub fn text_block(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: Some(text.into()),
            media_type: None,
            base64: None,
        }
    }
}

/// Encode a [`ContentBlock`] into the Anthropic Messages API content-block JSON.
///
/// Image blocks become `{"type":"image","source":{"type":"base64",...}}`; all
/// other blocks become `{"type":"text","text":...}`. The shape matches what the
/// Anthropic driver injects into a user message's `content` array.
#[must_use]
pub fn encode_anthropic_block(block: &ContentBlock) -> serde_json::Value {
    if block.kind == "image" {
        if let (Some(media_type), Some(base64)) = (&block.media_type, &block.base64) {
            return serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": base64,
                }
            });
        }
    }
    serde_json::json!({
        "type": "text",
        "text": block.text.clone().unwrap_or_default(),
    })
}

/// Encode a [`ContentBlock`] into an `OpenAI` chat-completions content part.
///
/// Image blocks become `{"type":"image_url","image_url":{"url":"data:..."}}`
/// with a data-URL; all other blocks become `{"type":"text","text":...}`. The
/// shape matches what the `OpenAI`-compat driver injects into a user message's
/// `content` array.
#[must_use]
pub fn encode_openai_block(block: &ContentBlock) -> serde_json::Value {
    if block.kind == "image" {
        if let (Some(media_type), Some(base64)) = (&block.media_type, &block.base64) {
            return serde_json::json!({
                "type": "image_url",
                "image_url": { "url": format!("data:{media_type};base64,{base64}") }
            });
        }
    }
    serde_json::json!({
        "type": "text",
        "text": block.text.clone().unwrap_or_default(),
    })
}

/// Classifies a byte buffer by magic bytes, falling back to the filename extension.
///
/// Magic-byte detection takes precedence; when the leading bytes match no known
/// signature, the optional `filename` extension is consulted. Inputs that match
/// neither are reported as [`MediaKind::Unknown`].
#[must_use]
pub fn classify(bytes: &[u8], filename: Option<&str>) -> MediaKind {
    if let Some(kind) = classify_magic(bytes) {
        return kind;
    }
    if let Some(name) = filename {
        if let Some(kind) = classify_extension(name) {
            return kind;
        }
    }
    MediaKind::Unknown
}

/// Detects a media kind purely from leading magic bytes.
fn classify_magic(bytes: &[u8]) -> Option<MediaKind> {
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    const PNG: &[u8] = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    // JPEG: FF D8 FF
    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF];
    // PDF: %PDF-
    const PDF: &[u8] = b"%PDF-";

    if bytes.starts_with(PNG) {
        return Some(MediaKind::Png);
    }
    if bytes.starts_with(JPEG) {
        return Some(MediaKind::Jpeg);
    }
    if bytes.starts_with(PDF) {
        return Some(MediaKind::Pdf);
    }
    // WebP: "RIFF" .... "WEBP"
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(MediaKind::Webp);
    }
    None
}

/// Detects a media kind from a filename extension (case-insensitive).
fn classify_extension(filename: &str) -> Option<MediaKind> {
    // `rsplit_once` requires an actual '.' to be present, so files with no
    // extension yield `None` and fall through to `MediaKind::Unknown`.
    let (_, ext) = filename.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(MediaKind::Png),
        "jpg" | "jpeg" => Some(MediaKind::Jpeg),
        "webp" => Some(MediaKind::Webp),
        "pdf" => Some(MediaKind::Pdf),
        "txt" | "text" | "md" | "markdown" => Some(MediaKind::Text),
        _ => None,
    }
}

/// Inspects an image buffer and returns its dimensions and kind.
///
/// # Errors
///
/// Returns [`MediaError::Unsupported`] when the bytes are not a recognized
/// image, and [`MediaError::Decode`] when the format is recognized but the
/// pixel data cannot be parsed.
pub fn image_meta(bytes: &[u8]) -> Result<ImageMeta, MediaError> {
    use image::GenericImageView;

    let kind = classify_magic(bytes).filter(|k| k.is_image());
    let Some(kind) = kind else {
        return Err(MediaError::Unsupported(
            "input is not a recognized image".to_owned(),
        ));
    };
    let img = image::load_from_memory(bytes).map_err(|e| MediaError::Decode(e.to_string()))?;
    let (width, height) = img.dimensions();
    Ok(ImageMeta {
        width,
        height,
        kind,
        bytes_len: bytes.len(),
    })
}

/// Extracts the embedded text from a PDF document.
///
/// # Errors
///
/// Returns [`MediaError::Unsupported`] when the bytes are not a PDF and
/// [`MediaError::Decode`] when extraction fails.
pub fn pdf_to_text(bytes: &[u8]) -> Result<String, MediaError> {
    if classify_magic(bytes) != Some(MediaKind::Pdf) {
        return Err(MediaError::Unsupported("input is not a PDF".to_owned()));
    }
    pdf_extract::extract_text_from_mem(bytes).map_err(|e| MediaError::Decode(e.to_string()))
}

/// Builds a [`ContentBlock`] from raw input, choosing the representation by kind.
///
/// Images become `{kind: "image", media_type, base64}`; PDFs are extracted to
/// `{kind: "text", text}`; plain text becomes `{kind: "text", text}`.
///
/// # Errors
///
/// Returns [`MediaError::Unsupported`] for [`MediaKind::Unknown`] input,
/// [`MediaError::Decode`] when PDF extraction or UTF-8 decoding of text fails,
/// and propagates errors from the image path.
pub fn to_content_block(bytes: &[u8], filename: Option<&str>) -> Result<ContentBlock, MediaError> {
    let kind = classify(bytes, filename);
    match kind {
        MediaKind::Png | MediaKind::Jpeg | MediaKind::Webp => {
            let media_type = kind.image_media_type().map(str::to_owned);
            Ok(ContentBlock {
                kind: "image".to_owned(),
                text: None,
                media_type,
                base64: Some(base64_encode(bytes)),
            })
        }
        MediaKind::Pdf => {
            let text = pdf_to_text(bytes)?;
            Ok(ContentBlock {
                kind: "text".to_owned(),
                text: Some(text),
                media_type: None,
                base64: None,
            })
        }
        MediaKind::Text => {
            let text = std::str::from_utf8(bytes)
                .map_err(|e| MediaError::Decode(e.to_string()))?
                .to_owned();
            Ok(ContentBlock {
                kind: "text".to_owned(),
                text: Some(text),
                media_type: None,
                base64: None,
            })
        }
        MediaKind::Unknown => Err(MediaError::Unsupported(
            "could not classify input by magic bytes or extension".to_owned(),
        )),
    }
}

/// Encodes bytes as standard (RFC 4648) base64 with `=` padding.
///
/// Implemented by hand to avoid an extra dependency.
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
#[allow(clippy::unwrap_used, clippy::float_cmp)]
mod tests {
    use super::{base64_encode, classify, to_content_block, ContentBlock, MediaError, MediaKind};

    #[test]
    fn classify_png_by_magic() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x01];
        assert_eq!(classify(&png, None), MediaKind::Png);
    }

    #[test]
    fn classify_jpeg_by_magic() {
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        assert_eq!(classify(&jpeg, None), MediaKind::Jpeg);
    }

    #[test]
    fn classify_pdf_by_magic() {
        let pdf = b"%PDF-1.7\n%rest";
        assert_eq!(classify(pdf, None), MediaKind::Pdf);
    }

    #[test]
    fn classify_webp_by_magic() {
        let mut webp = Vec::new();
        webp.extend_from_slice(b"RIFF");
        webp.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(classify(&webp, None), MediaKind::Webp);
    }

    #[test]
    fn classify_by_extension_fallback() {
        // No magic-byte match, so the extension decides.
        assert_eq!(classify(b"plain stuff", Some("notes.txt")), MediaKind::Text);
        assert_eq!(classify(b"\x00\x01\x02", Some("pic.JPG")), MediaKind::Jpeg);
        assert_eq!(classify(b"\x00\x01\x02", Some("doc.pdf")), MediaKind::Pdf);
    }

    #[test]
    fn classify_unknown_without_magic_or_extension() {
        assert_eq!(classify(b"\x00\x01\x02", None), MediaKind::Unknown);
        assert_eq!(classify(b"\x00\x01\x02", Some("noext")), MediaKind::Unknown);
    }

    #[test]
    fn base64_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn to_content_block_text() {
        let block = to_content_block(b"hello world", Some("greeting.txt")).unwrap();
        assert_eq!(
            block,
            ContentBlock {
                kind: "text".to_owned(),
                text: Some("hello world".to_owned()),
                media_type: None,
                base64: None,
            }
        );
    }

    #[test]
    fn to_content_block_image_uses_base64() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let block = to_content_block(&png, Some("x.png")).unwrap();
        assert_eq!(block.kind, "image");
        assert_eq!(block.media_type.as_deref(), Some("image/png"));
        assert_eq!(block.base64, Some(base64_encode(&png)));
        assert!(block.text.is_none());
    }

    #[test]
    fn to_content_block_unsupported_path() {
        let err = to_content_block(b"\x00\x01\x02", None).unwrap_err();
        assert!(matches!(err, MediaError::Unsupported(_)));
    }

    #[test]
    fn pdf_to_text_rejects_non_pdf() {
        let err = super::pdf_to_text(b"not a pdf").unwrap_err();
        assert!(matches!(err, MediaError::Unsupported(_)));
    }

    #[test]
    fn image_meta_rejects_non_image() {
        let err = super::image_meta(b"%PDF-1.7").unwrap_err();
        assert!(matches!(err, MediaError::Unsupported(_)));
    }
}
