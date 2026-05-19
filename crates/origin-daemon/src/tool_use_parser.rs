//! Incremental SAX-style JSON parser for `tool_use` input objects (N2.2).
//!
//! Consumes fragments of a streaming JSON object (the assistant's
//! `tool_use.input`) and emits a `Field` event the moment each top-level
//! key/value pair completes — *before* the outer closing `}` arrives. That
//! makes the parser the trigger for speculative tool dispatch.
//!
//! Scope: only the **outer object** is walked. Nested values are captured as
//! raw bytes between matching `{}`/`[]`/`""` boundaries; speculative pure
//! tools have flat-ish input schemas (`Read`, `Glob`, `Grep`) so capturing
//! raw inner bytes is enough for P3 — a richer typed view can be layered on
//! top later without a parser rewrite.

/// Events emitted by [`ToolUseParser`] as input fragments arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolUseDelta {
    /// A top-level key/value pair just completed.
    Field {
        tool_name: String,
        name: String,
        /// Raw UTF-8 bytes of the value. Strings have their quotes stripped
        /// and escape sequences resolved; objects/arrays are passed through
        /// as-is including their wrapping `{}`/`[]`.
        value: Vec<u8>,
    },
    /// The outer `}` of the `tool_use` input arrived.
    Closed { tool_name: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    BeforeKey,
    InKey,
    AfterKey,
    BeforeValue,
    InString,
    InStringEscape,
    InNumber,
    InBoolNull,
    InNested,
    AfterValue,
    Closed,
}

/// Incremental SAX-style JSON parser for a single `tool_use` input object.
///
/// Call [`begin_tool_use`](Self::begin_tool_use) when a `tool_use` block
/// starts, then repeatedly call [`feed`](Self::feed) with each incoming chunk.
/// The parser emits [`ToolUseDelta`] events as fields complete.
pub struct ToolUseParser {
    state: State,
    /// Active tool name set by `begin_tool_use`.
    tool_name: Option<String>,
    /// Buffer for the current key (accumulating between `"` and `"`).
    key_buf: Vec<u8>,
    /// Buffer for the current value (string body without wrapping quotes, or
    /// nested object/array bytes including wrappers).
    val_buf: Vec<u8>,
    /// Bracket depth while inside a nested value. Reaches 0 → value done.
    nest_depth: u32,
}

impl Default for ToolUseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolUseParser {
    /// Create a new parser in the idle state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            tool_name: None,
            key_buf: Vec::new(),
            val_buf: Vec::new(),
            nest_depth: 0,
        }
    }

    /// Set the tool name from the surrounding `tool_use` block-start event
    /// and prepare to receive the input JSON object.
    pub fn begin_tool_use(&mut self, name: impl Into<String>) {
        self.tool_name = Some(name.into());
        self.state = State::BeforeKey;
        self.key_buf.clear();
        self.val_buf.clear();
        self.nest_depth = 0;
    }

    /// Feed the next fragment and collect any completed field events.
    ///
    /// # Errors
    ///
    /// This function is infallible; malformed JSON is silently skipped.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<ToolUseDelta> {
        let mut out = Vec::new();
        for &b in chunk {
            self.step(b, &mut out);
        }
        out
    }

    // The state machine is intentionally kept as one large match for
    // readability — splitting it across helpers would obscure the flow.
    #[allow(clippy::too_many_lines)]
    fn step(&mut self, b: u8, out: &mut Vec<ToolUseDelta>) {
        match self.state {
            State::Idle | State::Closed => {}
            State::BeforeKey => {
                #[allow(clippy::match_same_arms)] // distinct semantics: `{` opens, `,` separates
                match b {
                    b'{' | b',' | b' ' | b'\t' | b'\r' | b'\n' => {}
                    b'"' => {
                        self.key_buf.clear();
                        self.state = State::InKey;
                    }
                    b'}' => self.finish_object(out),
                    _ => {}
                }
            }
            State::InKey => {
                if b == b'"' {
                    self.state = State::AfterKey;
                } else {
                    self.key_buf.push(b);
                }
            }
            State::AfterKey => {
                if b == b':' {
                    self.state = State::BeforeValue;
                }
            }
            State::BeforeValue => {
                self.val_buf.clear();
                match b {
                    b' ' | b'\t' | b'\r' | b'\n' => {}
                    b'"' => self.state = State::InString,
                    b'{' | b'[' => {
                        self.val_buf.push(b);
                        self.nest_depth = 1;
                        self.state = State::InNested;
                    }
                    b't' | b'f' | b'n' => {
                        self.val_buf.push(b);
                        self.state = State::InBoolNull;
                    }
                    _ => {
                        self.val_buf.push(b);
                        self.state = State::InNumber;
                    }
                }
            }
            State::InString => match b {
                b'\\' => self.state = State::InStringEscape,
                b'"' => self.emit_field(out),
                _ => self.val_buf.push(b),
            },
            State::InStringEscape => {
                // Minimal escape handling: pass through the next byte raw.
                // Sufficient for paths, which never use `\u`-style escapes
                // in this codebase. Richer escape decoding lands with N10.10.
                self.val_buf.push(b);
                self.state = State::InString;
            }
            State::InNumber => match b {
                b',' => self.emit_field(out),
                b'}' => {
                    self.emit_field(out);
                    self.finish_object(out);
                }
                _ if b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'+' | b'e' | b'E') => {
                    self.val_buf.push(b);
                }
                _ => {} // whitespace or other ignored bytes
            },
            State::InBoolNull => {
                // If a structural delimiter arrives before the literal matches, bail to
                // AfterValue and re-process the delimiter so a malformed literal can't
                // swallow the rest of the object.
                if b == b'}' || b == b',' {
                    self.val_buf.clear();
                    self.state = State::AfterValue;
                    self.step(b, out);
                    return;
                }
                self.val_buf.push(b);
                if self.val_buf.len() > 5 {
                    // Malformed literal — abandon and resync at the next delimiter.
                    self.val_buf.clear();
                    self.state = State::AfterValue;
                    return;
                }
                if matches!(self.val_buf.as_slice(), b"true" | b"false" | b"null") {
                    self.emit_field(out);
                }
            }
            // FIXME(N10.10): `InNested` tracks `{`/`[`/`}`/`]` as raw bytes without
            // distinguishing those that appear inside string literals. Adversarial or
            // unusual provider output could skew `nest_depth` and emit too early/late.
            // Fine for Read/Glob/Grep (no JSON-in-string values); revisit with the
            // full fuzz corpus in Phase 14.
            State::InNested => {
                self.val_buf.push(b);
                match b {
                    b'{' | b'[' => self.nest_depth = self.nest_depth.saturating_add(1),
                    b'}' | b']' => {
                        self.nest_depth = self.nest_depth.saturating_sub(1);
                        if self.nest_depth == 0 {
                            self.emit_field(out);
                        }
                    }
                    _ => {}
                }
            }
            State::AfterValue => match b {
                b',' => self.state = State::BeforeKey,
                b'}' => self.finish_object(out),
                _ => {}
            },
        }
    }

    fn emit_field(&mut self, out: &mut Vec<ToolUseDelta>) {
        let tool_name = self.tool_name.clone().unwrap_or_else(|| "<unknown>".into());
        let name = String::from_utf8_lossy(&self.key_buf).into_owned();
        let value = std::mem::take(&mut self.val_buf);
        out.push(ToolUseDelta::Field {
            tool_name,
            name,
            value,
        });
        self.state = State::AfterValue;
    }

    fn finish_object(&mut self, out: &mut Vec<ToolUseDelta>) {
        let tool_name = self.tool_name.clone().unwrap_or_else(|| "<unknown>".into());
        out.push(ToolUseDelta::Closed { tool_name });
        self.state = State::Closed;
    }
}
