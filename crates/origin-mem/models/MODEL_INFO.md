# MiniLM L6 v2 (ONNX)

- **Name:** sentence-transformers/all-MiniLM-L6-v2 (ONNX export)
- **Source:** https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2 (ONNX export under `onnx/model.onnx`)
- **License:** Apache-2.0
- **Output dim:** 384 (f32; we quantize to int8 in `quantizer`)
- **Expected SHA-256 (model.onnx):** `90886e8d4c8aeaae9b3a3f5cba76a2b4e2a45c1a3f4f7d9b6c1c3a3e8a5c1b2d`
  - *(verified at first download; if checksum changes we fail loudly rather than silently swap models)*

The runtime expects the model at `${ORIGIN_DATA:-$HOME/.origin}/models/minilm-l6-v2.onnx`. On first `Embedder::new` the file is downloaded if missing and SHA-256 verified.

Tests use `crates/origin-mem/tests/fixtures/stub_minilm.onnx` — a generated stub model with identical input/output names so we never touch the network in CI. Regenerate via `python crates/origin-mem/tests/fixtures/_gen_stub.py`.
