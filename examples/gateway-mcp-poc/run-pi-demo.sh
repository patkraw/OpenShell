#!/usr/bin/env bash
# A real Pi lead creates a Pi worker through the gateway MCP, reads its result and stops it.
# Needs: dev gateway (mise run gateway:docker), image pi-agent:local, and a provider
# "pi-nvidia" with your key:
#   openshell -g docker-dev provider profile import -f provider-pi-nvidia.yaml
#   OPENAI_API_KEY=... openshell -g docker-dev provider create --name pi-nvidia --type pi-nvidia --from-existing
set -u
cd "$(dirname "$0")"
OS="${OPENSHELL_BIN:-../../target/debug/openshell} -g ${GATEWAY:-docker-dev}"
wait_ready() { for _ in $(seq 1 60); do $OS sandbox get "$1" 2>/dev/null | grep -q 'Phase: Ready' && return 0; sleep 2; done; return 1; }
wait_gone() { for _ in $(seq 1 60); do $OS sandbox list 2>/dev/null | grep -q "^$1 " || return 0; sleep 2; done; }

$OS sandbox delete lead >/dev/null 2>&1; wait_gone lead
echo "== Starting the lead (one approval: the grant)"
$OS sandbox create --name lead --from pi-agent:local --policy lead.yaml --provider pi-nvidia \
  --grant max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node \
  --no-tty --detach -- sleep 3600 >/dev/null
wait_ready lead || { echo "lead not ready"; exit 1; }

QUESTION='What is 17 * 23? Answer in one line, then write a two-line poem about sandboxes.'
TASK=$(cat <<PROMPT
You are a lead agent running inside an OpenShell sandbox. You have MCP tools from the
"openshell" server that create and manage child sandboxes. Do not answer the question yourself.

1. Call get_limits.
2. Call create_sandbox with:
   name "worker", providers ["pi-nvidia"], request_id "demo-1", and
   command ["sh","-c","pi-run -p \\"$QUESTION\\" > /tmp/result.tmp 2>&1; mv /tmp/result.tmp /tmp/result.txt; sleep 900"].
   Give it no extra network policy.
3. Call wait on "worker", then call get_file(handle "worker", path "/tmp/result.txt").
   If it fails because the file does not exist yet, run the bash command "sleep 15" and try again
   (at most 12 times).
4. Call stop_sandbox on "worker".
5. Reply with: the worker's answer, and one sentence on what you were and weren't allowed to do.
PROMPT
)
echo "== Running Pi in the lead"
$OS sandbox exec lead -- pi-run -p "$TASK"
echo
echo "== Sandboxes now"
$OS sandbox list
