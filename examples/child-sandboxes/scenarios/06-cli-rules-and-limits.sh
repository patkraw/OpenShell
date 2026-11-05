#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Scenario 6: the rules a user meets from the CLI, and the per-parent limit.
# A child cannot bring its own policy or a provider its parent lacks, a
# retried MCP create does not count twice, the eleventh child is refused at
# the default limit of 10, and a parent with children cannot be deleted.

source "$(dirname "$0")/lib.sh"

ROOT="s6-root-${TAG}"
LIMIT="${LIMIT:-10}"
step "Root ${ROOT} with no providers"
new_root "${ROOT}" --from child-base:local --policy "${IMAGES}/lead-policy.yaml" || exit 1

step "A CLI child with its own policy"
r="$("${OPENSHELL}" sandbox create --name "${ROOT}-p" --parent "${ROOT}" --policy "${IMAGES}/lead-policy.yaml" --detach 2>&1)"
track "${ROOT}-p"
expect "refused" "${r}" '[Pp]olicy'

step "A CLI child with a provider its parent lacks"
r="$("${OPENSHELL}" sandbox create --name "${ROOT}-v" --parent "${ROOT}" --provider "${NV_PROVIDER}" --detach 2>&1)"
track "${ROOT}-v"
expect "refused" "${r}" "not attached to parent"

step "An MCP create retried with the same name"
first="$(mcp "${ROOT}" create_sandbox "{\"name\": \"${ROOT}-c1\"}")"
track "${ROOT}-c1"
again="$(mcp "${ROOT}" create_sandbox "{\"name\": \"${ROOT}-c1\"}")"
expect "first create is new" "$(printf '%s' "${first}" | jget structuredContent.already_existed)" '^false$'
expect "retry returns the same child" "$(printf '%s' "${again}" | jget structuredContent.already_existed)" '^true$'

step "Fill the parent up to the limit of ${LIMIT} from the CLI"
for i in $(seq 2 "${LIMIT}"); do
  track "${ROOT}-c${i}"
  "${OPENSHELL}" sandbox create --name "${ROOT}-c${i}" --parent "${ROOT}" --detach >/dev/null 2>&1
done
count="$("${OPENSHELL}" sandbox list --parent "${ROOT}" 2>&1 | grep -c "^${ROOT}-c")"
expect "${LIMIT} children listed" "${count}" "^${LIMIT}$"

step "One more child"
r="$(mcp "${ROOT}" create_sandbox "{\"name\": \"${ROOT}-over\"}")"
track "${ROOT}-over"
expect "refused at the limit" "$(printf '%s' "${r}" | jget structuredContent.message)" 'limit|at most|children'
info "$(printf '%s' "${r}" | jget structuredContent.message)"

step "Delete the parent while it has children"
r="$("${OPENSHELL}" sandbox delete "${ROOT}" 2>&1)"
expect "refused" "${r}" "has ${LIMIT} child sandboxes; delete them first"

step "Delete the children, then the parent"
for i in $(seq 1 "${LIMIT}"); do
  "${OPENSHELL}" sandbox delete "${ROOT}-c${i}" >/dev/null 2>&1
done
deleted=""
for _ in $(seq 1 20); do
  deleted="$("${OPENSHELL}" sandbox delete "${ROOT}" 2>&1)" && break
  sleep 3
done
expect_not "the parent can be deleted" "${deleted}" 'delete them first'
