# SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# One shared base and one image per harness, for the mixed-harness child
# sandbox demo. Every harness is a global npm install on the same base, so
# the three images share their layout, user and folders, and only the
# harness differs:
#
#   docker build --target base   -t child-base:local   examples/child-sandboxes/images
#   docker build --target claude -t child-claude:local examples/child-sandboxes/images
#   docker build --target codex  -t child-codex:local  examples/child-sandboxes/images
#   docker build --target pi     -t child-pi:local     examples/child-sandboxes/images

FROM node:24-bookworm-slim AS base
RUN apt-get update \
    && apt-get install -y --no-install-recommends bash ca-certificates curl git procps ripgrep \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /workspace && chown node:node /workspace
# Harness state goes under /tmp, which every child can write.
ENV CLAUDE_CONFIG_DIR=/tmp/claude \
    CODEX_HOME=/tmp/codex \
    PI_CODING_AGENT_DIR=/tmp/pi-agent \
    PI_TELEMETRY=0
WORKDIR /workspace
USER node

FROM base AS claude
USER root
RUN npm install -g --no-audit --no-fund @anthropic-ai/claude-code && claude --version
USER node

FROM base AS codex
USER root
RUN npm install -g --no-audit --no-fund @openai/codex && codex --version
USER node

FROM base AS pi
USER root
RUN npm install -g --ignore-scripts --no-audit --no-fund @earendil-works/pi-coding-agent \
    && pi --version
USER node
