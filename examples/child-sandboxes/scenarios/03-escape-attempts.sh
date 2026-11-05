#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Scenario 3: a child behaves like a compromised agent and tries to get more
# than it was given. Every attempt must fail.

source "$(dirname "$0")/lib.sh"

LEAD="s3-lead-${TAG}"
A="${LEAD}-a"
B="${LEAD}-b"
step "Lead ${LEAD} with two children, ${A} and ${B}"
new_root "${LEAD}" --from child-base:local --policy "${IMAGES}/lead-policy.yaml" || exit 1
for child in "${A}" "${B}"; do
  created="$(mcp "${LEAD}" create_sandbox "{\"name\": \"${child}\"}")"
  track "${child}"
  expect "${child} is created" "$(printf '%s' "${created}" | jget structuredContent.state)" '^ready$'
done

step "A child tries to create its own child over MCP"
r="$(mcp "${A}" create_sandbox "{\"name\": \"${A}-x\"}")"
track "${A}-x"
expect "refused" "$(printf '%s' "${r}" | jget isError)" '^true$'
info "$(printf '%s' "${r}" | jget structuredContent.message)"

step "A child tries to run a command in its sibling"
r="$(mcp "${A}" exec "{\"name\": \"${B}\", \"command\": [\"id\"]}")"
expect "sibling reads as not found" "$(printf '%s' "${r}" | jget structuredContent.error)" '^not_found$'

step "The lead tries to run a command in itself over MCP"
r="$(mcp "${LEAD}" exec "{\"name\": \"${LEAD}\", \"command\": [\"id\"]}")"
expect "itself reads as not found" "$(printf '%s' "${r}" | jget structuredContent.error)" '^not_found$'

step "A child tries to read the sandbox's gateway token"
r="$(in_sandbox "${A}" 'cat /etc/openshell/auth/sandbox.jwt >/dev/null 2>&1 && echo READ || echo DENIED')"
expect "token unreadable" "${r}" 'DENIED'

step "A child tries to reach a host its parent's policy does not allow"
r="$(in_sandbox "${A}" 'curl -s -o /dev/null -w "%{http_code}" --max-time 10 https://example.com/; true')"
expect_not "example.com blocked" "${r}" '^200$'

step "A child tries to call the gateway's gRPC API through the door"
r="$(in_sandbox "${A}" 'curl -s -o /dev/null -w "%{http_code}" --max-time 10 -X POST -H "content-type: application/grpc" http://openshell.local/openshell.v1.OpenShell/ListSandboxes; true')"
expect_not "only /mcp is forwarded" "${r}" '^200$'
info "HTTP ${r}"

step "The host tries to change a child's policy"
r="$("${OPENSHELL}" policy set "${A}" --policy "${IMAGES}/lead-policy.yaml" 2>&1)"
expect "policy set on a child is refused" "${r}" '[Cc]hild|parent'

step "The host tries to create a grandchild"
r="$("${OPENSHELL}" sandbox create --name "${A}-g" --parent "${A}" --detach 2>&1)"
track "${A}-g"
expect "grandchild refused" "${r}" 'only one level of child sandboxes is supported'
