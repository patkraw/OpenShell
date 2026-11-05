# 00 · Setup

**Goal:** build OpenShell, start a dev gateway with child sandboxes turned
on, and build the agent images the other tutorials use.

## 1. Build the CLI and the gateway

From the repository root:

```shell
cargo build -p openshell-cli -p openshell-gateway
```

The binaries land in `target/debug/`. The shell setup in the
[index](README.md) puts them first on `PATH`.

## 2. Start a dev gateway with child sandboxes on

Child sandboxes are experimental and off by default. The dev gateway task
turns them on when you ask:

```shell
OPENSHELL_EXPERIMENTAL_CHILD_SANDBOXES=1 mise run gateway:docker
```

The task builds the supervisor and sandbox images, writes
`.cache/gateway-docker/gateway.toml`, and serves plaintext HTTP on
`127.0.0.1:18080` as the gateway `docker-dev`. Leave it running and open a
second terminal for the tutorials.

The variable adds one line to the generated config:

```toml
[openshell.gateway]
experimental_enable_child_sandboxes = true
```

On any other gateway, set it in its TOML config and restart it. There is no
environment variable or CLI flag for the gateway itself. The same setting
turns on the `/mcp` endpoint. To change how many children one root may have,
set `max_concurrent_children` next to it (default 10, at most 1000; 0 allows
none).

Check that the CLI reaches the gateway:

```shell
openshell status
```

## 3. Build the agent images

All images share one base on `node:24-bookworm-slim`, with `bash`, `curl`,
`git` and `ripgrep`, user `node` and working directory `/workspace`. Only the
harness differs:

```shell
docker build --target base   -t child-base:local   examples/child-sandboxes/images
docker build --target claude -t child-claude:local examples/child-sandboxes/images
docker build --target codex  -t child-codex:local  examples/child-sandboxes/images
docker build --target pi     -t child-pi:local     examples/child-sandboxes/images
```

You need `child-base:local` for every tutorial. The harness images are for
04 and the demos. Harness state lives under `/tmp`, which every child can
write.

## 4. Create a provider to share

Tutorials 01, 02 and 05 share a provider called `my-github` with children. A
new gateway has no provider profiles, so import the GitHub one first. The
token does not need to be real; the tutorials never call GitHub:

```shell
openshell profile import -f providers/github.yaml
openshell provider create --name my-github --type github --credential GITHUB_TOKEN=not-a-real-token
```

```text
Imported 1 provider profile.
✓ Created provider my-github
```

## Troubleshooting

- **`child sandboxes are disabled on this gateway`**: the gateway started
  without the setting. Stop it and start it again with
  `OPENSHELL_EXPERIMENTAL_CHILD_SANDBOXES=1`; the task rewrites its config on
  every start, so editing the file by hand does not survive a restart.
- **`port 18080 is already in use`**: another dev gateway is running. Stop it,
  or run a second one next to it and use its name in `OPENSHELL_GATEWAY`:

  ```shell
  OPENSHELL_EXPERIMENTAL_CHILD_SANDBOXES=1 OPENSHELL_SERVER_PORT=18190 \
    OPENSHELL_DOCKER_GATEWAY_NAME=tut-dev OPENSHELL_SANDBOX_NAMESPACE=tut-dev \
    OPENSHELL_DOCKER_GATEWAY_STATE_DIR="$PWD/.cache/gateway-tut" mise run gateway:docker
  ```
- **`provider profile 'github' not found`**: import the profile first, as in
  step 4.

## What you learned

- Child sandboxes need one gateway setting, which also turns on `/mcp`.
- The tutorials use a shared base image so every child has the same layout.
