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

/// Binary format magic word.
const MAGIC: u32 = 0xC0FF_EE42;
/// Binary format version.
const VERSION: u32 = 1;

/// Number of Lloyd iterations before giving up.
const MAX_ITERS: u32 = 25;
/// Total centroid movement threshold for convergence.
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
// The `Quantizer` prefix mirrors the module name; we suppress the lint to
// keep the error type unambiguously nameable from outside the crate.
#[allow(clippy::module_name_repetitions)]
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
#[derive(Debug)]
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

        // Compute global scale from max |delta| across all training vectors.
        let mut max_abs: f32 = 0.0;
        for v in training {
            let cid = nearest_centroid(&centroids, v);
            for (vi, ci) in v.iter().zip(centroids[cid].iter()) {
                let d = (vi - ci).abs();
                if d > max_abs {
                    max_abs = d;
                }
            }
        }
        // Prevent division by zero; if all deltas are zero, scale = 1.0 is fine.
        let scale = if max_abs == 0.0 { 1.0 } else { max_abs / 127.0 };

        // Normalise centroids to unit sphere so cosine == dot for queries.
        for c in centroids.iter_mut() {
            let norm: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            for x in c.iter_mut() {
                *x /= norm;
            }
        }

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
            // `raw` is finite (scale > 0 and inputs are bounded); clamp before
            // narrowing.  The i32 intermediate is [-127, 127] so the i8 cast
            // cannot truncate — the allow covers both casts on this expression.
            #[allow(clippy::cast_possible_truncation)]
            {
                *slot = (raw.round() as i32).clamp(-127, 127) as i8;
            }
        }
        // NUM_CENTROIDS == 256, so cid ∈ [0, 255] and fits in u8.
        #[allow(clippy::cast_possible_truncation)]
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
        // 4 (magic) + 4 (version) + 4 (scale) + centroids
        let centroid_bytes = NUM_CENTROIDS * EMBED_DIM * 4;
        let mut buf = Vec::with_capacity(12 + centroid_bytes);
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
    /// Panics if the buffer length check passes but a slice of exactly 4 bytes
    /// cannot be taken — this is unreachable given correct `expected_len`.
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
            return Err(QuantizerError::TooFewSamples { got: 0, min: 1 });
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().expect("slice is 4 bytes"));
        if version != VERSION {
            return Err(QuantizerError::TooFewSamples { got: 0, min: 1 });
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

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// k-means++ initialisation: pick centroids sequentially with probability
/// proportional to squared distance to the nearest already-chosen centroid.
fn kmeans_plus_plus_init(
    training: &[[f32; EMBED_DIM]],
    rng: &mut ChaCha8Rng,
) -> Box<[[f32; EMBED_DIM]; NUM_CENTROIDS]> {
    let mut chosen: Vec<[f32; EMBED_DIM]> = Vec::with_capacity(NUM_CENTROIDS);

    // First centroid: uniform random.
    let first_idx = rng.gen_range(0..training.len());
    chosen.push(training[first_idx]);

    while chosen.len() < NUM_CENTROIDS {
        // Compute squared distance to nearest already-chosen centroid for each point.
        let dists: Vec<f32> = training
            .iter()
            .map(|v| chosen.iter().map(|c| sq_dist(v, c)).fold(f32::INFINITY, f32::min))
            .collect();

        let total: f32 = dists.iter().sum();
        if total == 0.0 {
            // All points coincide with existing centroids; pick randomly.
            let idx = rng.gen_range(0..training.len());
            chosen.push(training[idx]);
            continue;
        }

        // Sample proportional to squared distance.
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

    // Move into a boxed fixed-size array.
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
    for iter in 0..MAX_ITERS {
        // Assign each point to its nearest centroid.
        let assignments: Vec<usize> = training.iter().map(|v| nearest_centroid(&centroids, v)).collect();

        // Recompute centroids as the mean of their cluster members.
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
                // Empty cluster: keep previous centroid.
                new_centroids[k] = centroids[k];
            } else {
                // Casting usize to f32 can lose precision for very large counts,
                // but training sets are bounded by memory; 2^23 vectors is ~3 GiB
                // of f32 embeddings, so the cast is safe in practice.
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

        // Last iteration: if we haven't converged, return error.
        if iter == MAX_ITERS - 1 {
            return Err(QuantizerError::NoConverge { iters: MAX_ITERS });
        }
    }
    // Unreachable: the loop always returns Ok or Err before this point.
    Ok(centroids)
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
