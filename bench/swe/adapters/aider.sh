#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# aider competitor adapter (worked example of the contract in _template.sh).
# Requires `aider` on PATH and the same model + provider key as the origin run.
#   $1=repo_dir  $2=issue_file  $3=metrics_out_json   stdout=patch
set -uo pipefail
repo="$1"; issue_file="$2"; metrics_out="$3"
model="${BENCH_MODEL:-claude-opus-4-8}"          # hold the model constant!
timeout_s="${BENCH_TIMEOUT:-1200}"

cd "$repo" || exit 1
log="$(mktemp)"
start=$(date +%s.%N)
# --yes auto-confirms; --no-auto-commits leaves changes in the working tree so we
# diff them ourselves; aider prints token usage we scrape below.
timeout "$timeout_s" aider --model "$model" --yes --no-auto-commits --no-stream \
  --message "$(cat "$issue_file")" >"$log" 2>&1 || true
end=$(date +%s.%N)

git add -A >/dev/null 2>&1 || true
git diff --cached
git reset -q >/dev/null 2>&1 || true

# Best-effort token scrape from aider's "Tokens: 1.2k sent, 340 received" lines.
python3 - "$log" "$metrics_out" "$model" "$start" "$end" <<'PY'
import json, re, sys
log, out, model, start, end = sys.argv[1:6]
sent = recv = 0
def num(s):
    s = s.replace(",", "").strip().lower()
    return int(float(s[:-1]) * 1000) if s.endswith("k") else int(float(s))
for line in open(log, encoding="utf-8", errors="ignore"):
    m = re.search(r"([\d.,]+k?)\s*sent.*?([\d.,]+k?)\s*received", line, re.I)
    if m:
        sent += num(m.group(1)); recv += num(m.group(2))
json.dump({"input_tokens": sent, "output_tokens": recv, "cache_read": 0,
           "cache_creation": 0, "wall_s": float(end) - float(start), "model": model},
          open(out, "w"))
PY
rm -f "$log"
