#!/usr/bin/env bash
# A real Pi lead delegates to a real Pi worker. run-child saves the worker's
# answer to /tmp/result.txt; the lead's wait (until_file) returns once the file
# exists, with the file's content in the reply (return_file), and stops the worker. The worker never
# calls the gateway, and the gateway stores nothing.
# Needs: dev gateway (mise run gateway:docker), image pi-agent:local, and a
# provider "harness-nvidia" with your key (see harnesses/provider-harness-nvidia.yaml).
set -u
cd "$(dirname "$0")"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
PROVIDER="${PROVIDER:-harness-nvidia}"
wait_ready() { for _ in $(seq 1 60); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; }

$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
echo "== Starting the lead"
$OS sandbox create --name lead --from pi-agent:local --policy lead.yaml --provider "$PROVIDER" \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h \
  --no-tty --detach -- sleep 3600 >/dev/null
wait_ready lead || { echo "lead not ready"; exit 1; }

WORKER_TASK='What is 17 * 23? Then write a two-line poem about sandboxes. Reply with the answer only.'
IFS= read -r -d '' TASK <<PROMPT
You are a lead agent running inside an OpenShell sandbox. You have MCP tools from the
"openshell" server that create and manage child sandboxes. Do not answer the question yourself.

1. Call create_sandbox with name "worker", providers ["$PROVIDER"], request_id "result-demo-1",
   and command ["run-child","pi-run","-p","$WORKER_TASK"].
2. Call wait with handle "worker", until_file "/tmp/result.txt", return_file true and
   timeout_s 300. If it timed out, call wait again the same way (at most 3 times).
3. When "file_ready" is true, the reply's "content" is the worker's answer. Only then call
   stop_sandbox with handle "worker".
4. Reply with: the worker's answer exactly as in the file, and one sentence on how you
   received it.
PROMPT
echo "== Running Pi in the lead"
$OS sandbox exec lead -- pi-run -p "$TASK"
echo
echo "== Sandboxes now"
$OS sandbox list
