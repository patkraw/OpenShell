#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Scenario 1: a lead finds its templates in the MCP tool list, creates one
# child per harness (Claude Code, Codex, Pi), asks all three the same
# question as background jobs running at the same time, and checks every
# answer.

source "$(dirname "$0")/lib.sh"

LEAD="s1-lead-${TAG}"
step "Lead ${LEAD} on the base image, with the NVIDIA provider and the lead policy"
new_root "${LEAD}" --from child-base:local --policy "${IMAGES}/lead-policy.yaml" --provider "${NV_PROVIDER}" || exit 1

step "The tool list offers the three harness templates"
offered="$(mcp_raw "${LEAD}" tools/list '{}' | python3 -c '
import json, sys
tools = json.load(sys.stdin)["result"]["tools"]
create = next(t for t in tools if t["name"] == "create_sandbox")
print(" ".join(create["inputSchema"]["properties"]["template"].get("enum", [])))')"
for harness in claude codex pi; do
  expect "tools/list offers ${harness}-agent" "${offered}" "(^| )${harness}-agent( |$)"
done

QUESTION="What is 17 times 23? Reply with only the number."
JOBS=()
for harness in claude codex pi; do
  child="s1-${harness}-${TAG}"
  step "Create ${child} from ${harness}-agent and start its turn"
  created="$(mcp "${LEAD}" create_sandbox "{\"name\": \"${child}\", \"template\": \"${harness}-agent\", \"providers\": [\"${NV_PROVIDER}\"]}")"
  track "${child}"
  expect "${child} is ready" "$(printf '%s' "${created}" | jget structuredContent.state)" '^ready$'
  started="$(mcp "${LEAD}" exec "{\"name\": \"${child}\", \"background\": true, \"command\": $(harness_command "${harness}" "${QUESTION}")}")"
  JOBS+=("$(printf '%s' "${started}" | jget structuredContent.job)")
done

step "Collect the three answers"
i=0
for harness in claude codex pi; do
  child="s1-${harness}-${TAG}"
  result="$(job_result "${LEAD}" "${child}" "${JOBS[$i]}")"
  answer="$(printf '%s' "${result}" | jget structuredContent.stdout | tr -d '\n')"
  info "${harness}: ${answer}"
  expect "${harness} exits 0" "$(printf '%s' "${result}" | jget structuredContent.exit_code)" '^0$'
  expect "${harness} answers 391" "${answer}" '391'
  i=$((i + 1))
done
