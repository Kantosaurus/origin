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

/// Sub-state of [`State::InNested`] tracking whether the cursor is currently
/// inside a JSON string literal so that `{`/`}`/`[`/`]` bytes appearing inside
/// strings are not miscounted toward bracket depth (N10.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NestedState {
    /// Not inside a string literal — brace/bracket bytes affect depth.
    Outside,
    /// Inside a `"…"` string literal. `escape` is set after a `\` byte so the
    /// following byte is consumed verbatim (and not interpreted as `"`).
    InString { escape: bool },
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
    /// String-tracking sub-state for [`State::InNested`] (N10.10).
    nest_string: NestedState,
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
            nest_string: NestedState::Outside,
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
        self.nest_string = NestedState::Outside;
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
                        self.nest_string = NestedState::Outside;
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
            // `InNested` tracks `{`/`[`/`}`/`]` as bracket depth, but only when
            // the cursor is *outside* a JSON string literal. Inside a string
            // those bytes are payload and must not skew `nest_depth`. `\` toggles
            // a single-byte escape so the following `"` is treated as data (N10.10).
            State::InNested => {
                self.val_buf.push(b);
                match self.nest_string {
                    NestedState::Outside => match b {
                        b'"' => self.nest_string = NestedState::InString { escape: false },
                        b'{' | b'[' => self.nest_depth = self.nest_depth.saturating_add(1),
                        b'}' | b']' => {
                            self.nest_depth = self.nest_depth.saturating_sub(1);
                            if self.nest_depth == 0 {
                                self.emit_field(out);
                            }
                        }
                        _ => {}
                    },
                    NestedState::InString { escape: true } => {
                        // Previous byte was `\`; consume this one verbatim and
                        // clear the escape flag regardless of what it is.
                        self.nest_string = NestedState::InString { escape: false };
                    }
                    NestedState::InString { escape: false } => match b {
                        b'\\' => self.nest_string = NestedState::InString { escape: true },
                        b'"' => self.nest_string = NestedState::Outside,
                        _ => {}
                    },
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

/// Aggregated outcome of a test-only parser run: the raw bytes fed to the
/// parser (verbatim, useful for round-trip assertions) and whether the outer
/// object's closing `}` was observed.
#[derive(Debug, Clone, Default)]
pub struct CompletedToolUse {
    /// The exact byte stream fed into the parser, reassembled into a `String`.
    pub input_json: String,
    /// `true` once a [`ToolUseDelta::Closed`] event has been emitted, signalling
    /// the outer `}` arrived and depth tracking balanced out.
    pub complete: bool,
}

/// Test-only handle wrapping a [`ToolUseParser`] that captures fed bytes and
/// surfaces completion as a single `CompletedToolUse` view.
///
/// Intended for unit tests that want to assert on the raw input round-trip
/// and on whether the parser correctly tracked nested-brace depth (especially
/// when those braces appear inside JSON string literals — N10.10).
pub struct ParserHandle {
    inner: ToolUseParser,
    input: Vec<u8>,
    complete: bool,
}

impl ParserHandle {
    /// Feed a chunk of bytes into the underlying parser, recording them for
    /// later inspection via [`Self::finish`].
    pub fn feed(&mut self, chunk: &[u8]) {
        self.input.extend_from_slice(chunk);
        for delta in self.inner.feed(chunk) {
            if matches!(delta, ToolUseDelta::Closed { .. }) {
                self.complete = true;
            }
        }
    }

    /// Consume the handle and return the aggregated outcome.
    #[must_use]
    pub fn finish(self) -> CompletedToolUse {
        CompletedToolUse {
            input_json: String::from_utf8_lossy(&self.input).into_owned(),
            complete: self.complete,
        }
    }
}

/// Build a fresh [`ParserHandle`] pre-armed with a `tool_use` start so tests
/// can immediately call `.feed(...)`.
#[must_use]
pub fn feed_for_test() -> ParserHandle {
    let mut inner = ToolUseParser::new();
    inner.begin_tool_use("test");
    ParserHandle {
        inner,
        input: Vec::new(),
        complete: false,
    }
}
