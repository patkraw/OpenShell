#!/usr/bin/env bash
# Build the harness images, then a ":bench" tag of each with the benchmark data
# and run-child (Dockerfile.bench). Run from anywhere.
set -eu
cd "$(dirname "$0")/.."
python3 -I bench/split-count/make_log.py >/dev/null  # writes app.log next to it
docker build -q -t pi-agent:local -f Dockerfile.pi .
docker build -q -t harness-hermes:local -f harnesses/Dockerfile.hermes .
for h in python qwen codex opencode; do
  docker build -q -t "harness-$h:local" -f "harnesses/Dockerfile.$h" harnesses
done
for base in pi-agent harness-python harness-qwen harness-codex harness-opencode harness-hermes; do
  docker build -q --build-arg "BASE=$base:local" -t "$base:bench" -f bench/Dockerfile.bench .
done
docker images --format '{{.Repository}}:{{.Tag}}' | grep ':bench$'
