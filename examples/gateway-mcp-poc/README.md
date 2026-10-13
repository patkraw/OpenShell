# Gateway MCP proof of concept

Agents create sandboxes through OpenShell. A sandbox started with a grant can
create child sandboxes, within limits, through an MCP endpoint in the gateway;
an agent on the host can create sandboxes as the user. Design:
[RFC draft](../../rfc/NNNN-gateway-mcp-sandbox-creation/README.md) (on branch
`docs/gateway-mcp-rfc/patkraw`).

## What is where

| Part | Path | What it does |
|------|------|--------------|
| MCP surface | `crates/openshell-gateway-mcp/` | MCP protocol only: handshake, tool list, argument parsing, errors. Talks to OpenShell through the `SandboxApi` trait; tested on its own with a fake backend. |
| Gateway rules | `crates/openshell-server/src/children.rs` | Implements `SandboxApi`: grants, lineage, the prover check against the ceiling, limits, retries, tool sandboxes, cascade stop. |
| Supervisor door | `crates/openshell-supervisor-network/src/gateway_mcp_door.rs` | Serves `http://openshell.local/mcp` in each sandbox; checks the calling program against the door rule and forwards with the sandbox's token. |
| CLI | `openshell sandbox create --grant ...` | Starts a sandbox that may create children. |

The MCP endpoint is on by default. Set `OPENSHELL_GATEWAY_MCP_ENABLED=false`
on the gateway to turn it off; grants and the CLI keep working.

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
examples/gateway-mcp-poc/run-checks.sh      # 36 checks: grants, door, refusals, retries, tool sandboxes, host user, cascade stop
```

Try it by hand:

```shell
# Start a lead that may create two workers
target/debug/openshell -g docker-dev sandbox create --name lead --from pi-agent:local \
  --policy examples/gateway-mcp-poc/lead.yaml \
  --grant max_depth=1,max_breadth=2,max_total=3,expires=1h,program=/usr/local/bin/node \
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

A real Pi lead creating a Pi worker: `run-pi-demo.sh` (needs a model provider,
see the script header).

## Grant options

`--grant` takes comma-separated `key=value` pairs:

| Key | Meaning |
|-----|---------|
| `max_depth` | Levels below this sandbox. `1`: its children cannot create children. |
| `max_breadth` | Children per parent. |
| `max_total` | Sandboxes in the tree, including this one. |
| `expires` | e.g. `4h`. |
| `program` | Executable allowed to use the door, e.g. `/usr/local/bin/node` for Pi. |
| `child_policy` | `narrow` (default): agents write their children's network rules within the ceiling. `inherit`: every child gets this sandbox's policy unchanged. |

## Known limits of the proof of concept

- Grants and lineage live in memory; a gateway restart loses them.
- The door forwards the sandbox's session token, not a dedicated short-lived token.
- The global policy works as today (`replace`); a child create with a policy is refused while one is active. `maximum` mode from the RFC is not built.
- The door handles only plain HTTP to the gateway (the dev gateway has TLS off).
- The child-sandbox gRPC service from the RFC is not built; the MCP surface calls the gateway's rules directly.
