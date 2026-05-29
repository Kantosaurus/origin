// SPDX-License-Identifier: Apache-2.0
//! Int8 quantizer with 256 per-cluster centroid offsets.
//!
//! Each vector is stored as `(centroid_id, deltas)` where each delta is the
//! f32 residual from the centroid, scaled to i8 by a global per-quantizer
//! scale factor.  The result is a ~32× memory reduction over raw f32 vectors
//! with asymmetric dot-product queries.
//!
//! # Binary format (little-endian)
//! `[u32 magic][u32 version][f32 scale][NUM_CENTROIDS * EMBED_DIM × f32 centroids]`

use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use thiserror::Error;

use crate::EMBED_DIM;

/// Number of cluster centroids learned by [`Quantizer::fit`]. Constant per spec N6.1.
pub const NUM_CENTROIDS: usize = 256;

const MAGIC: u32 = 0xC0FF_EE42;
const VERSION: u32 = 1;
const MAX_ITERS: u32 = 25;
const CONVERGE_THRESHOLD: f32 = 1e-4;

/// One quantized vector: `(centroid_id, deltas)` where each delta is the f32
/// residual from the centroid, scaled to i8 by a per-quantizer global scale.
#[derive(Debug, Clone)]
pub struct EncodedVector {
    /// Index of the nearest centroid.
    pub centroid_id: u8,
    /// Residual deltas encoded as i8: `real_delta ≈ i8_delta * scale`.
    pub deltas: Box<[i8; EMBED_DIM]>,
}

/// Errors from [`Quantizer`] operations.
#[allow(clippy::module_name_repetitions)] // unambiguous from outside the crate
#[derive(Debug, Error)]
pub enum QuantizerError {
    /// Training set is too small to learn `NUM_CENTROIDS` centroids.
    #[error("training set must contain at least {min} vectors, got {got}")]
    TooFewSamples {
        /// Actual number of training vectors supplied.
        got: usize,
        /// Minimum required.
        min: usize,
    },
    /// Lloyd iterations exhausted without convergence.
    #[error("k-means failed to converge after {iters} iterations")]
    NoConverge {
        /// Number of iterations attempted.
        iters: u32,
    },
}

/// Int8 product quantizer: 256 centroids + a global delta scale.
#[derive(Debug, Clone)]
pub struct Quantizer {
    centroids: Box<[[f32; EMBED_DIM]; NUM_CENTROIDS]>,
    /// Global i8 scale: `real_delta = i8_delta * scale`.
    scale: f32,
}

impl Quantizer {
    /// Train via k-means++ init + Lloyd refinement (max 25 iters).
    ///
    /// Requires at least `NUM_CENTROIDS` training vectors.
    ///
    /// # Errors
    /// - [`QuantizerError::TooFewSamples`] if `training.len() < NUM_CENTROIDS`.
    /// - [`QuantizerError::NoConverge`] if the iteration budget is exceeded
    ///   with centroid movement still above 1e-4.
    #[must_use = "training produces a Quantizer that must be used to encode/decode vectors"]
    pub fn fit(training: &[[f32; EMBED_DIM]], rng_seed: u64) -> Result<Self, QuantizerError> {
        if training.len() < NUM_CENTROIDS {
            return Err(QuantizerError::TooFewSamples {
                got: training.len(),
                min: NUM_CENTROIDS,
            });
        }
        let mut rng = ChaCha8Rng::seed_from_u64(rng_seed);
        let centroids = kmeans_plus_plus_init(training, &mut rng);
        let mut centroids = lloyd(centroids, training)?;
        // Normalise centroids to unit sphere so cosine == dot for queries.
        for c in centroids.iter_mut() {
            let norm: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            for x in c.iter_mut() {
                *x /= norm;
            }
        }
        // Global scale from max |delta| across all training vectors, computed
        // AGAINST THE NORMALISED CENTROIDS. Quantization at query time stores
        // residuals against these normalised centroids, so the scale must be
        // sized from those same residuals — computing it before normalisation
        // undersizes `scale`, clipping/saturating real deltas to ±127.
        let mut max_abs: f32 = 0.0;
        for v in training {
            let cid = nearest_centroid(&centroids, v);
            for (vi, ci) in v.iter().zip(centroids[cid].iter()) {
                max_abs = max_abs.max((vi - ci).abs());
            }
        }
        let scale = if max_abs == 0.0 { 1.0 } else { max_abs / 127.0 };
        Ok(Self { centroids, scale })
    }

    /// Encode `v` as the nearest centroid plus i8 residual deltas.
    #[must_use]
    pub fn encode(&self, v: &[f32; EMBED_DIM]) -> EncodedVector {
        debug_assert!(
            {
                let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                (norm - 1.0).abs() < 1e-3
            },
            "encode: input vector is not unit-normalised (norm differs from 1 by >1e-3)"
        );

        let cid = nearest_centroid(&self.centroids, v);
        let centroid = &self.centroids[cid];
        let mut deltas = Box::new([0_i8; EMBED_DIM]);
        for (slot, (vi, ci)) in deltas.iter_mut().zip(v.iter().zip(centroid.iter())) {
            let raw = (vi - ci) / self.scale;
            // i32 intermediate is [-127,127]; both narrowing casts are safe.
            #[allow(clippy::cast_possible_truncation)]
            {
                *slot = (raw.round() as i32).clamp(-127, 127) as i8;
            }
        }
        #[allow(clippy::cast_possible_truncation)] // NUM_CENTROIDS==256 → fits u8
        let centroid_id = cid as u8;
        EncodedVector { centroid_id, deltas }
    }

    /// Reconstruct (lossy) the original vector — useful for HNSW seeding.
    #[must_use]
    pub fn decode(&self, e: &EncodedVector) -> [f32; EMBED_DIM] {
        let centroid = &self.centroids[e.centroid_id as usize];
        let mut out = [0_f32; EMBED_DIM];
        for (slot, (ci, di)) in out.iter_mut().zip(centroid.iter().zip(e.deltas.iter())) {
            *slot = f32::from(*di).mul_add(self.scale, *ci);
        }
        out
    }

    /// Approximate dot product of a fresh f32 query against an encoded vector.
    ///
    /// Uses `dot(query, centroid) + Σ query[i] * scale * delta[i]` in one pass.
    #[must_use]
    pub fn dot(&self, query: &[f32; EMBED_DIM], e: &EncodedVector) -> f32 {
        let centroid = &self.centroids[e.centroid_id as usize];
        let mut acc = 0.0_f32;
        for ((qi, ci), di) in query.iter().zip(centroid.iter()).zip(e.deltas.iter()) {
            acc += qi * f32::from(*di).mul_add(self.scale, *ci);
        }
        acc
    }

    /// Serialise centroids and scale for persistence.
    ///
    /// Format (all little-endian):
    /// `[u32 magic][u32 version][f32 scale][NUM_CENTROIDS * EMBED_DIM × f32]`
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(12 + NUM_CENTROIDS * EMBED_DIM * 4);
        buf.extend_from_slice(&MAGIC.to_le_bytes());
        buf.extend_from_slice(&VERSION.to_le_bytes());
        buf.extend_from_slice(&self.scale.to_le_bytes());
        for c in self.centroids.as_ref() {
            for v in c {
                buf.extend_from_slice(&v.to_le_bytes());
            }
        }
        buf
    }

    /// Deserialise from bytes produced by [`Self::to_bytes`].
    ///
    /// # Errors
    /// Returns [`QuantizerError::TooFewSamples`] (repurposed as "malformed buffer")
    /// when the buffer length, magic, or version does not match.
    ///
    /// # Panics
    /// Panics if the length check passes but a 4-byte slice cannot be taken —
    /// structurally unreachable given correct `expected_len`.
    #[must_use = "deserialised Quantizer must be used to encode/decode vectors"]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, QuantizerError> {
        let expected_len = 12 + NUM_CENTROIDS * EMBED_DIM * 4;
        if bytes.len() != expected_len {
            return Err(QuantizerError::TooFewSamples {
                got: bytes.len(),
                min: expected_len,
            });
        }
        let magic = u32::from_le_bytes(bytes[0..4].try_into().expect("slice is 4 bytes"));
        if magic != MAGIC {
            return Err(QuantizerError::TooFewSamples {
                got: magic as usize,
                min: MAGIC as usize,
            });
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("slice is 4 bytes"));
        if version != VERSION {
            return Err(QuantizerError::TooFewSamples {
                got: version as usize,
                min: VERSION as usize,
            });
        }
        let scale = f32::from_le_bytes(bytes[8..12].try_into().expect("slice is 4 bytes"));
        let mut centroids = Box::new([[0_f32; EMBED_DIM]; NUM_CENTROIDS]);
        let mut offset = 12_usize;
        for c in centroids.iter_mut() {
            for v in c.iter_mut() {
                *v = f32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("slice is 4 bytes"));
                offset += 4;
            }
        }
        Ok(Self { centroids, scale })
    }
}

// --- internal helpers ---
/// k-means++ initialisation: pick centroids sequentially with probability
/// proportional to squared distance to the nearest already-chosen centroid.
fn kmeans_plus_plus_init(
    training: &[[f32; EMBED_DIM]],
    rng: &mut ChaCha8Rng,
) -> Box<[[f32; EMBED_DIM]; NUM_CENTROIDS]> {
    let mut chosen: Vec<[f32; EMBED_DIM]> = Vec::with_capacity(NUM_CENTROIDS);
    chosen.push(training[rng.gen_range(0..training.len())]);

    while chosen.len() < NUM_CENTROIDS {
        let dists: Vec<f32> = training
            .iter()
            .map(|v| chosen.iter().map(|c| sq_dist(v, c)).fold(f32::INFINITY, f32::min))
            .collect();
        let total: f32 = dists.iter().sum();
        if total == 0.0 {
            chosen.push(training[rng.gen_range(0..training.len())]);
            continue;
        }
        let threshold: f32 = rng.gen_range(0.0..total);
        let mut cumsum = 0.0_f32;
        let mut picked = training.len() - 1;
        for (i, &d) in dists.iter().enumerate() {
            cumsum += d;
            if cumsum >= threshold {
                picked = i;
                break;
            }
        }
        chosen.push(training[picked]);
    }

    let mut out = Box::new([[0_f32; EMBED_DIM]; NUM_CENTROIDS]);
    for (slot, c) in out.iter_mut().zip(chosen) {
        *slot = c;
    }
    out
}

/// Lloyd's algorithm: iterative centroid refinement.
///
/// Stops when total centroid movement < `CONVERGE_THRESHOLD` or after
/// `MAX_ITERS` iterations.
fn lloyd(
    mut centroids: Box<[[f32; EMBED_DIM]; NUM_CENTROIDS]>,
    training: &[[f32; EMBED_DIM]],
) -> Result<Box<[[f32; EMBED_DIM]; NUM_CENTROIDS]>, QuantizerError> {
    for _iter in 0..MAX_ITERS {
        let assignments: Vec<usize> = training.iter().map(|v| nearest_centroid(&centroids, v)).collect();

        let mut sums = vec![[0_f32; EMBED_DIM]; NUM_CENTROIDS];
        let mut counts = vec![0_usize; NUM_CENTROIDS];
        for (v, &cid) in training.iter().zip(assignments.iter()) {
            counts[cid] += 1;
            for (s, vi) in sums[cid].iter_mut().zip(v.iter()) {
                *s += vi;
            }
        }

        let mut total_movement = 0.0_f32;
        let mut new_centroids = Box::new([[0_f32; EMBED_DIM]; NUM_CENTROIDS]);
        for k in 0..NUM_CENTROIDS {
            if counts[k] == 0 {
                new_centroids[k] = centroids[k];
            } else {
                // usize→f32: safe for training sets bounded by memory (<2^23 vecs).
                #[allow(clippy::cast_precision_loss)]
                let n = counts[k] as f32;
                let mut new_c = [0_f32; EMBED_DIM];
                for (nc, s) in new_c.iter_mut().zip(sums[k].iter()) {
                    *nc = s / n;
                }
                total_movement += sq_dist(&new_c, &centroids[k]).sqrt();
                new_centroids[k] = new_c;
            }
        }
        centroids = new_centroids;

        if total_movement < CONVERGE_THRESHOLD {
            return Ok(centroids);
        }
    }
    Err(QuantizerError::NoConverge { iters: MAX_ITERS })
}

/// Return the index of the centroid nearest to `v` (by squared L2 distance).
#[inline]
fn nearest_centroid(centroids: &[[f32; EMBED_DIM]; NUM_CENTROIDS], v: &[f32; EMBED_DIM]) -> usize {
    let mut best = 0;
    let mut best_dist = f32::INFINITY;
    for (k, c) in centroids.iter().enumerate() {
        let d = sq_dist(v, c);
        if d < best_dist {
            best_dist = d;
            best = k;
        }
    }
    best
}

/// Squared L2 distance between two fixed-size vectors.
#[inline]
fn sq_dist(a: &[f32; EMBED_DIM], b: &[f32; EMBED_DIM]) -> f32 {
    a.iter().zip(b.iter()).map(|(ai, bi)| (ai - bi) * (ai - bi)).sum()
}
