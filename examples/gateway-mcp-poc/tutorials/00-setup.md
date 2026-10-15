# 00 · Setup

**Goal:** a running dev gateway, the `openshell` CLI, and the agent image the
other tutorials use.

## What you need

- **Docker** (Docker Desktop is fine), running.
- The **Rust toolchain** from `rust-toolchain.toml`, **mise**, and **uv**.
- About 10 minutes for the first build.

## 1. Get the code

```shell
git clone https://github.com/patkraw/OpenShell.git && cd OpenShell && git checkout gateway-mcp-poc
```

Every command in these tutorials runs from this repository root.

## 2. Start the dev gateway

In a terminal of its own, from the repository root:

```shell
mise run gateway:docker
```

- Builds the gateway and the supervisor image, then serves on
  **`http://127.0.0.1:18080`** (plaintext, local only).
- It also **registers the gateway as `docker-dev`** for the CLI.
- Leave it running. Ready when you see:

```text
WARN openshell_server::cli: TLS disabled — listening on plaintext HTTP
```

The gateway MCP endpoint is on by default. It is read **at startup**: to turn it
off, set `enabled = false` under `[openshell.gateway.sandbox_mcp]` in the
gateway's TOML config (or `OPENSHELL_GATEWAY_MCP_ENABLED=false`) and restart the
gateway.

## 3. Build the CLI and the agent image

In a second terminal, from the repository root:

```shell
mise exec -- cargo build -p openshell-cli
docker build -t pi-agent:local -f examples/gateway-mcp-poc/Dockerfile.pi examples/gateway-mcp-poc
```

- `mise exec --` runs `cargo` with the repository's toolchain, so it works even
  if your shell does not activate mise.
- `pi-agent:local` is Pi's image plus two MCP clients: a tiny one
  (`mcp-client.mjs`) and the official TypeScript MCP SDK.

## 4. Set up your shell

```shell
export PATH="$PWD/target/debug:$PATH" OPENSHELL_GATEWAY=docker-dev
mcp() { openshell sandbox exec "$1" -- node /usr/local/lib/gateway-mcp/mcp-client.mjs "${@:2}"; }
```

Check it:

```shell
openshell gateway list
```

```text
  NAME        ENDPOINT                 TYPE   SOURCE  AUTH
  docker-dev  http://127.0.0.1:18080   local  user    plaintext
```

```shell
openshell sandbox list
```

```text
No sandboxes found.
```

## Troubleshooting

- **`connection refused` on 18080:** the gateway is not up yet, or stopped.
  Look at the first terminal; restart `mise run gateway:docker`.
- **Port 18080 already in use:** an old gateway is still running. Stop it
  (`lsof -nP -iTCP:18080 -sTCP:LISTEN` shows the process), then start again.
- **`docker-dev` missing from `gateway list`:** register it by hand:
  `openshell gateway add http://127.0.0.1:18080 --name docker-dev`.
- **Sandbox stuck in `Provisioning`, or "image not found":** build
  `pi-agent:local` (step 3) and check `docker images pi-agent:local`.
- **The first sandbox takes a minute:** the supervisor image is still being
  prepared. Later sandboxes start in seconds.
- **Grants are gone after a gateway restart:** the proof of concept keeps
  grants and lineage in memory. Delete old sandboxes and start again.

## Clean up

```shell
openshell sandbox list            # see what is left
openshell sandbox delete <name>   # deleting a parent also stops its children
```

Stop the gateway with Ctrl-C in its terminal.

## What you learned

- **`mise run gateway:docker`** gives you a local gateway with the gateway MCP
  on, registered as `docker-dev`.
- The tutorials use the **`openshell` CLI** you built and a **`mcp` helper**
  that calls the gateway from inside a sandbox.
