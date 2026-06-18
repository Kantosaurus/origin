# SPDX-License-Identifier: Apache-2.0
"""Score SWE-bench predictions with the OFFICIAL harness, then aggregate.

Path B, steps 2-3. Correctness is judged by the canonical `swebench` evaluator
(Docker per task, hidden FAIL_TO_PASS / PASS_TO_PASS tests) — we never decide
pass/fail ourselves. We then JOIN the resolved set with the token/$/wall metrics
captured during generation and report, per harness:

  * pass@1  (mean resolve rate across all instance x seed runs)
  * mean input/output tokens, $ cost, wall-clock per solved+attempted task
  * 95% bootstrap confidence intervals over instances (variance, not a single
    number — the whole point of running multiple seeds)

Usage:
    python bench/swe/evaluate.py --out bench/swe/out --max-workers 8
    # or, if you scored elsewhere, pass resolved-id files instead of re-running:
    python bench/swe/evaluate.py --out bench/swe/out --resolved origin=origin.resolved.txt
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import subprocess
import sys

DATASET = "princeton-nlp/SWE-bench_Verified"


def read_jsonl(path: str) -> list[dict]:
    with open(path, encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


def run_official(pred_file: str, run_id: str, max_workers: int) -> set[str]:
    """Invoke `swebench.harness.run_evaluation` and parse its report for the
    resolved instance_ids. Robust to minor report-path differences across
    swebench versions: after the run we glob for a JSON carrying `resolved_ids`."""
    cmd = [
        sys.executable, "-m", "swebench.harness.run_evaluation",
        "--dataset_name", DATASET,
        "--predictions_path", pred_file,
        "--max_workers", str(max_workers),
        "--run_id", run_id,
    ]
    print("  $ " + " ".join(cmd), file=sys.stderr)
    subprocess.run(cmd, check=False)
    # Newer swebench writes "<model>.<run_id>.json"; search broadly for the report.
    for cand in sorted(glob.glob(f"**/*{run_id}*.json", recursive=True), key=len):
        try:
            data = json.load(open(cand, encoding="utf-8"))
        except Exception:
            continue
        if isinstance(data, dict) and ("resolved_ids" in data or "resolved" in data):
            ids = data.get("resolved_ids") or data.get("resolved") or []
            print(f"  report: {cand}  resolved={len(ids)}", file=sys.stderr)
            return set(ids)
    print(f"  WARNING: no resolved-ids report found for run_id={run_id}; "
          "treating all as unresolved. Check your swebench version's output path.", file=sys.stderr)
    return set()


def bootstrap_ci(values: list[float], iters: int = 2000, alpha: float = 0.05) -> tuple[float, float, float]:
    """Mean + percentile [alpha/2, 1-alpha/2] bootstrap CI. Deterministic seed so
    re-running the aggregation gives identical bars."""
    import random

    if not values:
        return 0.0, 0.0, 0.0
    rng = random.Random(1234)
    n = len(values)
    means = []
    for _ in range(iters):
        s = sum(values[rng.randrange(n)] for _ in range(n)) / n
        means.append(s)
    means.sort()
    lo = means[int((alpha / 2) * iters)]
    hi = means[int((1 - alpha / 2) * iters) - 1]
    return sum(values) / n, lo, hi


def main() -> int:
    ap = argparse.ArgumentParser(description="Score + aggregate SWE-bench predictions.")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "out"))
    ap.add_argument("--max-workers", type=int, default=8)
    ap.add_argument("--resolved", action="append", default=[],
                    help="harness=path-to-resolved-ids.txt — skip running the official harness")
    args = ap.parse_args()

    pred_dir = os.path.join(args.out, "predictions")
    met_dir = os.path.join(args.out, "metrics")
    preset_resolved = {kv.split("=", 1)[0]: kv.split("=", 1)[1] for kv in args.resolved}

    rows = []  # one per (harness, instance, seed): resolved + metrics
    harnesses = sorted(os.path.splitext(os.path.basename(p))[0] for p in glob.glob(os.path.join(pred_dir, "*.jsonl")))
    for harness in harnesses:
        preds = read_jsonl(os.path.join(pred_dir, f"{harness}.jsonl"))
        metrics = {(m["instance_id"], m["seed"]): m for m in read_jsonl(os.path.join(met_dir, f"{harness}.jsonl"))}
        seeds = sorted({p.get("_seed", 0) for p in preds})
        for seed in seeds:
            seed_preds = [p for p in preds if p.get("_seed", 0) == seed]
            if harness in preset_resolved:
                resolved = {x.strip() for x in open(preset_resolved[harness], encoding="utf-8") if x.strip()}
            else:
                # one prediction per instance for the official scorer
                tmp = os.path.join(args.out, f".score.{harness}.seed{seed}.jsonl")
                with open(tmp, "w", encoding="utf-8") as f:
                    for p in seed_preds:
                        f.write(json.dumps({k: v for k, v in p.items() if not k.startswith("_")}) + "\n")
                resolved = run_official(tmp, f"{harness}-s{seed}", args.max_workers)
            for p in seed_preds:
                iid = p["instance_id"]
                m = metrics.get((iid, seed), {})
                rows.append({
                    "harness": harness, "instance_id": iid, "seed": seed,
                    "resolved": iid in resolved,
                    "input_tokens": m.get("input_tokens", 0), "output_tokens": m.get("output_tokens", 0),
                    "cost_usd": m.get("cost_usd", 0.0), "wall_s": m.get("wall_s", 0.0),
                })

    # Aggregate per harness.
    table = []
    for harness in harnesses:
        hr = [r for r in rows if r["harness"] == harness]
        if not hr:
            continue
        passed = [1.0 if r["resolved"] else 0.0 for r in hr]
        p1, p1lo, p1hi = bootstrap_ci(passed)
        tok = [r["input_tokens"] + r["output_tokens"] for r in hr]
        cost = [r["cost_usd"] for r in hr]
        wall = [r["wall_s"] for r in hr]
        table.append({
            "harness": harness, "runs": len(hr),
            "pass_at_1": round(p1, 4), "pass_ci": [round(p1lo, 4), round(p1hi, 4)],
            "mean_tokens": round(sum(tok) / len(tok), 1),
            "mean_cost_usd": round(sum(cost) / len(cost), 6),
            "mean_wall_s": round(sum(wall) / len(wall), 1),
        })
    table.sort(key=lambda r: (-r["pass_at_1"], r["mean_cost_usd"]))

    out_json = os.path.join(args.out, "leaderboard.json")
    json.dump({"rows_per_run": rows, "leaderboard": table}, open(out_json, "w", encoding="utf-8"), indent=2)

    md = ["# SWE-bench Verified — A/B leaderboard", "",
          "| harness | runs | pass@1 (95% CI) | mean tokens | mean $ | mean wall (s) |",
          "|---|---:|---:|---:|---:|---:|"]
    for r in table:
        md.append(f"| {r['harness']} | {r['runs']} | {r['pass_at_1']:.1%} "
                  f"[{r['pass_ci'][0]:.1%}, {r['pass_ci'][1]:.1%}] | {r['mean_tokens']:.0f} | "
                  f"${r['mean_cost_usd']:.4f} | {r['mean_wall_s']:.0f} |")
    md += ["", "_Same model held constant across harnesses; pass/fail from the official "
           "swebench evaluator; $ from origin-cost rates (bench/swe/prices.json)._"]
    out_md = os.path.join(args.out, "leaderboard.md")
    open(out_md, "w", encoding="utf-8").write("\n".join(md) + "\n")
    print("\n".join(md))
    print(f"\nwrote {out_md}\nwrote {out_json}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
