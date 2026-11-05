#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Child sandbox demo (RFC 0021): a scripted lead in a root sandbox creates a
# worker through the MCP tools, runs two turns of Claude Code in it as
# background jobs, resumes the conversation by its session id, and deletes
# everything at the end.
#
# The lead's calls go from inside the root to http://openshell.local/mcp,
# exactly as an agent's would. The script never reads, prints or passes a
# credential: the root gets the named provider, and the worker inherits it.
#
# Usage:
#   examples/child-sandboxes/demo-two-turns.sh --provider anthropic --image claude-agent:local
#
# Needs: an active gateway with experimental_enable_child_sandboxes on, the
# openshell CLI, python3 on the host, and an image that has `claude` and
# `curl` or `node`. Set DEMO_FAKE_HARNESS=1 to replace Claude with a stand-in that needs
# no provider, to check the plumbing.

set -euo pipefail

PROVIDER=""
IMAGE=""
OPENSHELL="${OPENSHELL:-openshell}"
FAKE="${DEMO_FAKE_HARNESS:-0}"

usage() {
  sed -n '14,15p' "$0" | sed 's/^# //'
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --provider) PROVIDER="${2:-}"; shift 2 ;;
    --image) IMAGE="${2:-}"; shift 2 ;;
    -h | --help) usage ;;
    *) echo "unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "${IMAGE}" ] || usage
[ -n "${PROVIDER}" ] || [ "${FAKE}" = "1" ] || usage

# Sandbox names are at most 19 characters.
LEAD="lead-$$"
WORKER="worker-$$"

step() { printf '\n== %s\n' "$*"; }
info() { printf '   %s\n' "$*"; }

cleanup() {
  local status=$?
  step "Clean up: delete the worker, then the lead"
  local name
  for name in "${WORKER}" "${LEAD}"; do
    if "${OPENSHELL}" sandbox delete "${name}" >/dev/null 2>&1; then
      info "${name} deleted or already gone"
    else
      info "could not delete ${name}; run: openshell sandbox delete ${name}"
    fi
  done
  exit "${status}"
}
trap cleanup EXIT

# Read one field from a JSON document on stdin; prints nothing when absent.
# shellcheck disable=SC2016 # Python code, not shell
json_field() {
  python3 -c '
import json, sys
value = json.load(sys.stdin)
for key in sys.argv[1].split("."):
    value = value.get(key) if isinstance(value, dict) else None
print("" if value is None else (value if isinstance(value, str) else json.dumps(value)))
' "$1"
}

# Build a JSON-RPC tools/call body from a tool name and a JSON arguments object.
# shellcheck disable=SC2016 # Python code, not shell
mcp_body() {
  python3 -c '
import json, sys
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                  "params": {"name": sys.argv[1], "arguments": json.loads(sys.argv[2])}}))
' "$1" "$2"
}

# POST one JSON-RPC body, given as $1, to the MCP door from inside the lead,
# with curl when the image has it and Node's fetch otherwise.
# shellcheck disable=SC2016 # expanded by the lead's shell
LEAD_POST='
if command -v curl >/dev/null 2>&1; then
  exec curl -s --max-time 90 -X POST http://openshell.local/mcp -H "content-type: application/json" -d "$1"
fi
exec node -e "
fetch(\"http://openshell.local/mcp\", {method: \"POST\", headers: {\"content-type\": \"application/json\"}, body: process.argv[1]})
  .then((r) => r.text()).then((t) => process.stdout.write(t))
  .catch((e) => { console.error(e.message); process.exit(1); });
" "$1"'

# Call one MCP tool as the lead, from inside the lead sandbox. Prints the
# tool's structured result; fails when the tool reports an error.
mcp() {
  local tool=$1 arguments=$2 reply result
  reply="$("${OPENSHELL}" sandbox exec --name "${LEAD}" --no-tty -- \
    sh -c "${LEAD_POST}" sh "$(mcp_body "${tool}" "${arguments}")")"
  result="$(printf '%s' "${reply}" | json_field result.structuredContent)"
  if [ "$(printf '%s' "${reply}" | json_field result.isError)" = "true" ] || [ -z "${result}" ]; then
    echo "MCP ${tool} failed: ${reply}" >&2
    return 1
  fi
  printf '%s' "${result}"
}

# Run a background job in the worker and wait for it; prints its stdout.
run_turn() {
  local command_json=$1 started job state result
  started="$(mcp exec "{\"name\": \"${WORKER}\", \"background\": true, \"command\": ${command_json}}")"
  job="$(printf '%s' "${started}" | json_field job)"
  info "job ${job} started"
  while :; do
    result="$(mcp wait "{\"name\": \"${WORKER}\", \"job\": \"${job}\", \"timeout_s\": 40}")"
    state="$(printf '%s' "${result}" | json_field state)"
    info "wait: ${state}"
    [ "${state}" = "running" ] || break
  done
  if [ "${state}" != "completed" ]; then
    echo "job ${job} ended as ${state}" >&2
    return 1
  fi
  info "exit code $(printf '%s' "${result}" | json_field exit_code)"
  printf '%s' "${result}" | json_field stdout >"${TURN_OUTPUT}"
}

TURN_OUTPUT="$(mktemp)"
trap 'rm -f "${TURN_OUTPUT}"; cleanup' EXIT

step "Create the lead sandbox ${LEAD} (image ${IMAGE}${PROVIDER:+, provider ${PROVIDER}})"
create_args=(sandbox create --name "${LEAD}" --from "${IMAGE}" --detach)
[ -n "${PROVIDER}" ] && create_args+=(--provider "${PROVIDER}")
"${OPENSHELL}" "${create_args[@]}" >/dev/null
for _ in $(seq 1 60); do
  "${OPENSHELL}" sandbox exec --name "${LEAD}" --no-tty -- true >/dev/null 2>&1 && break
  sleep 3
done
"${OPENSHELL}" sandbox exec --name "${LEAD}" --no-tty -- sh -c 'command -v curl || command -v node' >/dev/null \
  || { echo "the image has neither curl nor node, which the scripted lead needs" >&2; exit 1; }
info "ready"

step "Lead: create_sandbox ${WORKER} through openshell.local/mcp"
created="$(mcp create_sandbox "{\"name\": \"${WORKER}\"}")" || {
  echo "If the error says the user no longer has access, add yourself to the workspace:" >&2
  echo "  openshell workspace member add --workspace default --subject <you> --role user" >&2
  exit 1
}
info "state $(printf '%s' "${created}" | json_field state); copies the lead's image${PROVIDER:+ and provider ${PROVIDER}}"

if [ "${FAKE}" = "1" ]; then
  # shellcheck disable=SC2016 # expanded by the worker's shell
  TURN1='["sh", "-c", "echo \"$0\"", "{\"session_id\": \"fake-session\", \"result\": \"a fake first answer\"}"]'
  # shellcheck disable=SC2016 # expanded by the worker's shell
  TURN2_PREFIX='["sh", "-c", "echo resumed $0: $1", "'
else
  step "Lead: check that the worker has claude"
  found="$(mcp exec "{\"name\": \"${WORKER}\", \"command\": [\"sh\", \"-c\", \"command -v claude || true\"]}")"
  [ -n "$(printf '%s' "${found}" | json_field stdout)" ] \
    || { echo "the image has no claude binary; pick an image with Claude Code installed" >&2; exit 1; }
  info "found $(printf '%s' "${found}" | json_field stdout)"
  TURN1='["claude", "-p", "--output-format", "json", "Write a four-line poem about a sandbox inside a sandbox. Reply with the poem only."]'
fi

step "Turn 1: background job, collected with wait"
run_turn "${TURN1}"
SESSION_ID="$(json_field session_id <"${TURN_OUTPUT}")"
info "answer: $(json_field result <"${TURN_OUTPUT}")"
[ -n "${SESSION_ID}" ] || { echo "turn 1 returned no session_id" >&2; exit 1; }
info "session id ${SESSION_ID}"

step "Turn 2: resume the same conversation by its session id"
if [ "${FAKE}" = "1" ]; then
  run_turn "${TURN2_PREFIX}${SESSION_ID}\", \"add a title\"]"
  info "answer: $(cat "${TURN_OUTPUT}")"
else
  run_turn "[\"claude\", \"-p\", \"--resume\", \"${SESSION_ID}\", \"--output-format\", \"json\", \"Give the poem a title. Reply with the title only.\"]"
  info "answer: $(json_field result <"${TURN_OUTPUT}")"
fi

step "Lead: delete ${WORKER} through MCP"
mcp delete "{\"name\": \"${WORKER}\"}" >/dev/null
info "deleting"
step "Done"
