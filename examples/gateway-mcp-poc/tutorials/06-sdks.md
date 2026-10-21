# 06 · SDKs

**Goal:** use the same features from code: **OpenShell's Python SDK** (gRPC)
on the host, and the **official MCP SDKs** for Python (host) and TypeScript
(inside a sandbox).

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## Which one when

| You are writing | Use | Talks to |
|---|---|---|
| A host program, a runner, an RL environment service | **OpenShell Python SDK** | the gateway's gRPC API |
| Any program that already speaks MCP | **official MCP SDK** (Python, TypeScript, …) | `/mcp` |
| Code **inside** a sandbox | **MCP SDK** to `http://openshell.local/mcp` | `/mcp` through the door (gRPC through the door is later work) |

## 1. OpenShell's Python SDK (gRPC)

[`sdk/host_tour.py`](../sdk/host_tour.py) creates a lead with a grant, lets it
create a child, lists children, widens the grant, attaches a grant to a running
sandbox, revokes it and cleans up:

```shell
uv run --frozen python examples/gateway-mcp-poc/sdk/host_tour.py
```

```text
grant: gr-09a518ef 1 2 3
child: {"isError":false,"can_create_children":false,"depth":1,"handle":"sdk-worker","kind":"agent",...}
children: [('sdk-worker', 'agent')]
widened: 3 4
attached: gr-0ee0e814 to sdk-plain
revoked: True
deleted
```

The calls it uses:

```python
from openshell import SandboxClient

client = SandboxClient("127.0.0.1:18080")
client.create(workspace="default", spec=spec, name="sdk-lead",
              grant="max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node")
client.list_grants()                                   # -> [SandboxGrant, ...]
client.list_children("sdk-lead", workspace="default")  # -> [ChildSandboxRef, ...]
client.update_grant(grant_id, max_breadth=3, max_total=4)
client.create_grant("sdk-plain", workspace="default", program="/usr/local/bin/node", max_total=2)
client.revoke_grant(grant_id)
```

## 2. Official MCP SDK for Python, on the host

[`sdk/host_create.py`](../sdk/host_create.py) uses the `mcp` package, unchanged:

```shell
uv run --with "mcp==2.3.0" python examples/gateway-mcp-poc/sdk/host_create.py
```

```text
tools: ['create_sandbox', 'grant_list', 'grant_create', 'grant_update']
create_sandbox: ok {"handle": "sdk-lead", "has_grant": true}
grant_list: {"grants": [{"grant_id": "gr-cbbb9b35", "holder": "sdk-lead", "max_breadth": 2, "max_depth": 1, "max_total": 3, ...}]}
```

## 3. Official MCP SDK for TypeScript, inside a sandbox

[`sdk/sandbox_child.mjs`](../sdk/sandbox_child.mjs) uses
`@modelcontextprotocol/sdk` (installed in the image). It runs in the sandbox
the previous step created, creates a tool sandbox, runs a command and stops it:

`sdk-lead` takes a few seconds to start; wait until it is ready, then run the
script in it:

```shell
until openshell sandbox get sdk-lead 2>/dev/null | grep -q 'Phase: Ready'; do sleep 2; done
openshell sandbox exec sdk-lead -- node /usr/local/lib/gateway-mcp/sandbox_child.mjs
```

```text
tools: create_sandbox, wait, exec, get_file, get_output, stop_sandbox, list_children, get_limits
get_limits: ok {"can_create_children":true,"can_create_tools":true,"children_created":0,"depth":0,...}
create_sandbox: ok {"can_create_children":false,"depth":1,"handle":"sdk-tool","idle_timeout_s":600,"kind":"tool",...}
wait: ok {"done":false,"exit_code":null,"handle":"sdk-tool","phase":"SANDBOX_PHASE_READY","timed_out":false}
exec: ok {"exit_code":0,"handle":"sdk-tool","stderr":"","stdout":"hello from 480950705d36\n"}
stop_sandbox: ok {"handle":"sdk-tool","stopped":true}
```

No URL changes and no credential: the SDK just points at
`http://openshell.local/mcp`.

Clean up: `openshell sandbox delete sdk-lead`.

## What you learned

- **Host code** can use OpenShell's **Python SDK** for grants and children.
- The endpoint is **standard MCP** (Streamable HTTP, JSON responses): the
  official SDKs work **unchanged**, on the host and in a sandbox.
- Inside a sandbox, use **MCP**; gRPC through the door comes later.
