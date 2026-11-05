# Child sandbox tutorials

**Goal:** run child sandboxes yourself and try every feature: from the CLI,
from inside a root sandbox the way an agent calls the gateway, and from the
host through `openshell mcp serve`.

Each tutorial is short and copy-pasteable, and shows real output from a dev
gateway with the Docker driver. Your ids, times and container names differ.
Start with **00**; the rest can be done in any order, but 02 to 06 reuse the
sandboxes from 01.

| # | Tutorial | You will |
|---|---|---|
| 00 | [Setup](00-setup.md) | build, start a dev gateway with child sandboxes on, build the agent images |
| 01 | [CLI children](01-cli-children.md) | create a root and a child, list a tree, see each CLI refusal, delete in order |
| 02 | [MCP inside a root](02-mcp-inside-a-root.md) | call every tool from inside the root, retry a create, see each refusal |
| 03 | [Long work](03-long-work.md) | run background jobs, wait for them, watch progress, run workers in parallel |
| 04 | [Templates](04-templates.md) | publish a template for agents, start a child from it, see the label check |
| 05 | [Policy and providers](05-policy-and-providers.md) | change the parent's policy and watch children follow, share and remove a provider |
| 06 | [MCP from the host](06-mcp-from-host.md) | connect Claude Code, Codex or Pi on your machine, and drive the tools from Python |

**Shell setup used in every tutorial** (run it once per terminal, from the
repository root):

```shell
export PATH="$PWD/target/debug:$PATH" OPENSHELL_GATEWAY=docker-dev
mcp() {
  local root=$1 tool=$2 args=${3:-'{}'}
  openshell sandbox exec --name "$root" --no-tty -- curl -s -X POST http://openshell.local/mcp \
    -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"$tool\",\"arguments\":$args}}" \
    | jq -c '.result.structuredContent'
}
```

- `openshell` is the CLI you build in 00.
- `mcp <root> <tool> '<json>'` calls one MCP tool from **inside** that root,
  the way an agent would, and prints the reply. It needs `jq` on the host and
  `curl` in the root's image.

Two scripted demos build on these tutorials:

- [`demo-two-turns.sh`](../demo-two-turns.sh): a lead runs two turns of Claude
  Code in one worker and resumes the conversation.
- [`demo-mixed-harnesses.sh`](../demo-mixed-harnesses.sh): a lead runs Claude
  Code, Codex and Pi side by side, each from its own template.

The user guide is
[Child sandboxes](../../../docs/how-it-works/sandboxes/child-sandboxes.mdx);
the design is RFC 0021, "Sandboxed Subagents".
