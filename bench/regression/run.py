# SPDX-License-Identifier: Apache-2.0
"""origin regression benchmark: N trials per task through `origin run` headless.

For each task file (see bench/regression/README.md for the schema):

  1. prepare a pristine repo once (copy the fixture dir, or clone url@commit),
     and check that the verify command does NOT already pass on it,
  2. per trial: copy the pristine repo to a throwaway dir, run
     `origin run <prompt> --model M --effort E --output-format stream-json`
     there, then run the verify command and compare its exit status,
  3. sum the per-request token counters (`request_usage` stream events) and
     price them with bench/swe/cost.py.

Writes one results JSON with pass@1, pass^k, tokens, dollars and wall time,
per task and overall, stamped with the model/effort it was run at.

Examples:
    python bench/regression/run.py --model claude-opus-4-8 --effort high --dry-run
    python bench/regression/run.py --model claude-opus-4-8 --effort high --trials 3 \\
        --out bench/regression/baselines/baseline.json
    python bench/regression/run.py --self-test
"""
from __future__ import annotations

import argparse
import datetime
import glob
import json
import os
import platform
import shlex
import shutil
import stat
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, "..", "swe"))
import cost as costlib  # noqa: E402  bench/swe/cost.py, one price table for both benches

# `origin run --effort` levels (crates/origin-cli/src/cli_def.rs). An unknown
# level is silently ignored by origin, which would make the pinned value a lie.
EFFORTS = ("fast", "low", "medium", "high", "max")

# Child env for `origin run`: no supervisor, no self-update (as in
# bench/swe/adapters/origin.sh), and auto-approve every tool. ORIGIN_ALLOW_ALL
# is ignored by binaries that predate it, so it is safe on old and new origin.
ORIGIN_ENV = {"ORIGIN_NO_SUPERVISOR": "1", "ORIGIN_NO_UPDATE": "1", "ORIGIN_ALLOW_ALL": "1"}


class TaskError(Exception):
    pass


# ---------------------------------------------------------------- task files

def load_task(path: str) -> dict:
    """Read and validate one task file; raise TaskError naming the problem."""
    try:
        with open(path, encoding="utf-8") as f:
            t = json.load(f)
    except (OSError, ValueError) as e:
        raise TaskError(f"{path}: cannot read task file: {e}") from None

    def need(obj: dict, key: str, typ: type, where: str = ""):
        if key not in obj:
            raise TaskError(f"{path}: missing required field '{where}{key}'")
        v = obj[key]
        if not isinstance(v, typ) or isinstance(v, bool) and typ is int:
            raise TaskError(f"{path}: field '{where}{key}' must be {typ.__name__}, got {type(v).__name__}")
        return v

    if not isinstance(t, dict):
        raise TaskError(f"{path}: task file must be a JSON object")
    need(t, "id", str)
    if not need(t, "prompt", str).strip():
        raise TaskError(f"{path}: field 'prompt' is empty")
    if need(t, "timeout_s", int) <= 0:
        raise TaskError(f"{path}: field 'timeout_s' must be > 0")

    repo = need(t, "repo", dict)
    if ("fixture" in repo) == ("git" in repo):
        raise TaskError(f"{path}: 'repo' needs exactly one of 'fixture' or 'git'")
    if "fixture" in repo:
        fx = os.path.join(os.path.dirname(os.path.abspath(path)), need(repo, "fixture", str, "repo."))
        if not os.path.isdir(fx):
            raise TaskError(f"{path}: fixture directory not found: {fx}")
        t["_fixture"] = fx
    else:
        need(repo, "git", str, "repo.")
        need(repo, "commit", str, "repo.")

    verify = need(t, "verify", dict)
    cmd = need(verify, "cmd", list, "verify.")
    if not cmd or not all(isinstance(a, str) and a for a in cmd):
        raise TaskError(f"{path}: 'verify.cmd' must be a non-empty list of non-empty strings")
    need(verify, "expect_exit", int, "verify.")
    return t


def find_task_files(paths: list[str]) -> list[str]:
    out = []
    for p in paths:
        out += sorted(glob.glob(os.path.join(p, "*", "task.json"))) if os.path.isdir(p) else [p]
    return out


# ---------------------------------------------------------------- execution

def rmtree(path: str) -> None:
    def force(func, p, _exc):  # git object files are read-only on Windows
        os.chmod(p, stat.S_IWRITE)
        func(p)
    kw = {"onexc": force} if sys.version_info >= (3, 12) else {"onerror": force}
    shutil.rmtree(path, **kw)


def git(*args: str, cwd: str) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True)


def prepare_pristine(t: dict, dest: str) -> None:
    """Lay the task's starting repo down at `dest` (once per task)."""
    if "_fixture" in t:
        shutil.copytree(t["_fixture"], dest)
        # Make it a repo with one base commit, like bench/swe/run.py does, so
        # the agent's `git diff`/`git status` behave as in a real checkout.
        git("init", "--quiet", cwd=dest)
        git("add", "-A", cwd=dest)
        git("-c", "user.email=b@b", "-c", "user.name=b", "commit", "--quiet", "-m", "base", cwd=dest)
    else:
        # ponytail: one clone per task per run, add a bare-clone cache (as in
        # bench/swe/run.py) if git tasks get large or many.
        git("clone", "--quiet", t["repo"]["git"], dest, cwd=os.path.dirname(dest))
        git("checkout", "--quiet", "--force", t["repo"]["commit"], cwd=dest)


def verify_argv(t: dict) -> list[str]:
    # `{python}` resolves to this interpreter, so tasks work where only
    # `python3` (Linux) or only `python` (Windows) is on PATH.
    return [sys.executable if a == "{python}" else a for a in t["verify"]["cmd"]]


def run_verify(t: dict, cwd: str) -> int:
    try:
        return subprocess.run(verify_argv(t), cwd=cwd, capture_output=True, timeout=t["timeout_s"]).returncode
    except subprocess.TimeoutExpired:
        return -1


def origin_argv(origin: str, t: dict, model: str, effort: str) -> list[str]:
    return [origin, "run", t["prompt"], "--model", model, "--effort", effort, "--output-format", "stream-json"]


def parse_stream(lines) -> dict:
    """Sum the B1 per-request counters from `origin run --output-format stream-json`.

    `request_usage` is emitted once per provider request; its `input_tokens`
    is UNCACHED input (cache reads and writes are reported separately).
    `prompt_prefix` (system + tool schemas, ~bytes/4) is emitted once per session.
    """
    u = {"uncached_input_tokens": 0, "cache_read_tokens": 0, "cache_write_tokens": 0,
         "output_tokens": 0, "requests": 0, "prompt_prefix_approx_tokens": None}
    for line in lines:
        try:
            ev = json.loads(line)
        except ValueError:
            continue
        if not isinstance(ev, dict):
            continue
        if ev.get("kind") == "request_usage":
            u["requests"] += 1
            u["uncached_input_tokens"] += ev.get("input_tokens", 0)
            u["cache_read_tokens"] += ev.get("cache_read_input_tokens", 0)
            u["cache_write_tokens"] += ev.get("cache_creation_input_tokens", 0)
            u["output_tokens"] += ev.get("output_tokens", 0)
        elif ev.get("kind") == "prompt_prefix":
            u["prompt_prefix_approx_tokens"] = ev.get("approx_tokens")
    u["input_tokens"] = u["uncached_input_tokens"] + u["cache_read_tokens"] + u["cache_write_tokens"]
    return u


def run_trial(t: dict, pristine: str, trial: int, args) -> dict:
    work = tempfile.mkdtemp(prefix=f"regr_{t['id']}_t{trial}_")
    repo = os.path.join(work, "repo")
    try:
        shutil.copytree(pristine, repo)
        env = {**os.environ, **ORIGIN_ENV}
        t0 = time.time()
        timed_out = False
        try:
            r = subprocess.run(origin_argv(args.origin, t, args.model, args.effort), cwd=repo, env=env,
                               capture_output=True, text=True, encoding="utf-8", errors="replace",
                               timeout=t["timeout_s"])
            stdout, origin_exit = r.stdout, r.returncode
        except subprocess.TimeoutExpired as e:
            stdout, origin_exit, timed_out = e.stdout or "", None, True
            if isinstance(stdout, bytes):  # TimeoutExpired may carry undecoded bytes
                stdout = stdout.decode("utf-8", "replace")
        wall = time.time() - t0
        if os.environ.get("BENCH_KILL_DAEMON"):  # same knob as bench/swe/adapters/origin.sh
            subprocess.run(["pkill", "-f", "origin-daemon"], capture_output=True)
        verify_exit = run_verify(t, repo)
    finally:
        rmtree(work)
    u = parse_stream(stdout.splitlines())
    return {
        "trial": trial,
        # `origin run` has no seed option; the trial index is the pairing key.
        "seed": None,
        "passed": verify_exit == t["verify"]["expect_exit"],
        "verify_exit": verify_exit,
        "origin_exit": origin_exit,
        "timed_out": timed_out,
        "wall_s": round(wall, 3),
        **u,
        "cost_usd": round(costlib.cost_usd(args.model, u["uncached_input_tokens"], u["output_tokens"],
                                           u["cache_read_tokens"], u["cache_write_tokens"]), 6),
    }


# ---------------------------------------------------------------- metrics

TOKEN_KEYS = ("input_tokens", "uncached_input_tokens", "cache_read_tokens", "cache_write_tokens", "output_tokens")


def summarize(tasks: list[dict]) -> dict:
    """pass@1 = share of tasks whose FIRST trial passed; pass^k = share of
    tasks where ALL k trials passed. Fills the per-task fields in place."""
    for t in tasks:
        tr = t["trials"]
        t["pass@1"] = 1.0 if tr and tr[0]["passed"] else 0.0
        t["pass^k"] = 1.0 if tr and all(x["passed"] for x in tr) else 0.0
        t["passes"] = sum(x["passed"] for x in tr)
    n = len(tasks)
    trials = [x for t in tasks for x in t["trials"]]
    return {
        "tasks": n,
        "trials": len(trials),
        "pass@1": round(sum(t["pass@1"] for t in tasks) / n, 4) if n else 0.0,
        "pass^k": round(sum(t["pass^k"] for t in tasks) / n, 4) if n else 0.0,
        **{k: sum(x[k] for x in trials) for k in TOKEN_KEYS},
        "cost_usd": round(sum(x["cost_usd"] for x in trials), 6),
        "wall_s": round(sum(x["wall_s"] for x in trials), 3),
    }


def self_test() -> int:
    def tr(passed: bool) -> dict:
        return {"passed": passed, "cost_usd": 0.5, "wall_s": 1.0, **{k: 10 for k in TOKEN_KEYS}}
    tasks = [
        {"id": "a", "trials": [tr(True), tr(True), tr(True)]},     # pass@1 1, pass^k 1
        {"id": "b", "trials": [tr(True), tr(False), tr(True)]},    # pass@1 1, pass^k 0
        {"id": "c", "trials": [tr(False), tr(True), tr(True)]},    # pass@1 0, pass^k 0
        {"id": "d", "trials": [tr(False), tr(False), tr(False)]},  # pass@1 0, pass^k 0
    ]
    s = summarize(tasks)
    assert s["pass@1"] == 0.5, s
    assert s["pass^k"] == 0.25, s
    assert [t["passes"] for t in tasks] == [3, 2, 2, 0]
    assert s["trials"] == 12 and s["output_tokens"] == 120 and s["cost_usd"] == 6.0, s
    u = parse_stream([
        '{"kind":"prompt_prefix","system_bytes":400,"tools_bytes":400,"approx_tokens":200}',
        '{"kind":"usage","input_tokens":999,"output_tokens":999,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}',
        '{"kind":"request_usage","input_tokens":100,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":800}',
        '{"kind":"request_usage","input_tokens":50,"output_tokens":30,"cache_read_input_tokens":800,"cache_creation_input_tokens":0}',
        'not json',
    ])
    assert u == {"uncached_input_tokens": 150, "cache_read_tokens": 800, "cache_write_tokens": 800,
                 "output_tokens": 50, "requests": 2, "prompt_prefix_approx_tokens": 200,
                 "input_tokens": 1750}, u
    print("self-test ok")
    return 0


# ---------------------------------------------------------------- main

def main() -> int:
    ap = argparse.ArgumentParser(description="Run origin's regression benchmark (N trials per task).")
    ap.add_argument("--self-test", action="store_true", help="check the metric arithmetic and exit")
    ap.add_argument("--model", help="REQUIRED: model id passed to `origin run --model`")
    ap.add_argument("--effort", choices=EFFORTS, help="REQUIRED: `origin run --effort` level")
    ap.add_argument("--trials", type=int, default=3, help="trials per task (protocol: 3-5)")
    ap.add_argument("--tasks", nargs="+", default=[os.path.join(HERE, "tasks")],
                    help="task dirs (globbed for */task.json) or task files")
    ap.add_argument("--origin", default="origin", help="origin binary")
    ap.add_argument("--out", default=os.path.join(HERE, "out", "results.json"))
    ap.add_argument("--dry-run", action="store_true",
                    help="validate tasks, check each fails before a fix, print commands; never calls origin")
    args = ap.parse_args()
    sys.stdout.reconfigure(line_buffering=True)  # keep stdout/stderr lines in order when piped

    if args.self_test:
        return self_test()
    if not args.model or not args.effort:
        ap.error("--model and --effort are required (a baseline must state what it was run at)")
    if args.trials < 1:
        ap.error("--trials must be >= 1")

    files = find_task_files(args.tasks)
    if not files:
        print(f"no task files found under {args.tasks}", file=sys.stderr)
        return 2
    try:
        tasks = [load_task(f) for f in files]
    except TaskError as e:
        print(f"invalid task: {e}", file=sys.stderr)
        return 2
    ids = [t["id"] for t in tasks]
    dupes = sorted({i for i in ids if ids.count(i) > 1})
    if dupes:
        print(f"invalid task: duplicate id(s) {dupes}", file=sys.stderr)
        return 2

    env_prefix = " ".join(f"{k}={v}" for k, v in ORIGIN_ENV.items())
    print(f"{len(tasks)} task(s) x {args.trials} trial(s), model={args.model} effort={args.effort}", file=sys.stderr)
    results = []
    for t in tasks:
        scratch = tempfile.mkdtemp(prefix=f"regr_{t['id']}_")
        pristine = os.path.join(scratch, "pristine")
        try:
            prepare_pristine(t, pristine)
            # A task that passes before the agent touches it measures nothing.
            # Checked on a copy so the verify run's build output (__pycache__,
            # target/, ...) never leaks into the trials' starting repo.
            shutil.copytree(pristine, os.path.join(scratch, "precheck"))
            pre = run_verify(t, os.path.join(scratch, "precheck"))
            if pre == t["verify"]["expect_exit"]:
                print(f"invalid task: {t['id']}: verify already passes on the untouched repo", file=sys.stderr)
                return 2
            print(f"[{t['id']}] pre-fix verify exit={pre} (expected {t['verify']['expect_exit']} after the fix)",
                  file=sys.stderr)
            if args.dry_run:
                print(f"[{t['id']}] cwd=<copy of pristine repo> timeout={t['timeout_s']}s")
                print(f"  {env_prefix} {shlex.join(origin_argv(args.origin, t, args.model, args.effort))}")
                print(f"  {shlex.join(verify_argv(t))}  # pass iff exit == {t['verify']['expect_exit']}")
                continue
            trials = []
            for i in range(args.trials):
                r = run_trial(t, pristine, i, args)
                trials.append(r)
                print(f"  {t['id']} trial {i}: {'PASS' if r['passed'] else 'FAIL'} "
                      f"{r['wall_s']:.0f}s ${r['cost_usd']:.4f}", file=sys.stderr)
            results.append({"id": t["id"], "trials": trials})
        finally:
            rmtree(scratch)

    if args.dry_run:
        print("dry run ok: no origin calls made", file=sys.stderr)
        return 0

    try:
        version = subprocess.run([args.origin, "--version"], capture_output=True, text=True).stdout.strip()
    except OSError:
        version = ""
    doc = {
        "model": args.model,
        "effort": args.effort,
        "trials_per_task": args.trials,
        "origin_version": version,
        "seed_note": "origin run exposes no seed option; trials are paired by trial index",
        "host": {"platform": platform.platform(), "cpu_count": os.cpu_count()},
        "finished_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "summary": summarize(results),
        "tasks": results,
    }
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    s = doc["summary"]
    print(f"pass@1={s['pass@1']} pass^{args.trials}={s['pass^k']} tokens={s['input_tokens']}+{s['output_tokens']} "
          f"${s['cost_usd']:.4f}\nwrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
