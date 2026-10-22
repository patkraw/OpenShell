#!/usr/bin/env bash
# Build the harness images, then a ":bench" tag of each with the benchmark data,
# prompts, run-child and agent-run (Dockerfile.bench). Run from anywhere.
set -eu
cd "$(dirname "$0")/.."
python3 -I bench/split-count/make_log.py >/dev/null  # writes app.log next to it
python3 -I bench/make_data.py                        # writes bench/data/
docker build -q -t pi-agent:local -f Dockerfile.pi .
docker build -q -t harness-hermes:local -f harnesses/Dockerfile.hermes .
for h in python qwen codex opencode; do
  docker build -q -t "harness-$h:local" -f "harnesses/Dockerfile.$h" harnesses
done
bench() { # base agent-command
  docker build -q --build-arg "BASE=$1:local" --build-arg "AGENT=$2" -t "$1:bench" -f bench/Dockerfile.bench .
}
bench pi-agent "pi-run -p"
bench harness-qwen "qwen-run -p"
bench harness-codex "codex-run"
bench harness-opencode "opencode-run"
bench harness-hermes "hermes-run"
bench harness-python "/opt/deep/bin/python /opt/deep/deepagents_lead.py"
docker images --format '{{.Repository}}:{{.Tag}}' | grep ':bench$'
