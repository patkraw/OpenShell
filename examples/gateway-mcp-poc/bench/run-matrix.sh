#!/usr/bin/env bash
# Run every task with every harness ROUNDS times. Each run's files go to
# results/round<N>/<task>/<harness>/, and every summary line is appended to
# results/matrix.jsonl. A failed run does not stop the matrix.
# Usage: run-matrix.sh [ROUNDS] [HARNESS...]
set -u
cd "$(dirname "$0")"
ROUNDS="${1:-3}"; shift || true
HARNESSES=("$@")
[ ${#HARNESSES[@]} -gt 0 ] || HARNESSES=(pi opencode qwen codex deepagents hermes)
TASKS=(split-count budget tree least-privilege injection crash bug-hunt hang big-result)
mkdir -p results
for r in $(seq 1 "$ROUNDS"); do
  for h in "${HARNESSES[@]}"; do
    for t in "${TASKS[@]}"; do
      line=$(OUT_DIR="results/round$r" ./run-task.sh "$t" "$h" 2>&1 | tail -1)
      case "$line" in
        '{"task"'*) echo "{\"round\":$r,${line#\{}" ;;
        *) echo "{\"round\":$r,\"task\":\"$t\",\"harness\":\"$h\",\"error\":$(printf '%s' "$line" | python3 -c 'import json,sys;print(json.dumps(sys.stdin.read()))')}" ;;
      esac | tee -a results/matrix.jsonl
    done
  done
done
