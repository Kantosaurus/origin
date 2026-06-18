# SPDX-License-Identifier: Apache-2.0
"""Generate SWE-bench predictions + per-task metrics for ONE harness.

Path B, step 1 (generation). For each instance in a subset of SWE-bench Verified
and each seed:

  1. lay down a clean checkout of <repo>@<base_commit> (cached bare clone + a
     throwaway worktree),
  2. run the harness adapter (`adapters/<harness>.sh <repo_dir> <issue_file>
     <metrics_out_json>`) which edits the repo IN PLACE and prints the unified
     diff (the candidate patch) to stdout,
  3. record the prediction in SWE-bench format -> predictions/<harness>.jsonl and
     the token/$/wall metrics -> metrics/<harness>.jsonl.

Scoring is intentionally NOT done here — `evaluate.py` runs the OFFICIAL
`swebench` harness (Docker, hidden FAIL_TO_PASS/PASS_TO_PASS tests) over these
predictions so correctness is judged by the canonical scorer, not by us.

Example:
    python bench/swe/run.py --harness origin --adapter bench/swe/adapters/origin.sh \\
        -n 50 --seeds 3 --out bench/swe/out
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

import cost as costlib  # bench/swe/cost.py

DATASET = "princeton-nlp/SWE-bench_Verified"


def sh(args: list[str], cwd: str | None = None, timeout: int | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(args, cwd=cwd, timeout=timeout, capture_output=True, text=True)


def load_instances(split: str, n: int | None, ids_file: str | None) -> list[dict]:
    """Load SWE-bench Verified rows. A fixed slice (`-n`) keeps the subset stable
    across harnesses so every contestant solves the SAME tasks."""
    from datasets import load_dataset  # lazy: only needed at run time

    ds = load_dataset(DATASET, split=split)
    rows = [dict(r) for r in ds]
    rows.sort(key=lambda r: r["instance_id"])  # deterministic ordering
    if ids_file:
        wanted = {x.strip() for x in open(ids_file, encoding="utf-8") if x.strip()}
        rows = [r for r in rows if r["instance_id"] in wanted]
    if n is not None:
        rows = rows[:n]
    return rows


def repo_cache(cache_root: str, repo: str) -> str:
    """A persistent bare clone of `owner/name`, fetched once and reused."""
    safe = repo.replace("/", "__")
    bare = os.path.join(cache_root, safe + ".git")
    if not os.path.isdir(bare):
        os.makedirs(cache_root, exist_ok=True)
        sh(["git", "clone", "--bare", f"https://github.com/{repo}.git", bare])
    return bare


def checkout(bare: str, base_commit: str, dest: str) -> bool:
    """Lay down a clean worktree of `base_commit` at `dest`. Fetches the commit
    on demand (Verified base commits are sometimes not on a default branch)."""
    sh(["git", "fetch", "--quiet", "origin", base_commit], cwd=bare)
    r = sh(["git", "--work-tree", dest, "--git-dir", bare, "checkout", "--force", base_commit, "--", "."])
    # Stamp the worktree as its own repo so the adapter's `git diff` is clean.
    if r.returncode != 0:
        # Fallback: full clone + checkout (slower but robust).
        sh(["git", "clone", "--quiet", bare, dest])
        if sh(["git", "checkout", "--force", base_commit], cwd=dest).returncode != 0:
            return False
        return True
    sh(["git", "init", "--quiet", dest])
    sh(["git", "-C", dest, "add", "-A"])
    sh(["git", "-C", dest, "-c", "user.email=b@b", "-c", "user.name=b", "commit", "--quiet", "-m", "base"])
    return True


def main() -> int:
    ap = argparse.ArgumentParser(description="Generate SWE-bench predictions + metrics for one harness.")
    ap.add_argument("--harness", required=True, help="label recorded as model_name_or_path (e.g. origin, aider, claude-code)")
    ap.add_argument("--adapter", required=True, help="path to the harness adapter script")
    ap.add_argument("--split", default="test")
    ap.add_argument("-n", "--num", type=int, default=None, help="first N instances (stable subset)")
    ap.add_argument("--ids", default=None, help="file of instance_ids (one per line) to restrict to")
    ap.add_argument("--seeds", type=int, default=1, help="independent runs per task (pass@1 variance)")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "out"))
    ap.add_argument("--cache", default=os.path.join(os.path.dirname(__file__), ".cache", "repos"))
    ap.add_argument("--timeout", type=int, default=1200, help="per-task wall-clock cap (s)")
    args = ap.parse_args()

    adapter = os.path.abspath(args.adapter)
    if not os.path.isfile(adapter):
        print(f"adapter not found: {adapter}", file=sys.stderr)
        return 2

    rows = load_instances(args.split, args.num, args.ids)
    print(f"[{args.harness}] {len(rows)} instances x {args.seeds} seed(s)", file=sys.stderr)

    pred_dir = os.path.join(args.out, "predictions")
    met_dir = os.path.join(args.out, "metrics")
    os.makedirs(pred_dir, exist_ok=True)
    os.makedirs(met_dir, exist_ok=True)
    pred_path = os.path.join(pred_dir, f"{args.harness}.jsonl")
    met_path = os.path.join(met_dir, f"{args.harness}.jsonl")

    with open(pred_path, "w", encoding="utf-8") as pf, open(met_path, "w", encoding="utf-8") as mf:
        for row in rows:
            iid = row["instance_id"]
            bare = repo_cache(args.cache, row["repo"])
            for seed in range(args.seeds):
                work = tempfile.mkdtemp(prefix="swe_")
                metrics_out = os.path.join(work, "_metrics.json")
                issue_file = os.path.join(work, "_issue.txt")
                patch = ""
                ok = checkout(bare, row["base_commit"], work)
                if ok:
                    with open(issue_file, "w", encoding="utf-8") as f:
                        f.write(row["problem_statement"])
                    t0 = time.time()
                    try:
                        r = sh(["bash", adapter, work, issue_file, metrics_out], timeout=args.timeout)
                        patch = r.stdout
                    except subprocess.TimeoutExpired:
                        patch = ""
                    wall = time.time() - t0
                else:
                    wall = 0.0

                m = {"input_tokens": 0, "output_tokens": 0, "cache_read": 0, "cache_creation": 0, "model": ""}
                if os.path.isfile(metrics_out):
                    try:
                        m.update(json.load(open(metrics_out, encoding="utf-8")))
                    except Exception:
                        pass

                pf.write(json.dumps({
                    "instance_id": iid,
                    "model_name_or_path": f"{args.harness}",
                    "model_patch": patch,
                    "_seed": seed,
                }) + "\n")
                mf.write(json.dumps({
                    "instance_id": iid,
                    "harness": args.harness,
                    "seed": seed,
                    "model": m.get("model", ""),
                    "input_tokens": m.get("input_tokens", 0),
                    "output_tokens": m.get("output_tokens", 0),
                    "cache_read": m.get("cache_read", 0),
                    "cache_creation": m.get("cache_creation", 0),
                    "wall_s": round(m.get("wall_s", wall), 3),
                    "cost_usd": round(costlib.cost_usd(
                        m.get("model", ""), m.get("input_tokens", 0), m.get("output_tokens", 0),
                        m.get("cache_read", 0), m.get("cache_creation", 0)), 6),
                    "empty_patch": not patch.strip(),
                }) + "\n")
                pf.flush()
                mf.flush()
                shutil.rmtree(work, ignore_errors=True)
                print(f"  {iid} seed{seed}: {'patch' if patch.strip() else 'EMPTY'} {wall:.0f}s", file=sys.stderr)

    print(f"wrote {pred_path}\nwrote {met_path}")
    print("Next: score with the official harness, then aggregate:\n"
          "  python bench/swe/evaluate.py --out " + args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
