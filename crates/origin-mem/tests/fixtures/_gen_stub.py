"""Generate a tiny ONNX stub for `origin-mem` tests.

The stub takes (input_ids: int64[1, N], attention_mask: int64[1, N])
and returns sentence_embedding: float32[1, 384].

The graph is intentionally trivial and deterministic:
  - cast input_ids to float
  - reduce-mean along seq dim -> scalar per batch
  - multiply by a [1, 384] constant of ones -> broadcast to [batch, 384]
  - the mask is passed through Identity so it isn't pruned

Run once:
    python _gen_stub.py
to (re)produce `stub_minilm.onnx` in this directory.
"""
from __future__ import annotations

import os
import sys

import onnx
from onnx import TensorProto, helper, numpy_helper

import numpy as np


HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "stub_minilm.onnx")

EMBED_DIM = 384


def build() -> onnx.ModelProto:
    ids = helper.make_tensor_value_info(
        "input_ids", TensorProto.INT64, ["batch", "seq"]
    )
    mask = helper.make_tensor_value_info(
        "attention_mask", TensorProto.INT64, ["batch", "seq"]
    )
    out = helper.make_tensor_value_info(
        "sentence_embedding", TensorProto.FLOAT, ["batch", EMBED_DIM]
    )

    ones_384 = numpy_helper.from_array(
        np.ones((1, EMBED_DIM), dtype=np.float32), name="ones_384"
    )

    cast_node = helper.make_node(
        "Cast", ["input_ids"], ["ids_float"], to=TensorProto.FLOAT
    )
    reduce_node = helper.make_node(
        "ReduceMean",
        ["ids_float"],
        ["seq_mean"],
        axes=[1],
        keepdims=1,
    )
    mul_node = helper.make_node(
        "Mul", ["seq_mean", "ones_384"], ["sentence_embedding"]
    )
    identity_mask = helper.make_node(
        "Identity", ["attention_mask"], ["mask_passthrough"]
    )

    graph = helper.make_graph(
        nodes=[cast_node, reduce_node, mul_node, identity_mask],
        name="stub_minilm",
        inputs=[ids, mask],
        outputs=[out],
        initializer=[ones_384],
    )

    opset = helper.make_opsetid("", 13)
    model = helper.make_model(graph, opset_imports=[opset])
    model.ir_version = 7
    onnx.checker.check_model(model)
    return model


def main() -> int:
    model = build()
    onnx.save(model, OUT)
    print(f"wrote {OUT} ({os.path.getsize(OUT)} bytes)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
