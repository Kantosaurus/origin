# origin-multimodal

> Image and PDF context ingestion: classify, extract text, and build content blocks.

## Purpose

`origin-multimodal` turns raw input bytes (a pasted screenshot, an attached PDF,
a text file) into provider-agnostic content blocks the model drivers can embed
in a request. It classifies bytes by magic number (with a filename-extension
fallback), inspects image dimensions, extracts PDF text, and emits a
`ContentBlock` that the Anthropic and OpenAI encoders translate into each
provider's wire shape. All decoding is pure and offline.

## Public API surface

| Item | Kind | Summary |
| --- | --- | --- |
| `MediaKind` | enum | `Png` / `Jpeg` / `Webp` / `Pdf` / `Text` / `Unknown`; `is_image()`, `image_media_type()`. |
| `MediaError` | enum | `Decode(String)` / `Unsupported(String)`. |
| `ImageMeta` | struct | `{ width, height, kind, bytes_len }`. |
| `ContentBlock` | struct | `{ kind, text, media_type, base64 }`; `image(...)`, `text_block(...)`. |
| `classify` | fn | Magic-byte detection, extension fallback. |
| `image_meta` | fn | Decode dimensions for a recognized image. |
| `pdf_to_text` | fn | Extract embedded text from a PDF. |
| `to_content_block` | fn | Build a `ContentBlock` from bytes (+ optional filename). |
| `encode_anthropic_block` / `encode_openai_block` | fn | Render a block to the provider's JSON. |
| `base64_encode` | fn | Hand-rolled RFC 4648 base64 (no extra dep). |

## Key types

```rust
pub enum MediaKind { Png, Jpeg, Webp, Pdf, Text, Unknown }

pub struct ContentBlock {
    pub kind: String,             // "image" | "text"
    pub text: Option<String>,     // text / PDF blocks
    pub media_type: Option<String>, // image blocks
    pub base64: Option<String>,   // image blocks
}
```

## How it works

`classify` checks leading magic bytes first (`89 50 4E 47…` → PNG, `FF D8 FF`
→ JPEG, `%PDF-` → PDF, `RIFF…WEBP` → WebP) and only consults the filename
extension when no signature matches. `to_content_block` then branches on the
kind: images become `{kind:"image", media_type, base64}`, PDFs are run through
`pdf_to_text` into a text block, plain UTF-8 becomes a text block, and `Unknown`
is an error. The two encoders shape the same block differently — Anthropic gets
`{"type":"image","source":{"type":"base64",…}}`; OpenAI gets
`{"type":"image_url","image_url":{"url":"data:<mt>;base64,<b64>"}}` — matching
what each provider driver injects into a user message's `content` array.

```
bytes (+filename?) ─▶ classify ─▶ MediaKind
                                   ├─image─▶ base64 ─▶ ContentBlock{image}
                                   ├─pdf───▶ pdf_to_text ─▶ ContentBlock{text}
                                   └─text──▶ utf8 ─▶ ContentBlock{text}
ContentBlock ─▶ encode_anthropic_block | encode_openai_block ─▶ provider JSON
```

## Dependencies & features

`#![forbid(unsafe_code)]`. `image` decodes image dimensions; `pdf-extract`
pulls PDF text; `serde`/`serde_json` serialize `ContentBlock` and build provider
JSON; `thiserror` for `MediaError`. No network, no async.

## Used by

```
crates/origin-cli/Cargo.toml
crates/origin-daemon/Cargo.toml
crates/origin-multimodal/Cargo.toml
crates/origin-provider-anthropic/Cargo.toml
crates/origin-provider-gemini/Cargo.toml
crates/origin-provider-ollama/Cargo.toml
crates/origin-provider-openai-compat/Cargo.toml
crates/origin-provider/Cargo.toml
```

## Testing

In-file tests cover magic-byte classification for every kind, the extension
fallback (and unknown-without-extension), RFC 4648 base64 vectors
(`""`/`f`/`fo`/`foo`/…/`foobar`), the text and image `to_content_block` paths,
and the rejection paths (`pdf_to_text` / `image_meta` on the wrong kind).

## See also

- [Tools subsystem](../subsystems/tools.md)
- [Crate index](../crates/README.md)

_Last reviewed against workspace version 0.9.8._
