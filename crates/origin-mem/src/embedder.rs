// SPDX-License-Identifier: Apache-2.0
//! ONNX `MiniLM` wrapper. Loads a sentence-transformer ONNX graph and exposes
//! `embed(text) -> [f32; 384]`. CPU execution provider only.

use ort::session::Session;
use ort::value::TensorRef;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokenizers::models::wordlevel::WordLevel;
use tokenizers::pre_tokenizers::whitespace::Whitespace;
use tokenizers::{ModelWrapper, Tokenizer};

/// Output dimension of `MiniLM` L6 v2.
pub const EMBED_DIM: usize = 384;

/// Errors raised while loading or running the [`Embedder`].
//
// The "Embedder" prefix repeats the module name; we accept that to keep
// `EmbedderError` callable from outside the crate without the user having to
// disambiguate which module's `Error` this is.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Error)]
pub enum EmbedderError {
    /// Filesystem IO error opening the model or tokenizer.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// Underlying ONNX Runtime error.
    #[error("ort: {0}")]
    Ort(#[from] ort::Error),

    /// Tokenizer parse / encode error (the upstream `tokenizers` crate uses a
    /// boxed dyn error so we capture only its message).
    #[error("tokenizer: {0}")]
    Tokenizer(String),

    /// The model produced a tensor of unexpected shape.
    #[error("model output had unexpected shape: got {got:?}, want [_, 384]")]
    BadShape {
        /// Actual shape returned by the model.
        got: Vec<usize>,
    },

    /// The requested model path does not exist on disk.
    #[error("model file not found at {0:?}")]
    NotFound(PathBuf),

    /// The session mutex was poisoned by a prior panic mid-inference.
    #[error("embedder session lock poisoned")]
    SessionPoisoned,
}

/// Sentence-embedding pipeline over an ONNX MiniLM-class model.
///
/// One [`Embedder`] owns a single ONNX [`Session`] and tokenizer. It is cheap
/// to call [`Self::embed`] repeatedly; expensive to construct.
///
/// The session is behind a `Mutex` because ort rc.12's `Session::run` takes
/// `&mut self`, while `embed` must stay `&self` (callers hold `&Embedder`, e.g.
/// a shared `Option<&Embedder>`). Inference therefore serializes per embedder —
/// fine, since a single CPU session is not meant to run concurrently anyway.
pub struct Embedder {
    session: std::sync::Mutex<Session>,
    tokenizer: Tokenizer,
}

impl Embedder {
    /// Load an ONNX model from `path`. Tokenizer JSON is expected next to it
    /// at `<path stem>.tokenizer.json`; if missing, a minimal whitespace
    /// word-level tokenizer is configured (sufficient for the test stub —
    /// production callers ship a real `MiniLM` tokenizer alongside the model).
    ///
    /// # Errors
    /// Returns [`EmbedderError::NotFound`] if `path` does not exist;
    /// [`EmbedderError::Ort`] for ONNX errors;
    /// [`EmbedderError::Tokenizer`] for tokenizer parse errors.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, EmbedderError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(EmbedderError::NotFound(path.to_owned()));
        }
        let session = std::sync::Mutex::new(Session::builder()?.commit_from_file(path)?);
        let tok_path = path.with_extension("tokenizer.json");
        let tokenizer = if tok_path.exists() {
            Tokenizer::from_file(&tok_path).map_err(|e| EmbedderError::Tokenizer(e.to_string()))?
        } else {
            Self::default_stub_tokenizer().map_err(|e| EmbedderError::Tokenizer(e.to_string()))?
        };
        Ok(Self { session, tokenizer })
    }

    /// Encode `text` and return a 384-dim f32 vector.
    ///
    /// # Errors
    /// Propagates tokenizer and ONNX errors; returns [`EmbedderError::BadShape`]
    /// if the output rank is not `[batch=1, 384]`.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedderError> {
        let enc = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&u| i64::from(u)).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&u| i64::from(u)).collect();
        // ONNX graphs trained on real text expect at least one token; guard
        // empty sequences here so we never hand the runtime an `[1, 0]`
        // tensor (some op libs reject that).
        let seq_len = if ids.is_empty() { 1 } else { ids.len() };
        let ids_buf = if ids.is_empty() { vec![0_i64] } else { ids };
        let mask_buf = if mask.is_empty() { vec![1_i64] } else { mask };

        // ort rc.12: build `[1, seq_len]` i64 tensors that BORROW the flat
        // buffers (shape tuple + slice) — no `ndarray::Array2` intermediary.
        let ids_t = TensorRef::from_array_view(([1_usize, seq_len], ids_buf.as_slice()))?;
        let mask_t = TensorRef::from_array_view(([1_usize, seq_len], mask_buf.as_slice()))?;
        // rc.12 `Session::run` takes `&mut self`; lock the session for the call
        // and hold the guard through extraction (the output view borrows it).
        let mut session = self.session.lock().map_err(|_| EmbedderError::SessionPoisoned)?;
        let outputs = session.run(ort::inputs!["input_ids" => ids_t, "attention_mask" => mask_t])?;

        // `try_extract_array` yields a dynamic-dim ndarray view; read it by shape
        // so we stay agnostic to ort's internal ndarray version.
        let arr = outputs[0].try_extract_array::<f32>()?;
        let shape = arr.shape();
        if shape.len() != 2 || shape[0] != 1 || shape[1] != EMBED_DIM {
            return Err(EmbedderError::BadShape { got: shape.to_vec() });
        }
        Ok(arr.iter().copied().collect())
    }

    /// Minimal whitespace word-level tokenizer used when no sibling
    /// `*.tokenizer.json` exists. The vocab is `{"[UNK]": 0}` so every word
    /// maps to id 0 — sufficient for the test stub which only needs a
    /// deterministic non-empty `(ids, mask)` pair to drive the ONNX graph.
    fn default_stub_tokenizer() -> Result<Tokenizer, Box<dyn std::error::Error + Send + Sync>> {
        let mut vocab: HashMap<String, u32> = HashMap::new();
        vocab.insert("[UNK]".to_string(), 0);
        let model: WordLevel = WordLevel::builder()
            .vocab(vocab)
            .unk_token("[UNK]".to_string())
            .build()?;
        let wrapper: ModelWrapper = model.into();
        let mut tok = Tokenizer::new(wrapper);
        tok.with_pre_tokenizer(Some(Whitespace {}));
        Ok(tok)
    }
}
