// SPDX-License-Identifier: Apache-2.0
use origin_mem::quantizer::{Quantizer, NUM_CENTROIDS};
use origin_mem::EMBED_DIM;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

fn synth(rng_seed: u64, n: usize) -> Vec<[f32; EMBED_DIM]> {
    let mut rng = ChaCha8Rng::seed_from_u64(rng_seed);
    (0..n)
        .map(|_| {
            let mut v = [0_f32; EMBED_DIM];
            for slot in &mut v {
                *slot = rng.gen_range(-1.0..1.0);
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            for slot in &mut v {
                *slot /= norm;
            }
            v
        })
        .collect()
}

#[test]
fn fit_encode_dot_approximates_f32() {
    // 0x00C0_FFEE is a memorable hex seed; the `unreadable_literal` lint would
    // prefer this form but it is not a production constant, just a test seed.
    #[allow(clippy::unreadable_literal)]
    let training = synth(0xC0FFEE, NUM_CENTROIDS * 4);
    #[allow(clippy::unreadable_literal)]
    let q = Quantizer::fit(&training, 0xC0FFEE).expect("fit");
    let query = training[0];
    let target = training[1];
    let f32_dot: f32 = query.iter().zip(target.iter()).map(|(a, b)| a * b).sum();
    let enc = q.encode(&target);
    let approx = q.dot(&query, &enc);
    let err = (approx - f32_dot).abs();
    assert!(
        err < 0.02,
        "approx dot off by {err} (f32={f32_dot} approx={approx})"
    );
}

#[test]
fn round_trip_bytes_preserves_dot() {
    let training = synth(0xBEEF, NUM_CENTROIDS * 4);
    let q = Quantizer::fit(&training, 0xBEEF).expect("fit");
    let bytes = q.to_bytes();
    let q2 = Quantizer::from_bytes(&bytes).expect("from_bytes");
    let enc = q.encode(&training[0]);
    let enc2 = q2.encode(&training[0]);
    assert_eq!(enc.centroid_id, enc2.centroid_id);
    assert_eq!(enc.deltas[..], enc2.deltas[..]);
}

#[test]
fn too_few_samples_errors() {
    let training = synth(1, NUM_CENTROIDS - 1);
    let err = Quantizer::fit(&training, 1).expect_err("must error");
    match err {
        origin_mem::quantizer::QuantizerError::TooFewSamples { got, min } => {
            assert_eq!(got, NUM_CENTROIDS - 1);
            assert_eq!(min, NUM_CENTROIDS);
        }
        // Only two variants exist; this arm is exhaustive and the message is
        // useful for diagnosing unexpected test failures.
        #[allow(clippy::panic)]
        other @ origin_mem::quantizer::QuantizerError::NoConverge { .. } => {
            panic!("wrong variant: {other:?}");
        }
    }
}
