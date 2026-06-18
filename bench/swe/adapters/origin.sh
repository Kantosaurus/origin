#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# origin headless adapter for the SWE-bench A/B harness.
#
# Contract (shared by every adapter):
#   $1 = repo_dir          a clean checkout of <repo>@<base_commit> (HEAD == base)
#   $2 = issue_file        the SWE-bench problem_statement
#   $3 = metrics_out_json  write {input_tokens,output_tokens,cache_read,cache_creation,wall_s,model}
#   stdout                 the candidate patch (a `git diff` the scorer will apply)
#
# Env: ORIGIN_MODEL (default claude-opus-4-8), BENCH_TIMEOUT (s, default 1200),
#      BENCH_KILL_DAEMON=1 to kill leftover origin-daemons between tasks
#      (recommended on a dedicated bench host). Run on Linux for the Docker scorer.
set -uo pipefail
repo="$1"; issue_file="$2"; metrics_out="$3"
model="${ORIGIN_MODEL:-claude-opus-4-8}"
timeout_s="${BENCH_TIMEOUT:-1200}"

cd "$repo" || exit 1
run="$(mktemp)"

start=$(date +%s.%N)
# Headless, no supervisor (per-cwd daemon = isolated per task), no self-update.
ORIGIN_MODEL="$model" ORIGIN_NO_SUPERVISOR=1 ORIGIN_NO_UPDATE=1 \
  timeout "$timeout_s" origin run "$(cat "$issue_file")" \
    --json --output-format stream-json >"$run" 2>/dev/null || true
end=$(date +%s.%N)
[ -n "${BENCH_KILL_DAEMON:-}" ] && pkill -f origin-daemon 2>/dev/null || true

# Candidate patch: stage everything (so new files are captured) and diff vs base.
git add -A >/dev/null 2>&1 || true
git diff --cached
git reset -q >/dev/null 2>&1 || true

# Metrics: sum the per-turn `{"kind":"usage",...}` stream events.
python3 - "$run" "$metrics_out" "$model" "$start" "$end" <<'PY'
import json, sys
run, out, model, start, end = sys.argv[1:6]
ti = to = cr = cc = 0
try:
    for line in open(run, encoding="utf-8", errors="ignore"):
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
        except Exception:
            continue
        if ev.get("kind") == "usage":
            ti += ev.get("input_tokens", 0)
            to += ev.get("output_tokens", 0)
            cr += ev.get("cache_read_input_tokens", 0)
            cc += ev.get("cache_creation_input_tokens", 0)
except FileNotFoundError:
    pass
json.dump({"input_tokens": ti, "output_tokens": to, "cache_read": cr,
           "cache_creation": cc, "wall_s": float(end) - float(start), "model": model},
          open(out, "w"))
PY
rm -f "$run"
