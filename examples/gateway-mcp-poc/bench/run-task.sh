#!/usr/bin/env bash
# Run one benchmark task with one harness and score it.
#
# Usage: run-task.sh TASK HARNESS
#   TASK:    split-count | budget | tree | least-privilege | injection | crash | bug-hunt | hang | big-result
#   HARNESS: pi | qwen | codex | opencode | hermes | deepagents
#
# A task is tasks/<TASK>/task.sh (the grant, the lead's prompt and optional
# hooks) and tasks/<TASK>/score.py. The run is scored on the lead's reply and
# on what the gateway saw: the sandboxes created, the calls it refused (from
# the gateway log), what was still running at the end, and sandbox logs.
#
# Needs: the dev gateway with its output in $GATEWAY_LOG, the :bench images
# (build-images.sh) and a provider "harness-nvidia" with your key.
set -u
cd "$(dirname "$0")"
TASK="${1:?task}" H="${2:?harness}"
OS="${OPENSHELL_BIN:-../../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
PROVIDER="${PROVIDER:-harness-nvidia}"
GATEWAY_LOG="${GATEWAY_LOG:-../../../.cache/gateway-docker/gateway.log}"
OUT="${OUT_DIR:-results}/$TASK/$H"
rm -rf "$OUT"; mkdir -p "$OUT"
[ -f "tasks/$TASK/task.sh" ] || { echo "unknown task $TASK"; exit 2; }
[ -f "$GATEWAY_LOG" ] || { echo "no gateway log at $GATEWAY_LOG"; exit 2; }

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

# Defaults a task may change.
GRANT=(--grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3)
KEEP_LOGS=()      # children whose sandbox logs to keep (network denials)
post() { :; }     # runs after the lead finishes, before clean-up
LEAD="You are the lead agent inside an OpenShell sandbox, with the tools of the \"openshell\" MCP server."
WAIT='call wait with its handle, until_file "/tmp/result.txt", return_file true and timeout_s 45; if it timed out, call wait again the same way until file_ready is true or the child has exited'
. "tasks/$TASK/task.sh"
printf '%s\n' "$PROMPT" > "$OUT/prompt.txt"

wait_ready() { for _ in $(seq 1 90); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; return 1; }
names() { $OS sandbox list 2>/dev/null | awk 'NR>1{print $1}'; }

$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
$OS sandbox create --name lead --from "$IMAGE" --policy policy.yaml --provider "$PROVIDER" \
  --grant-program "$PROGRAM" "${GRANT[@]}" --grant-expires 1h \
  --no-tty --detach -- sleep 3600 >/dev/null
wait_ready lead || { echo "lead not ready"; exit 1; }

offset=$(wc -c < "$GATEWAY_LOG")
( while :; do names; sleep 2; done ) > "$OUT/seen.log" 2>&1 &
WATCH=$!
start=$(date +%s)
$OS sandbox exec lead -- "${RUN[@]}" "$PROMPT" > "$OUT/reply.txt" 2> "$OUT/stderr.txt" &
EXEC=$!
( sleep "${RUN_TIMEOUT_S:-1200}"; kill $EXEC 2>/dev/null ) >/dev/null 2>&1 &
DOG=$!
wait $EXEC
code=$?
secs=$(( $(date +%s) - start ))
pkill -P $DOG 2>/dev/null; kill $DOG 2>/dev/null; wait $DOG 2>/dev/null

post
for n in ${KEEP_LOGS[@]+"${KEEP_LOGS[@]}"}; do $OS logs "$n" -n 1000 > "$OUT/logs-$n.txt" 2>&1; done
names | grep -v '^lead$' > "$OUT/left.txt"
# Keep each still-running child's own stderr and exit code from run-child.
while read -r n; do
  $OS sandbox exec "$n" -- sh -c 'echo "exit: $(cat /tmp/result.exit 2>/dev/null)"; tail -c 4000 /tmp/result.err 2>/dev/null' \
    > "$OUT/child-$n.err" 2>&1
  $OS sandbox exec "$n" -- cat /tmp/result.txt > "$OUT/child-$n.result" 2>/dev/null
done < "$OUT/left.txt"
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null
sort -u "$OUT/seen.log" | grep -v '^lead$' > "$OUT/seen.txt"
tail -c +$((offset + 1)) "$GATEWAY_LOG" | sed $'s/\x1b\\[[0-9;]*m//g' | grep 'mcp: ' > "$OUT/gateway.txt"

# Deleting the lead must remove every sandbox in its tree.
$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
sleep 5
comm -12 <(sort -u "$OUT/seen.txt") <(names | sort -u) > "$OUT/after-delete.txt"

score=$(PYTHONPATH=tasks python3 "tasks/$TASK/score.py" "$OUT")
printf '{"task":"%s","harness":"%s","exit":%d,"seconds":%d,"score":%s}\n' \
  "$TASK" "$H" "$code" "$secs" "$score" | tee "$OUT/summary.json"
