//! ONNX `MiniLM` wrapper. Loads a sentence-transformer ONNX graph and exposes
//! `embed(text) -> [f32; 384]`. CPU execution provider only.

use ndarray::Array2;
use ort::{Session, SessionInputValue, Value};
use std::borrow::Cow;
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
}

/// Sentence-embedding pipeline over an ONNX MiniLM-class model.
///
/// One [`Embedder`] owns a single ONNX [`Session`] and tokenizer. It is cheap
/// to call [`Self::embed`] repeatedly; expensive to construct.
pub struct Embedder {
    session: Session,
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
        let session = Session::builder()?.commit_from_file(path)?;
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
        let ids_arr: Array2<i64> = Array2::from_shape_vec((1, seq_len), ids_buf)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;
        let mask_arr: Array2<i64> = Array2::from_shape_vec((1, seq_len), mask_buf)
            .map_err(|e| EmbedderError::Tokenizer(e.to_string()))?;

        let ids_val = Value::from_array(ids_arr)?;
        let mask_val = Value::from_array(mask_arr)?;
        let inputs: Vec<(Cow<'_, str>, SessionInputValue<'_>)> = vec![
            (Cow::Borrowed("input_ids"), ids_val.into()),
            (Cow::Borrowed("attention_mask"), mask_val.into()),
        ];
        let outputs = self.session.run(inputs)?;

        let tensor = outputs[0].try_extract_tensor::<f32>()?;
        let view = tensor.view();
        let shape = view.shape();
        if shape.len() != 2 || shape[0] != 1 || shape[1] != EMBED_DIM {
            return Err(EmbedderError::BadShape { got: shape.to_vec() });
        }
        Ok(view.iter().copied().collect())
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
