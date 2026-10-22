# Gateway MCP proof of concept

Agents create sandboxes through OpenShell. A sandbox started with a grant can
create child sandboxes, within limits, through an MCP endpoint in the gateway;
an agent on the host can create sandboxes as the user. Design:
[RFC draft](https://github.com/patkraw/OpenShell/blob/docs/gateway-mcp-rfc/patkraw/rfc/NNNN-gateway-mcp-sandbox-creation/README.md) (on branch
`docs/gateway-mcp-rfc/patkraw`). Issue:
[NVIDIA/OpenShell#4574](https://github.com/NVIDIA/OpenShell/issues/4574).

Get the code:

```shell
git clone https://github.com/patkraw/OpenShell.git && cd OpenShell && git checkout gateway-mcp-poc
```

- **[tutorials/](tutorials/README.md)**: run it yourself, one feature at a time:
  setup, the CLI, MCP inside a sandbox, child policies, asking for more, MCP
  from the host, the SDKs and tool sandboxes. Real output in every step.
- **[TUTORIAL.md](TUTORIAL.md)**: a quick tour of everything on one page.
- **[PROTOCOL.md](PROTOCOL.md)**: the protocol agreement (transport, methods,
  tools per caller, results, error codes) with real captured exchanges.

## What is where

| Part | Path | What it does |
|------|------|--------------|
| MCP surface | `crates/openshell-gateway-mcp/` | MCP protocol only: handshake, tool list, argument parsing, errors. Talks to OpenShell through the `SandboxApi` trait; tested on its own with a fake backend. |
| Gateway rules | `crates/openshell-server/src/children.rs` | Implements `SandboxApi`: grants, lineage, the prover check against the parent's policy, limits, retries, tool sandboxes, cascade stop. |
| Supervisor door | `crates/openshell-supervisor-network/src/gateway_mcp_door.rs` | Serves `http://openshell.local/mcp` in every sandbox; forwards with the sandbox's token and the calling program's path. The gateway decides every call. |
| gRPC API | `proto/openshell.proto` (`CreateChildSandbox`, `ListSandboxGrants`, ...) | The same operations as the MCP tools, for the SDKs and the CLI. Handlers in `children.rs` (`grpc_surface`) call the same rules as MCP. |
| CLI | `openshell sandbox create --grant-program ... --grant-max-depth ...`, `openshell grant create/get/list/update/revoke`, `openshell sandbox list --tree`, `openshell sandbox children` | Start a tree, add a grant to a running sandbox, manage grants, see trees and children. |
| Python SDK | `SandboxClient.create(grant=...)`, `list_grants`, `create_grant`, `update_grant`, `revoke_grant`, `list_children` | The same from code, over gRPC. |

The MCP endpoint is on by default. It is **startup configuration**: turn it off
with `enabled = false` in the gateway's `[openshell.gateway.sandbox_mcp]` TOML
section, `OPENSHELL_GATEWAY_MCP_ENABLED=false`, or `--sandbox-mcp-enabled=false`
(flag and environment override the file), then **restart the gateway**. Grants
and the CLI keep working while it is off.

## Test it

Unit tests (no Docker needed):

```shell
cargo test -p openshell-gateway-mcp
cargo test -p openshell-server --lib children
cargo test -p openshell-supervisor-network --lib gateway_mcp_door
```

End to end, with a development gateway and the Docker driver (macOS or Linux,
Docker running):

```shell
mise run gateway:docker                     # builds the gateway and supervisor image, serves on :18080
docker build -t pi-agent:local -f examples/gateway-mcp-poc/Dockerfile.pi examples/gateway-mcp-poc
cargo build -p openshell-cli
examples/gateway-mcp-poc/run-checks.sh      # end-to-end checks; see Results below
examples/gateway-mcp-poc/check-child-results.sh  # wait until_file, get_file
```

Try it by hand:

```shell
# Start a lead that may create two workers
target/debug/openshell -g docker-dev sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h \
  --no-tty --detach -- sleep 3600

# From inside the lead, through the door (no credential)
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs tools/list
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs create_sandbox '{"name":"w1","command":["sleep","300"]}'

# From the host, as the user
examples/gateway-mcp-poc/user-mcp.py grant_list
examples/gateway-mcp-poc/user-mcp.py create_sandbox name=box1 image=pi-agent:local 'command=["sleep","300"]'
```

Point any MCP client at it: inside a sandbox the URL is
`http://openshell.local/mcp`; on the host it is `http://127.0.0.1:18080/mcp`.

SDK examples: `sdk/host_create.py` (official Python MCP SDK, on the host) and
`sdk/sandbox_child.mjs` (official TypeScript MCP SDK, inside a sandbox). See the
tutorial.

A real Pi lead creating a Pi worker: `run-pi-demo.sh` (needs a model provider,
see the script header).

## Grant options

`openshell sandbox create` takes typed `--grant-*` flags, the same options as
`openshell grant create` (which uses them without the `grant-` prefix).
`--grant-program` is required as soon as any grant flag is given; the others
default to `1`, `2`, `3`, `4h` and `narrow`.

| Flag | Meaning |
|-----|---------|
| `--grant-max-depth` | Levels of agents below this sandbox. `1`: its children cannot create agents. Tool sandboxes ignore depth and count only toward breadth and total. |
| `--grant-max-breadth` | Children per parent. |
| `--grant-max-total` | Sandboxes in the tree, including this one. |
| `--grant-expires` | e.g. `4h`. |
| `--grant-program` | Executable allowed to call the gateway MCP from this tree, e.g. `/usr/local/bin/node` for Pi; the gateway refuses other programs with `PROGRAM_NOT_ALLOWED`. |
| `--grant-child-policy` | `narrow` (default): agents write their children's policies (network, filesystem, process, Landlock) within their own policy. `inherit`: every child gets this sandbox's policy unchanged. |

The sandbox's policy (`--policy`) is the ceiling for its children, and each child's policy is the ceiling for its own children, so each level can only narrow. Without one, the
restrictive default policy is used; a tree never relies on an image's own policy.

Later changes to a child's policy are held to the same rule: a user `policy set`, a supervisor sync, or an approved (or auto-approved) proposal that would take a child beyond its parent's current policy is refused with `EXCEEDS_CEILING`.

Grants belong to the user who set them. Only that user, or a Workspace Admin of
the holder's workspace, sees a grant in `grant list`/`grant get` and can update,
revoke or replace it. To change `expires`, `program` or `child_policy`, revoke
the grant and run `grant create` on the same sandbox: the new grant covers the
running tree, and its running sandboxes count toward the new total.

Limits and retries hold under concurrency: depth, breadth and total are checked
and charged in one step when a child slot is reserved, and a `request_id` is
scoped to the parent and reserved before the child launches, so a concurrent
duplicate gets `IN_PROGRESS` (or, later, the first child's handle) and never a
second child.

## Results

**74/74 end-to-end checks pass at commit `a1b94e583`** (dev gateway, Docker
driver, macOS arm64, 2026-10-15). Unit tests at the same commit: 2,049
`openshell-server`, all `openshell-gateway-mcp` and `openshell-cli`, and 190
Python. One `openshell-supervisor-network` proxy test
(`mediated_connect_keeps_workload_bytes_read_with_the_synthesized_header`)
fails on this machine with and without the proof of concept's changes.

**Child results (`wait` with `until_file`): 74/74 plus 17/17 in
`check-child-results.sh`** (2026-10-21, same setup). `run-pi-result-demo.sh` runs
it with real Pi agents: `run-child` saves the worker's answer, and the lead waits
for the file, reads it with `get_file` and stops the worker. The worker never
calls the gateway.

**Harness matrix: 6/6 harnesses pass the split-count benchmark** (2026-10-21):
Pi, OpenCode, Qwen Code, Codex, Deep Agents and Hermes each split a log between
two child sandboxes and combined their results. How to run it, per-harness
setup and findings: [bench/README.md](bench/README.md).

Run `examples/gateway-mcp-poc/run-checks.sh` against a dev gateway (see Test it)
to reproduce.

## Known limits of the proof of concept

- Grants and lineage live in memory; a gateway restart loses them.
- `wait`'s `until_file` is on MCP only; the gRPC `WaitChildSandbox` request does
  not take it yet.
- A child's result lives only in its files, so it is lost if the child exits or
  is stopped before the parent reads it.
- The door forwards the sandbox's session token, not a dedicated short-lived token.
- The global policy works as today (`replace`); a child create with a policy is refused while one is active. `maximum` mode from the RFC is not built.
- The door handles only plain HTTP to the gateway (the dev gateway has TLS off).
- The door forwards only MCP, not gRPC: code inside a sandbox uses MCP; the gRPC child functions are used from the host.
- Children already running are not re-checked when their parent's policy shrinks (upstream issue #3808).
- No per-sandbox rate limit on the gateway MCP yet.
