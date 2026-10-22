#!/usr/bin/env bash
# Split-count benchmark: a lead agent splits /data/app.log between two child
# agents of the same harness, collects both results with wait(until_file) and
# get_file, and combines them. Scored against expected.json.
#
# Usage: run.sh HARNESS   (pi | qwen | codex | opencode | hermes | deepagents)
# Needs: dev gateway (mise run gateway:docker), the <harness>:bench images
# (build-images.sh) and a provider "harness-nvidia" with your key.
set -u
cd "$(dirname "$0")"
H="${1:?harness}"
OS="${OPENSHELL_BIN:-../../../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
PROVIDER="${PROVIDER:-harness-nvidia}"
OUT="${OUT_DIR:-results}/$H"
mkdir -p "$OUT"

CODEX_BIN=/usr/local/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-arm64/vendor/aarch64-unknown-linux-musl/bin/codex
case "$H" in
  pi)         IMAGE=pi-agent:bench;         PROGRAM=/usr/local/bin/node;    RUN=(pi-run -p) ;;
  qwen)       IMAGE=harness-qwen:bench;     PROGRAM=/usr/local/bin/node;    RUN=(qwen-run -p) ;;
  codex)      IMAGE=harness-codex:bench;    PROGRAM=$CODEX_BIN;             RUN=(codex-run) ;;
  opencode)   IMAGE=harness-opencode:bench; PROGRAM=/usr/local/lib/node_modules/opencode-ai/bin/opencode.exe; RUN=(opencode-run) ;;
  hermes)     IMAGE=harness-hermes:bench;   PROGRAM=/opt/python/bin/python3.14; RUN=(hermes-run) ;;
  deepagents) IMAGE=harness-python:bench;   PROGRAM=/usr/bin/python3.11;    RUN=(/opt/deep/bin/python /opt/deep/deepagents_lead.py) ;;
  *) echo "unknown harness $H"; exit 2 ;;
esac

child_task() { # start end
  echo "Read lines $1 to $2 of /data/app.log, for example with sed -n '$1,$2p'. Each line is: timestamp LEVEL [service] message (req=id). Count the lines whose LEVEL (the second field) is exactly ERROR, and count them per message (the text after [service], without the (req=...) part). Reply with only this JSON and nothing else: {\"lines\": \"$1-$2\", \"errors\": N, \"by_message\": {\"message\": count}}"
}
json_cmd() { # task -> JSON array: run-child + harness command + task
  python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' run-child "${RUN[@]}" "$1"
}
C1=$(json_cmd "$(child_task 1 5000)")
C2=$(json_cmd "$(child_task 5001 10000)")
GOAL='The file /data/app.log has 10,000 log lines. Each line is: timestamp LEVEL [service] message (req=id). Find (1) how many lines have LEVEL ERROR, and (2) which error message is the most common across the whole file, and how many times it occurs. Do not read /data/app.log yourself: split the work between two child agents in their own sandboxes, as below.'
FINAL='Combine the two results: add the counts for each message across both halves. Reply with only this JSON: {"total_errors": N, "top_error": "message", "top_error_count": N}'
TASK="You are the lead agent inside an OpenShell sandbox. $GOAL Use the tools of the \"openshell\" MCP server:
1. Call create_sandbox with name \"half1\", providers [\"$PROVIDER\"] and command $C1
2. Call create_sandbox with name \"half2\", providers [\"$PROVIDER\"] and command $C2
3. For each child, call wait with its handle, until_file \"/tmp/result.txt\" and timeout_s 600. If it timed out, call wait again the same way.
4. When file_ready is true, call get_file with the handle and path \"/tmp/result.txt\". It holds the child's JSON reply.
5. Call stop_sandbox for both children.
6. $FINAL"

wait_ready() { for _ in $(seq 1 90); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; }

$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
$OS sandbox create --name lead --from "$IMAGE" --policy policy.yaml --provider "$PROVIDER" \
  --grant-program "$PROGRAM" \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h \
  --no-tty --detach -- sleep 3600 >/dev/null
wait_ready lead || { echo "lead not ready"; exit 1; }

# Record which sandboxes exist while the lead runs, to see whether it used
# OpenShell children or its harness's built-in sub-agents.
( while :; do $OS sandbox list 2>/dev/null | awk 'NR>1{print $1}'; sleep 3; done ) > "$OUT/sandboxes.log" 2>&1 &
WATCH=$!
start=$(date +%s)
$OS sandbox exec lead -- "${RUN[@]}" "$TASK" > "$OUT/reply.txt" 2> "$OUT/stderr.txt" &
EXEC=$!
# A stuck harness must not block the next one.
( sleep "${RUN_TIMEOUT_S:-900}"; kill $EXEC 2>/dev/null ) >/dev/null 2>&1 &
DOG=$!
wait $EXEC
code=$?
pkill -P $DOG 2>/dev/null; kill $DOG 2>/dev/null; wait $DOG 2>/dev/null
secs=$(( $(date +%s) - start ))
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null
children=$(sort -u "$OUT/sandboxes.log" | grep -c -E '^half[12]$')
left=$($OS sandbox list 2>/dev/null | awk 'NR>1{print $1}' | grep -c -E '^half[12]$')
score=$(python3 score.py expected.json "$OUT/reply.txt")
$OS sandbox delete lead >/dev/null 2>&1
printf '{"harness":"%s","exit":%d,"seconds":%d,"openshell_children":%d,"children_left_running":%d,"score":%s}\n' \
  "$H" "$code" "$secs" "$children" "$left" "$score" | tee "$OUT/summary.json"
