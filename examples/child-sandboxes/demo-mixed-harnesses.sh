#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Mixed-harness child sandbox demo (RFC 0021): a scripted lead in a root
# sandbox creates one child per harness through the MCP tools, each from its
# own template image (Claude Code, Codex, Pi on one shared base), runs one
# turn in each as a background job, collects the answers and deletes
# everything. Every harness talks to the NVIDIA inference API, each with a
# different model.
#
# Usage:
#   examples/child-sandboxes/demo-mixed-harnesses.sh --provider <nvidia-inference provider>
#
# Needs: an active gateway with experimental_enable_child_sandboxes on, the
# images, templates and provider profile from examples/child-sandboxes/images,
# the openshell CLI and python3 on the host. The script never reads, prints or
# passes a credential: each harness reads the provider's NVIDIA_API_KEY
# placeholder, which the sandbox proxy replaces on the way out.

set -euo pipefail

PROVIDER=""
OPENSHELL="${OPENSHELL:-openshell}"
HERE="$(cd "$(dirname "$0")" && pwd)"

usage() {
  sed -n '12,13p' "$0" | sed 's/^# //'
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --provider) PROVIDER="${2:-}"; shift 2 ;;
    -h | --help) usage ;;
    *) usage ;;
  esac
done
[ -n "${PROVIDER}" ] || usage

# Sandbox names are at most 19 characters.
LEAD="lead-$$"
CHILDREN=("claude-$$" "codex-$$" "pi-$$")

step() { printf '\n== %s\n' "$*"; }
info() { printf '   %s\n' "$*"; }

cleanup() {
  local status=$?
  step "Clean up: delete the children, then the lead"
  local name
  for name in "${CHILDREN[@]}" "${LEAD}"; do
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

# POST one JSON-RPC body, given as $1, to the MCP door from inside the lead.
# shellcheck disable=SC2016 # expanded by the lead's shell
LEAD_POST='exec curl -s --max-time 90 -X POST http://openshell.local/mcp -H "content-type: application/json" -d "$1"'

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

PROMPT="In one sentence, say which coding agent you are and which model answers."
NV_URL="https://inference-api.nvidia.com"

# The command each child runs, as a JSON argument list. Each harness is pointed
# at the NVIDIA endpoint with a different model; the prompt is passed as $0.
# shellcheck disable=SC2016 # expanded by the child's shell
harness_command() {
  local script
  case "$1" in
    claude) script='ANTHROPIC_BASE_URL='"${NV_URL}"' ANTHROPIC_AUTH_TOKEN="$NVIDIA_API_KEY" \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 ANTHROPIC_DEFAULT_HAIKU_MODEL=aws/anthropic/bedrock-claude-opus-5-5 \
exec claude -p --model aws/anthropic/bedrock-claude-opus-5-5 "$0"' ;;
    codex) script='exec codex exec --skip-git-repo-check -m openai/openai/gpt-5.3-codex -c model_provider=nv \
-c '"'"'model_providers.nv={name="NVIDIA",base_url="'"${NV_URL}"'/v1",env_key="NVIDIA_API_KEY",wire_api="responses"}'"'"' "$0"' ;;
    pi) script='export PI_CODING_AGENT_DIR="${PI_CODING_AGENT_DIR:-/tmp/pi-agent}" && mkdir -p "$PI_CODING_AGENT_DIR" && printf %s '"'"'{"providers":{"nv":{"baseUrl":"'"${NV_URL}"'/v1",
"api":"openai-completions","apiKey":"$NVIDIA_API_KEY","models":[{"id":"nvidia/nvidia/nemotron-3-ultra"}]}}}'"'"' \
> "$PI_CODING_AGENT_DIR/models.json" && exec pi -p --no-session --provider nv --model nvidia/nvidia/nemotron-3-ultra "$0"' ;;
  esac
  python3 -c 'import json, sys; print(json.dumps(["sh", "-c", sys.argv[1], sys.argv[2]]))' "${script}" "${PROMPT}"
}

step "Create the lead ${LEAD}: base image, the NVIDIA provider, a policy naming every harness"
"${OPENSHELL}" sandbox create --name "${LEAD}" --from child-base:local --detach \
  --policy "${HERE}/images/lead-policy.yaml" --provider "${PROVIDER}" >/dev/null
for _ in $(seq 1 60); do
  "${OPENSHELL}" sandbox exec --name "${LEAD}" --no-tty -- true >/dev/null 2>&1 && break
  sleep 3
done
info "ready"

# Job ids in the order of CHILDREN (macOS bash 3.2 has no associative arrays).
JOBS=()
for child in "${CHILDREN[@]}"; do
  harness="${child%-*}"
  step "Lead: create ${child} from template ${harness}-agent with provider ${PROVIDER}"
  created="$(mcp create_sandbox "{\"name\": \"${child}\", \"template\": \"${harness}-agent\", \"providers\": [\"${PROVIDER}\"]}")"
  info "state $(printf '%s' "${created}" | json_field state)"
  started="$(mcp exec "{\"name\": \"${child}\", \"background\": true, \"command\": $(harness_command "${harness}")}")"
  job="$(printf '%s' "${started}" | json_field job)"
  JOBS+=("${job}")
  info "job ${job} started (${harness})"
done

step "Lead: collect each child's answer"
for i in "${!CHILDREN[@]}"; do
  child="${CHILDREN[$i]}"
  while :; do
    result="$(mcp wait "{\"name\": \"${child}\", \"job\": \"${JOBS[$i]}\", \"timeout_s\": 40}")"
    state="$(printf '%s' "${result}" | json_field state)"
    [ "${state}" = "running" ] || break
    info "${child}: still running"
  done
  info "${child}: ${state}, exit $(printf '%s' "${result}" | json_field exit_code)"
  printf '%s' "${result}" | json_field stdout | sed 's/^/     | /'
  stderr="$(printf '%s' "${result}" | json_field stderr)"
  [ -n "${stderr}" ] && printf '%s\n' "${stderr}" | tail -n 5 | sed 's/^/     ! /'
done
