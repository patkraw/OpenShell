# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Shared helpers for the child sandbox scenarios. Source it; it never reads,
# prints or passes a credential. Runs on macOS bash 3.2.

set -uo pipefail

OPENSHELL="${OPENSHELL:-openshell}"
SCENARIOS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IMAGES="${SCENARIOS}/../images"
NV_PROVIDER="${NV_PROVIDER:-nv-p}"
NV_URL="https://inference-api.nvidia.com"
TAG="$$"

PASSED=0
FAILED=0
SANDBOXES=()

step() { printf '\n== %s\n' "$*"; }
info() { printf '   %s\n' "$*"; }
pass() { PASSED=$((PASSED + 1)); printf '   PASS  %s\n' "$1"; }
fail() { FAILED=$((FAILED + 1)); printf '   FAIL  %s\n' "$1"; [ -n "${2:-}" ] && printf '         %s\n' "$2"; }

# expect <description> <actual> <extended regex the actual must match>
expect() {
  if printf '%s' "$2" | grep -Eq -- "$3"; then pass "$1"; else fail "$1" "got: $(printf '%s' "$2" | head -c 300)"; fi
}

# expect_not <description> <actual> <extended regex the actual must not match>
expect_not() {
  if printf '%s' "$2" | grep -Eq -- "$3"; then fail "$1" "got: $(printf '%s' "$2" | head -c 300)"; else pass "$1"; fi
}

# Delete every sandbox this run created, newest first, so children go before
# their roots, then print the totals.
finish() {
  local status=$?
  step "Clean up"
  local i
  for ((i = ${#SANDBOXES[@]} - 1; i >= 0; i--)); do
    "${OPENSHELL}" sandbox delete "${SANDBOXES[$i]}" >/dev/null 2>&1 || true
  done
  info "deleted ${#SANDBOXES[@]} sandbox(es)"
  printf '\nRESULT  %d passed, %d failed\n' "${PASSED}" "${FAILED}"
  [ "${FAILED}" -eq 0 ] && [ "${status}" -eq 0 ] && exit 0
  exit 1
}
trap finish EXIT

track() { SANDBOXES+=("$1"); }

# jget <dotted path> reads JSON on stdin and prints the value: strings bare,
# anything else as JSON, nothing when absent. Numbers in the path index lists.
# shellcheck disable=SC2016 # Python code, not shell
jget() {
  python3 -c '
import json, sys
try:
    value = json.load(sys.stdin)
except ValueError:
    sys.exit(0)
for key in [k for k in sys.argv[1].split(".") if k]:
    if isinstance(value, list) and key.isdigit():
        value = value[int(key)] if int(key) < len(value) else None
    elif isinstance(value, dict):
        value = value.get(key)
    else:
        value = None
print("" if value is None else (value if isinstance(value, str) else json.dumps(value)))
' "$1"
}

# wait_ready <sandbox>: until a command runs in it, for at most three minutes.
wait_ready() {
  local _
  for _ in $(seq 1 60); do
    "${OPENSHELL}" sandbox exec --name "$1" --no-tty -- true >/dev/null 2>&1 && return 0
    sleep 3
  done
  echo "sandbox $1 never became ready" >&2
  return 1
}

# new_root <name> [sandbox create options...]: create a root and wait for it.
new_root() {
  local name=$1
  shift
  track "${name}"
  "${OPENSHELL}" sandbox create --name "${name}" --detach "$@" >/dev/null || return 1
  wait_ready "${name}"
}

# in_sandbox <sandbox> <shell script>: run a script inside a sandbox from the
# host CLI and print its output.
in_sandbox() {
  "${OPENSHELL}" sandbox exec --name "$1" --no-tty -- sh -c "$2" 2>&1
}

# mcp_raw <sandbox> <method> <params JSON>: one JSON-RPC request to
# openshell.local/mcp from inside a sandbox; prints the whole reply.
# shellcheck disable=SC2016 # Python code and the sandbox's shell
mcp_raw() {
  local body
  body="$(python3 -c '
import json, sys
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": sys.argv[1], "params": json.loads(sys.argv[2])}))
' "$2" "$3")"
  "${OPENSHELL}" sandbox exec --name "$1" --no-tty -- sh -c \
    'curl -s --max-time 90 -X POST http://openshell.local/mcp -H "content-type: application/json" -d "$1"' \
    sh "${body}" 2>/dev/null | grep '^{' | tail -n 1
}

# mcp <sandbox> <tool> <arguments JSON>: call a tool; prints its result
# (isError and structuredContent).
mcp() {
  mcp_raw "$1" tools/call "{\"name\": \"$2\", \"arguments\": $3}" | jget result
}

# job_result <sandbox> <child> <job>: wait until the job leaves "running", at
# most about four minutes; prints the last wait result.
job_result() {
  local result _
  for _ in $(seq 1 6); do
    result="$(mcp "$1" wait "{\"name\": \"$2\", \"job\": \"$3\", \"timeout_s\": 40}")"
    [ "$(printf '%s' "${result}" | jget structuredContent.state)" = "running" ] || break
  done
  printf '%s' "${result}"
}

# harness_command <claude|codex|pi> <prompt>: the JSON argument list that runs
# one turn of that harness against the NVIDIA endpoint, each with its own
# model. The harness reads the provider's NVIDIA_API_KEY placeholder, which
# the sandbox proxy replaces on the way out.
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
  python3 -c 'import json, sys; print(json.dumps(["sh", "-c", sys.argv[1], sys.argv[2]]))' "${script}" "$2"
}

