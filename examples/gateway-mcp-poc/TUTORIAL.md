# Tutorial: create sandboxes from the CLI, MCP and SDKs

This tutorial walks through the gateway MCP proof of concept end to end. You
start a sandbox that may create children, create children from inside it, do the
same from the host, and then use the official MCP SDKs from your own code.

Every command here was run against a development gateway with the Docker
driver. The protocol is described in [PROTOCOL.md](PROTOCOL.md).

This page is the quick tour. For one feature at a time, with real output in
every step, see the [tutorials](tutorials/README.md).

## 0. Set up

You need Docker, the Rust toolchain from `rust-toolchain.toml`, `mise`, and `uv`
for the Python example. From the repository root:

```shell
mise run gateway:docker      # builds the gateway and supervisor image; serves on 127.0.0.1:18080
```

In a second terminal:

```shell
cargo build -p openshell-cli
docker build -t pi-agent:local -f examples/gateway-mcp-poc/Dockerfile.pi examples/gateway-mcp-poc
```

The image is Pi's tutorial image plus two MCP clients used below: a small one
(`mcp-client.mjs`, built-in `fetch`) and the official TypeScript SDK.

## 1. CLI: start a sandbox that may create children

```shell
target/debug/openshell -g docker-dev sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant-program /usr/local/bin/node \
  --grant-max-depth 1 --grant-max-breadth 2 --grant-max-total 3 --grant-expires 1h \
  --no-tty --detach -- sleep 3600
```

The `--grant-*` flags are the only approval. They mean:

| Flag | Here | Meaning |
|---|---|---|
| `--grant-max-depth` | `1` | the lead's children cannot create agents (tool sandboxes still allowed) |
| `--grant-max-breadth` | `2` | at most two children per parent |
| `--grant-max-total` | `3` | at most three sandboxes in the tree, including the lead |
| `--grant-expires` | `1h` | the grant ends after an hour |
| `--grant-program` | `/usr/local/bin/node` | only Node may call the gateway MCP (Pi runs on Node) |
| `--grant-child-policy` | (default `narrow`) | agents write their children's policies; `inherit` gives every child the lead's policy |

`lead.yaml` becomes the **ceiling** for the lead's children: no child may get
more. Each child's policy is in turn the ceiling for its own children, so each
level can only narrow. Nothing is added to the policy: the door to the gateway
MCP is open in every sandbox, and the gateway decides every call from the grant.

## 2. From inside the sandbox: MCP through the door

Inside a sandbox, the gateway MCP is at `http://openshell.local/mcp`. Nothing to
configure and no credential: the supervisor adds the sandbox's token and the
calling program's path, and the gateway checks both.

```shell
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs tools/list
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs get_limits
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs create_sandbox '{"name":"w1","command":["sleep","300"]}'
```

The child `w1` runs the lead's template (same image, CPU and memory, no GPU). Its
depth is 1, the maximum. It reaches the gateway MCP and sees the same tools, but
the gateway refuses its `create_sandbox` with `LIMIT_REACHED` until the user
raises `max_depth`. Then the same call succeeds, with no policy change.

Things that are refused, and why:

```shell
# curl is not the grant's program: 403 PROGRAM_NOT_ALLOWED
target/debug/openshell -g docker-dev sandbox exec lead -- \
  curl -s -X POST http://openshell.local/mcp -d '{}'

# w1 has no depth left: LIMIT_REACHED
target/debug/openshell -g docker-dev sandbox exec w1 -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs create_sandbox '{"name":"g1","command":["true"]}'

# a child wider than lead.yaml: EXCEEDS_CEILING with the prover's counterexample
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs create_sandbox \
  '{"name":"wide","command":["true"],"policy":{"network_policies":{"gh":{"name":"gh","endpoints":[{"host":"api.github.com","port":443,"protocol":"rest","enforcement":"enforce","rules":[{"allow":{"method":"POST","path":"/repos/**"}}]}],"binaries":[{"path":"/usr/local/bin/node"}]}}}}'
```

### Tool sandboxes

A child with `"kind": "tool"` runs no agent. The parent runs commands in it, and
it is stopped after 10 idle minutes:

```shell
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs create_sandbox '{"name":"t1","kind":"tool"}'
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs exec '{"handle":"t1","command":["uname","-a"]}'
```

`exec` on an agent child such as `w1` is refused with `WRONG_KIND`.

### Getting results

`wait` reports when a child is ready or has exited; `get_file` reads a file from
a running child. READY does not mean an agent child has finished: have it write
its result to a temporary file and rename it into place, then poll `get_file`
until the file exists ([tutorial 02](tutorials/02-mcp-inside-sandbox.md#collect-an-agent-childs-result)):

```shell
target/debug/openshell -g docker-dev sandbox exec lead -- \
  node /usr/local/lib/gateway-mcp/mcp-client.mjs get_file '{"handle":"w1","path":"/etc/hostname"}'
```

## 3. From the host: MCP as the user

On the host the endpoint is the gateway URL plus `/mcp`. The caller is the user,
so it creates top-level sandboxes like the CLI does, and may give them a grant.
`user-mcp.py` is a tiny client (`key=value` arguments):

```shell
examples/gateway-mcp-poc/user-mcp.py grant_list
examples/gateway-mcp-poc/user-mcp.py create_sandbox name=box1 image=pi-agent:local 'command=["sleep","300"]'
```

Manage grants and see a tree with the CLI (these call the gateway's gRPC API;
only the user can change a grant, and changes apply from the next call):

```shell
target/debug/openshell -g docker-dev grant list
target/debug/openshell -g docker-dev grant create <sandbox> --program /usr/local/bin/node --max-depth 1
target/debug/openshell -g docker-dev grant update <grant-id> --max-breadth 3 --max-total 4
target/debug/openshell -g docker-dev grant revoke <grant-id>
target/debug/openshell -g docker-dev sandbox children lead
```

The same through MCP: `user-mcp.py grant_update grant_id=<id> max_breadth=3 max_total=4`.

`grant create` gives a running top-level sandbox that has no grant one, so it
can create children from then on; its current policy bounds them. A sandbox that
already holds a grant, or is itself a child, is refused.

To use it from an agent on your machine, add the URL to that agent's MCP
configuration, for example for Pi (`~/.pi/agent/mcp.json`):

```json
{ "mcpServers": { "openshell": { "url": "http://127.0.0.1:18080/mcp" } } }
```

Inside a sandbox, the same entry uses `http://openshell.local/mcp`
(`pi-seed/mcp.json`).

## 4. SDKs

The endpoint is plain MCP, so the official MCP SDKs work without changes.

### Python, on the host

[`sdk/host_create.py`](sdk/host_create.py) creates a sandbox with a grant and
lists grants, using the official `mcp` package:

```shell
uv run --with "mcp==2.3.0" python examples/gateway-mcp-poc/sdk/host_create.py
```

```text
tools: ['create_sandbox', 'grant_list', 'grant_create', 'grant_update']
create_sandbox: ok {"handle": "sdk-lead", "has_grant": true}
grant_list: {"grants": [{"grant_id": "gr-...", "holder": "sdk-lead", "max_breadth": 2, "max_depth": 1, "max_total": 3, ...}]}
```

### TypeScript, inside a sandbox

[`sdk/sandbox_child.mjs`](sdk/sandbox_child.mjs) runs inside the sandbox the
Python example created. With `@modelcontextprotocol/sdk` 1.31.0 (installed in the
image) it creates a tool sandbox, runs a command in it and stops it:

```shell
target/debug/openshell -g docker-dev sandbox exec sdk-lead -- \
  node /usr/local/lib/gateway-mcp/sandbox_child.mjs
```

```text
create_sandbox: ok {"can_create_children":false,"depth":1,"handle":"sdk-tool","kind":"tool",...}
wait: ok {"handle":"sdk-tool","phase":"SANDBOX_PHASE_READY",...}
exec: ok {"exit_code":0,"stdout":"hello from 44774938feb6\n",...}
stop_sandbox: ok {"handle":"sdk-tool","stopped":true}
```

### OpenShell's own SDK (gRPC)

The OpenShell Python SDK talks to the gateway's gRPC API, which has the same
child-sandbox and grant functions as the MCP tools, backed by the same rules.
[`sdk/host_grpc.py`](sdk/host_grpc.py) creates a sandbox with a grant, widens
the grant and lists the sandbox's children:

```shell
uv run --frozen python examples/gateway-mcp-poc/sdk/host_grpc.py
```

```text
created: grpc-lead
grant: SandboxGrant(grant_id='gr-...', holder='grpc-lead', max_depth=1, max_breadth=2, max_total=3, total=1, ...)
widened: 3 4
children: []
```

New SDK methods: `SandboxClient.create(..., grant=...)`, `list_grants()`,
`update_grant(...)`, `revoke_grant(...)`, `list_children(parent, workspace=...)`.

The gRPC functions a sandbox calls for its own children (`CreateChildSandbox`,
`WaitChildSandbox`, `ExecChildSandbox`, `GetChildSandboxFile`, `StopChildSandbox`,
`GetSandboxLimits`) exist too, but the door forwards only MCP so far, so code
inside a sandbox uses MCP.

### Rust: implement the backend yourself

`openshell-gateway-mcp` only speaks MCP. To serve it from another program, or to
test a client without a gateway, implement its `SandboxApi` trait
(`authenticate`, `access`, `call`) and mount `openshell_gateway_mcp::router`. The
crate's tests do exactly that with a fake backend.

## 5. Clean up

Deleting a sandbox stops its children first:

```shell
target/debug/openshell -g docker-dev sandbox delete lead
target/debug/openshell -g docker-dev sandbox delete sdk-lead
```

## 6. Run all checks

```shell
examples/gateway-mcp-poc/run-checks.sh
```

It checks grants (including revoke and replace), the program check, refusals,
retries and concurrent duplicates, tool sandboxes, child policies, the proposal
flow, the user on the host, and cascade stop. The current result is in the
[README](README.md#results).
