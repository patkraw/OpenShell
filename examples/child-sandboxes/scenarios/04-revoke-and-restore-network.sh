#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Scenario 4: a user takes the model credential away from a lead while a Pi
# child is working for it. The child must lose it too, without anyone
# touching the child. Giving it back to the lead reaches only children
# created afterwards; existing children stay without it, by design.

source "$(dirname "$0")/lib.sh"

LEAD="s4-lead-${TAG}"
CHILD="${LEAD}-pi"
LATER="${LEAD}-pi2"
QUESTION="What is 6 times 7? Reply with only the number."

# ask <child>: one Pi turn in the child; prints the wait result.
ask() {
  local started job
  started="$(mcp "${LEAD}" exec "{\"name\": \"$1\", \"background\": true, \"command\": $(harness_command pi "${QUESTION}")}")"
  job="$(printf '%s' "${started}" | jget structuredContent.job)"
  job_result "${LEAD}" "$1" "${job}"
}

providers() { "${OPENSHELL}" sandbox provider list "$1" 2>&1; }

step "Lead ${LEAD} with a Pi child"
new_root "${LEAD}" --from child-base:local --policy "${IMAGES}/lead-policy.yaml" --provider "${NV_PROVIDER}" || exit 1
mcp "${LEAD}" create_sandbox "{\"name\": \"${CHILD}\", \"template\": \"pi-agent\"}" >/dev/null
track "${CHILD}"

step "Before: the child reaches the model"
r="$(ask "${CHILD}")"
expect "answers 42" "$(printf '%s' "${r}" | jget structuredContent.stdout)" '42'

step "The user detaches the provider from the lead"
out="$("${OPENSHELL}" sandbox provider detach "${LEAD}" "${NV_PROVIDER}" 2>&1)"
status=$?
expect "detach from the lead succeeds" "${status}" '^0$'
[ "${status}" = 0 ] || info "${out}"
expect_not "the child no longer lists the provider" "$(providers "${CHILD}")" "${NV_PROVIDER}"
# Removal reaches a running child asynchronously, within seconds.
detached_at=$(date +%s)
code=0
for _ in $(seq 1 6); do
  r="$(ask "${CHILD}")"
  code="$(printf '%s' "${r}" | jget structuredContent.exit_code)"
  [ "${code}" != 0 ] && break
  sleep 10
done
expect_not "the child no longer gets an answer, within a minute" "${code}" '^0$'
info "cut off after about $(($(date +%s) - detached_at)) s: $(printf '%s' "${r}" | jget structuredContent.stderr | head -n 1)"

step "The user attaches it to the lead again"
"${OPENSHELL}" sandbox provider attach "${LEAD}" "${NV_PROVIDER}" >/dev/null 2>&1
expect_not "the existing child stays without it" "$(providers "${CHILD}")" "${NV_PROVIDER}"
mcp "${LEAD}" create_sandbox "{\"name\": \"${LATER}\", \"template\": \"pi-agent\"}" >/dev/null
track "${LATER}"
expect "a new child gets it" "$(providers "${LATER}")" "${NV_PROVIDER}"
r="$(ask "${LATER}")"
expect "the new child answers 42" "$(printf '%s' "${r}" | jget structuredContent.stdout)" '42'
