# Child sandbox scenarios

Six runnable scenarios that exercise child sandboxes (RFC 0021) end to end,
through the MCP tools and the CLI, with real coding agents where it matters.
Each script creates what it needs, checks the outcome and prints a `PASS` or
`FAIL` line per check, and deletes everything when it ends.

```shell
# Every scenario; one log per scenario in ./logs
OPENSHELL_GATEWAY=docker-dev examples/child-sandboxes/scenarios/run-all.sh
# Only some
OPENSHELL_GATEWAY=docker-dev examples/child-sandboxes/scenarios/run-all.sh 3 5
```

Needs:

- a gateway with `experimental_enable_child_sandboxes = true` and the default
  limit of 10 children per parent;
- the images, templates and provider profile from [`../images`](../images):
  `child-base`, `child-claude`, `child-codex` and `child-pi` built, and
  templates `claude-agent`, `codex-agent` and `pi-agent` labelled
  `openshell.ai/agent-usable=true`;
- for scenarios 1, 2 and 4, an `nvidia-inference` provider named `nv-p` (or
  `NV_PROVIDER`) holding a key for `inference-api.nvidia.com`.

The scripts never read, print or pass a credential: harnesses read the
provider's placeholder, which the sandbox proxy replaces on the way out.

## The scenarios

| # | Scenario | What it shows |
|---|---|---|
| 1 | Three harnesses, one question | A lead finds the harness templates in its MCP tool list, creates a Claude Code, a Codex and a Pi child, runs one turn in each at the same time and collects all three answers. |
| 2 | Claude Code drives the tools itself | A real agent as the lead: Claude Code, given `openshell.local/mcp` as its MCP server and a plain-language task, creates a Pi child, runs a command there and reports the output. The script checks the gateway's view, not only the agent's words. |
| 3 | Escape attempts | A child acts like a compromised agent: it tries to create a child, reach its sibling, read the gateway token, reach a host its parent's policy does not allow, and call the gRPC API through the door; the lead tries to exec in itself; the host tries to change a child's policy and to create a grandchild. Every attempt must fail. |
| 4 | Revoke a credential | A user detaches the model provider from the lead while a Pi child works for it. The child must lose it without anyone touching the child. Attaching it again reaches only children created afterwards. |
| 5 | Job edge cases | A foreground command past its timeout, a job still running when `wait` gives up, a second job while one runs, output too big to return, a failing exit code, and a job killed from outside. |
| 6 | CLI rules and the limit | A CLI child with its own policy or a provider its parent lacks, a retried MCP create, filling a parent to 10 children, the 11th, and deleting a parent that has children. |

## Results

Run on 2026-11-05 against the local Docker gateway built from
`feat/child-sandboxes/patkraw` at `0058bd414`, macOS, Docker Desktop.
Models through `inference-api.nvidia.com`: Claude Code on
`aws/anthropic/bedrock-claude-opus-5-5`, Codex on `openai/openai/gpt-5.3-codex`,
Pi on `nvidia/nvidia/nemotron-3-ultra`.

| # | Scenario | Result | Checks |
|---|---|---|---|
| 1 | Three harnesses, one question | **Passed** | 12 / 12 |
| 2 | Claude Code drives the tools itself | **Passed** | 4 / 4 |
| 3 | Escape attempts | **Passed** | 10 / 10 |
| 4 | Revoke a credential | **Passed** | 7 / 7 |
| 5 | Job edge cases | **Passed** | 10 / 10 |
| 6 | CLI rules and the limit | **Passed** | 8 / 8 |

### 1. Three harnesses, one question: passed

- `tools/list` offered `claude-agent`, `codex-agent` and `pi-agent`.
- All three children were created from their templates and ready.
- Asked "What is 17 times 23?", Claude Code, Codex and Pi each answered `391`
  and exited 0, running at the same time as background jobs.

### 2. Claude Code drives the tools itself: passed

- Claude Code created `s2-w-…` from `pi-agent`, ran the command with `exec`
  and replied with its exact output (`marker-from-child`, `1.5.0`).
- The gateway lists the child under the lead, and the child runs the Pi image.

### 3. Escape attempts: passed

| Attempt | Outcome |
|---|---|
| Child calls `create_sandbox` | Refused: "only root sandboxes can create child sandboxes" |
| Child runs `exec` in its sibling | `not_found` |
| Lead runs `exec` in itself over MCP | `not_found` |
| Child reads `/etc/openshell/auth/sandbox.jwt` | Permission denied |
| Child reaches `https://example.com` | Blocked |
| Child calls `ListSandboxes` over gRPC through `openshell.local` | Not forwarded; only `/mcp` is |
| Host runs `policy set` on a child | Refused |
| Host creates a grandchild | Refused: "only one level of child sandboxes is supported" |

### 4. Revoke a credential: passed

- Before: the Pi child answered `42`.
- After the provider was detached from the lead, the child no longer listed
  it, and its next turns failed with "No API key found" about 15 seconds
  later. Removal reaches a running child asynchronously, as the RFC says.
- After attaching it to the lead again, the existing child stayed without it
  and a new child got it and answered `42`, as decided in the RFC.

### 5. Job edge cases: passed

- `sleep 20` with `timeout_s: 3` came back `timed_out: true`.
- A 25-second job read as `running` on a 3-second `wait`; a second job was
  refused as "already running"; the next `wait` returned `completed` with its
  output.
- 300 kB of output came back marked `stdout_truncated` with its end kept.
- `exit 7` came back as exit code 7 with its stderr.
- A job whose process was killed with `kill -9` read as `lost`.

### 6. CLI rules and the limit: passed

- `sandbox create --parent … --policy …` and `--provider` with a provider the
  parent lacks were refused.
- A repeated MCP create returned the same child with `already_existed: true`.
- With 10 children, the 11th was refused: "sandbox '…' has reached its limit
  of 10 children".
- Deleting the parent was refused while it had children, and worked after
  they were deleted.

### What earlier runs found

The first run had 3 of 6 passing. None of the failures was a product bug:

- **Scenario 6** could not start any sandbox: something outside this run
  rebuilt the shared `openshell/supervisor:dev` image, and the gateway still
  pointed at the deleted one. The dev gateway now uses its own
  `openshell/supervisor:children` tag. `run-all.sh` also reported this
  scenario as passed with no checks; a scenario now passes only when it ran
  at least one check and exited 0.
- **Scenario 2:** Claude Code's `--allowedTools` took the prompt as another
  tool name. The prompt now comes first.
- **Scenario 4** first revoked access by removing the lead's network policy,
  but the model stayed reachable: the provider's own profile allows its
  endpoint, independent of the policy. Revoking the provider is the real
  control, so the scenario now does that. Its next run asked straight after
  the detach and still got an answer; the change takes a few seconds to reach
  a running child, so the scenario now retries for up to a minute.
