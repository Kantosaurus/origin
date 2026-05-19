//! Learned-dictionary zstd compression (N3.2).

use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DictVersion(pub u32);

#[derive(Debug, Error)]
pub enum DictError {
    #[error("training failed: {0}")]
    Train(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not enough samples: have {have}, need {need}")]
    Insufficient { have: usize, need: usize },
}

pub const TARGET_DICT_BYTES: usize = 64 * 1024;
pub const MIN_SAMPLES_FOR_TRAINING: usize = 16;

/// Train a 64KB zstd dictionary from `samples`.
///
/// # Errors
/// Returns `Insufficient` if there are fewer than `MIN_SAMPLES_FOR_TRAINING`
/// samples, or `Train` if zstd rejects the training set.
pub fn train(samples: &[Vec<u8>]) -> Result<Vec<u8>, DictError> {
    if samples.len() < MIN_SAMPLES_FOR_TRAINING {
        return Err(DictError::Insufficient {
            have: samples.len(),
            need: MIN_SAMPLES_FOR_TRAINING,
        });
    }
    zstd::dict::from_samples(samples, TARGET_DICT_BYTES).map_err(|e| DictError::Train(e.to_string()))
}

/// Read the persisted dict file at `path`. Returns `None` if absent.
///
/// # Errors
/// Returns `Io` for any read error other than NotFound.
pub fn load_dict_file(path: &Path) -> Result<Option<Vec<u8>>, DictError> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(DictError::Io(e)),
    }
}
