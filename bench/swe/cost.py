# SPDX-License-Identifier: Apache-2.0
"""Cost a run's token usage with origin's OWN price table.

`prices.json` is generated from the `origin-cost` crate via
`cargo run -p xtask -- prices > bench/swe/prices.json`, so every contestant in
the A/B (origin and competitors alike) is costed with identical rates — the
cross-harness $ comparison stays apples-to-apples.

This mirrors `origin_cost::price_for`: normalize the model id (drop any
provider prefix and `:tag` suffix), then pick the LONGEST matching prefix.
"""
from __future__ import annotations

import json
import os
from functools import lru_cache

_HERE = os.path.dirname(os.path.abspath(__file__))
_PRICES_PATH = os.path.join(_HERE, "prices.json")


@lru_cache(maxsize=1)
def _table() -> list[dict]:
    with open(_PRICES_PATH, encoding="utf-8") as f:
        return json.load(f)


def _normalize(model: str) -> str:
    """Mirror origin-cost::normalize — lowercase, take the segment after the last
    '/', then strip anything from the first ':' (e.g. an ollama `:tag`)."""
    s = model.lower()
    s = s.rsplit("/", 1)[-1]
    s = s.split(":", 1)[0]
    return s


def price_for(model: str) -> dict | None:
    """Longest-prefix price row for `model`, or None if unknown (cost 0)."""
    needle = _normalize(model)
    best = None
    for row in _table():
        p = row["prefix"]
        if needle.startswith(p) and (best is None or len(p) > len(best["prefix"])):
            best = row
    return best


def cost_usd(
    model: str,
    input_tokens: int = 0,
    output_tokens: int = 0,
    cache_read: int = 0,
    cache_write: int = 0,
) -> float:
    """USD cost of a token usage at origin's rates. Unknown model ⇒ 0.0."""
    p = price_for(model)
    if p is None:
        return 0.0
    m = 1_000_000.0
    return (
        input_tokens * p["input_per_mtok"]
        + output_tokens * p["output_per_mtok"]
        + cache_read * p["cache_read_per_mtok"]
        + cache_write * p["cache_write_per_mtok"]
    ) / m


if __name__ == "__main__":  # tiny self-check: `python cost.py claude-opus-4-8 1000 500`
    import sys

    if len(sys.argv) >= 2:
        mdl = sys.argv[1]
        ins = int(sys.argv[2]) if len(sys.argv) > 2 else 0
        outs = int(sys.argv[3]) if len(sys.argv) > 3 else 0
        print(f"{mdl}: ${cost_usd(mdl, ins, outs):.6f}  (price row: {price_for(mdl)})")
