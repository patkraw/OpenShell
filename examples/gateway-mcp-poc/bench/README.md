# Harness benchmark for the gateway MCP

**Nine delegation tasks, run with six agent harnesses, test the gateway MCP
server end to end: 161 of 162 runs passed, and OpenShell's guards held in every
run.** Each task starts a lead agent in a sandbox. The lead creates child
sandboxes through the MCP server, collects their results and answers. A run
passes only if the answer is right **and** the gateway's own evidence shows the
platform behaved correctly.

- **Results and recommendations:** [REPORT.md](REPORT.md)
- **What each task does and proves:** [TASKS.md](TASKS.md)
- **What each harness needs to run here:** [HARNESS-INTEGRATION.md](HARNESS-INTEGRATION.md)

| Task | What it proves |
|---|---|
| split-count | fan-out, `wait` with `return_file`, combining results, clean-up |
| budget | breadth and total are budgets; the agent plans within them |
| tree | two levels, the depth limit, results across levels, stop cascades |
| least-privilege | narrower child policies and the ceiling proof |
| injection | every guard holds against a compromised child |
| crash | a crashed child is seen; `request_id` deduplicates a retry |
| bug-hunt | agent and tool sandboxes, `exec` only on tools |
| hang | a stuck child is given up on and stopped |
| big-result | a result over 256 KiB is cut and the cut is reported |

## How to run it

You need Docker, the repository's `mise` toolchain and an NVIDIA inference API
key. Run everything from the repository root unless a step says otherwise.

**1. Build the CLI and the images.** `build-images.sh` generates the seeded data,
builds each harness image and a `:bench` tag of each with the tasks' data,
prompts and scripted children. The answers stay outside the images, in
`bench/expected/`.

```shell
mise exec -- cargo build -p openshell-cli
examples/gateway-mcp-poc/bench/build-images.sh
```

**2. Start the dev gateway with its output in a file.** The scorers read the
gateway's `mcp: call refused` lines from this log.

```shell
mkdir -p .cache/gateway-docker
mise run gateway:docker > .cache/gateway-docker/gateway.log 2>&1 &
```

**3. Register the shared provider once.** It lists every harness's calling
program, so the supervisor swaps in the real key for each of them; inside a
sandbox `OPENAI_API_KEY` is only a placeholder.

```shell
target/debug/openshell -g docker-dev provider profile import \
  -f examples/gateway-mcp-poc/harnesses/provider-harness-nvidia.yaml
OPENAI_API_KEY="<your key>" target/debug/openshell -g docker-dev provider create \
  --name harness-nvidia --type harness-nvidia --from-existing
```

**4. Run one task with one harness.** This is the quickest check that the setup
works; it takes about 20 seconds.

```shell
cd examples/gateway-mcp-poc/bench
./run-task.sh split-count pi
```

It prints one JSON line with `"pass": true` or the checks that failed.

**5. Run the whole matrix and summarize it.** `run-matrix.sh [ROUNDS]
[HARNESS...]` runs every task with every harness (default: three rounds, all
six harnesses), one run after another. All 162 runs take about two hours.

```shell
caffeinate -i ./run-matrix.sh 3          # or: ./run-matrix.sh 1 pi codex
python3 summarize.py                     # Markdown table plus failed checks
```

## Before a long run

- **Keep the machine awake.** On a Mac, a sleeping laptop stops Docker
  Desktop's VM clock; when it wakes briefly, sandboxes reject their session
  tokens as "issued in the future". `caffeinate -i` prevents idle sleep; keep
  the lid open.
- **Don't restart the gateway during a run.** The POC keeps grants and
  parent-child links in memory, so a restart leaves children running that no
  lead owns.
- **Start with no sandboxes.** Every run uses a lead named `lead`. Check with
  `target/debug/openshell -g docker-dev sandbox list` and delete leftovers.

## What a run leaves behind

Each run writes to `results/<task>/<harness>/`, or under
`results/round<N>/` when `run-matrix.sh` runs it. `results/` is ignored by git.

| File | What it holds |
|---|---|
| `summary.json` | the run's score: pass, each check, the parsed answer |
| `prompt.txt`, `reply.txt`, `stderr.txt` | the lead's prompt, answer and error output |
| `seen.txt` | every sandbox that existed during the run (polled every 2 s) |
| `gateway.txt` | the gateway's `mcp:` log lines during the run, with each tool call and refusal |
| `left.txt` | children still running when the lead finished |
| `after-delete.txt` | anything left after the lead was deleted (should be empty) |
| `logs-<name>.txt`, `child-<name>.*` | kept children's sandbox logs, error output and result files |

`run-matrix.sh` also appends every summary line to `results/matrix.jsonl`, and
`summarize.py` re-scores every run into `results/rescored.jsonl`.

## Settings

| Variable | Default | What it changes |
|---|---|---|
| `RUN_TIMEOUT_S` | 1200 | stop a run that takes longer |
| `OUT_DIR` | `results` | where a run's files go |
| `GATEWAY_LOG` | `.cache/gateway-docker/gateway.log` | the gateway log the scorers read |
| `GATEWAY` | `docker-dev` | which registered gateway to use |
| `PROVIDER` | `harness-nvidia` | the provider attached to the lead and its children |
| `OPENSHELL_BIN` | `target/debug/openshell` | the CLI to use |

## The files

| Path | What it is |
|---|---|
| [`run-task.sh`](run-task.sh) | runs one task with one harness: creates the lead with the task's grant, runs the harness, collects the evidence, scores it |
| [`run-matrix.sh`](run-matrix.sh) | runs every task × harness × round |
| [`summarize.py`](summarize.py) | re-scores every run and prints the results table |
| [`tasks/<task>/task.sh`](tasks/) | the task's grant and the lead's prompt |
| [`tasks/<task>/score.py`](tasks/) | the task's checks; shared helpers in [`tasks/scorelib.py`](tasks/scorelib.py) |
| [`share/prompts/`](share/prompts/) | the children's prompts |
| [`share/`](share/) | scripted children: [`attack.mjs`](share/attack.mjs) (attacker), [`fast-count`](share/fast-count) (crashes), [`stuck`](share/stuck) (hangs), [`big-report`](share/big-report) (500 KiB result) |
| [`make_data.py`](make_data.py), [`split-count/make_log.py`](split-count/make_log.py) | generate the seeded data and the expected answers |
| [`Dockerfile.bench`](Dockerfile.bench), [`build-images.sh`](build-images.sh) | build the `:bench` images |
| [`policy.yaml`](policy.yaml) | the lead's sandbox policy; children inherit it |
| [`../harnesses/`](../harnesses/) | each harness's image, seed config and wrapper |

## The harnesses

Every harness uses the same model (`aws/anthropic/bedrock-claude-sonnet-5-5` on
NVIDIA's inference API), and the same harness runs in the lead and its
children.

| Harness | Image | Calling program (`--grant-program`) | One-shot command |
|---|---|---|---|
| Pi | `pi-agent:bench` | `/usr/local/bin/node` | `pi-run -p` |
| OpenCode | `harness-opencode:bench` | `.../opencode-ai/bin/opencode.exe` | `opencode-run` |
| Qwen Code | `harness-qwen:bench` | `/usr/local/bin/node` | `qwen-run -p` |
| Codex | `harness-codex:bench` | the native `codex` binary | `codex-run` |
| Deep Agents | `harness-python:bench` | `/usr/bin/python3.11` | `deepagents_lead.py` |
| Hermes | `harness-hermes:bench` | `/opt/python/bin/python3.14` | `hermes-run` |

Children are started with `agent-run`, which runs the image's harness once on a
prompt through `run-child`. `run-child` closes stdin, saves the reply to
`/tmp/result.txt` and keeps the sandbox running, so the parent's `wait` with
`until_file` and `return_file` gets the result. **Children never call the
gateway**, so the grant's single calling program (the lead's harness) is
enough. To add a harness, add its image to `build-images.sh` and a line to the
harness table in [`run-task.sh`](run-task.sh#L26-L33).
