#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Run every scenario in order, keep each one's log next to it, and print one
# line per scenario. Pass scenario numbers to run only those: run-all.sh 3 5
#
# Needs: a gateway with experimental_enable_child_sandboxes on, the images,
# templates and provider profile from ../images, and an nvidia-inference
# provider (NV_PROVIDER, default nv-p) for scenarios 1, 2 and 4.

cd "$(dirname "$0")" || exit 1
LOGS="${LOGS:-logs}"
mkdir -p "${LOGS}"
status=0
for script in [0-9][0-9]-*.sh; do
  number="${script%%-*}"
  if [ "$#" -gt 0 ] && ! printf ' %s ' "$@" | grep -q " $((10#${number})) "; then
    continue
  fi
  log="${LOGS}/${script%.sh}.log"
  bash "${script}" > "${log}" 2>&1
  code=$?
  result="$(grep '^RESULT' "${log}" | tail -n 1)"
  # A scenario passes only when it ran checks, none failed, and it exited 0.
  if [ "${code}" -eq 0 ] && grep -q '^RESULT  [1-9][0-9]* passed, 0 failed' "${log}"; then
    printf 'PASS  %s  (%s)\n' "${script}" "${result#RESULT  }"
  else
    printf 'FAIL  %s  (%s)\n' "${script}" "${result#RESULT  }"
    status=1
  fi
done
exit "${status}"
