// SPDX-License-Identifier: Apache-2.0
use origin_mem::{Embedder, EMBED_DIM};

const STUB_PATH: &str = "tests/fixtures/stub_minilm.onnx";

#[test]
fn embedder_returns_384_dim_vector() {
    let e = Embedder::from_path(STUB_PATH).expect("load stub");
    let v = e.embed("hello world").expect("embed");
    assert_eq!(v.len(), EMBED_DIM);
    assert_eq!(EMBED_DIM, 384);
}

#[test]
fn embedder_is_deterministic_for_same_input() {
    let e = Embedder::from_path(STUB_PATH).expect("load stub");
    let a = e.embed("the quick brown fox").expect("embed a");
    let b = e.embed("the quick brown fox").expect("embed b");
    // Exact bit-equality; ONNX with a fixed graph + CPU exec provider is deterministic.
    assert_eq!(a, b);
}
