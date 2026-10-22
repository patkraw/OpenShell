# What it took to run six harnesses on the gateway MCP

**No harness needed a code change except Deep Agents, which is a library you
build an agent with. Every harness needed configuration, and the five
command-line harnesses each needed a small start-up wrapper.** Most of that work
comes from how harnesses start inside a sandbox (stdin, HOME, timeouts), not
from the MCP server itself, so much of it can be removed once in OpenShell
instead of once per harness. This report lists each problem we hit, how we
worked around it in the proof of concept, and what we recommend so harnesses
work out of the box.

Measured with the nine-task benchmark ([REPORT.md](REPORT.md)): Pi, OpenCode,
Qwen Code, Codex, Deep Agents and Hermes passed 161 of 162 runs. The harness
files referred to below are in [`../harnesses/`](../harnesses/) and
[`../pi-seed/`](../pi-seed/).

## Summary

| # | Problem | Who hit it | Workaround in the POC | Recommended fix | Where |
|---|---|---|---|---|---|
| 1 | Telling the harness where the MCP server is | all | an MCP entry in each harness's config file | a documented snippet per harness; a flag or provider file where the harness allows it | docs + OpenShell |
| 2 | `wait` can outlast harness tool timeouts (60 s in Pi and OpenCode) | all | a 600 s tool timeout in every config | `wait` returns within about 45 s and the agent calls it again; or progress notifications | gateway MCP |
| 3 | stdin is open but never written | Pi, OpenCode, Codex hang | `run-child` and the wrappers close stdin | close stdin in the launcher; optional `stdin: closed` on create. Not by default: `sandbox connect` attaches to it | launcher + gateway MCP |
| 4 | No writable HOME | all but Deep Agents fail | wrappers set `HOME` or the harness's own variable under `/tmp` | give sandboxes a writable HOME by default | OpenShell runtime |
| 5 | `sandbox exec` does not pass the image's ENV | Pi, Hermes | wrappers set the variables again | pass the image's ENV to exec, or document it | OpenShell runtime |
| 6 | The grant needs the exact calling program | all | a different `--grant-program` per harness | detect the harness's program, or name it once per harness preset | gateway + CLI |
| 7 | The provider needs every calling program | all | one profile listing five programs | same as 6: one place that knows each harness's program | providers |
| 8 | Deep Agents has no real filesystem by default | Deep Agents | `LocalShellBackend` in our agent script | document it; the agent author chooses it | docs |
| 9 | Harnesses with their own sandbox | Codex | `sandbox_mode = "danger-full-access"` | document that OpenShell is the sandbox | docs |
| 10 | Harness start-up network calls | Hermes | none needed: they are denied and ignored | nothing; denied requests are visible in the logs | none |
| 11 | Results only exist while a child runs | all | `run-child` writes `/tmp/result.txt` and keeps the child running; `wait` returns it (`return_file`) | keep `until_file` and `return_file`; ship `run-child` | gateway MCP |

## The problems in detail

### 1. Telling the harness where the MCP server is

**What happened.** Every harness has its own config format for MCP servers, and
none of them knows about `http://openshell.local/mcp` by itself:

| Harness | Where we configured it |
|---|---|
| Pi | `pi-seed/mcp.json`, `mcpServers.openshell.url` |
| OpenCode | `opencode-seed/opencode.json`, `mcp.openshell` with `type: remote` |
| Qwen Code | `qwen-seed/settings.json`, `mcpServers.openshell.httpUrl` |
| Codex | `codex-seed/config.toml`, `[mcp_servers.openshell] url` |
| Hermes | `hermes-seed/config.yaml`, `mcp_servers.openshell.url` |
| Deep Agents | Python code: `MultiServerMCPClient({"openshell": {"transport": "streamable_http", "url": ...}})` |

The entry itself is small and identical in meaning: a name, the URL, no
credential. The door adds the sandbox's identity, so the harness never needs a
token.

**Workaround.** Each image ships a seed config, and its `<harness>-run` wrapper
copies it into place before starting the harness.

**Recommendation.** This is the one step that stays per harness, but it can come
from OpenShell instead of the image:

- **Provider files.** OpenShell can already place read-only files in a sandbox
  from a provider profile (`/run/openshell/providers/...`, with an environment
  variable pointing at the file; see `examples/provider-managed-files/`). A
  profile per harness, or one profile with a file per harness, would add the MCP
  entry without rebuilding images. This fits harnesses that take a config file path
  from an environment variable, and only where the file can be read-only.
- **Some harnesses take the entry on the command line or in an environment
  variable**, with no file at all. Checked in the images:

  | Harness | Add the MCP server without a config file | Provider file fits? |
  |---|---|---|
  | OpenCode | `OPENCODE_CONFIG_CONTENT` (inline JSON), or `OPENCODE_CONFIG` (file path) | ✅ `OPENCODE_CONFIG` pointing at the provider file |
  | Qwen Code | `--mcp-config '<json or path>'` | ✅ as the flag's path |
  | Codex | `-c 'mcp_servers.openshell.url="http://openshell.local/mcp"'` | ❌ config lives in `CODEX_HOME`, which must be writable; the flag is simpler |
  | Pi | none: only `mcp.json` in `PI_CODING_AGENT_DIR` (writable) | ❌ |
  | Hermes | none: only `config.yaml` in `HERMES_HOME` (writable) | ❌ |
  | Deep Agents | in code | n/a; read the URL from an environment variable |

  For Pi and Hermes the entry has to be written into their writable config
  directory, so they need a wrapper or a harness-side change (an environment
  variable or flag for MCP servers) to work out of the box.
- **A short snippet per harness in the docs**, for users who build their own
  images.
- Avoid a generic "MCP server" setting in OpenShell that rewrites harness
  configs: the formats differ and change between versions.

### 2. `wait` blocks longer than harness tool timeouts

**What happened.** `wait` with `until_file` holds the tool call open until the
child's file exists, up to `timeout_s` (we asked for 600 s). Harnesses put their
own limit on how long an MCP tool call may take. Defaults, checked in the
images when the config sets none:

| Harness | Tool-call timeout | Reset by MCP progress? | What the model sees |
|---|---|---|---|
| Pi 1.3.0 | **60 s** | yes | a timeout error; not retried |
| OpenCode 1.18.34 | **60 s** (MCP SDK default) | yes | a tool error; not retried |
| Qwen Code 0.25.0 | 600 s, **plus a 300 s idle timeout** | yes (only progress resets the idle timer) | a tool error |
| Codex 0.160.1 | **300 s** (startup 30 s) | not checked | `tool call error: timed out` |
| Hermes 0.22.2 | **300 s** (connect 60 s) | not checked | a tool error; not retried |
| Deep Agents 0.7.22 | **none** at the MCP level; HTTP read 300 s | n/a | an error after 300 s, or possibly a hang on a silent stream (not verified) |

So a `wait` of more than 60 s fails in Pi and OpenCode unless the config raises
the limit, and **Qwen's 300 s idle timeout cuts a silent call short even with
our 600 s setting.** The benchmark did not hit these: its prompts ask for
`timeout_s` 45 and to call `wait` again until the file is ready, and most
children finish in well under a minute.

**Workaround.** Every config sets a 600 s tool timeout (`timeout: 600` in Pi and
Hermes, `timeout: 600000` ms in OpenCode and Qwen Code, `tool_timeout_sec = 600`
in Codex). The benchmark prompts also keep each `wait` at 45 s and call it
again, so no single call relies on the raised limits; running with every
harness's default timeout is still untested (next step 1).

**Recommendation.** Make `wait` friendly to default timeouts instead of asking
every user to raise them:

- default `timeout_s` to about 30 s and cap it at about 45 s, below the
  smallest default we found (60 s);
- on timeout, return `timed_out: true` with the phase (already the case), and
  say in the tool description to call `wait` again;
- keep long waits available for callers that set a higher timeout;
- or send MCP progress notifications while `wait` blocks. Pi, OpenCode and
  Qwen Code reset their timers on progress, so a long wait would survive their
  defaults. This needs the gateway to stream the response, which the POC does
  not do yet.

The benchmark already shows models handle "timed out, call again": the prompt
asked for it and no lead failed on it.

### 3. stdin is open but never written

**What happened.** When a harness is a sandbox's main command (every child in
the benchmark), stdin is an open pipe that nothing writes to or closes. Some
one-shot modes read stdin before they start, and wait forever. OpenCode's
children hung for 36 minutes before we noticed; Pi had the same problem earlier
in the POC. Checked in the images with a pipe that stays open:

| Harness | One-shot command | With stdin open and silent |
|---|---|---|
| Pi | `pi -p` | **hangs**: waits for the end of stdin, never calls the model |
| OpenCode | `opencode run` | **hangs**: stops after start-up, never calls the model |
| Codex | `codex exec` | **hangs**: "Reading additional input from stdin..." |
| Qwen Code | `qwen -p` | waits 0.5 s for input, then carries on |
| Hermes | `hermes -z` | does not read stdin |
| Deep Agents | our script | does not read stdin |

**Workaround.** `run-child` starts the agent with `< /dev/null`. `pi-run` and
`codex-run` close stdin too. (`hermes-run` also does, which turns out to be
unnecessary.)

**Recommendation.** **Don't close stdin for every sandbox.** OpenShell keeps the
main command's stdin open on purpose: `openshell sandbox create` attaches to
it, and `openshell sandbox connect` re-attaches to it later (like
`docker attach`), so a person can type to an agent running as the main
command. Closing it by default would break that. Instead:

- **Close it in the launcher** that starts a one-shot agent. `run-child` knows
  the agent runs once and nobody will type to it. If OpenShell ships
  `run-child`, images no longer need their own wrappers.
- **Add an opt-in** for callers that start a one-shot command directly:
  `create_sandbox { stdin: "closed" }` in MCP and a matching CLI flag. Nothing
  changes unless it is asked for, so attaching keeps working.

### 4. No writable HOME

**What happened.** The POC policy makes only `/tmp` writable. Harnesses write
their config, sessions, caches or logs under `HOME` at start-up. Checked in the
images with a HOME that cannot be written:

| Harness | One-shot with HOME not writable | Its own variable that fixes it |
|---|---|---|
| Pi | fails: `EACCES mkdir ~/.pi/agent/sessions` | `PI_CODING_AGENT_DIR` |
| OpenCode | fails, **even `--version`** | `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_STATE_HOME` |
| Qwen Code | fails: `mkdir ~/.qwen` | `QWEN_HOME` |
| Codex | fails: "Permission denied (os error 13)" | `CODEX_HOME` |
| Hermes | fails: "Cannot initialize Hermes directory" | `HERMES_HOME` |
| Deep Agents | works | not needed |

**Workaround.** Each wrapper points `HOME` (or the harness's own variable:
`PI_CODING_AGENT_DIR`, `CODEX_HOME`, `HERMES_HOME`) at a directory under `/tmp`
and copies the seed config there.

**Recommendation.** Give every sandbox a writable home directory by default (for
example a per-sandbox `HOME` inside the writable area), so a harness can start
without a wrapper. Policies that want a read-only home can still say so.

### 5. `sandbox exec` does not pass the image's ENV

**What happened.** Commands started with `openshell sandbox exec` (how the
benchmark starts each lead) do not see the `ENV` lines of the image. Settings
such as `PI_CODING_AGENT_DIR` or `HERMES_HOME` were missing.

**Workaround.** `pi-run` and `hermes-run` set the same variables again.

**Recommendation.** Pass the image's environment to `exec`, as `docker exec`
does, or document clearly that it is not passed.

### 6 and 7. The grant and the provider need each harness's exact program

**What happened.** The program check compares the calling executable's path
with the grant's `program`, exactly. Every harness calls from a different
program, and some of those paths are long and platform-specific:

| Harness | Calling program |
|---|---|
| Pi, Qwen Code | `/usr/local/bin/node` |
| Codex | `/usr/local/lib/node_modules/@openai/codex/node_modules/@openai/codex-linux-arm64/vendor/aarch64-unknown-linux-musl/bin/codex` |
| OpenCode | `/usr/local/lib/node_modules/opencode-ai/bin/opencode.exe` |
| Deep Agents | `/usr/bin/python3.11` |
| Hermes | `/opt/python/bin/python3.14` |

The provider profile needs the same list, so that the supervisor swaps in the
real API key for each harness's model calls.

We found the paths by setting a wrong `--grant-program` and reading the refusal,
which names the real caller. A user should not have to do that. The Codex path
also changes between arm64 and amd64.

**Workaround.** `run.sh` hard-codes the program per harness, and
`provider-harness-nvidia.yaml` lists all five.

**Recommendation.**

- **Harness presets.** A small, versioned table (in docs or in the CLI) that
  maps a harness name to its calling program on each platform, so the user
  writes `--grant-harness codex` instead of a path.
- **A discovery aid.** `openshell sandbox exec <name> -- <harness>` once, then
  report the programs that called the gateway or the model, as a suggested
  grant and provider list.
- **Keep the rule of one program per tree.** The benchmark did not need more:
  with `until_file`, children never call the gateway, so only the lead's
  harness does. Node-based harnesses (Pi and Qwen Code) remain
  indistinguishable from any other Node program, which the RFC already notes.

### 8. Deep Agents has no real filesystem by default

**What happened.** The Deep Agents children answered that `/data` was empty. By
default its file tools work on a virtual, in-memory filesystem and it has no
shell, so the agent never saw the sandbox's disk.

**Workaround.** `deepagents_lead.py` passes
`backend=LocalShellBackend(root_dir="/", virtual_mode=False, inherit_env=True)`,
which gives the agent the real disk and a shell.

**Recommendation.** Document it in the Deep Agents snippet. Deep Agents' own
documentation recommends `LocalShellBackend` only inside a sandbox; OpenShell
is that sandbox, which is a good story to tell.

### 9. Harnesses with their own sandbox

**What happened.** Codex runs commands in its own sandbox (bubblewrap on Linux)
and asks for approval by default. Nested inside OpenShell, that is redundant
and can block commands and network calls. `codex exec` already turns approvals
off, but its own sandbox still applies unless configured otherwise. Whether
bubblewrap even works inside an OpenShell sandbox was not tested. The other
harnesses have no OS sandbox of their own; they only have approval prompts,
which they decline when nobody can answer:

| Harness | Without a "yes" mode | What we used |
|---|---|---|
| Pi | no shell prompts | `--no-approve` (skips project-local config) |
| OpenCode | "ask" rules are auto-rejected in `run` | `--auto` |
| Qwen Code | tools needing approval are declined | `--yolo` |
| Codex | approvals off in `exec`; its own sandbox on | `sandbox_mode = "danger-full-access"` |
| Hermes | `-z` already turns approvals off (a small floor remains) | `--yolo` |
| Deep Agents | approvals only if `interrupt_on` is set | none |

**Workaround.** `config.toml` sets `sandbox_mode = "danger-full-access"` and
`approval_policy = "never"`. Other harnesses ran with their "yes to everything"
mode (`--yolo` for Qwen Code and Hermes, `--auto` for OpenCode,
`--no-approve` for Pi).

**Recommendation.** Document per harness that OpenShell is the sandbox and the
harness's own sandbox and prompts should be off. The settings sound alarming
("danger-full-access", "yolo"), so the docs should explain why it is safe here:
the OpenShell policy still applies to everything the harness does.

### 10. Harness start-up network calls

**What happened.** Hermes fetches `https://models.dev/api.json` at start-up and
checks GitHub for a newer release. The sandbox policy does not allow those
hosts, so they are denied.

**Workaround.** None needed; Hermes carries on.

**Recommendation.** Nothing to change. This is the policy doing its job, and the
denials are visible in the sandbox logs.

### 11. Results only exist while a child runs

**What happened.** A child's result lives in its files, and its files go when it
exits. A harness in one-shot mode exits as soon as it answers.

**Workaround.** `run-child` runs the harness, saves its reply to
`/tmp/result.txt` (written whole, then renamed) and keeps the sandbox running.
The parent's `wait` with `until_file` and `return_file` returns once the file
exists, with its content in the reply, and the parent then stops the child. The
child never calls the gateway.

Reading the file in `wait` matters for harnesses that send tool calls in
parallel. With a separate `get_file`, Codex and Deep Agents sent it together
with `stop_sandbox`, the stop won and the result was lost (five runs in the
first matrix). With `return_file` the crash task passes 18 of 18. Results are
capped at 256 KiB; `wait` and `get_file` say `truncated: true` beyond that.

**Recommendation.** Keep this design: the gateway stores nothing and the grant's
single program still covers the tree. Two follow-ups:

- ship `run-child` (or the same behaviour) with OpenShell, so images do not need
  it;
- reap a child that has written its result and not been read for a long time,
  so forgotten children do not run for the full linger time.

## What "out of the box" would look like

With a writable HOME and the image's ENV in OpenShell (4 and 5), a short
`wait` (2) and a shipped `run-child` (3 and 11), a harness would need two things
from the user:

1. **The MCP entry** (problem 1): a flag or environment variable for OpenCode,
   Qwen Code and Codex, a config file for Pi and Hermes, a few lines of code for
   Deep Agents.
2. **The harness preset** for the grant and the provider (problems 6 and 7).

Everything else in today's wrappers (setting HOME, repeating the image's
environment, raising timeouts, closing stdin) would go away.

## Recommended next steps

1. Short default `wait` timeout with "call again" in the tool description
   (problem 2), then rerun the benchmark with every harness's default timeout.
2. Ship `run-child` with OpenShell and add the `stdin: "closed"` opt-in
   (problem 3).
3. A per-sandbox writable HOME and the image's ENV for `exec` (problems 4
   and 5).
4. Harness presets for `--grant-program` and the provider (problems 6 and 7).
5. Try one harness with its MCP entry delivered as a provider file (problem 1).
6. Rerun the benchmark with the wrappers removed, to prove each fix.
