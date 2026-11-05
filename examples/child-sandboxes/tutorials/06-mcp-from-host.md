# 06 · MCP from the host

**Goal:** use the same tools from your machine: connect Claude Code, Codex or
Pi running on the host, and drive the tools from a Python program.

Setup: [00](00-setup.md), a root called `lead`
([01](01-cli-children.md)), plus the shell setup from the
[index](README.md).

`openshell mcp serve` is an MCP server over stdio. It forwards each call to
the gateway with your CLI login, so the tools act as **you**, not as a
sandbox. That changes three things:

- You see every sandbox you may see in the workspace, roots included.
- `create_sandbox` without `parent` creates a **root**. Pass `parent` to
  create a child, and `image` to pick an image without a template.
- `list` takes `parent` to show one root's children.
- The tool list does not name templates, as a root's does
  ([04](04-templates.md)); a user can list them with
  `openshell sandbox template list`.

## 1. Try it by hand

Each line on stdin is one JSON-RPC message:

```shell
{ printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"shell","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list","arguments":{"parent":"lead"}}}'
  sleep 3; } | openshell mcp serve | jq -c '{id, r: (.result.structuredContent // .result.serverInfo)}'
```

```json
{"id":1,"r":{"name":"openshell-gateway","version":"0.1.7-dev.84+g0058bd414"}}
{"id":2,"r":{"sandboxes":[{"created_at":"2026-11-05T02:43:35.200Z","id":"9618565a-…","name":"w1","parent":"lead","state":"ready"}]}}
```

The server answers calls as they finish, so replies can arrive out of order;
match them by `id`.

## 2. Connect a harness on the host

Every MCP client takes the same command. For Claude Code, in `.mcp.json` in
the project; for Pi, in `~/.pi/agent/mcp.json`:

```json
{
  "mcpServers": {
    "openshell": {
      "command": "openshell",
      "args": ["mcp", "serve"]
    }
  }
}
```

For Codex, in `~/.codex/config.toml`:

```toml
[mcp_servers.openshell]
command = "openshell"
args = ["mcp", "serve"]
```

- `openshell` must be on the client's `PATH`, or use the full path to
  `target/debug/openshell`.
- The server uses the gateway and workspace the CLI would use. Add
  `"--gateway", "docker-dev"` or `"--workspace", "team"` to `args` to pin
  them.

These configurations are shown, not run, in this tutorial. Then ask the
harness, for example: *"Create a child of lead called w2, run
`uname -a` in it, and delete it."*

## 3. Drive the tools from Python

[`host_client.py`](../host_client.py) uses the official MCP Python SDK to
create a child, run a background job, wait for it and delete the child:

```shell
uv run --with 'mcp>=2' examples/child-sandboxes/host_client.py lead
```

```text
tools: create_sandbox, list, exec, wait, delete, logs
created: py1 under lead ready
job: completed finished in f01a01c47119
deleted: py1
```

The SDK starts the server with a minimal environment, so the script passes
its own: without that, `OPENSHELL_GATEWAY` does not reach `openshell mcp
serve`, and `initialize` fails with `cannot reach the gateway`.

## Troubleshooting

- **`cannot reach the gateway`**: the server picked a different gateway than
  your shell. Add `--gateway` to `args`, or make sure `OPENSHELL_GATEWAY`
  reaches it.
- **`child sandboxes are disabled on this gateway`**: see
  [00](00-setup.md).

## Clean up

Delete the sandboxes you made, children first, as in
[01](01-cli-children.md).

## What you learned

- `openshell mcp serve` gives any MCP client the same six tools, acting as
  you.
- From the host, `create_sandbox` makes roots unless you pass `parent`.
- Any MCP SDK works; pass the environment when it starts the server.
