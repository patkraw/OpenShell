# 05 · MCP from the host

**Goal:** call the gateway MCP from your own machine, as **you** (the user),
and connect an agent such as Pi, Claude Code or Codex to it.

Setup: [00](00-setup.md), plus the shell setup from the [index](README.md).

## How it works

- On the host the endpoint is the **gateway URL + `/mcp`**:
  `http://127.0.0.1:18080/mcp` for the dev gateway.
- The caller is **the user** (your login; on the dev gateway, the local
  unauthenticated user). No grant is needed: it can do what you can do with the
  CLI.
- Users see **`create_sandbox`** (top-level sandboxes) and the **grant tools**.
- The agent on the host is **not** sandboxed by this; only the sandboxes it
  creates are.

## 1. Plain JSON-RPC with curl

```shell
curl -s http://127.0.0.1:18080/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"curl","version":"0"}}}'
echo
```

```text
{"id":1,"jsonrpc":"2.0","result":{"capabilities":{"tools":{"listChanged":false}},"instructions":"Create and manage OpenShell sandboxes. Call get_limits to see what you may create.","protocolVersion":"2025-11-25","serverInfo":{"name":"openshell-gateway","version":"0.0.0"}}}
```

```shell
curl -s http://127.0.0.1:18080/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  | python3 -c 'import json,sys; print([t["name"] for t in json.load(sys.stdin)["result"]["tools"]])'
```

```text
['create_sandbox', 'grant_list', 'grant_create', 'grant_update']
```

Create a sandbox **with a grant** (over MCP the grant is one string of
`key=value` pairs; the CLI uses `--grant-*` flags instead):

```shell
curl -s http://127.0.0.1:18080/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"create_sandbox","arguments":{"name":"hostlead","image":"pi-agent:local","command":["sleep","3600"],"grant":"max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node"}}}'
echo
```

```text
{"id":3,"jsonrpc":"2.0","result":{"content":[{"text":"{\"handle\":\"hostlead\",\"has_grant\":true}","type":"text"}],"isError":false,"structuredContent":{"handle":"hostlead","has_grant":true}}}
```

## 2. The same with `user-mcp.py`

[`user-mcp.py`](../user-mcp.py) is a tiny client: `tool key=value ...`.

```shell
examples/gateway-mcp-poc/user-mcp.py grant_list
```

```text
{"isError": false, "grants": [{"grant_id": "gr-ecfd434f", "holder": "hostlead", "max_breadth": 2, "max_depth": 1, "max_total": 3, "program": "/usr/local/bin/node", "total": 1, "user": "unauthenticated-local-dev", "workspace": "default"}]}
```

Attach a grant to a sandbox that has none, and widen another. Use the
`grant_id` that `grant_list` printed for `hostlead` (yours differs):

```shell
GRANT_ID=$(examples/gateway-mcp-poc/user-mcp.py grant_list | python3 -c 'import json,sys; print(next(g["grant_id"] for g in json.load(sys.stdin)["grants"] if g["holder"]=="hostlead"))')
examples/gateway-mcp-poc/user-mcp.py create_sandbox name=box2 image=pi-agent:local 'command=["sleep","3600"]'
examples/gateway-mcp-poc/user-mcp.py grant_create sandbox=box2 grant=max_depth=1,max_breadth=1,max_total=2,expires=1h,program=/usr/local/bin/node
examples/gateway-mcp-poc/user-mcp.py grant_update grant_id="$GRANT_ID" max_total=5
```

```text
{"isError": false, "handle": "box2", "has_grant": false}
{"isError": false, "grant_id": "gr-c66acdbd", "holder": "box2", "max_breadth": 1, "max_depth": 1, "max_total": 2, ...}
{"isError": false, "grant_id": "gr-ecfd434f", "max_breadth": 2, "max_depth": 1, "max_total": 5}
```

Sandbox-only tools are refused for users:

```shell
examples/gateway-mcp-poc/user-mcp.py get_limits
```

```text
{"isError": true, "error": "NOT_ALLOWED", "message": "'get_limits' is not available to you"}
```

Clean up: `openshell sandbox delete hostlead; openshell sandbox delete box2`.

## 3. Connect an agent

Any MCP client that speaks **Streamable HTTP** can use the endpoint. Configure
it, then ask the agent to "create an OpenShell sandbox".

**Claude Code** (on the host):

```shell
claude mcp add --transport http openshell http://127.0.0.1:18080/mcp
```

**Pi**, in `~/.pi/agent/mcp.json`:

```json
{ "mcpServers": { "openshell": { "url": "http://127.0.0.1:18080/mcp" } } }
```

**Codex**, in `~/.codex/config.toml` (check your Codex version's MCP docs for
HTTP servers):

```toml
[mcp_servers.openshell]
url = "http://127.0.0.1:18080/mcp"
```

**Inside a sandbox** the same entry uses `http://openshell.local/mcp`; the
agent image already carries it ([`pi-seed/mcp.json`](../pi-seed/mcp.json)).

These configurations are shown, not run, in this tutorial.

## What you learned

- On the host, **`/mcp` acts as you**: create top-level sandboxes and manage
  grants, no grant needed.
- It is **MCP's Streamable HTTP transport with JSON responses**: curl, a
  15-line script or any MCP client works.
- One config line connects **Claude Code, Pi or Codex**.
