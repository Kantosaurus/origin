#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Adapter contract template — copy this to add a competitor harness.
#
#   $1 = repo_dir          clean checkout of <repo>@<base_commit> (HEAD == base)
#   $2 = issue_file        the SWE-bench problem_statement (the task prompt)
#   $3 = metrics_out_json  write the token/wall metrics here
#   stdout                 the candidate patch (a `git diff` the scorer applies)
#
# THE GOLDEN RULE: every harness must run the SAME model (hold the model
# constant — you are comparing harnesses, not models). Point each adapter at the
# same provider/model the origin adapter uses (e.g. claude-opus-4-8 via the
# Anthropic API). A different model invalidates the whole comparison.
#
# metrics_out_json schema (all optional; missing -> 0):
#   {"input_tokens":N,"output_tokens":N,"cache_read":N,"cache_creation":N,
#    "wall_s":F,"model":"<id>"}
# cost is derived from these by bench/swe/cost.py using origin's price table, so
# every harness is costed identically — only the token counts must be honest.
set -uo pipefail
repo="$1"; issue_file="$2"; metrics_out="$3"
model="${BENCH_MODEL:-claude-opus-4-8}"

cd "$repo" || exit 1
start=$(date +%s.%N)

# >>> run your harness headlessly here, editing the repo in place <<<
#   the_harness --model "$model" --message "$(cat "$issue_file")" ...
# Capture its token usage if it reports any (parse its logs/JSON).

end=$(date +%s.%N)

git add -A >/dev/null 2>&1 || true
git diff --cached
git reset -q >/dev/null 2>&1 || true

python3 - "$metrics_out" "$model" "$start" "$end" <<'PY'
import json, sys
out, model, start, end = sys.argv[1:5]
# Fill in real token counts parsed from your harness's output.
json.dump({"input_tokens": 0, "output_tokens": 0, "cache_read": 0,
           "cache_creation": 0, "wall_s": float(end) - float(start), "model": model},
          open(out, "w"))
PY
