# Gateway MCP tutorials

**Goal:** run the gateway MCP proof of concept yourself and try every feature,
from the CLI, from inside a sandbox (MCP), from the host (MCP) and from the
SDKs.

Each tutorial is short, copy-pasteable and shows real output from a dev
gateway with the Docker driver. Start with **00**; the rest can be done in any
order.

| # | Tutorial | You will |
|---|---|---|
| 00 | [Setup](00-setup.md) | build, start the dev gateway, build the agent image |
| 01 | [CLI: your first tree](01-cli-first-tree.md) | start a sandbox with a grant, change and revoke grants, attach a grant later, cascade delete |
| 02 | [MCP inside a sandbox](02-mcp-inside-sandbox.md) | call every child tool from inside a sandbox, and see each refusal |
| 03 | [Child policies](03-child-policies.md) | narrow network and filesystem, see `EXCEEDS_CEILING`, `inherit`, parents with MCP rules, grandchildren |
| 04 | [Asking for more](04-asking-for-more.md) | the proposal flow end to end, and later policy changes to a child |
| 05 | [MCP from the host](05-mcp-from-host.md) | call `/mcp` with curl, use the grant tools, connect Pi, Claude Code or Codex |
| 06 | [SDKs](06-sdks.md) | OpenShell's Python SDK (gRPC), and the official MCP SDKs for Python and TypeScript |
| 07 | [Tool sandboxes](07-tool-sandboxes.md) | run commands in tool sandboxes, idle stop, tools at the bottom of a tree |

**Shell setup used in every tutorial** (run it once per terminal, from the
repository root):

```shell
export PATH="$PWD/target/debug:$PATH" OPENSHELL_GATEWAY=docker-dev
mcp() { openshell sandbox exec "$1" -- node /usr/local/lib/gateway-mcp/mcp-client.mjs "${@:2}"; }
```

- `openshell` is the CLI you build in 00.
- `mcp <sandbox> <tool> '<json>'` runs a tiny MCP client **inside** that
  sandbox, the way an agent would call the gateway.

The design is in the RFC (`rfc/NNNN-gateway-mcp-sandbox-creation`); the wire
format is in [PROTOCOL.md](../PROTOCOL.md).
