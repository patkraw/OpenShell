# Harness matrix: the split-count benchmark

**Six agent harnesses ran the same delegation task through the gateway MCP
server, and all six got the right answer** (one run each). Each lead
agent created two child sandboxes with `create_sandbox`, waited for their result
files with `wait` and `until_file`, read them with `get_file`, combined the
counts and stopped both children.

| Harness | Version | Correct | Time | Children created | Left running |
|---|---|---|---|---|---|
| OpenCode | 1.18.34 | ✅ | 17 s | 2 | 0 |
| Qwen Code | 0.25.0 | ✅ | 20 s | 2 | 0 |
| Codex | 0.160.1 | ✅ | 21 s | 2 | 0 |
| Deep Agents | 0.7.22 | ✅ | 23 s | 2 | 0 |
| Pi | latest at build | ✅ | 24 s | 2 | 0 |
| Hermes | 0.22.2 | ✅ | 36 s | 2 | 0 |

Every harness used the same model (`aws/anthropic/bedrock-claude-sonnet-5-5`
on NVIDIA's inference API), and the same harness ran in the lead and in both
children. Times are wall-clock for the lead's whole run and come from a single
run, so treat them as indicative.

## The task

`/data/app.log` has 10,000 log lines, seeded so the file and the answer are the
same on every run (`split-count/make_log.py`). The lead is asked for the number
of ERROR lines and the most common error message, and is told not to read the
file itself:

1. `create_sandbox` **half1** for lines 1 to 5,000 and **half2** for lines
   5,001 to 10,000. Each child runs the same harness, wrapped in `run-child`.
2. `wait` on each with `until_file: "/tmp/result.txt"`.
3. `get_file` each child's `/tmp/result.txt`, which holds its JSON counts.
4. `stop_sandbox` both children.
5. Add the counts and reply with
   `{"total_errors": N, "top_error": "...", "top_error_count": N}`.

Two traps check that the result really came from combining both children:

- **Decoy lines.** 40 WARN lines contain the word "ERROR" in their text. A plain
  `grep ERROR` counts 830; the answer is **790**.
- **A hidden top error.** "cache unavailable" is only second in each half: half
  1's top error is "database timeout" and half 2's is "authentication failed".
  Only adding both children's counts gives the answer, **285**.

`run-child` runs the child's harness with stdin closed, saves its reply to
`/tmp/result.txt` (written whole, then renamed) and keeps the sandbox running
so the parent can read the file. **The child never calls the gateway**, so the
grant's single calling program (the lead's harness) is enough.

## How to run it

You need the dev gateway, the harness images and a provider with your NVIDIA
inference API key. From the repository root:

```shell
mise run gateway:docker                                  # dev gateway on 127.0.0.1:18080
mise exec -- cargo build -p openshell-cli                # target/debug/openshell
examples/gateway-mcp-poc/bench/build-images.sh           # harness images + :bench tags
```

Register the shared provider once. It lists every harness's calling program, so
the supervisor swaps in the real key for each of them; inside a sandbox
`OPENAI_API_KEY` is only a placeholder.

```shell
target/debug/openshell -g docker-dev provider profile import \
  -f examples/gateway-mcp-poc/harnesses/provider-harness-nvidia.yaml
OPENAI_API_KEY="<your key>" target/debug/openshell -g docker-dev provider create \
  --name harness-nvidia --type harness-nvidia --from-existing
```

Start the dev gateway with its output in a file, so runs can read the
gateway's refusal log:

```shell
mise run gateway:docker > .cache/gateway-docker/gateway.log 2>&1 &
```

Run one task with one harness, or every task:

```shell
cd examples/gateway-mcp-poc/bench
./run-task.sh split-count pi
for t in split-count budget tree least-privilege injection crash bug-hunt; do ./run-task.sh $t pi; done
```

Each run prints one JSON line and keeps the lead's prompt and reply, the
sandboxes seen, the gateway's log lines, what was left running and kept sandbox
logs in `results/<task>/<harness>/`. A run that takes longer than 20 minutes is
stopped (`RUN_TIMEOUT_S` changes the limit). The tasks are described in
[TASKS.md](TASKS.md).

## The harnesses

| Harness | Image | Calling program (`--grant-program`) | One-shot command | Built-in sub-agent |
|---|---|---|---|---|
| Pi | `pi-agent:bench` | `/usr/local/bin/node` | `pi-run -p` | none |
| Qwen Code | `harness-qwen:bench` | `/usr/local/bin/node` | `qwen-run -p` | `agent` |
| Codex | `harness-codex:bench` | the native `codex` binary | `codex-run` | `spawn_agent` |
| OpenCode | `harness-opencode:bench` | `.../opencode-ai/bin/opencode.exe` | `opencode-run` | `task` |
| Deep Agents | `harness-python:bench` | `/usr/bin/python3.11` | `deepagents_lead.py` | `task`, turned off |
| Hermes | `harness-hermes:bench` | `/opt/python/bin/python3.14` | `hermes-run` | `delegate_task` |

- **Each harness reaches the gateway from its own program,** so the grant names a
  different program for each. Pi and Qwen Code both run in Node and can't be
  told apart by the program check.
- **Every harness found and used the MCP tools** when the task named them.
  The built-in sub-agent tools stayed on (except in Deep Agents) and none of
  the leads used them, but the task asked for OpenShell children explicitly, so
  this does not show which path a model picks unprompted.
- Image recipes, seed configs and wrappers are in [`../harnesses/`](../harnesses/).

## What we had to fix

- **OpenCode's children hung.** As a sandbox's main command, stdin is open but
  never written, and `opencode run` waits on it forever. `run-child` now closes
  stdin for every harness. (`pi-run` and `hermes-run` already did.)
- **Deep Agents' children could not see `/data`.** Its default backend is a
  virtual, in-memory filesystem with no shell. `deepagents_lead.py` now uses
  `LocalShellBackend`, the real disk and a shell, which Deep Agents recommends
  only inside a sandbox.
- **The sandbox policy must allow the data and the harness.** `policy.yaml` is
  `lead.yaml` plus read access to `/data` and `/opt` (where the Python harnesses
  live). Children inherit it.

## Known gaps

- **One run per harness.** Repeat runs are needed for real timing numbers.
- **One task.** Split counting checks the plumbing (create, wait, read, stop and
  combining results), not harder delegation.
- **Hermes reaches out at startup** (`models.dev` and GitHub). In the sandbox
  those show up as denied requests and do not affect the run.
- **mini-SWE-agent was dropped**: it has no MCP client.
