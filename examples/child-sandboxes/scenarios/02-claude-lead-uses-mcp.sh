#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Scenario 2: a real agent drives the tools. Claude Code runs as the lead,
# with openshell.local/mcp as its MCP server, and is asked in plain words to
# create a Pi child, run a command there and report the output. The script
# then checks what the gateway did, not only what the agent says.

source "$(dirname "$0")/lib.sh"

LEAD="s2-lead-${TAG}"
CHILD="s2-w-${TAG}"
step "Lead ${LEAD} on the Claude Code image"
new_root "${LEAD}" --from child-claude:local --policy "${IMAGES}/lead-policy.yaml" --provider "${NV_PROVIDER}" || exit 1
# The agent creates this child; deleted at the end even when the agent fails.
track "${CHILD}"

PROMPT="Use the openshell MCP tools. Create a child sandbox named ${CHILD} from the pi-agent template. \
Then run the command [\"sh\", \"-c\", \"echo marker-from-child; pi --version\"] in it with the exec tool. \
Do not delete the child. Finally reply with the exact stdout of that command."

# shellcheck disable=SC2016 # expanded by the lead's shell
SCRIPT='ANTHROPIC_BASE_URL='"${NV_URL}"' ANTHROPIC_AUTH_TOKEN="$NVIDIA_API_KEY" \
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 ANTHROPIC_DEFAULT_HAIKU_MODEL=aws/anthropic/bedrock-claude-opus-5-5 \
CLAUDE_CONFIG_DIR=/tmp/claude exec claude -p "$0" --model aws/anthropic/bedrock-claude-opus-5-5 \
--mcp-config '"'"'{"mcpServers":{"openshell":{"type":"http","url":"http://openshell.local/mcp"}}}'"'"' \
--allowedTools mcp__openshell'

step "Ask Claude Code to do the work"
answer="$("${OPENSHELL}" sandbox exec --name "${LEAD}" --no-tty -- sh -c "${SCRIPT}" "${PROMPT}" 2>&1)"
status=$?
printf '%s\n' "${answer}" | tail -n 8 | sed 's/^/     | /'
expect "Claude Code exits 0" "${status}" '^0$'
expect "the agent reports the child's output" "${answer}" 'marker-from-child'

step "Check the gateway's view"
listed="$("${OPENSHELL}" sandbox list --parent "${LEAD}" 2>&1)"
expect "the child exists under the lead" "${listed}" "${CHILD}"
version="$(in_sandbox "${CHILD}" 'pi --version')"
expect "the child runs the Pi image" "${version}" '[0-9]+\.[0-9]+'
