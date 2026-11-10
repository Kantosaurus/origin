# SPDX-License-Identifier: Apache-2.0
"""origin regression benchmark: N trials per task through `origin run` headless.

For each task file (see bench/regression/README.md for the schema):

  1. pre-flight, for EVERY task before any trial: prepare a pristine repo
     (copy the fixture dir, or clone url@commit) and check that the verify
     command does NOT already pass on it,
  2. per trial: copy the pristine repo to a throwaway dir, run
     `origin run <prompt> --model M --effort E --output-format stream-json`
     there, restore the task's protected files (`verify.restore`) from the
     pristine repo, then run the verify command and compare its exit status,
  3. sum the per-request token counters (`request_usage` stream events) and
     price them with bench/swe/cost.py.

The results JSON (pass@1, pass^k, tokens, dollars, wall time, per task and
overall, stamped with model/effort) is rewritten after every trial, so an
aborted run keeps what it paid for, marked `"complete": false`.

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
import signal
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

STDERR_TAIL_BYTES = 500


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
    restore = verify.setdefault("restore", [])
    if not isinstance(restore, list) or not all(
            isinstance(p, str) and p and not os.path.isabs(p) and ".." not in p.replace("\\", "/").split("/")
            for p in restore):
        raise TaskError(f"{path}: 'verify.restore' must be a list of relative paths inside the repo")
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
        for argv in git_setup_cmds(t, dest):
            subprocess.run(argv, check=True, capture_output=True, text=True)


def git_setup_cmds(t: dict, dest: str) -> list[list[str]]:
    """The commands that lay a git task's repo down at `dest`."""
    return [["git", "clone", "--quiet", t["repo"]["git"], dest],
            ["git", "-C", dest, "checkout", "--quiet", "--force", t["repo"]["commit"]]]


def run_proc(argv: list[str], cwd: str, timeout: int, out_path: str, err_path: str,
             env: dict | None = None) -> tuple[int | None, bool]:
    """Run `argv` with output to files (a pipe can hang on Windows when a
    grandchild keeps it open). On timeout, or if we are interrupted, the whole
    process tree is killed. Returns (exit status or None, timed_out)."""
    group = ({"creationflags": subprocess.CREATE_NEW_PROCESS_GROUP} if os.name == "nt"
             else {"start_new_session": True})
    with open(out_path, "wb") as out, open(err_path, "wb") as err:
        p = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=out, stderr=err, **group)
        try:
            return p.wait(timeout=timeout), False
        except subprocess.TimeoutExpired:
            return None, True
        finally:
            if p.poll() is None:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/T", "/F", "/PID", str(p.pid)], capture_output=True)
                else:
                    try:
                        os.killpg(p.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                p.wait()


def verify_argv(t: dict) -> list[str]:
    # `{python}` resolves to this interpreter, so tasks work where only
    # `python3` (Linux) or only `python` (Windows) is on PATH.
    return [sys.executable if a == "{python}" else a for a in t["verify"]["cmd"]]


def run_verify(t: dict, repo: str, logdir: str) -> tuple[int | None, bool]:
    return run_proc(verify_argv(t), repo, t["timeout_s"],
                    os.path.join(logdir, "verify.out"), os.path.join(logdir, "verify.err"))


def snapshot(path: str) -> dict | None:
    """{relative path: bytes} of a file or directory tree; None if absent."""
    if os.path.isfile(path):
        with open(path, "rb") as f:
            return {"": f.read()}
    if not os.path.isdir(path):
        return None
    snap = {}
    for root, _dirs, files in os.walk(path):
        for name in files:
            p = os.path.join(root, name)
            with open(p, "rb") as f:
                snap[os.path.relpath(p, path)] = f.read()
    return snap


def restore_protected(t: dict, pristine: str, repo: str) -> bool:
    """Put `verify.restore` paths back exactly as in the pristine repo, so the
    agent cannot pass by editing or deleting the tests. Returns True if any of
    them differed from what the agent left (a restored directory is replaced
    whole: files the agent added inside it are dropped and count as a change)."""
    modified = False
    for rel in t["verify"]["restore"]:
        src, dst = os.path.join(pristine, rel), os.path.join(repo, rel)
        if snapshot(src) == snapshot(dst):
            continue
        modified = True
        if os.path.isdir(dst):
            rmtree(dst)
        elif os.path.lexists(dst):
            os.remove(dst)
        os.makedirs(os.path.dirname(dst) or ".", exist_ok=True)
        if os.path.isdir(src):
            shutil.copytree(src, dst)
        else:
            shutil.copy2(src, dst)
    return modified


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
    out_path, err_path = os.path.join(work, "origin.out"), os.path.join(work, "origin.err")
    try:
        shutil.copytree(pristine, repo)
        t0 = time.time()
        origin_exit, timed_out = run_proc(origin_argv(args.origin, t, args.model, args.effort), repo,
                                          t["timeout_s"], out_path, err_path, env={**os.environ, **ORIGIN_ENV})
        wall = time.time() - t0
        # Same knob as bench/swe/adapters/origin.sh. pkill does not exist on
        # Windows, where the tree kill above is all we do.
        if os.environ.get("BENCH_KILL_DAEMON") and os.name != "nt":
            subprocess.run(["pkill", "-f", "origin-daemon"], capture_output=True)
        tests_modified = restore_protected(t, pristine, repo)
        verify_exit, verify_timed_out = run_verify(t, repo, work)
        with open(out_path, encoding="utf-8", errors="replace") as f:
            u = parse_stream(f)
        stderr_tail = None
        if origin_exit != 0:
            with open(err_path, "rb") as f:
                f.seek(max(0, os.path.getsize(err_path) - STDERR_TAIL_BYTES))
                stderr_tail = f.read().decode("utf-8", "replace")
    finally:
        rmtree(work)
    if origin_exit == 0 and u["requests"] == 0:
        print(f"  WARNING {t['id']} trial {trial}: origin exited 0 but reported no request_usage events "
              f"(zero tokens counted; origin older than B1?)", file=sys.stderr)
    return {
        "trial": trial,
        # `origin run` has no seed option; the trial index is the pairing key.
        "seed": None,
        "passed": not verify_timed_out and verify_exit == t["verify"]["expect_exit"],
        "verify_exit": verify_exit,
        "verify_timed_out": verify_timed_out,
        "tests_modified": tests_modified,
        "origin_exit": origin_exit,
        "timed_out": timed_out,
        "origin_stderr_tail": stderr_tail,
        "wall_s": round(wall, 3),
        **u,
        "cost_usd": round(costlib.cost_usd(args.model, u["uncached_input_tokens"], u["output_tokens"],
                                           u["cache_read_tokens"], u["cache_write_tokens"]), 6),
    }


# ---------------------------------------------------------------- metrics

TOKEN_KEYS = ("input_tokens", "uncached_input_tokens", "cache_read_tokens", "cache_write_tokens", "output_tokens",
              "requests")


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


def write_results(doc: dict, results: list[dict], path: str) -> None:
    """Rewrite the whole results file (atomically) from what has run so far."""
    done = [r for r in results if r["trials"]]
    doc["summary"] = summarize(done)
    doc["tasks"] = done
    doc["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    with open(path + ".tmp", "w", encoding="utf-8") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")
    os.replace(path + ".tmp", path)


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
    assert s["trials"] == 12 and s["output_tokens"] == 120 and s["requests"] == 120 and s["cost_usd"] == 6.0, s
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

    # restore_protected: an untouched test is not flagged; an edited, a deleted
    # and a padded test directory are flagged and put back byte-for-byte.
    tmp = tempfile.mkdtemp(prefix="regr_selftest_")
    try:
        pristine, repo = os.path.join(tmp, "p"), os.path.join(tmp, "r")
        os.makedirs(os.path.join(pristine, "tests"))
        for rel, body in (("test_a.py", "assert 1 == 2\n"), ("test_b.py", "B\n"), ("tests/t.py", "T\n")):
            with open(os.path.join(pristine, rel), "w") as f:
                f.write(body)
        shutil.copytree(pristine, repo)
        t = {"verify": {"restore": ["test_a.py", "test_b.py", "tests"]}}
        assert restore_protected(t, pristine, repo) is False
        with open(os.path.join(repo, "test_a.py"), "w") as f:
            f.write("assert True\n")  # weakened
        os.remove(os.path.join(repo, "test_b.py"))  # deleted
        with open(os.path.join(repo, "tests", "extra.py"), "w") as f:
            f.write("x\n")  # added inside a restored dir
        assert restore_protected(t, pristine, repo) is True
        assert snapshot(repo) == snapshot(pristine), "restore did not put the files back"
    finally:
        rmtree(tmp)
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
                    help="validate tasks, check each fixture task fails before a fix, print commands; "
                         "never calls origin or the network")
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
    # ponytail: every task's pristine repo is kept on disk for the whole run
    # (pre-flight must finish before the first paid trial); fine for 20-50 small repos.
    scratch = tempfile.mkdtemp(prefix="regr_run_")
    try:
        # Pre-flight: every task is set up and checked before any trial spends tokens.
        for t in tasks:
            if args.dry_run and "_fixture" not in t:
                # No network in a dry run: show the clone, skip the pre-fix check.
                print(f"[{t['id']}] git task: not cloned in a dry run, pre-fix check skipped", file=sys.stderr)
                for argv in git_setup_cmds(t, "<pristine>"):
                    print(f"  {shlex.join(argv)}")
                continue
            tdir = os.path.join(scratch, t["id"])
            t["_pristine"] = os.path.join(tdir, "pristine")
            try:
                prepare_pristine(t, t["_pristine"])
            except (OSError, subprocess.CalledProcessError) as e:
                detail = getattr(e, "stderr", "") or e
                print(f"invalid task: {t['id']}: setup failed: {detail}", file=sys.stderr)
                return 2
            missing = [p for p in t["verify"]["restore"] if snapshot(os.path.join(t["_pristine"], p)) is None]
            if missing:
                print(f"invalid task: {t['id']}: verify.restore path(s) not in the repo: {missing}", file=sys.stderr)
                return 2
            # A task that passes before the agent touches it measures nothing.
            # Checked on a copy so the verify run's build output (__pycache__,
            # target/, ...) never leaks into the trials' starting repo.
            pre_repo = os.path.join(tdir, "precheck")
            shutil.copytree(t["_pristine"], pre_repo)
            pre, pre_timed_out = run_verify(t, pre_repo, tdir)
            if not pre_timed_out and pre == t["verify"]["expect_exit"]:
                print(f"invalid task: {t['id']}: verify already passes on the untouched repo", file=sys.stderr)
                return 2
            print(f"[{t['id']}] pre-fix verify exit={pre}{' (timed out)' if pre_timed_out else ''} "
                  f"(expected {t['verify']['expect_exit']} after the fix)", file=sys.stderr)

        if args.dry_run:
            for t in tasks:
                print(f"[{t['id']}] cwd=<copy of pristine repo> timeout={t['timeout_s']}s")
                print(f"  {env_prefix} {shlex.join(origin_argv(args.origin, t, args.model, args.effort))}")
                if t["verify"]["restore"]:
                    print(f"  restore from pristine: {shlex.join(t['verify']['restore'])}")
                print(f"  {shlex.join(verify_argv(t))}  # pass iff exit == {t['verify']['expect_exit']}")
            print("dry run ok: no origin calls, no network", file=sys.stderr)
            return 0

        try:
            version = subprocess.run([args.origin, "--version"], capture_output=True, text=True).stdout.strip()
        except OSError:
            version = ""
        doc = {
            "model": args.model,
            "effort": args.effort,
            "trials_per_task": args.trials,
            "complete": False,
            "origin_version": version,
            "seed_note": "origin run exposes no seed option; trials are paired by trial index",
            "host": {"platform": platform.platform(), "cpu_count": os.cpu_count()},
        }
        results: list[dict] = []
        try:
            for t in tasks:
                trials: list[dict] = []
                results.append({"id": t["id"], "trials": trials})
                for i in range(args.trials):
                    r = run_trial(t, t["_pristine"], i, args)
                    trials.append(r)
                    print(f"  {t['id']} trial {i}: {'PASS' if r['passed'] else 'FAIL'}"
                          f"{' (tests modified)' if r['tests_modified'] else ''} "
                          f"{r['wall_s']:.0f}s ${r['cost_usd']:.4f}", file=sys.stderr)
                    write_results(doc, results, args.out)
            doc["complete"] = True
        finally:
            # Also on an abort (setup error, Ctrl-C): keep what was paid for.
            write_results(doc, results, args.out)
            if not doc["complete"]:
                print(f"run aborted: partial results written to {args.out} (complete: false)", file=sys.stderr)
    finally:
        rmtree(scratch)

    s = doc["summary"]
    print(f"pass@1={s['pass@1']} pass^{args.trials}={s['pass^k']} tokens={s['input_tokens']}+{s['output_tokens']} "
          f"requests={s['requests']} ${s['cost_usd']:.4f}\nwrote {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
