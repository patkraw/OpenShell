#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Scenario 5: the awkward parts of running work in a child: a foreground
# command that outlives its timeout, a job still running when wait gives up,
# a second job while one runs, output too big to return, a failing exit code
# and a job killed from outside.

source "$(dirname "$0")/lib.sh"

LEAD="s5-lead-${TAG}"
W="${LEAD}-w"
step "Lead ${LEAD} with worker ${W}"
new_root "${LEAD}" --from child-base:local --policy "${IMAGES}/lead-policy.yaml" || exit 1
mcp "${LEAD}" create_sandbox "{\"name\": \"${W}\"}" >/dev/null
track "${W}"

step "A foreground command longer than its timeout"
r="$(mcp "${LEAD}" exec "{\"name\": \"${W}\", \"command\": [\"sleep\", \"20\"], \"timeout_s\": 3}")"
expect "reported as timed out" "$(printf '%s' "${r}" | jget structuredContent.timed_out)" '^true$'

step "A background job outlives the first wait"
started="$(mcp "${LEAD}" exec "{\"name\": \"${W}\", \"background\": true, \"command\": [\"sh\", \"-c\", \"sleep 25; echo finished\"]}")"
job="$(printf '%s' "${started}" | jget structuredContent.job)"
r="$(mcp "${LEAD}" wait "{\"name\": \"${W}\", \"job\": \"${job}\", \"timeout_s\": 3}")"
expect "first wait says running" "$(printf '%s' "${r}" | jget structuredContent.state)" '^running$'

step "A second job while the first runs"
r="$(mcp "${LEAD}" exec "{\"name\": \"${W}\", \"background\": true, \"command\": [\"true\"]}")"
expect "refused as already running" "$(printf '%s' "${r}" | jget structuredContent.message)" 'already running'

step "Waiting again collects the first job"
r="$(job_result "${LEAD}" "${W}" "${job}")"
expect "completed" "$(printf '%s' "${r}" | jget structuredContent.state)" '^completed$'
expect "with its output" "$(printf '%s' "${r}" | jget structuredContent.stdout)" 'finished'

step "A job whose output is too big to return"
started="$(mcp "${LEAD}" exec "{\"name\": \"${W}\", \"background\": true, \"command\": [\"sh\", \"-c\", \"yes a | head -c 300000; echo the-end\"]}")"
r="$(job_result "${LEAD}" "${W}" "$(printf '%s' "${started}" | jget structuredContent.job)")"
expect "marked truncated" "$(printf '%s' "${r}" | jget structuredContent.stdout_truncated)" '^true$'
expect "keeps the end of the output" "$(printf '%s' "${r}" | jget structuredContent.stdout)" 'the-end'

step "A job that fails"
started="$(mcp "${LEAD}" exec "{\"name\": \"${W}\", \"background\": true, \"command\": [\"sh\", \"-c\", \"echo oops >&2; exit 7\"]}")"
r="$(job_result "${LEAD}" "${W}" "$(printf '%s' "${started}" | jget structuredContent.job)")"
expect "exit code 7 comes back" "$(printf '%s' "${r}" | jget structuredContent.exit_code)" '^7$'
expect "with its stderr" "$(printf '%s' "${r}" | jget structuredContent.stderr)" 'oops'

step "A job killed from outside"
started="$(mcp "${LEAD}" exec "{\"name\": \"${W}\", \"background\": true, \"command\": [\"sleep\", \"300\"]}")"
job="$(printf '%s' "${started}" | jget structuredContent.job)"
n="${job%%-*}"
sleep 2
in_sandbox "${W}" "kill -9 \$(cut -d' ' -f1 /tmp/openshell/jobs/${n}/pid)" >/dev/null
r="$(mcp "${LEAD}" wait "{\"name\": \"${W}\", \"job\": \"${job}\", \"timeout_s\": 10}")"
expect "reported as lost" "$(printf '%s' "${r}" | jget structuredContent.state)" '^lost$'
